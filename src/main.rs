//! hub-tui —— Orca 终端通信录 TUI 常驻服务。
//!
//! 架构(ADR 映射):
//! - ADR-1: MVU (Model-View-Update + 单 fan-in channel)
//! - ADR-2: 不引 tokio,用 std::thread + mpsc
//! - ADR-3: Unix socket 服务端 (socket.rs)
//! - ADR-5: 数据源 = orca-ide terminal list + last-status.json watch
//! - ADR-6: 双通道发现 (hub-directory.json + socket)
//! - ADR-7: send 幂等回灌
mod command;
mod db;
mod msg;
mod model;
mod render;
mod transport;
mod service;
mod shell;
mod update;
mod view;
mod socket;
use std::io::{self, stdout};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::{backend::CrosstermBackend, Terminal};

use msg::AppMsg;
use model::Model;
use shell::Shell;
use socket::SocketServer;
use service::Service;

fn main() -> io::Result<()> {
    // ADR-1: 单一 fan-in channel(所有事件源 → AppMsg → 主 loop)。
    let (tx, rx) = std::sync::mpsc::sync_channel::<AppMsg>(256);

    // ──── 终端初始化(顺序: raw → alt screen → mouse) ────
    enable_raw_mode()?;
    // Guard 紧跟 enable_raw_mode: 任何退出路径(正常/error/panic)都经 Drop 恢复终端。
    let _guard = TerminalGuard;

    let mut out = stdout();
    execute!(out, EnterAlternateScreen, crossterm::event::EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    let model = Arc::new(RwLock::new(Model::new()));
    let mut shell = Shell::new();
    let (mut svc, bootstrap) = Service::new(tx.clone());

    // 从 DB 恢复数据(fast startup paint)
    {
        let mut mdl = model.write();
        for agent in bootstrap.agents {
            mdl.directory.insert(agent.handle.clone(), agent);
        }
        mdl.groups = bootstrap.groups;
        for msg in bootstrap.messages {
            mdl.push_message(msg);
        }
        mdl.apply_config(bootstrap.config);
        let events = bootstrap_events(&svc);
        mdl.apply_events(events);
        let history = bootstrap_history(&svc);
        mdl.apply_history(history);
        let pinned = svc.db.as_ref().map(|db| db.load_pinned()).unwrap_or_default();
        mdl.apply_pinned(pinned);
        let tags = svc.db.as_ref().map(|db| db.load_tags()).unwrap_or_default();
        mdl.apply_tags(tags);
        mdl.generation += 1; // 触发 hub-directory.json 写出
    }

    // T3: 启动 Unix socket 服务端(ADR-3+ADR-6 + DB persistence)
    let _socket = match svc.db.clone() {
        Some(db) => SocketServer::start(model.clone(), db),
        None => return Err(io::Error::new(io::ErrorKind::Other, "no db for socket server")),
    };

    // T2: 启动时立即拉一次数据(ADR-5)
    // 启动时立即全量刷新(不等 5s tick): agents + status + messages
    svc.execute(vec![
        update::Cmd::RefreshAgents,
        update::Cmd::RefreshStatus,
        update::Cmd::DrainMessages,
        update::Cmd::RefreshUnread,
    ]);

    run_loop(&mut term, &rx, &model, &mut shell, &mut svc, &tx)
}

/// RAII 终端恢复守卫。Drop 时无条件执行 best-effort 清理:
/// 退出 alt screen → 关闭 mouse capture → 关闭 raw mode。
/// 放在 enable_raw_mode 之后即可覆盖所有退出路径(含 panic unwind)。
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = execute!(out, crossterm::event::DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

/// MVU 主 loop(ADR-1+ADR-2)。
fn run_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rx: &std::sync::mpsc::Receiver<AppMsg>,
    model: &Arc<RwLock<Model>>,
    shell: &mut Shell,
    svc: &mut Service,
    tx: &std::sync::mpsc::SyncSender<AppMsg>,
) -> io::Result<()> {
    let mut quit = false;
    let mut last_written_gen: u64 = 0;
    loop {
        // ──── ADR-1: drain fan-in(非阻塞 try_recv) ────
        // 拿 write lock 一次,处理所有待处理消息(update 批量)
        let mut mdl = model.write();
        while let Ok(m) = rx.try_recv() {
            let cmds = update::update(&mut mdl, shell, m);
            // 检查 quit
            if cmds.iter().any(|c| matches!(c, update::Cmd::Quit)) {
                quit = true;
                break;
            }
            svc.execute(cmds);
        }
        // 写 hub-directory.json(ADR-6)— 只在 generation 变化时写(非每帧)
        if mdl.generation != last_written_gen {
            write_directory(&mdl);
            last_written_gen = mdl.generation;
        }
        drop(mdl); // 释放 write lock,socket 线程可读

        if quit {
            return Ok(());
        }

        // ──── Tick(spinner 动画 + 周期刷新) ────
        let cmds = update::update(&mut model.write(), shell, AppMsg::Tick);
        svc.execute(cmds);

        // ──── 画帧(Node D: view::draw) ────
        {
            let mdl = model.read();
            term.draw(|f| view::draw(f, &mdl, shell))?;
            drop(mdl);
        }

        // ──── 键鼠 poll(50ms 非阻塞) ────
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    let cmds = update::update(&mut model.write(), shell, AppMsg::Key(k));
                    if cmds.iter().any(|c| matches!(c, update::Cmd::Quit)) {
                        return Ok(());
                    }
                    svc.execute(cmds);
                }
                Event::Mouse(m) => {
                    if m.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) {
                        let cmds = update::update(
                            &mut model.write(),
                            shell,
                            AppMsg::MouseLeftClick { x: m.column, y: m.row },
                        );
                        svc.execute(cmds);
                    }
                }
                Event::Resize(w, h) => {
                    let cmds = update::update(&mut model.write(), shell, AppMsg::Resize { width: w, height: h });
                    svc.execute(cmds);
                }
                _ => {}
            }
        }
    }
}

/// ADR-6: 写 hub-directory.json(agent 查 handle 发现用)。只在 generation 变化时调用。
fn write_directory(model: &Model) {
    use std::io::Write;
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let path = format!("{}/.orca/hub-directory.json", home);
    let agents: Vec<&model::Agent> = model.directory.values().collect();
    let json = serde_json::json!({
        "agents": agents.iter().map(|a| serde_json::json!({
            "handle": a.handle,
            "cwd": a.cwd,
            "title": a.title,
            "connected": a.connected,
            "source": a.source,
            "state": a.state,
            "lastOutputAt": a.last_output_at,
        })).collect::<Vec<_>>(),
        "groups": model.groups,
        "updated": now_secs(),
    });
    if let Ok(mut f) = std::fs::File::create(&path) {
        let _ = serde_json::to_writer_pretty(&mut f, &json);
    }
}

fn now_secs() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{}", secs)
}

/// 启动时从 DB 加载最近活动日志事件(若 DB 不可用则返回空)。
fn bootstrap_events(svc: &Service) -> Vec<model::Event> {
    svc.db.as_ref().map(|db| db.load_recent_events(2000)).unwrap_or_default()
}

/// 启动时从 DB 加载输入历史(若 DB 不可用则返回空)。
fn bootstrap_history(svc: &Service) -> Vec<model::HistoryEntry> {
    svc.db.as_ref().map(|db| db.load_recent_history(500)).unwrap_or_default()
}
