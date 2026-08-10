//! hub-tui —— Orca 终端通信录 TUI 常驻服务。
//!
//! 架构(ADR 映射):
//! - ADR-1: MVU (Model-View-Update + 单 fan-in channel)
//! - ADR-2: 不引 tokio,用 std::thread + mpsc
//! - ADR-3: Unix socket 服务端 (socket.rs)
//! - ADR-5: 数据源 = orca-ide terminal list + last-status.json watch
//! - ADR-6: 双通道发现 (hub-directory.json + socket)
//! - ADR-7: send 幂等回灌
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

    // TUI 启动
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, crossterm::event::EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    let model = Arc::new(RwLock::new(Model::new()));
    let mut shell = Shell::new();
    let mut svc = Service::new(tx.clone());

    // T3: 启动 Unix socket 服务端(ADR-3+ADR-6)
    let _socket = SocketServer::start(model.clone());

    // T2: 启动时立即拉一次数据(ADR-5)
    svc.execute(vec![update::Cmd::RefreshAgents]);

    let result = run_loop(&mut term, &rx, &model, &mut shell, &mut svc, &tx);

    // restore
    execute!(term.backend_mut(), crossterm::event::DisableMouseCapture)?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    result
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
