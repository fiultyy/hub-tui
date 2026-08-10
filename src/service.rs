//! service.rs —— IO 隔离层(ADR-2: spawn thread fire-and-forget, 产物回灌 mpsc)。
//!
//! 执行 `update::Cmd` Vec: 每 Cmd spawn std::thread, **fire-and-forget**(绝不阻塞主 loop)。
//! 产物经 mpsc 回灌成 `AppMsg`。
//!
//! 策略:
//! - `Cmd::RefreshAgents`: ADR-7 5s 间隔 + `AtomicBool` 不重叠 spawn
//! - `Cmd::RefreshStatus`: mtime poll(非 inotify, ADR-5), 变化才 parse
//! - `Cmd::WriteDirectory`: stub; 实际由 main.rs 在 apply 后直接写文件(ADR-6)

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::msg::AppMsg;
use crate::transport;

/// 终端列表刷新间隔(ADR-7: 5s)。
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// 服务层:持有 fan-in sender + 防重叠状态。
pub struct Service {
    tx: SyncSender<AppMsg>,
    /// 上次 fetch_terminals 的时间。
    last_terminal_fetch: Instant,
    /// fetch 是否正在飞(AtomicBool 防重叠 spawn)。
    terminal_fetch_in_flight: Arc<AtomicBool>,
    /// 上次观察到的 last-status.json mtime(用于 mtime poll)。
    last_status_mtime: Option<std::time::SystemTime>,
    /// 串行化 CLI spawn(防并发 orca/orchestration 进程堆积, MINOR-1)。
    cli_lock: Arc<Mutex<()>>,
}

impl Service {
    pub fn new(tx: SyncSender<AppMsg>) -> Self {
        // 初始预填 mtime,避免首次 tick 就重读
        let initial_mtime = transport::last_status_mtime();

        Self {
            tx,
            last_terminal_fetch: Instant::now() - REFRESH_INTERVAL,
            terminal_fetch_in_flight: Arc::new(AtomicBool::new(false)),
            last_status_mtime: initial_mtime,
            cli_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 执行 Cmd Vec(主 loop 每帧调用)。fire-and-forget: spawn 线程后即返。
    ///
    /// 后续 Node 会补充更多 Cmd variant(OrchestrationSend, SocketReply 等),
    /// 此处未识别的 Cmd 静默跳过。
    pub fn execute(&mut self, cmds: Vec<crate::update::Cmd>) {
        for cmd in cmds {
            match cmd {
                crate::update::Cmd::RefreshAgents => {
                    // ADR-7: 5s 间隔 + 不重叠 spawn
                    if self.last_terminal_fetch.elapsed() < REFRESH_INTERVAL {
                        continue;
                    }
                    if self.terminal_fetch_in_flight.swap(true, Ordering::SeqCst) {
                        continue;
                    }
                    self.last_terminal_fetch = Instant::now();
                    let tx = self.tx.clone();
                    let flag = Arc::clone(&self.terminal_fetch_in_flight);
                    thread::spawn(move || {
                        match transport::fetch_terminals() {
                            Ok(agents) => {
                                let _ = tx.send(AppMsg::AgentsLoaded(agents));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(e));
                            }
                        }
                        flag.store(false, Ordering::SeqCst);
                    });
                }
                crate::update::Cmd::RefreshStatus => {
                    // ADR-5: mtime poll, 变化才 parse
                    let mtime = transport::last_status_mtime();
                    if mtime == self.last_status_mtime {
                        continue;
                    }
                    self.last_status_mtime = mtime;
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        match transport::read_last_status() {
                            Ok(statuses) => {
                                let _ = tx.send(AppMsg::StatusUpdated(statuses));
                            }
                            Err(_) => {
                                // 静默: 文件可能在 stat 后被删除
                            }
                        }
                    });
                }
                crate::update::Cmd::WriteDirectory => {
                    // ADR-6: hub-directory.json 写出。
                    // service 不持有 Model 快照,由 main.rs 直接写。此处 stub。
                }
                crate::update::Cmd::OrchestrationSend { to, subject, body } => {
                    // ADR-7: spawn 执行, 结果回灌 SendOk/SendFailed
                    let tx = self.tx.clone();
                    let lock = Arc::clone(&self.cli_lock);
                    thread::spawn(move || {
                        let _guard = lock.lock();
                        match transport::orchestration_send(&to, &subject, &body) {
                            Ok(id) => {
                                let _ = tx.send(AppMsg::SendOk(id));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::SendFailed(e));
                            }
                        }
                    });
                }
                crate::update::Cmd::DrainMessages => {
                    let tx = self.tx.clone();
                    let lock = Arc::clone(&self.cli_lock);
                    thread::spawn(move || {
                        let _guard = lock.lock();
                        match transport::orchestration_check() {
                            Ok(msgs) => {
                                let _ = tx.send(AppMsg::MessagesDrained(msgs));
                            }
                            Err(_) => {} // 静默: inbox 可能不存在
                        }
                    });
                }
                crate::update::Cmd::SwitchTerminal { handle } => {
                    let lock = Arc::clone(&self.cli_lock);
                    thread::spawn(move || {
                        let _guard = lock.lock();
                        let _ = std::process::Command::new("orca-ide")
                            .args(["terminal", "switch", "--terminal", &handle])
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status();
                    });
                }
                _ => {}
            }
        }
    }
}

