//! service.rs —— IO 隔离层(ADR-2: spawn thread fire-and-forget, 产物回灌 mpsc)。
//!
//! 执行 `update::Cmd` Vec: 每 Cmd spawn std::thread, **fire-and-forget**(绝不阻塞主 loop)。
//! 产物经 mpsc 回灌成 `AppMsg`。
//!
//! 持久化: Service 持有 Db。启动时从 Db 加载 bootstrap 数据;
//! 运行时由 Service::persist_* 方法在主 loop 中 update 之后调用。

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::db::Db;
use crate::msg::AppMsg;
use crate::model::{Agent, OrchMessage};
use crate::transport;

/// 终端列表刷新间隔(ADR-7: 5s)。
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Agent activity retention (days).
const ACTIVITY_RETENTION_DAYS: i64 = 7;

/// 服务层:持有 fan-in sender + 防重叠状态 + SQLite Db。
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
    /// SQLite persistence (None if db open failed — TUI continues without persistence)。
    pub db: Option<Db>,
}

/// Data loaded from DB on startup for fast first paint.
pub struct DbBootstrap {
    /// Cached agents from last session (for display before first CLI poll).
    pub agents: Vec<Agent>,
    /// Persisted groups from last session.
    pub groups: HashMap<String, HashSet<String>>,
    /// Cached messages from last session.
    pub messages: Vec<OrchMessage>,
    /// 配置 key-value pairs.
    pub config: HashMap<String, String>,
}

impl Service {
    /// Create service with SQLite persistence. Returns (Service, DbBootstrap).
    /// DbBootstrap contains cached data loaded from DB for fast startup.
    pub fn new(tx: SyncSender<AppMsg>) -> (Self, DbBootstrap) {
        let initial_mtime = transport::last_status_mtime();

        let db = Db::open(None);
        let bootstrap = db.as_ref().map_or_else(
            || DbBootstrap {
                agents: Vec::new(),
                groups: HashMap::new(),
                messages: Vec::new(),
                config: HashMap::new(),
            },
            |db| {
                db.prune_activity(ACTIVITY_RETENTION_DAYS);
                DbBootstrap {
                    agents: db.get_all_agents().into_iter().map(|r| r.into_agent()).collect(),
                    groups: db.get_groups()
                        .into_iter()
                        .map(|(k, v)| (k, v.into_iter().collect()))
                        .collect(),
                    messages: db.get_recent_messages(500),
                    config: db.get_all_config(),
                 }
            },
        );

        let svc = Self {
            tx,
            last_terminal_fetch: Instant::now() - REFRESH_INTERVAL,
            terminal_fetch_in_flight: Arc::new(AtomicBool::new(false)),
            last_status_mtime: None,
            cli_lock: Arc::new(Mutex::new(())),
            db,
        };

        (svc, bootstrap)
    }

    /// Persist agents to DB (call from main loop after AgentsLoaded is processed).
    pub fn persist_agents(&self, agents: &[Agent]) {
        let Some(ref db) = self.db else { return };
        let now = now_iso();
        for a in agents {
            db.upsert_agent_activity(
                &a.handle,
                &a.cwd,
                a.title.as_deref(),
                a.connected,
                a.state.as_deref(),
                a.source.as_deref(),
                &now,
            );
        }
    }

    /// Persist messages to DB (call from main loop after MessagesDrained is processed).
    pub fn persist_messages(&self, msgs: &[OrchMessage]) {
        let Some(ref db) = self.db else { return };
        for msg in msgs {
            db.insert_message(msg, false);
        }
    }

    /// Persist a locally-sent message to DB.
    pub fn persist_local_message(&self, msg: &OrchMessage) {
        if let Some(ref db) = self.db {
            db.insert_message(msg, true);
        }
    }

    /// Persist group join to DB.
    pub fn persist_group_join(&self, name: &str, handle: &str) {
        if let Some(ref db) = self.db {
            db.join_group(name, handle);
        }
    }

    /// Persist group leave to DB.
    pub fn persist_group_leave(&self, name: &str, handle: &str) {
        if let Some(ref db) = self.db {
            db.leave_group(name, handle);
        }
    }

    /// 执行 Cmd Vec(主 loop 每帧调用)。fire-and-forget: spawn 线程后即返。
    pub fn execute(&mut self, cmds: Vec<crate::update::Cmd>) {
        for cmd in cmds {
            match cmd {
                crate::update::Cmd::RefreshAgents => {
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
                            Err(_) => {}
                        }
                    });
                }
                crate::update::Cmd::RefreshUnread => {
                    let tx = self.tx.clone();
                    let lock = Arc::clone(&self.cli_lock);
                    thread::spawn(move || {
                        let _guard = lock.lock();
                        match transport::orchestration_inbox_unread() {
                            Ok(counts) => {
                                let _ = tx.send(AppMsg::UnreadUpdated(counts));
                            }
                            Err(_) => {}
                        }
                    });
                }
                crate::update::Cmd::WriteDirectory => {
                    // Stub: main.rs writes hub-directory.json directly.
                }
                crate::update::Cmd::OrchestrationSend { to, subject, body } => {
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
                        // 拉 inbox 全量(hub-tui 发出的消息历史)
                        match transport::orchestration_check() {
                            Ok(msgs) => {
                                let _ = tx.send(AppMsg::MessagesDrained(msgs));
                            }
                            Err(_) => {}
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
                crate::update::Cmd::PersistAgents(agents) => {
                    self.persist_agents(&agents);
                }
                crate::update::Cmd::PersistMessages(msgs) => {
                    self.persist_messages(&msgs);
                }
                crate::update::Cmd::PersistActivityEvent(ev) => {
                    if let Some(db) = &self.db {
                        db.insert_event(&ev);
                    }
                }
                crate::update::Cmd::PersistHistoryEntry(text) => {
                    if let Some(db) = &self.db {
                        db.insert_history(&text);
                    }
                }
                crate::update::Cmd::PersistPinAdd { handle } => {
                    if let Some(db) = &self.db {
                        db.upsert_pinned(&handle);
                    }
                }
                crate::update::Cmd::PersistPinRemove { handle } => {
                    if let Some(db) = &self.db {
                        db.remove_pinned(&handle);
                    }
                }
                crate::update::Cmd::PersistTagAdd { handle, tag } => {
                    if let Some(db) = &self.db {
                        db.upsert_tag(&handle, &tag);
                    }
                }
                crate::update::Cmd::PersistTagRemove { handle, tag } => {
                    if let Some(db) = &self.db {
                        db.remove_tag(&handle, &tag);
                    }
                }
                crate::update::Cmd::PersistSnippet { name, text } => {
                    if let Some(db) = &self.db {
                        db.upsert_snippet(&name, &text);
                    }
                }
                crate::update::Cmd::RemoveSnippet { name } => {
                    if let Some(db) = &self.db {
                        db.remove_snippet(&name);
                    }
                }
                crate::update::Cmd::PersistAlertRule(rule) => {
                    if let Some(db) = &self.db {
                        let id = db.upsert_alert_rule(rule.rule_type.as_str(), &rule.value);
                        // 更新内存中的 id(DB 分配的)
                        // NOTE: 这里无法更新 model, 但 id=0 的规则仍能匹配(check 不依赖 id)
                    }
                }
                crate::update::Cmd::RemoveAlertRule { id } => {
                    if let Some(db) = &self.db {
                        db.remove_alert_rule(id);
                    }
                }
                crate::update::Cmd::PersistMacro(m) => {
                    if let Some(db) = &self.db {
                        db.upsert_macro(&m.name, &m.key_events_json);
                    }
                }
                crate::update::Cmd::RemoveMacro { name } => {
                    if let Some(db) = &self.db {
                        db.remove_macro(&name);
                    }
                }
                crate::update::Cmd::PersistView { name, json } => {
                    if let Some(db) = &self.db {
                        db.upsert_saved_view(&name, &json);
                    }
                }
                crate::update::Cmd::RemoveView { name } => {
                    if let Some(db) = &self.db {
                        db.remove_saved_view(&name);
                    }
                }
                crate::update::Cmd::PersistNote { handle, text } => {
                    if let Some(db) = &self.db {
                        db.upsert_note(&handle, &text);
                    }
                }
                crate::update::Cmd::RemoveNote { handle } => {
                    if let Some(db) = &self.db {
                        db.remove_note(&handle);
                    }
                }
                crate::update::Cmd::ExportSettings { path, bundle } => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let json = match serde_json::to_string_pretty(&bundle) {
                            Ok(j) => j,
                            Err(e) => { let _ = tx.send(AppMsg::ExportFailed { reason: e.to_string() }); return; }
                        };
                        let count = bundle.config.len() + bundle.tags.len() + bundle.snippets.len()
                            + bundle.macros.len() + bundle.saved_views.len() + bundle.pinned.len()
                            + bundle.alert_rules.len() + bundle.notes.len();
                        match std::fs::write(&path, json) {
                            Ok(_) => { let _ = tx.send(AppMsg::ExportOk { path, count }); }
                            Err(e) => { let _ = tx.send(AppMsg::ExportFailed { reason: e.to_string() }); }
                        }
                    });
                }
                crate::update::Cmd::ImportSettings { path } => {
                    let tx = self.tx.clone();
                    let db = self.db.clone();
                    thread::spawn(move || {
                        let data = match std::fs::read_to_string(&path) {
                            Ok(d) => d,
                            Err(e) => { let _ = tx.send(AppMsg::ImportFailed { reason: e.to_string() }); return; }
                        };
                        let bundle: crate::model::ExportBundle = match serde_json::from_str(&data) {
                            Ok(b) => b,
                            Err(e) => { let _ = tx.send(AppMsg::ImportFailed { reason: e.to_string() }); return; }
                        };
                        // Clear + re-insert to DB
                        if let Some(db) = &db {
                            db.clear_user_data();
                            for (k, v) in &bundle.config { db.set_config(k, v); }
                            for (h, tags) in &bundle.tags { for t in tags { db.upsert_tag(h, t); } }
                            for (n, t) in &bundle.snippets { db.upsert_snippet(n, t); }
                            for m in &bundle.macros { db.upsert_macro(&m.name, &m.key_events_json); }
                            for (n, snap) in &bundle.saved_views {
                                if let Ok(json) = serde_json::to_string(snap) { db.upsert_saved_view(n, &json); }
                            }
                            for h in &bundle.pinned { db.upsert_pinned(h); }
                            for r in &bundle.alert_rules { db.upsert_alert_rule(r.rule_type.as_str(), &r.value); }
                            for (h, note) in &bundle.notes { db.upsert_note(h, note); }
                            for (n, e) in &bundle.aliases { db.upsert_alias(n, e); }
                            for (k, c) in &bundle.hotkeys { db.upsert_hotkey(k, c); }
                            for h in &bundle.watched { db.upsert_watched(h); }
                            for (n, b) in &bundle.templates { db.upsert_template(n, b); }
                        }
                        let _ = tx.send(AppMsg::ImportOk { path, bundle });
                    });
                }
                crate::update::Cmd::PersistAlias { name, expansion } => {
                    if let Some(db) = &self.db {
                        db.upsert_alias(&name, &expansion);
                    }
                }
                crate::update::Cmd::RemoveAlias { name } => {
                    if let Some(db) = &self.db {
                        db.remove_alias(&name);
                    }
                }
                crate::update::Cmd::PersistHotkey { key, command } => {
                    if let Some(db) = &self.db {
                        db.upsert_hotkey(&key, &command);
                    }
                }
                crate::update::Cmd::RemoveHotkey { key } => {
                    if let Some(db) = &self.db {
                        db.remove_hotkey(&key);
                    }
                }
                crate::update::Cmd::PersistWatchAdd { handle } => {
                    if let Some(db) = &self.db { db.upsert_watched(&handle); }
                }
                crate::update::Cmd::PersistWatchRemove { handle } => {
                    if let Some(db) = &self.db { db.remove_watched(&handle); }
                }
                crate::update::Cmd::PersistTemplate { name, body } => {
                    if let Some(db) = &self.db { db.upsert_template(&name, &body); }
                }
                crate::update::Cmd::RemoveTemplate { name } => {
                    if let Some(db) = &self.db { db.remove_template(&name); }
                }
                crate::update::Cmd::PersistGroupJoin { name, handle } => {
                    self.persist_group_join(&name, &handle);
                }
                crate::update::Cmd::PersistGroupLeave { name, handle } => {
                    self.persist_group_leave(&name, &handle);
                }
                crate::update::Cmd::TerminalSend { handle, text } => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        match crate::transport::terminal_send(&handle, &text) {
                            Ok(n) => { let _ = tx.send(AppMsg::InjectOk(n)); }
                            Err(e) => { let _ = tx.send(AppMsg::InjectFailed(e)); }
                        }
                    });
                }
                crate::update::Cmd::CloseTerminal { handle } => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        match crate::transport::terminal_close(&handle) {
                            Ok(()) => { let _ = tx.send(AppMsg::Info(format!("closed {handle}"))); }
                            Err(e) => { let _ = tx.send(AppMsg::Error(e)); }
                        }
                    });
                }
                crate::update::Cmd::RenameTerminal { handle, new_title } => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        match crate::transport::terminal_rename(&handle, &new_title) {
                            Ok(()) => { let _ = tx.send(AppMsg::Info(format!("renamed to {new_title}"))); }
                            Err(e) => { let _ = tx.send(AppMsg::Error(e)); }
                        }
                    });
                }
                crate::update::Cmd::ReadTerminal { handle } => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        match crate::transport::terminal_read_output(&handle) {
                            Ok(text) => { let _ = tx.send(AppMsg::TerminalOutput(text)); }
                            Err(e) => { let _ = tx.send(AppMsg::Error(e)); }
                        }
                    });
                }
                crate::update::Cmd::RefreshWorktreePs => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        match crate::transport::fetch_worktree_ps() {
                            Ok(entries) => { let _ = tx.send(AppMsg::WorktreePsLoaded(entries)); }
                            Err(e) => { let _ = tx.send(AppMsg::Error(e)); }
                        }
                    });
                }
                crate::update::Cmd::CreateTerminal { worktree, command, title } => {
                    let tx = self.tx.clone();
                    let lock = Arc::clone(&self.cli_lock);
                    thread::spawn(move || {
                        let _guard = lock.lock();
                        match transport::terminal_create(
                            worktree.as_deref(),
                            &command,
                            title.as_deref(),
                        ) {
                            Ok(result) => {
                                let _ = tx.send(AppMsg::TerminalCreated {
                                    handle: result.handle,
                                    title: result.title,
                                });
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(e));
                            }
                        }
                    });
                }
                crate::update::Cmd::GroupBroadcast { name, message, handles } => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let results = transport::group_broadcast(
                            &handles,
                            &format!("[{name}]"),
                            &message,
                        );
                        let ok = results.iter().filter(|r| r.is_ok()).count();
                        let fail = results.len() - ok;
                        let _ = tx.send(AppMsg::GroupActionOk(
                            format!("Broadcast to {name}: {ok} ok, {fail} failed"),
                        ));
                    });
                }
                crate::update::Cmd::SetConfig { key, value } => {
                    // 同步写 DB,然后回灌到 model
                    if let Some(db) = &self.db {
                        db.set_config(&key, &value);
                    }
                    let _ = self.tx.send(AppMsg::ConfigUpdated { key, value });
                }
                crate::update::Cmd::OrchestrationReply { id, body } => {
                    let tx = self.tx.clone();
                    let lock = Arc::clone(&self.cli_lock);
                    thread::spawn(move || {
                        let _guard = lock.lock();
                        match transport::orchestration_reply(&id, &body) {
                            Ok(_) => { let _ = tx.send(AppMsg::SendOk(id)); }
                            Err(e) => { let _ = tx.send(AppMsg::SendFailed(e)); }
                        }
                    });
                }
                crate::update::Cmd::MarkRead { delivery_id } => {
                    let tx = self.tx.clone();
                    let lock = Arc::clone(&self.cli_lock);
                    thread::spawn(move || {
                        let _guard = lock.lock();
                        match transport::orchestration_ack(&delivery_id) {
                            Ok(()) => { let _ = tx.send(AppMsg::AckOk(delivery_id)); }
                            Err(e) => { let _ = tx.send(AppMsg::AckFailed(e)); }
                        }
                    });
                }
                crate::update::Cmd::RefreshOrchTasks => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        // 并行 fetch 三个列表(join 后合并回灌)
                        let runs = transport::fetch_run_list().unwrap_or_default();
                        let tasks = transport::fetch_task_list().unwrap_or_default();
                        let gates = transport::fetch_gate_list().unwrap_or_default();
                        let snapshot = crate::model::OrchSnapshot { runs, tasks, gates };
                        let _ = tx.send(AppMsg::OrchSnapshotLoaded(Box::new(snapshot)));
                    });
                }
                _ => {}
            }
        }
    }
}


/// Simple ISO 8601-ish UTC timestamp (no chrono dependency).
fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut d = secs;
    let mut year = 1970u64;
    loop {
        let diy = if is_leap(year) { 366 } else { 365 };
        if d < diy { break; }
        d -= diy;
        year += 1;
    }
    let md = if is_leap(year) {
        [31,29,31,30,31,30,31,31,30,31,30,31]
    } else {
        [31,28,31,30,31,30,31,31,30,31,30,31]
    };
    let mut month = 1u64;
    for &m in &md {
        if d < m { break; }
        d -= m;
        month += 1;
    }
    let day = d + 1;
    let tod = secs % 86400;
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            tod / 3600, (tod % 3600) / 60, tod % 60)
}

fn is_leap(y: u64) -> bool { (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 }
