//! update.rs —— 范式 2 纯函数 reducer + 范式 3 异步派发命令(ADR-1 + ADR-7)。
//!
//! `fn update(model, shell, msg) -> Vec<Cmd>`: 纯函数, **绝不 IO**。
//! - Model/Shell 改在原地(&mut); Cmd 返回 Vec 给 service.rs 执行(范式 3 fire-and-forget)。
//! - 状态转移穷尽 match AppMsg 所有 variant。
//! - send 回灌: Cmd::OrchestrationSend → service spawn → AppMsg::SendOk/SendFailed 回来(ADR-7)。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::msg::AppMsg;
use crate::model::{directory_sorted_with_mode, SortMode, Agent, OrchMessage, Model, EventCategory, EventSeverity, StatusCategory, ViewSnapshot};
use crate::shell::{FocusTarget, Shell, Tab};
use crate::view::{directory_layout, directory_scroll, LayoutItem};

// ───────────────────────── Cmd(意图声明, service 执行) ─────────────────────────

/// 异步命令(范式 3)。update 返回 Vec<Cmd>, service.rs 逐个 spawn std::thread 执行。
/// 终态经 AppMsg 回灌(SendOk/SendFailed), 绝不阻塞主 loop。
#[derive(Debug)]
pub enum Cmd {
    /// spawn: `orca-ide terminal list --json` 刷新通信录。
    RefreshAgents,
    /// poll: `last-status.json` mtime 变化则 parse 并合并(ADR-5)。
    RefreshStatus,
    /// 编排发送(ADR-7: 声明意图, service spawn 执行, 结果回灌)。
    OrchestrationSend {
        to: String,
        subject: String,
        body: String,
    },
    /// spawn: `orca orchestration check --json` drain inbox(ADR-4)。
    DrainMessages,
    /// spawn: `orca-ide orchestration inbox --json` 刷新全量未读数。
    RefreshUnread,
    /// 写 `~/.orca/hub-directory.json` 快照(ADR-6)。
    WriteDirectory,
    /// socket 查询处理(ADR-3)。
    QuerySocket { req: crate::msg::SocketReq },
    /// spawn: `orca terminal switch --terminal <handle>` 激活 tab。
    SwitchTerminal { handle: String },
    /// 持久化 agents 到 SQLite(非 IO: service.execute 内同步写 DB)。
    PersistAgents(Vec<Agent>),
    /// 持久化消息到 SQLite。
    PersistMessages(Vec<OrchMessage>),
    /// 持久化群组加入。
    PersistGroupJoin { name: String, handle: String },
    /// 持久化群组退出。
    PersistGroupLeave { name: String, handle: String },
    /// spawn: PTY 直接注入文本。
    TerminalSend { handle: String, text: String },
    /// spawn: 关闭终端。
    CloseTerminal { handle: String },
    /// spawn: 重命名终端 tab。
    RenameTerminal { handle: String, new_title: String },
    /// spawn: 读取终端输出。
    ReadTerminal { handle: String },
    /// spawn: worktree ps 编排摘要。
    RefreshWorktreePs,
    /// 群组 broadcast: 向群组所有成员发消息(handles 由 update 从 model 提取)。
    GroupBroadcast { name: String, message: String, handles: Vec<String> },
    /// spawn: 创建新终端(orca terminal create)。
    CreateTerminal { worktree: Option<String>, command: String, title: Option<String> },
    /// 持久化配置项到 DB(同步写, 结果回灌 ConfigUpdated)。
    SetConfig { key: String, value: String },
    /// spawn: orchestration reply --id <msg_id> --body <text>。
    OrchestrationReply { id: String, body: String },
    /// spawn: orchestration check --ack <delivery_id> 标记消息已读。
    MarkRead { delivery_id: String },
    /// spawn: 并行刷新 run-list + task-list + gate-list 编排快照。
    RefreshOrchTasks,
    /// 持久化活动日志事件到 SQLite(service 同步写)。
    PersistActivityEvent(crate::model::Event),
    /// 持久化输入历史到 SQLite。
    PersistHistoryEntry(String),
    /// 持久化置顶 agent(添加到 pinned_agents 表)。
    PersistPinAdd { handle: String },
    /// 移除置顶 agent。
    PersistPinRemove { handle: String },
    /// 持久化标签(添加到 agent_tags 表)。
    PersistTagAdd { handle: String, tag: String },
    /// 移除标签。
    PersistTagRemove { handle: String, tag: String },
    /// 持久化代码片段到 DB。
    PersistSnippet { name: String, text: String },
    /// 移除代码片段。
    RemoveSnippet { name: String },
    /// 持久化告警规则到 DB。
    PersistAlertRule(crate::model::AlertRule),
    /// 移除告警规则。
    RemoveAlertRule { id: i64 },
    /// 持久化宏到 DB。
    PersistMacro(crate::model::RecordedMacro),
    /// 移除宏。
    RemoveMacro { name: String },
    /// 持久化视图预设到 DB。
    PersistView { name: String, json: String },
    /// 移除视图预设。
    RemoveView { name: String },
    /// 持久化 agent 笔记到 DB。
    PersistNote { handle: String, text: String },
    /// 移除 agent 笔记。
    RemoveNote { handle: String },
    /// 导出用户数据到 JSON 文件。
    ExportSettings { path: String, bundle: crate::model::ExportBundle },
    /// 从 JSON 文件导入用户数据。
    ImportSettings { path: String },
    /// 持久化命令别名到 DB。
    PersistAlias { name: String, expansion: String },
    /// 移除命令别名。
    RemoveAlias { name: String },
    /// 持久化热键到 DB。
    PersistHotkey { key: String, command: String },
    /// 移除热键。
    RemoveHotkey { key: String },
    /// 持久化监控 agent。
    PersistWatchAdd { handle: String },
    /// 移除监控 agent。
    PersistWatchRemove { handle: String },
    /// 无操作。
    Noop,
    /// 退出。
    Quit,
}


/// 判断是否该刷新 agents。
/// 从 model.config 读取 refresh_interval_ms,转换为 tick 数(tick=50ms)。
fn should_refresh_agents(model: &Model, shell: &Shell) -> bool {
    let interval_ms = model.refresh_interval_ms();
    // 转换为 tick 数: tick=50ms → ticks = interval_ms / 50
    let ticks = (interval_ms / 50).max(2) as usize; // 最少 100ms
    shell.spinner_frame % ticks == 0
}

/// 记录活动日志事件: 推入 model.events + 返回 PersistActivityEvent Cmd。
fn note_event(model: &mut Model, sev: EventSeverity, cat: EventCategory, source: &str, text: String) -> Cmd {
    let ev = crate::model::Event {
        id: 0,
        timestamp_ms: crate::model::now_ms(),
        severity: sev,
        category: cat,
        source: source.to_string(),
        text,
    };
    model.push_event(ev.clone());
    Cmd::PersistActivityEvent(ev)
}

// ───────────────────────── update(纯 reducer) ─────────────────────────

/// 纯函数 reducer。绝不 IO, 改 Model/Shell + 返 Cmd Vec。
pub fn update(model: &mut Model, shell: &mut Shell, msg: AppMsg) -> Vec<Cmd> {
    match msg {
        AppMsg::Key(k) => {
            // ── Replay cancel on real keypress ──
            if !shell.replay_queue.is_empty() {
                let cancelled = shell.replay_queue.len();
                shell.replay_queue.clear();
                shell.push_toast(format!("replay cancelled ({cancelled} keys remaining)"));
            }
            // ── Recording capture ──
            if shell.recording_active {
                let is_quit = matches!((k.code, k.modifiers),
                    (KeyCode::Char('q'), KeyModifiers::NONE) |
                    (KeyCode::Char('c'), KeyModifiers::CONTROL));
                if !is_quit {
                    shell.recording_buffer.push(k);
                }
            }
            handle_key(model, shell, k)
        }

        // ──── 鼠标左键点击(选中 card) ────
        AppMsg::MouseLeftClick { x, y } => {
            if let Some(idx) = hit_test_card(model, shell, x, y) {
                shell.cursor = idx;
            }
            vec![]
        }

        // ──── 终端尺寸变化 ────
        AppMsg::Resize { width, height } => {
            shell.size = (width, height);
            vec![]
        }

        // ──── 定时 tick(驱动 spinner/toast 超时/状态刷新) ────
        AppMsg::Tick => {
            shell.spinner_frame = shell.spinner_frame.wrapping_add(1);
            // 清理过期 toast(3 秒)
            shell.drain_toasts(3);

            let mut cmds = Vec::new();
            // 每次 tick 都刷新 status(轻量 stat)
            cmds.push(Cmd::RefreshStatus);
            // 周期性刷新 agents(可配置间隔)
            if should_refresh_agents(model, shell) {
                cmds.push(Cmd::RefreshAgents);
                cmds.push(Cmd::DrainMessages);
                cmds.push(Cmd::RefreshUnread);
            }
            // ── Macro replay pump: one key per tick (50ms = 20 keys/sec) ──
            if !shell.replay_queue.is_empty() {
                let event = shell.replay_queue.remove(0);
                cmds.extend(handle_key(model, shell, event));
            }
            cmds
        }

        // ──── terminal list 加载完成 ────
        AppMsg::AgentsLoaded(agents) => {
            let old_handles: std::collections::HashSet<String> = model.directory.keys().cloned().collect();
            let persist = agents.clone();
            model.apply_agents(agents);
            let new_handles: std::collections::HashSet<String> = model.directory.keys().cloned().collect();
            let mut cmds = vec![Cmd::PersistAgents(persist), Cmd::WriteDirectory];
            // 抑制空集风暴(瞬态 Orca 不稳定时全量消失/出现)
            if !old_handles.is_empty() && !new_handles.is_empty() {
                for h in new_handles.difference(&old_handles) {
                    cmds.push(note_event(model, EventSeverity::Info, EventCategory::Agent, h, format!("Agent appeared: {h}")));
                }
                for h in old_handles.difference(&new_handles) {
                    cmds.push(note_event(model, EventSeverity::Info, EventCategory::Agent, h, format!("Agent disappeared: {h}")));
                }
            }
            cmds
        }

        AppMsg::StatusUpdated(statuses) => {
            // 快照旧 StatusCategory(变更前)
            let old_cats: std::collections::HashMap<String, StatusCategory> = model.directory.iter()
                .map(|(h, a)| (h.clone(), StatusCategory::from_agent(a)))
                .collect();
            model.apply_status(statuses);
            let mut cmds = vec![Cmd::WriteDirectory];
            // 收集状态转移(borrow-checker 安全: 先收集再 emit)
            let transitions: Vec<(String, EventSeverity, String)> = model.directory.iter()
                .filter_map(|(handle, agent)| {
                    let new_cat = StatusCategory::from_agent(agent);
                    old_cats.get(handle).and_then(|old_cat| {
                        if old_cat != &new_cat {
                            let sev = match new_cat {
                                StatusCategory::Error | StatusCategory::Blocked => EventSeverity::Warn,
                                _ => EventSeverity::Info,
                            };
                            Some((handle.clone(), sev, format!("{}: {} → {}", handle, old_cat.label(), new_cat.label())))
                        } else {
                            None
                        }
                    })
                })
                .collect();
            for (handle, sev, text) in transitions {
                let sev_str = sev.as_str();
                cmds.push(note_event(model, sev, EventCategory::State, &handle, text));
                // ── Pin alert: proactive toast for pinned agents ──
                if model.pinned.contains(&handle) {
                    if let Some(agent) = model.directory.get(&handle) {
                        let new_cat = StatusCategory::from_agent(agent);
                        shell.push_toast(format!("📌 {} → {}", handle, new_cat.label()));
                    }
                }
                // ── Watch alert: toast for watched agents on state change ──
                if model.watched.contains(&handle) {
                    if let Some(agent) = model.directory.get(&handle) {
                        let new_cat = StatusCategory::from_agent(agent);
                        shell.push_toast(format!("👁 {} → {}", handle, new_cat.label()));
                    }
                }
                // ── Alert rules check ──
                let ctx = crate::model::CheckContext {
                    handle: Some(&handle),
                    new_state: model.directory.get(&handle).map(|a| StatusCategory::from_agent(a).label()),
                    event_severity: Some(sev_str),
                    event_source: Some(&handle),
                    is_new_message: false,
                };
                for toast in crate::model::check_alert_rules(&model.alert_rules, &ctx) {
                    shell.push_toast(toast);
                }
            }
            cmds
        }

        // ──── inbox 未读数刷新 ────
        AppMsg::UnreadUpdated(counts) => {
            model.apply_unread(counts);
            vec![Cmd::WriteDirectory]
        }

        // ──── 编排发送成功(ADR-7 回灌) ────
        AppMsg::SendOk(id) => {
            shell.push_toast(format!("Sent: {id}"));
            vec![note_event(model, EventSeverity::Info, EventCategory::Message, "system", format!("Sent: {id}"))]
        }

        // ──── 编排发送失败(ADR-7 回灌) ────
        AppMsg::SendFailed(e) => {
            shell.push_toast(format!("Send failed: {e}"));
            vec![note_event(model, EventSeverity::Error, EventCategory::Message, "system", format!("Send failed: {e}"))]
        }

        // ──── mark-read 成功(ADR-7 回灌) ────
        AppMsg::AckOk(id) => {
            shell.push_toast(format!("Marked read: {id}"));
            // 刷新未读数
            vec![Cmd::RefreshUnread, note_event(model, EventSeverity::Info, EventCategory::Message, "system", format!("Marked read: {id}"))]
        }

        // ──── mark-read 失败 ────
        AppMsg::AckFailed(e) => {
            shell.push_toast(format!("Ack failed: {e}"));
            vec![note_event(model, EventSeverity::Error, EventCategory::Message, "system", format!("Ack failed: {e}"))]
        }

        AppMsg::MessagesDrained(msgs) => {
            let n = msgs.len();
            let persist = msgs.clone();
            for msg in msgs {
                model.push_message(msg);
            }
            let mut cmds = vec![Cmd::PersistMessages(persist)];
            if n > 0 {
                cmds.push(note_event(model, EventSeverity::Info, EventCategory::Message, "system", format!("Received {n} messages")));
                let ctx = crate::model::CheckContext { is_new_message: true, ..Default::default() };
                for toast in crate::model::check_alert_rules(&model.alert_rules, &ctx) {
                    shell.push_toast(toast);
                }
            }
            cmds
        }

        AppMsg::SocketQuery(req) => {
            vec![Cmd::QuerySocket { req }]
        }
        // ──── PTY 注入结果 ────
        AppMsg::InjectOk(n) => {
            shell.push_toast(format!("PTY: sent {n} bytes"));
            vec![note_event(model, EventSeverity::Info, EventCategory::System, "system", format!("PTY inject: {n} bytes"))]
        }
        AppMsg::InjectFailed(e) => {
            shell.push_toast(format!("PTY failed: {e}"));
            vec![note_event(model, EventSeverity::Error, EventCategory::System, "system", format!("PTY failed: {e}"))]
        }

        // ──── terminal read 结果(浮层显示) ────
        AppMsg::TerminalOutput(text) => {
            shell.overlay_content = Some(text);
            shell.overlay_scroll = 0;
            vec![]
        }

        // ──── worktree ps 结果 ────
        AppMsg::WorktreePsLoaded(entries) => {
            model.apply_worktree_ps(entries);
            vec![]
        }

        // ──── terminal create 成功 ────
        AppMsg::TerminalCreated { handle, title } => {
            let label = title.as_deref().unwrap_or(&handle).to_string();
            shell.push_toast(format!("Created: {label}"));
            vec![
                Cmd::RefreshAgents,
                Cmd::WriteDirectory,
                note_event(model, EventSeverity::Info, EventCategory::Agent, &handle, format!("Created: {label}")),
            ]
        }
        // ──── 群组操作成功 ────
        AppMsg::GroupActionOk(msg) => {
            shell.push_toast(msg.clone());
            vec![Cmd::RefreshAgents, note_event(model, EventSeverity::Info, EventCategory::Group, "system", msg)]
        }

        // ──── 信息 toast ────
        AppMsg::Info(msg) => {
            shell.push_toast(msg);
            vec![Cmd::RefreshAgents]
        }

        // ──── 通用错误 ────
        AppMsg::Error(e) => {
            shell.push_toast(e.clone());
            vec![note_event(model, EventSeverity::Error, EventCategory::System, "system", e)]
        }

        // ──── 配置更新回灌 ────
        AppMsg::ConfigUpdated { key, value } => {
            model.set_config(key.clone(), value.clone());
            if key == "theme" {
                shell.theme_name = value.clone();
            }
            shell.push_toast(format!("config: {key} updated"));
            vec![]
        }

        // ──── 编排快照回灌 ────
        AppMsg::OrchSnapshotLoaded(snapshot) => {
            model.apply_orch_snapshot(*snapshot);
            vec![]
        }

        // ──── 导出/导入结果回灌 ────
        AppMsg::ExportOk { path, count } => {
            shell.push_toast(format!("✓ exported {count} items to {path}"));
            vec![]
        }
        AppMsg::ExportFailed { reason } => {
            shell.push_toast(format!("✖ export failed: {reason}"));
            vec![]
        }
        AppMsg::ImportOk { path, bundle } => {
            // Apply bundle to model (replace-all)
            model.apply_config(bundle.config.clone());
            model.apply_tags(bundle.tags.clone());
            model.apply_snippets(bundle.snippets.clone());
            model.apply_macros(bundle.macros.clone());
            model.apply_saved_views(bundle.saved_views.clone());
            model.apply_pinned(bundle.pinned.clone());
            model.alert_rules = bundle.alert_rules.clone();
            model.apply_notes(bundle.notes.clone());
            model.apply_aliases(bundle.aliases.clone());
            model.apply_hotkeys(bundle.hotkeys.clone());
            model.apply_watched(bundle.watched.clone());
            model.generation += 1;
            let count = bundle.config.len() + bundle.tags.len() + bundle.snippets.len()
                + bundle.macros.len() + bundle.saved_views.len() + bundle.pinned.len()
                + bundle.alert_rules.len() + bundle.notes.len() + bundle.aliases.len() + bundle.hotkeys.len() + bundle.watched.len();
            shell.push_toast(format!("✓ imported {count} items from {path}"));
            vec![]
        }
        AppMsg::ImportFailed { reason } => {
            shell.push_toast(format!("✖ import failed: {reason}"));
            vec![]
        }

        // ──── 退出 ────
        AppMsg::Quit => vec![Cmd::Quit],
    }
}

// ───────────────────────── 键盘处理 ─────────────────────────

/// 键盘处理。全局快捷键优先(q/Ctrl+C 退出, Tab 切 tab), 其余按 insert_mode 分流。
fn handle_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    // 全局搜索激活时,键盘走搜索处理
    if shell.search_active {
        return handle_search_key(model, shell, k);
    }
    // Ctrl-S: 切换全局搜索(非输入模式)
    if !shell.insert_mode {
        if let (KeyCode::Char('s'), KeyModifiers::CONTROL) = (k.code, k.modifiers) {
            shell.search_active = true;
            shell.search_query.clear();
            shell.search_cursor = 0;
            return vec![];
        }
    }
    // 命令面板激活时,所有键盘事件走面板处理
    if shell.palette_active {
        return handle_palette_key(model, shell, k);
    }
    // 过滤模式激活时,键盘事件走过滤输入处理
    if shell.filter_active {
        return handle_filter_key(model, shell, k);
    }
    // Quick-Switch 激活时,键盘走快速跳转处理
    if shell.quickswitch_active {
        return handle_quickswitch_key(model, shell, k);
    }
    if shell.overlay_content.is_some() || shell.worktree_ps_active || shell.group_detail_active || shell.cheatsheet_active || shell.config_overlay_active || shell.orch_tasks_active || shell.activity_active || shell.history_overlay_active || shell.dashboard_active || shell.snippet_overlay_active || shell.rule_overlay_active || shell.macro_overlay_active || shell.views_overlay_active || shell.metrics_overlay_active || shell.note_overlay_active || shell.quick_actions_active || shell.alias_overlay_active || shell.hotkeys_overlay_active || shell.theme_overlay_active {
        return handle_overlay_key(model, shell, k);
    }
    // Ctrl-W: toggle watch on current agent (non-insert mode)
    if !shell.insert_mode {
        if let (KeyCode::Char('w'), KeyModifiers::CONTROL) = (k.code, k.modifiers) {
            if let Some(handle) = selected_agent_handle(model, shell) {
                model.toggle_watch(&handle);
                let watching = model.is_watched(&handle);
                shell.push_toast(if watching { format!("👁 watching: {handle}") } else { format!("unwatched: {handle}") });
                return if watching { vec![Cmd::PersistWatchAdd { handle }] } else { vec![Cmd::PersistWatchRemove { handle }] };
            } else {
                shell.push_toast("no agent selected".into());
                return vec![];
            }
        }
    }




    // Esc stops recording (if recording and not in insert mode)
    if shell.recording_active && !shell.insert_mode && k.code == KeyCode::Esc {
        // Remove trailing Esc from buffer (it was just captured by update())
        if shell.recording_buffer.last().map(|e| e.code == KeyCode::Esc).unwrap_or(false) {
            shell.recording_buffer.pop();
        }
        let name = std::mem::take(&mut shell.recording_name);
        let events = std::mem::take(&mut shell.recording_buffer);
        shell.recording_active = false;
        if events.is_empty() {
            shell.push_toast(format!("macro empty: {name}"));
            return vec![];
        }
        let json = crate::model::serialize_key_events(&events);
        let m = crate::model::RecordedMacro { name: name.clone(), key_events_json: json, created_at_ms: crate::model::now_ms() };
        model.add_macro(m.clone());
        shell.push_toast(format!("macro saved: {name} ({} keys)", events.len()));
        return vec![Cmd::PersistMacro(m)];
    }

    match (k.code, k.modifiers) {
        // Ctrl-P 或 : 打开命令面板
        (KeyCode::Char('p'), KeyModifiers::CONTROL) | (KeyCode::Char(':'), KeyModifiers::NONE) => {
            shell.palette_active = true;
            shell.palette_query.clear();
            shell.palette_cursor = 0;
            return vec![];
        }

        // / 进入过滤模式(Directory tab / Messages tab)
        (KeyCode::Char('/'), KeyModifiers::NONE)
            if !shell.insert_mode && matches!(shell.tab, Tab::Directory | Tab::Messages) =>
        {
            shell.filter_active = true;
            shell.filter_query = Some(String::new());
            return vec![];
        }

        // w: worktree ps 浮层
        (KeyCode::Char('w'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.worktree_ps_active = true;
            return vec![Cmd::RefreshWorktreePs];
        }

        // t: 编排任务浮层(run-list + task-list + gate-list)
        (KeyCode::Char('t'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.orch_tasks_active = true;
            shell.overlay_scroll = 0;
            return vec![Cmd::RefreshOrchTasks];
        }

        // ?: cheatsheet 浮层
        (KeyCode::Char('?'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.cheatsheet_active = true;
            shell.overlay_scroll = 0;
            return vec![];
        }

        // a: activity log overlay (toggle)
        (KeyCode::Char('a'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.activity_active = !shell.activity_active;
            if shell.activity_active {
                shell.overlay_scroll = 0;
            }
            return vec![];
        }

        // H: history overlay (toggle)
        (KeyCode::Char('H'), KeyModifiers::SHIFT) if !shell.insert_mode => {
            shell.history_overlay_active = !shell.history_overlay_active;
            if shell.history_overlay_active {
                shell.overlay_scroll = 0;
            }
            return vec![];
        }

        // D(Shift+d): dashboard overlay — 排除 Messages tab(D=clear-all)
        (KeyCode::Char('D'), KeyModifiers::SHIFT) if !shell.insert_mode && shell.tab != Tab::Messages => {
            shell.dashboard_active = !shell.dashboard_active;
            if shell.dashboard_active {
                shell.overlay_scroll = 0;
            }
            return vec![];
        }

        // S(Shift+s): snippet library overlay (toggle)
        (KeyCode::Char('S'), KeyModifiers::SHIFT) if !shell.insert_mode => {
            shell.snippet_overlay_active = !shell.snippet_overlay_active;
            if shell.snippet_overlay_active {
                shell.overlay_scroll = 0;
            }
            return vec![];
        }

        (KeyCode::Char('N'), KeyModifiers::SHIFT) if !shell.insert_mode => {
            shell.rule_overlay_active = !shell.rule_overlay_active;
            if shell.rule_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }

        // e: macro library overlay (toggle)
        (KeyCode::Char('e'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.macro_overlay_active = !shell.macro_overlay_active;
            if shell.macro_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }

        // V: saved views overlay (toggle)
        (KeyCode::Char('V'), KeyModifiers::SHIFT) if !shell.insert_mode => {
            shell.views_overlay_active = !shell.views_overlay_active;
            if shell.views_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }

        // x: agent metrics overlay (toggle)
        (KeyCode::Char('x'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.metrics_overlay_active = !shell.metrics_overlay_active;
            if shell.metrics_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }


        // n: note overlay for current agent
        (KeyCode::Char('n'), KeyModifiers::NONE) if !shell.insert_mode => {
            if let Some(handle) = selected_agent_handle(model, shell) {
                shell.note_overlay_active = true;
                shell.note_viewing_handle = Some(handle.clone());
                shell.note_edit_buf = model.get_note(&handle).cloned().unwrap_or_default();
                shell.overlay_scroll = 0;
            } else {
                shell.push_toast("no agent selected".into());
            }
            return vec![];
        }
        // o: quick actions menu for current agent
        (KeyCode::Char('o'), KeyModifiers::NONE) if !shell.insert_mode => {
            if selected_agent_handle(model, shell).is_some() {
                shell.quick_actions_active = !shell.quick_actions_active;
                shell.quick_actions_cursor = 0;
            } else {
                shell.push_toast("no agent selected".into());
            }
            return vec![];
        }
        // l: alias overlay (toggle)
        (KeyCode::Char('l'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.alias_overlay_active = !shell.alias_overlay_active;
            if shell.alias_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }

        // r: hotkeys overlay (toggle)
        (KeyCode::Char('r'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.hotkeys_overlay_active = !shell.hotkeys_overlay_active;
            if shell.hotkeys_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }
        // z: theme customization overlay (toggle)
        (KeyCode::Char('z'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.theme_overlay_active = !shell.theme_overlay_active;
            if shell.theme_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }

        // v: agent quick-switch overlay
        (KeyCode::Char('v'), KeyModifiers::NONE) if !shell.insert_mode => {
            shell.quickswitch_active = true;
            shell.quickswitch_query.clear();
            shell.quickswitch_cursor = 0;
            return vec![];
        }

        // f: focus mode toggle (show only selected_set agents)
        (KeyCode::Char('f'), KeyModifiers::NONE) if !shell.insert_mode => {
            if shell.selected_set.is_empty() {
                shell.push_toast("no agents selected (Space to select)".into());
            } else {
                shell.focus_mode = !shell.focus_mode;
                shell.cursor = 0;
                let n = shell.selected_set.len();
                if shell.focus_mode {
                    shell.push_toast(format!("◉ FOCUS: showing {n} selected agents"));
                } else {
                    shell.push_toast("focus mode off".into());
                }
            }
            return vec![];
        }
        (KeyCode::Char('q'), KeyModifiers::NONE) if !shell.insert_mode => {
            return vec![Cmd::Quit];
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            return vec![Cmd::Quit];
        }

        // Tab 切换(全局, 不受 insert_mode 影响)
        (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::BackTab, KeyModifiers::SHIFT) => {
            shell.tab = shell.tab.next();
            shell.cursor = 0;
            shell.insert_mode = false;
            shell.group_detail_active = false;
            shell.selected_set.clear();
            shell.focus_mode = false;
            shell.input_buf.clear();
            // 同步 focus 到对应 tab
            shell.focus = match shell.tab {
                Tab::Directory => FocusTarget::Directory,
                Tab::Groups => FocusTarget::Groups,
                Tab::Messages => FocusTarget::Messages,
            };
            return vec![];
        }

        // Esc: 退出 insert_mode
        (KeyCode::Esc, KeyModifiers::NONE) => {
            if shell.insert_mode {
                shell.insert_mode = false;
                shell.input_buf.clear();
                shell.history_cursor = None;
                shell.saved_input.clear();
                // focus 回当前 tab
                shell.focus = match shell.tab {
                    Tab::Directory => FocusTarget::Directory,
                    Tab::Groups => FocusTarget::Groups,
                    Tab::Messages => FocusTarget::Messages,
                };
                return vec![];
            }
        }

        // Ctrl-d: scroll_down(半屏) — 远离底部,看旧内容
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            let len = list_len(model, shell);
            if !len.is_empty() {
                let half = (shell.size.1 as usize / 2).max(1);
                shell.cursor = (shell.cursor + half).min(len.len() - 1);
            }
            return vec![];
        }

        // Ctrl-u: scroll_up(半屏) — 回底部,看新内容(vim 一致)
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            let half = (shell.size.1 as usize / 2).max(1);
            shell.cursor = shell.cursor.saturating_sub(half);
            return vec![];
        }

        _ => {}
    }

    // Custom hotkey dispatch (after all built-in bindings, before normal key handling)
    if !shell.insert_mode {
        if let KeyCode::Char(c) = k.code {
            let key_str = c.to_string();
            if let Some(cmd_text) = model.get_hotkey(&key_str).cloned() {
                return dispatch_input(model, shell, cmd_text);
            }
        }
    }


    // insert_mode: 输入模式处理
    if shell.insert_mode {
        return handle_input_key(model, shell, k);
    }

    // 正常模式: 导航 + 进入输入
    handle_normal_key(model, shell, k)
}

/// 正常模式键盘处理: 导航(j/k/上下), 输入(i), Enter 选中/发送, g 切 tab。
fn handle_normal_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    match (k.code, k.modifiers) {
        // i: 进入输入模式
        (KeyCode::Char('i'), KeyModifiers::NONE) => {
            shell.insert_mode = true;
            shell.focus = FocusTarget::Input;
            vec![]
        }

        // g: 切到 Groups tab
        (KeyCode::Char('g'), KeyModifiers::NONE) => {
            let next_tab = match shell.tab {
                Tab::Directory => Tab::Groups,
                Tab::Groups => Tab::Messages,
                Tab::Messages => Tab::Directory,
            };
            shell.tab = next_tab;
            shell.cursor = 0;
            shell.focus = match next_tab {
                Tab::Directory => FocusTarget::Directory,
                Tab::Groups => FocusTarget::Groups,
                Tab::Messages => FocusTarget::Messages,
            };
            vec![]
        }

        // j/下: cursor 下移
        (KeyCode::Char('j'), KeyModifiers::NONE)
        | (KeyCode::Down, KeyModifiers::NONE) => {
            let len = list_len(model, shell);
            if !len.is_empty() && shell.cursor < len.len() - 1 {
                shell.cursor += 1;
            }
            vec![]
        }

        // k/上: cursor 上移
        (KeyCode::Char('k'), KeyModifiers::NONE)
        | (KeyCode::Up, KeyModifiers::NONE) => {
            if shell.cursor > 0 {
                shell.cursor -= 1;
            }
            vec![]
        }

        // Enter: 按 tab 分流
        (KeyCode::Enter, KeyModifiers::NONE) => match shell.tab {
            Tab::Directory => {
                // 选中 agent, 进入输入模式准备发送
                let selected_handle = selected_agent_handle(model, shell);
                if let Some(handle) = selected_handle {
                    shell.insert_mode = true;
                    shell.focus = FocusTarget::Input;
                    shell.input_buf = format!("to:{handle} ");
                }
                vec![]
            }
            Tab::Groups => {
                // 选中群组: 切换成员详情浮层
                if selected_group_name(model, shell).is_some() {
                    shell.group_detail_active = !shell.group_detail_active;
                    if shell.group_detail_active {
                        shell.overlay_scroll = 0;
                    }
                }
                vec![]
            }
            Tab::Messages => {
                // 选中消息后进入 reply 模式
                let msgs: Vec<_> = model.messages.iter().rev().collect();
                if let Some(msg) = msgs.get(shell.cursor) {
                    let id = &msg.id;
                    shell.insert_mode = true;
                    shell.focus = FocusTarget::Input;
                    shell.input_buf = format!("reply:{id} ");
                }
                vec![]
            }
        },
        // d(Messages tab): delete 当前消息(只删本地缓存)
        (KeyCode::Char('d'), KeyModifiers::NONE) if shell.tab == Tab::Messages => {
            let msgs: Vec<_> = model.messages.iter().rev().collect();
            if let Some(msg) = msgs.get(shell.cursor) {
                let msg_id = msg.id.clone();
                // 从 VecDeque 中移除。messages 按 push_back 追加，rev 遍历取第 cursor 条。
                if let Some(idx) = model.messages.iter().position(|m| m.id == msg_id) {
                    model.messages.remove(idx);
                }
                // 修正 cursor
                let len = model.messages.len();
                if len == 0 {
                    shell.cursor = 0;
                } else if shell.cursor >= len {
                    shell.cursor = len - 1;
                }
                shell.push_toast(format!("deleted: {msg_id}"));
            }
            vec![]
        }
        // D(Shift+d, Messages tab): clear all messages
        (KeyCode::Char('D'), KeyModifiers::SHIFT) if shell.tab == Tab::Messages => {
            let count = model.messages.len();
            model.messages.clear();
            shell.cursor = 0;
            shell.push_toast(format!("cleared {count} messages"));
            vec![]
        }
        // m(Messages tab): mark-read 当前消息(orchestration check --ack)
        (KeyCode::Char('m'), KeyModifiers::NONE) if shell.tab == Tab::Messages => {
            let msgs: Vec<_> = model.messages.iter().rev().collect();
            if let Some(msg) = msgs.get(shell.cursor) {
                let delivery_id = msg.id.clone();
                // 本地也标记已读
                if let Some(idx) = model.messages.iter().position(|m| m.id == delivery_id) {
                    model.messages[idx].read = 1;
                }
                return vec![Cmd::MarkRead { delivery_id }];
            }
            vec![]
        }
        // s(in Directory tab): switch/activate selected agent's tab
        (KeyCode::Char('s'), KeyModifiers::NONE) => {
            let selected_handle = selected_agent_handle(model, shell);
            if let Some(handle) = selected_handle {
                shell.push_toast(format!("switching to {}", &handle[..handle.len().min(20)]));
                vec![Cmd::SwitchTerminal { handle }]
            } else {
                vec![]
            }
        }

        // Space(in Directory tab): toggle multi-select
        (KeyCode::Char(' '), KeyModifiers::NONE) if shell.tab == Tab::Directory => {
            if let Some(handle) = selected_agent_handle(model, shell) {
                if shell.selected_set.contains(&handle) {
                    shell.selected_set.remove(&handle);
                } else {
                    shell.selected_set.insert(handle);
                }
            }
            vec![]
        }

        // p(in Directory tab): PTY 直接注入模式
        (KeyCode::Char('p'), KeyModifiers::NONE) => {
            let selected_handle = selected_agent_handle(model, shell);
            if let Some(handle) = selected_handle {
                shell.insert_mode = true;
                shell.focus = FocusTarget::Input;
                shell.input_buf = format!("pty:{handle} ");
            }
            vec![]
        }

        // y(in Directory tab): yank handle to clipboard
        (KeyCode::Char('y'), KeyModifiers::NONE) if shell.tab == Tab::Directory => {
            if let Some(handle) = selected_agent_handle(model, shell) {
                yank_to_clipboard(&handle);
                shell.push_toast(format!("Yanked: {handle}"));
            }
            vec![]
        }

        // @(Directory tab): toggle pin on selected agent
        (KeyCode::Char('@'), KeyModifiers::NONE) if shell.tab == Tab::Directory => {
            if let Some(handle) = selected_agent_handle(model, shell) {
                model.toggle_pin(&handle);
                // Cursor relock: sort changed, find handle's new position
                let sorted = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
                if let Some(idx) = sorted.iter().position(|h| h == &handle) {
                    shell.cursor = idx;
                }
                let is_pinned = model.is_pinned(&handle);
                let action = if is_pinned { "pinned" } else { "unpinned" };
                shell.push_toast(format!("{action}: {handle}"));
                let cmd = if is_pinned {
                    Cmd::PersistPinAdd { handle }
                } else {
                    Cmd::PersistPinRemove { handle }
                };
                vec![cmd]
            } else {
                vec![]
            }
        }


        // 1-9: jump to group N (Directory tab only)
        (KeyCode::Char(c @ '1'..='9'), KeyModifiers::NONE) if shell.tab == Tab::Directory => {
            let group_num = c.to_digit(10).unwrap() as usize; // 1-based
            let sorted = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
            // compute group start indices: walk sorted, find boundaries where cwd changes
            let mut group_starts: Vec<usize> = Vec::new();
            let mut prev_cwd: Option<&str> = None;
            for (idx, handle) in sorted.iter().enumerate() {
                let cwd = &model.directory[handle].cwd;
                if prev_cwd != Some(cwd.as_str()) {
                    group_starts.push(idx);
                    prev_cwd = Some(cwd.as_str());
                }
            }
            // group_num is 1-based; bounds check
            if group_num <= group_starts.len() {
                let target_idx = group_starts[group_num - 1];
                let group_cwd = &model.directory[&sorted[target_idx]].cwd;
                let display = if group_cwd.starts_with("/home/") {
                    format!("~/{}", &group_cwd[6..])
                } else {
                    group_cwd.clone()
                };
                shell.cursor = target_idx;
                shell.push_toast(format!("Jumped to group {group_num}: {display}"));
            }
            vec![]
        }

        _ => vec![],
    }
}
/// Restore a ViewSnapshot onto Shell state.
fn apply_view_snapshot(shell: &mut Shell, snap: ViewSnapshot) -> Vec<Cmd> {
    // Restore tab
    shell.tab = match snap.tab.as_str() {
        "groups" => Tab::Groups,
        "messages" => Tab::Messages,
        _ => Tab::Directory,
    };
    shell.focus = match shell.tab {
        Tab::Directory => FocusTarget::Directory,
        Tab::Groups => FocusTarget::Groups,
        Tab::Messages => FocusTarget::Messages,
    };
    shell.cursor = 0;

    // Restore filter
    match &snap.filter_query {
        Some(q) if !q.is_empty() => {
            shell.filter_active = true;
            shell.filter_query = Some(q.clone());
        }
        _ => {
            shell.filter_active = false;
            shell.filter_query = None;
        }
    }

    // Restore selection
    shell.selected_set = snap.selected_set.into_iter().collect();

    // Restore sort mode via SetConfig
    vec![Cmd::SetConfig { key: "sort".into(), value: snap.sort_mode }]
}

/// 解析输入栏提交: 11 个前缀分发。返回 Cmd vec(空=未匹配/验证失败)。

/// Expand leading alias token in input buffer.
/// If the first whitespace-delimited token matches an alias, replace it with the expansion.
fn expand_alias(model: &Model, buf: String) -> String {
    let first_word = buf.split_whitespace().next().unwrap_or(&buf);
    if let Some(expansion) = model.get_alias(first_word) {
        let rest = &buf[first_word.len()..];
        return format!("{expansion}{rest}");
    }
    buf
}

fn dispatch_input(model: &mut Model, shell: &mut Shell, buf: String) -> Vec<Cmd> {
    // Alias expansion: if the first word matches an alias, expand it before prefix matching
    let buf = expand_alias(model, buf);

    // Chain: split by ' | ' and dispatch each segment sequentially
    if let Some(rest) = buf.strip_prefix("chain:") {
        let segments: Vec<&str> = rest.split(" | ").collect();
        if segments.len() < 2 {
            shell.push_toast("chain: usage: chain:cmd1 | cmd2 | cmd3".into());
            return vec![];
        }
        let seg_count = segments.len();
        let mut all_cmds = Vec::new();
        for seg in segments {
            let seg = seg.trim();
            if seg.is_empty() { continue; }
            all_cmds.extend(dispatch_input(model, shell, seg.to_string()));
        }
        shell.push_toast(format!("✓ chain: {} commands executed", seg_count));
        return all_cmds;
    }
    if let Some((to, rest)) = buf.strip_prefix("to:").and_then(|s| s.split_once(' ')) {
        let subject = rest.to_string();
        let body = String::new();
        return vec![Cmd::OrchestrationSend { to: to.to_string(), subject, body }];
    }

    // 解析 "pty:handle text" 格式(PTY 直接注入)
    if let Some((handle, text)) = buf.strip_prefix("pty:").and_then(|s| s.split_once(' ')) {
        if !text.is_ascii() {
            shell.push_toast("PTY: non-ASCII may fail".into());
        }
        return vec![Cmd::TerminalSend { handle: handle.to_string(), text: text.to_string() }];
    }

    // inject:<text> — PTY inject text to ALL selected agents (selected_set)
    if let Some(text) = buf.strip_prefix("inject:") {
        let text = text.trim();
        if text.is_empty() { return vec![]; }
        if shell.selected_set.is_empty() {
            shell.push_toast("No agents selected (Space to select)".into());
            return vec![];
        }
        let count = shell.selected_set.len();
        shell.push_toast(format!("⚡ injected into {count} agents"));
        return shell.selected_set.iter().map(|h| Cmd::TerminalSend {
            handle: h.clone(),
            text: text.to_string(),
        }).collect();
    }

    // 解析 "rename:handle new_title" 格式
    if let Some((handle, title)) = buf.strip_prefix("rename:").and_then(|s| s.split_once(' ')) {
        if title.is_empty() {
            return vec![];
        }
        return vec![Cmd::RenameTerminal { handle: handle.to_string(), new_title: title.to_string() }];
    }

    // 解析 "group:<name>" 格式(创建群组并自动加入)
    if let Some(name) = buf.strip_prefix("group:") {
        let name = name.trim().to_string();
        if name.is_empty() {
            return vec![];
        }
        let self_handle = std::env::var("ORCA_TERMINAL_HANDLE").unwrap_or_default();
        if !self_handle.is_empty() {
            model.groups.entry(name.clone()).or_default().insert(self_handle.clone());
        }
        shell.push_toast(format!("Joined group: {name}"));
        return vec![Cmd::PersistGroupJoin { name: name.clone(), handle: self_handle }, Cmd::WriteDirectory];
    }

    // 解析 "join:<group>" 格式(加入群组,用 self_handle)
    if let Some(rest) = buf.strip_prefix("join:") {
        let group_name = rest.trim().to_string();
        if group_name.is_empty() {
            return vec![];
        }
        let self_handle = std::env::var("ORCA_TERMINAL_HANDLE").unwrap_or_default();
        if self_handle.is_empty() {
            shell.push_toast("ORCA_TERMINAL_HANDLE not set".into());
            return vec![];
        }
        model.groups.entry(group_name.clone()).or_default().insert(self_handle.clone());
        shell.push_toast(format!("Joined group: {group_name}"));
        return vec![Cmd::PersistGroupJoin { name: group_name.clone(), handle: self_handle }, Cmd::WriteDirectory];
    }

    // 解析 "leave:<group>" 格式(退出群组)
    if let Some(name) = buf.strip_prefix("leave:") {
        let name = name.trim().to_string();
        if name.is_empty() {
            return vec![];
        }
        let self_handle = std::env::var("ORCA_TERMINAL_HANDLE").unwrap_or_default();
        if !self_handle.is_empty() {
            if let Some(members) = model.groups.get_mut(&name) {
                members.remove(&self_handle);
                if members.is_empty() {
                    model.groups.remove(&name);
                }
            }
        }
        shell.push_toast(format!("Left group: {name}"));
        return vec![Cmd::PersistGroupLeave { name: name.clone(), handle: self_handle }, Cmd::WriteDirectory];
    }

    // 解析 "broadcast:<group> <message>" 格式(群组广播)
    if let Some(rest) = buf.strip_prefix("broadcast:") {
        let parts: Vec<&str> = rest.trim_start().splitn(2, ' ').collect();
        let group_name = match parts.first() {
            Some(&n) if !n.is_empty() => n.to_string(),
            _ => return vec![],
        };
        let message = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
        if message.is_empty() {
            return vec![];
        }
        // 从 model 提取群组成员 handles
        let handles: Vec<String> = model.groups
            .get(&group_name)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        if handles.is_empty() {
            shell.push_toast(format!("group '{group_name}' has no members"));
            return vec![];
        }
        return vec![Cmd::GroupBroadcast { name: group_name, message, handles }];
    }

    // 解析 "create:command" 格式(在当前 worktree 创建终端)
    // 解析 "create:worktree:command" 格式(指定 worktree 创建终端)
    if let Some(rest) = buf.strip_prefix("create:") {
        if rest.is_empty() {
            return vec![];
        }
        // 检查是否包含第二个 ':' (worktree:command)
        if let Some((wt, cmd)) = rest.split_once(':') {
            if !cmd.is_empty() {
                return vec![Cmd::CreateTerminal {
                    worktree: Some(wt.to_string()),
                    command: cmd.to_string(),
                    title: None,
                }];
            }
        }
        // 无第二个 ':' → 纯 command
        return vec![Cmd::CreateTerminal {
            worktree: None,
            command: rest.to_string(),
            title: None,
        }];
    }

    // 解析 "config:key=value" 格式(设置配置项)
    if let Some(rest) = buf.strip_prefix("config:") {
        if let Some((key, value)) = rest.split_once('=') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key.is_empty() {
                shell.push_toast("config: key cannot be empty".into());
                return vec![];
            }
            // 验证已知 key
            match key.as_str() {
                "refresh_interval_ms" => {
                    if value.parse::<u64>().is_err() {
                        shell.push_toast("config: refresh_interval_ms must be a number".into());
                        return vec![];
                    }
                }
                "theme" | "default_filter" | "sort" => {}
                _ => {
                    shell.push_toast(format!("config: unknown key '{key}'"));
                    return vec![];
                }
            }
            return vec![Cmd::SetConfig { key, value }];
        }
        // 无 = → 无效
        shell.push_toast("config: use format config:key=value".into());
        return vec![];
    }

    // 解析 "reply:<msg_id> <body>" 格式(回复消息)
    if let Some((id, body)) = buf.strip_prefix("reply:").and_then(|s| s.split_once(' ')) {
        if body.is_empty() {
            return vec![];
        }
        return vec![Cmd::OrchestrationReply { id: id.to_string(), body: body.to_string() }];
    }

    // tag:add:handle tagname / tag:rm:handle tagname / tag:handle tagname (default add)
    if let Some(rest) = buf.strip_prefix("tag:") {
        let (action, rest2) = if let Some(r) = rest.strip_prefix("add:") { ("add", r) }
            else if let Some(r) = rest.strip_prefix("rm:") { ("rm", r) }
            else { ("add", rest) };
        if let Some((handle, tag)) = rest2.split_once(' ') {
            let tag = tag.trim().to_string();
            if !tag.is_empty() {
                match action {
                    "add" => {
                        model.add_tag(handle, &tag);
                        shell.push_toast(format!("tagged {handle} +{tag}"));
                        return vec![Cmd::PersistTagAdd { handle: handle.to_string(), tag }];
                    }
                    "rm" => {
                        model.remove_tag(handle, &tag);
                        shell.push_toast(format!("untagged {handle} -{tag}"));
                        return vec![Cmd::PersistTagRemove { handle: handle.to_string(), tag }];
                    }
                    _ => {}
                }
            }
        }
        shell.push_toast("tag: usage: tag:[add|rm]:handle tagname".into());
        return vec![];
    }

    // 解析 "batch:<handles-comma> <message>" 格式(批量发送)
    if let Some((handles_str, msg)) = buf.strip_prefix("batch:").and_then(|s| s.split_once(' ')) {
        if msg.is_empty() {
            return vec![];
        }
        let handles: Vec<String> = handles_str.split(',').map(|s| s.trim().to_string()).collect();
        shell.push_toast(format!("Batch sending to {} agents...", handles.len()));
        return handles.into_iter().map(|h| Cmd::OrchestrationSend {
            to: h,
            subject: msg.to_string(),
            body: String::new(),
        }).collect();
    }

    // tagged:<tagname> <message> — send to all agents with given tag
    if let Some((tag, msg)) = buf.strip_prefix("tagged:").and_then(|s| s.split_once(' ')) {
        let tag = tag.trim();
        if tag.is_empty() || msg.is_empty() { return vec![]; }
        let handles: Vec<String> = model.tags.iter()
            .filter(|(_, tags)| tags.iter().any(|t| t == tag))
            .map(|(h, _)| h.clone())
            .collect();
        if handles.is_empty() {
            shell.push_toast(format!("no agents tagged '{tag}'"));
            return vec![];
        }
        shell.push_toast(format!("tagged batch to {} agents ({tag})", handles.len()));
        return handles.into_iter().map(|h| Cmd::OrchestrationSend {
            to: h, subject: msg.to_string(), body: String::new(),
        }).collect();
    }

    // snip:name text... / snip:rm:name — save/remove snippet
    if let Some(rest) = buf.strip_prefix("snip:") {
        if let Some(name) = rest.strip_prefix("rm:") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                model.remove_snippet(&name);
                shell.push_toast(format!("snippet removed: {name}"));
                return vec![Cmd::RemoveSnippet { name }];
            }
            return vec![];
        }
        if let Some((name, text)) = rest.split_once(' ') {
            let name = name.trim().to_string();
            let text = text.trim().to_string();
            if !name.is_empty() && !text.is_empty() {
                model.add_snippet(&name, &text);
                shell.push_toast(format!("snippet saved: {name}"));
                return vec![Cmd::PersistSnippet { name, text }];
            }
        }
        shell.push_toast("snip: usage: snip:name command_text".into());
        return vec![];
    }

    // run:name — replay snippet (enter input mode with text pre-filled)
    if let Some(name) = buf.strip_prefix("run:") {
        let name = name.trim();
        if let Some(text) = model.get_snippet(name) {
            shell.insert_mode = true;
            shell.focus = FocusTarget::Input;
            shell.input_buf = text.to_string();
            shell.push_toast(format!("replaying snippet: {name} (Enter to confirm)"));
        } else {
            shell.push_toast(format!("snippet not found: {name}"));
        }
        return vec![Cmd::Noop];
    }

    // rule:add type:value / rule:rm:id — add/remove alert rule
    if let Some(rest) = buf.strip_prefix("rule:") {
        if let Some(id_str) = rest.strip_prefix("rm:") {
            if let Ok(id) = id_str.trim().parse::<i64>() {
                model.remove_alert_rule(id);
                shell.push_toast(format!("alert rule removed: {id}"));
                return vec![Cmd::RemoveAlertRule { id }];
            }
            return vec![];
        }
        if let Some(spec) = rest.strip_prefix("add ") {
            // spec = "state:Error" or "source:term_abc" or "severity:Warn" or "message"
            let spec = spec.trim();
            if spec == "message" {
                let rule = crate::model::AlertRule {
                    id: 0,
                    rule_type: crate::model::AlertRuleType::Message,
                    value: String::new(),
                    created_at_ms: crate::model::now_ms(),
                };
                model.add_alert_rule(rule.clone());
                shell.push_toast("alert rule added: message".into());
                return vec![Cmd::PersistAlertRule(rule)];
            }
            if let Some((rtype, value)) = spec.split_once(':') {
                if let Some(rt) = crate::model::AlertRuleType::from_str(rtype) {
                    let rule = crate::model::AlertRule {
                        id: 0,
                        rule_type: rt.clone(),
                        value: value.to_string(),
                        created_at_ms: crate::model::now_ms(),
                    };
                    model.add_alert_rule(rule.clone());
                    shell.push_toast(format!("alert rule added: {}:{}", rt.as_str(), value));
                    return vec![Cmd::PersistAlertRule(rule)];
                }
            }
            shell.push_toast("rule: usage: rule:add state:Error | source:handle | severity:Warn | message".into());
            return vec![];
        }
        shell.push_toast("rule: usage: rule:add <type>:<value> or rule:rm:<id>".into());
        return vec![];
    }

    // macro:record:<name> / macro:stop / macro:run:<name> / macro:rm:<name> / macro:list
    if let Some(rest) = buf.strip_prefix("macro:") {
        if let Some(name) = rest.strip_prefix("record:") {
            let name = name.trim().to_string();
            if name.is_empty() {
                return vec![];
            }
            shell.recording_active = true;
            shell.recording_buffer.clear();
            shell.recording_name = name.clone();
            shell.push_toast(format!("\u{25cf} REC {name} (Esc to stop)"));
            return vec![Cmd::Noop];
        }
        if rest == "stop" {
            if !shell.recording_active {
                return vec![];
            }
            let name = std::mem::take(&mut shell.recording_name);
            let events = std::mem::take(&mut shell.recording_buffer);
            shell.recording_active = false;
            if events.is_empty() {
                shell.push_toast("macro empty (no keys recorded)".into());
                return vec![];
            }
            let json = crate::model::serialize_key_events(&events);
            let m = crate::model::RecordedMacro { name: name.clone(), key_events_json: json, created_at_ms: crate::model::now_ms() };
            model.add_macro(m.clone());
            shell.push_toast(format!("macro saved: {name} ({} keys)", events.len()));
            return vec![Cmd::PersistMacro(m)];
        }
        if let Some(name) = rest.strip_prefix("run:") {
            let name = name.trim();
            if let Some(macro_obj) = model.get_macro(name) {
                let events = crate::model::deserialize_key_events(&macro_obj.key_events_json);
                let count = events.len();
                shell.replay_queue = events;
                shell.push_toast(format!("\u{25b6} replaying: {name} ({count} keys)"));
            } else {
                shell.push_toast(format!("macro not found: {name}"));
            }
            return vec![Cmd::Noop];
        }
        if let Some(name) = rest.strip_prefix("rm:") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                model.remove_macro(&name);
                shell.push_toast(format!("macro removed: {name}"));
                return vec![Cmd::RemoveMacro { name }];
            }
            return vec![];
        }
        if rest == "list" {
            shell.macro_overlay_active = !shell.macro_overlay_active;
            if shell.macro_overlay_active {
                shell.overlay_scroll = 0;
            }
            return vec![];
        }
        shell.push_toast("macro: usage: macro:record:<name> | macro:stop | macro:run:<name> | macro:rm:<name> | macro:list".into());
        return vec![];
    }

    // view:save:<name> / view:load:<name> / view:rm:<name> / view:list
    if let Some(rest) = buf.strip_prefix("view:") {
        if let Some(name) = rest.strip_prefix("save:") {
            let name = name.trim().to_string();
            if name.is_empty() { return vec![]; }
            let snapshot = ViewSnapshot {
                tab: match shell.tab { Tab::Directory => "directory", Tab::Groups => "groups", Tab::Messages => "messages" }.into(),
                filter_query: shell.filter_query.clone(),
                sort_mode: model.sort_mode().label().to_string(),
                selected_set: shell.selected_set.iter().cloned().collect(),
                created_at_ms: crate::model::now_ms(),
            };
            let json = serde_json::to_string(&snapshot).unwrap_or_default();
            model.add_saved_view(name.clone(), snapshot);
            shell.push_toast(format!("view saved: {name}"));
            return vec![Cmd::PersistView { name, json }];
        }
        if let Some(name) = rest.strip_prefix("load:") {
            let name = name.trim().to_string();
            if let Some(snapshot) = model.get_saved_view(&name).cloned() {
                shell.push_toast(format!("view loaded: {name}"));
                return apply_view_snapshot(shell, snapshot);
            } else {
                shell.push_toast(format!("view not found: {name}"));
                return vec![];
            }
        }
        if let Some(name) = rest.strip_prefix("rm:") {
            let name = name.trim().to_string();
            if !name.is_empty() && model.remove_saved_view(&name) {
                shell.push_toast(format!("view removed: {name}"));
                return vec![Cmd::RemoveView { name }];
            }
            return vec![];
        }
        if rest.trim() == "list" {
            shell.views_overlay_active = !shell.views_overlay_active;
            if shell.views_overlay_active { shell.overlay_scroll = 0; }
            return vec![];
        }
        shell.push_toast("view: usage: view:save:name | view:load:name | view:rm:name | view:list".into());
        return vec![];
    }

    // note:<handle> <text> / note:rm:<handle>
    if let Some(rest) = buf.strip_prefix("note:") {
        if let Some(handle) = rest.strip_prefix("rm:") {
            let handle = handle.trim().to_string();
            if !handle.is_empty() {
                model.remove_note(&handle);
                shell.push_toast(format!("note removed: {handle}"));
                return vec![Cmd::RemoveNote { handle }];
            }
            return vec![];
        }
        if let Some((handle, text)) = rest.split_once(' ') {
            let handle = handle.trim().to_string();
            let text = text.trim().to_string();
            if !handle.is_empty() && !text.is_empty() {
                model.add_note(&handle, &text);
                shell.push_toast(format!("note saved: {handle}"));
                return vec![Cmd::PersistNote { handle, text }];
            }
        }
        shell.push_toast("note: usage: note:<handle> <text> | note:rm:<handle>".into());
        return vec![];
    }

    // export:<path> / import:<path>
    if let Some(rest) = buf.strip_prefix("export:") {
        let path = rest.trim().to_string();
        if path.is_empty() {
            shell.push_toast("export: usage: export:<path>".into());
            return vec![];
        }
        let bundle = crate::model::ExportBundle {
            config: model.config.clone(),
            tags: model.tags.clone(),
            snippets: model.snippets.clone(),
            macros: model.macros.values().cloned().collect(),
            saved_views: model.saved_views.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            pinned: model.pinned.iter().cloned().collect(),
            alert_rules: model.alert_rules.clone(),
            notes: model.notes.clone(),
            aliases: model.aliases.clone(),
            hotkeys: model.hotkeys.clone(),
            watched: model.watched.iter().cloned().collect(),
        };
        let count = bundle.config.len() + bundle.tags.len() + bundle.snippets.len()
            + bundle.macros.len() + bundle.saved_views.len() + bundle.pinned.len()
            + bundle.alert_rules.len() + bundle.notes.len() + bundle.aliases.len() + bundle.hotkeys.len() + bundle.watched.len();
        shell.push_toast(format!("exporting {count} items to {path}…"));
        return vec![Cmd::ExportSettings { path, bundle }];
    }
    if let Some(rest) = buf.strip_prefix("import:") {
        let path = rest.trim().to_string();
        if path.is_empty() {
            shell.push_toast("import: usage: import:<path>".into());
            return vec![];
        }
        shell.push_toast(format!("importing from {path}…"));
        return vec![Cmd::ImportSettings { path }];
    }
    // alias:<name> <expansion> / alias:rm:<name>
    if let Some(rest) = buf.strip_prefix("alias:") {
        if let Some(name) = rest.strip_prefix("rm:") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                model.remove_alias(&name);
                shell.push_toast(format!("alias removed: {name}"));
                return vec![Cmd::RemoveAlias { name }];
            }
            return vec![];
        }
        if let Some((name, expansion)) = rest.split_once(' ') {
            let name = name.trim().to_string();
            let expansion = expansion.trim().to_string();
            if !name.is_empty() && !expansion.is_empty() {
                model.add_alias(&name, &expansion);
                shell.push_toast(format!("alias saved: {name} → {expansion}"));
                return vec![Cmd::PersistAlias { name, expansion }];
            }
        }
        shell.push_toast("alias: usage: alias:<name> <expansion> | alias:rm:<name>".into());
        return vec![];
    }

    // hotkey:<key> <command> / hotkey:rm:<key>
    if let Some(rest) = buf.strip_prefix("hotkey:") {
        if let Some(key) = rest.strip_prefix("rm:") {
            let key = key.trim().to_string();
            if !key.is_empty() {
                model.remove_hotkey(&key);
                shell.push_toast(format!("hotkey removed: {key}"));
                return vec![Cmd::RemoveHotkey { key }];
            }
            return vec![];
        }
        if let Some((key, command)) = rest.split_once(' ') {
            let key = key.trim().to_string();
            let command = command.trim().to_string();
            if key.len() == 1 && !command.is_empty() {
                model.add_hotkey(&key, &command);
                shell.push_toast(format!("hotkey saved: {key} → {command}"));
                return vec![Cmd::PersistHotkey { key, command }];
            } else if key.len() != 1 {
                shell.push_toast("hotkey: key must be a single character".into());
            }
        }
        shell.push_toast("hotkey: usage: hotkey:<key> <command> | hotkey:rm:<key>".into());
        return vec![];
    }
    // theme:<key> <color> — set theme color override (e.g. theme:accent #ff0000, theme:bg reset)
    if let Some(rest) = buf.strip_prefix("theme:") {
        if let Some((key, value)) = rest.split_once(' ') {
            let key = key.trim();
            let value = value.trim();
            let full_key = format!("theme.{key}");
            let valid_keys = ["fg", "bg", "accent", "muted", "working", "idle", "error", "warn",
                "border", "border_focus", "selection_bg", "selection_fg", "success", "tab_active", "tab_inactive"];
            if valid_keys.contains(&key) {
                if value == "reset" || crate::render::theme::parse_color(value).is_some() {
                    model.set_config(full_key.clone(), value.to_string());
                    shell.push_toast(format!("theme.{key} = {value}"));
                    return vec![Cmd::SetConfig { key: full_key, value: value.to_string() }];
                } else {
                    shell.push_toast(format!("invalid color: {value} (use #RRGGBB or named color)"));
                }
            } else {
                shell.push_toast(format!("unknown theme key: {key}"));
            }
        } else {
            shell.push_toast("theme: usage: theme:<key> <color> (e.g. theme:accent #ff0000)".into());
        }
        return vec![];
    }


    // watch:<handle> — toggle watch on agent
    if let Some(rest) = buf.strip_prefix("watch:") {
        let handle = rest.trim().to_string();
        if !handle.is_empty() {
            model.toggle_watch(&handle);
            let watching = model.is_watched(&handle);
            shell.push_toast(if watching { format!("👁 watching: {handle}") } else { format!("unwatched: {handle}") });
            return if watching { vec![Cmd::PersistWatchAdd { handle }] } else { vec![Cmd::PersistWatchRemove { handle }] };
        }
        shell.push_toast("watch: usage: watch:<handle>".into());
        return vec![];
    }

    vec![]
}

/// 输入模式键盘处理: 字符追加, Enter 发送, Esc 退出。
fn handle_input_key(model: &mut Model, _shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    match (k.code, k.modifiers) {
        // Enter: 发送
        (KeyCode::Enter, KeyModifiers::NONE) => {
            let buf = std::mem::take(&mut _shell.input_buf);
            _shell.insert_mode = false;
            _shell.focus = match _shell.tab {
                Tab::Directory => FocusTarget::Directory,
                Tab::Groups => FocusTarget::Groups,
                Tab::Messages => FocusTarget::Messages,
            };
            _shell.history_cursor = None; // reset recall on submit
            let mut cmds = dispatch_input(model, _shell, buf.clone());
            if !cmds.is_empty() && !buf.trim().is_empty() {
                model.push_history(buf.clone());
                cmds.push(Cmd::PersistHistoryEntry(buf));
            }
            cmds
        }

        (KeyCode::Up, KeyModifiers::NONE) => {
            let hist = &model.history;
            if !hist.is_empty() {
                match _shell.history_cursor {
                    None => {
                        _shell.saved_input = std::mem::take(&mut _shell.input_buf);
                        let idx = hist.len() - 1;
                        _shell.history_cursor = Some(idx);
                        _shell.input_buf = hist[idx].text.clone();
                    }
                    Some(i) if i > 0 => {
                        _shell.history_cursor = Some(i - 1);
                        _shell.input_buf = hist[i - 1].text.clone();
                    }
                    _ => {}
                }
            }
            vec![]
        }
        (KeyCode::Down, KeyModifiers::NONE) => {
            match _shell.history_cursor {
                Some(i) => {
                    let hist = &model.history;
                    if i + 1 < hist.len() {
                        _shell.history_cursor = Some(i + 1);
                        _shell.input_buf = hist[i + 1].text.clone();
                    } else {
                        _shell.history_cursor = None;
                        _shell.input_buf = std::mem::take(&mut _shell.saved_input);
                    }
                }
                None => {}
            }
            vec![]
        }

        // Backspace: 删除末字符
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            _shell.input_buf.pop();
            vec![]
        }

        // 可打印字符追加
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            if _shell.history_cursor.is_some() {
                _shell.history_cursor = None;
            }
            _shell.input_buf.push(c);
            vec![]
        }

        _ => vec![],
    }
}

// ───────────────────────── 命令面板键盘处理 ─────────────────────────

/// 命令面板键盘: 输入过滤, j/k/↑↓ 导航, Enter 执行, Esc 关闭。
fn handle_palette_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    match (k.code, k.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) => {
            shell.palette_active = false;
            shell.palette_query.clear();
            shell.palette_cursor = 0;
            vec![]
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            let cmds_filtered = crate::command::filter_commands(&shell.palette_query);
            if let Some(cmd) = cmds_filtered.get(shell.palette_cursor) {
                let handler = cmd.handler;
                let result = handler(model, shell);
                shell.palette_active = false;
                shell.palette_query.clear();
                shell.palette_cursor = 0;
                return result;
            }
            vec![]
        }
        (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            let len = crate::command::filter_commands(&shell.palette_query).len();
            if shell.palette_cursor + 1 < len {
                shell.palette_cursor += 1;
            }
            vec![]
        }
        (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            shell.palette_cursor = shell.palette_cursor.saturating_sub(1);
            vec![]
        }
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            shell.palette_query.pop();
            shell.palette_cursor = 0;
            vec![]
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            shell.palette_query.push(c);
            shell.palette_cursor = 0;
            vec![]
        }
        _ => vec![],
    }
}

// ───────────────────────── 搜索浮层键盘处理 ─────────────────────────

/// 搜索浮层键盘: 输入查询, j/k 导航, Enter 跳转, Esc 关闭。
fn handle_search_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    match (k.code, k.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) => {
            shell.search_active = false;
            shell.search_query.clear();
            shell.search_cursor = 0;
            vec![]
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            dispatch_search_jump(model, shell)
        }
        (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            let results = crate::model::global_search(model, &shell.search_query);
            if shell.search_cursor + 1 < results.len() {
                shell.search_cursor += 1;
            }
            vec![]
        }
        (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            shell.search_cursor = shell.search_cursor.saturating_sub(1);
            vec![]
        }
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            shell.search_query.pop();
            shell.search_cursor = 0;
            vec![]
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            shell.search_query.push(c);
            shell.search_cursor = 0;
            vec![]
        }
        _ => vec![],
    }
}

/// 搜索结果选中后的跳转分发。
fn dispatch_search_jump(model: &mut Model, shell: &mut Shell) -> Vec<Cmd> {
    let results = crate::model::global_search(model, &shell.search_query);
    let result = match results.get(shell.search_cursor) {
        Some(r) => r.clone(),
        None => {
            shell.search_active = false;
            shell.search_query.clear();
            shell.search_cursor = 0;
            return vec![];
        }
    };
    // 先关闭搜索
    shell.search_active = false;
    shell.search_query.clear();
    shell.search_cursor = 0;

    use crate::model::JumpTarget;
    match result.jump_target {
        JumpTarget::AgentHandle(handle) => {
            let sorted = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
            if let Some(idx) = sorted.iter().position(|h| h == &handle) {
                shell.tab = Tab::Directory;
                shell.cursor = idx;
                shell.focus = FocusTarget::Directory;
                shell.filter_active = false;
                shell.filter_query = None;
            }
            vec![]
        }
        JumpTarget::MessageId(id) => {
            let msgs: Vec<_> = model.messages.iter().rev().collect();
            if let Some(idx) = msgs.iter().position(|m| m.id == id) {
                shell.tab = Tab::Messages;
                shell.cursor = idx;
                shell.focus = FocusTarget::Messages;
                shell.filter_active = false;
                shell.filter_query = None;
            }
            vec![]
        }
        JumpTarget::CommandName(name) => {
            let all = crate::command::builtin_commands();
            if let Some(cmd) = all.iter().find(|c| c.name == name) {
                let handler = cmd.handler;
                return handler(model, shell);
            }
            vec![]
        }
        JumpTarget::EventIndex(idx) => {
            shell.activity_active = true;
            // events 浮层以 iter().rev() 渲染(最新在前); scroll 从顶部算
            // idx 是 model.events 中的位置(0=最旧); 最新 = len-1
            // 浮层 scroll=0 显示最新; scroll 增加显示更旧
            let total = model.events.len();
            shell.overlay_scroll = total.saturating_sub(1).saturating_sub(idx);
            vec![]
        }
        JumpTarget::HistoryIndex(idx) => {
            shell.history_overlay_active = true;
            let total = model.history.len();
            shell.overlay_scroll = total.saturating_sub(1).saturating_sub(idx);
            vec![]
        }
    }
}

// ───────────────────────── 过滤模式键盘处理 ─────────────────────────

/// 过滤模式键盘: 输入查询, Esc 退出, Enter 选中第一个结果。
fn handle_filter_key(_model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    match (k.code, k.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) => {
            shell.filter_active = false;
            shell.filter_query = None;
            shell.cursor = 0;
            vec![]
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            // 退出过滤模式,保留 cursor 在当前选中项
            shell.filter_active = false;
            vec![]
        }
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            if let Some(q) = shell.filter_query.as_mut() {
                q.pop();
            }
            shell.cursor = 0;
            vec![]
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) if c != 'j' && c != 'k' => {
            if let Some(q) = shell.filter_query.as_mut() {
                q.push(c);
            }
            shell.cursor = 0;
            vec![]
        }
        // 过滤模式下仍允许 j/k 导航
        (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, KeyModifiers::NONE) => {
            shell.cursor += 1;
            vec![]
        }
        (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, KeyModifiers::NONE) => {
            shell.cursor = shell.cursor.saturating_sub(1);
            vec![]
        }
        _ => vec![],
    }
}


// ───────────────────────── Quick-Switch 键盘处理 ─────────────────────────

/// Quick-Switch: fuzzy 搜索 agent handle/title/cwd, Enter 跳转 cursor。
fn handle_quickswitch_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    // 计算匹配的 handles (复用 directory 排序 + 模糊匹配)
    let sorted = crate::model::directory_sorted_with_mode(
        &model.directory, model.sort_mode(), &model.pinned,
    );
    let q = shell.quickswitch_query.to_ascii_lowercase();
    let matches: Vec<String> = if q.is_empty() {
        sorted
    } else {
        sorted.into_iter().filter(|h| {
            let agent = match model.directory.get(h) { Some(a) => a, None => return false };
            let title = agent.title.as_deref().unwrap_or("").to_ascii_lowercase();
            let cwd = agent.cwd.to_ascii_lowercase();
            let handle = h.to_ascii_lowercase();
            handle.contains(&q) || title.contains(&q) || cwd.contains(&q)
        }).collect()
    };

    match (k.code, k.modifiers) {
        (KeyCode::Esc, _) => {
            shell.quickswitch_active = false;
            shell.quickswitch_query.clear();
            shell.quickswitch_cursor = 0;
            vec![]
        }
        (KeyCode::Enter, _) => {
            shell.quickswitch_active = false;
            shell.quickswitch_query.clear();
            // 跳转到选中的 agent: 在完整 sorted 列表中找到 cursor 对应的 handle
            let full_sorted = crate::model::directory_sorted_with_mode(
                &model.directory, model.sort_mode(), &model.pinned,
            );
            let full_sorted = crate::model::apply_focus_filter(
                full_sorted, shell.focus_mode, &shell.selected_set,
            );
            if let Some(target) = matches.get(shell.quickswitch_cursor) {
                // 找到 target 在 full_sorted 中的 index
                if let Some(idx) = full_sorted.iter().position(|h| h == target) {
                    shell.cursor = idx;
                    shell.tab = Tab::Directory;
                    shell.focus = FocusTarget::Directory;
                }
            }
            shell.quickswitch_cursor = 0;
            vec![]
        }
        (KeyCode::Backspace, _) => {
            shell.quickswitch_query.pop();
            shell.quickswitch_cursor = 0;
            vec![]
        }
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
            if !matches.is_empty() {
                shell.quickswitch_cursor = (shell.quickswitch_cursor + 1) % matches.len();
            }
            vec![]
        }
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
            if !matches.is_empty() {
                if shell.quickswitch_cursor == 0 {
                    shell.quickswitch_cursor = matches.len() - 1;
                } else {
                    shell.quickswitch_cursor -= 1;
                }
            }
            vec![]
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            shell.quickswitch_query.push(c);
            shell.quickswitch_cursor = 0;
            vec![]
        }
        _ => vec![],
    }
}
fn handle_overlay_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    // Quick actions overlay: highest priority, handles its own keys with early return
    if shell.quick_actions_active {
        return match (k.code, k.modifiers) {
            (KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('o'), _) => {
                shell.quick_actions_active = false;
                vec![]
            }
            (KeyCode::Char('j') | KeyCode::Down, KeyModifiers::NONE) => {
                shell.quick_actions_cursor = (shell.quick_actions_cursor + 1) % 10;
                vec![]
            }
            (KeyCode::Char('k') | KeyCode::Up, KeyModifiers::NONE) => {
                shell.quick_actions_cursor = if shell.quick_actions_cursor == 0 { 9 } else { shell.quick_actions_cursor - 1 };
                vec![]
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                shell.quick_actions_active = false;
                let handle = match selected_agent_handle(model, shell) {
                    Some(h) => h,
                    None => return vec![],
                };
                match shell.quick_actions_cursor {
                    0 => {
                        shell.input_buf = format!("to:{handle} ");
                        shell.insert_mode = true;
                        shell.focus = FocusTarget::Input;
                        vec![]
                    }
                    1 => {
                        shell.input_buf = format!("pty:{handle} ");
                        shell.insert_mode = true;
                        shell.focus = FocusTarget::Input;
                        vec![]
                    }
                    2 => {
                        shell.input_buf = format!("rename:{handle} ");
                        shell.insert_mode = true;
                        shell.focus = FocusTarget::Input;
                        vec![]
                    }
                    3 => {
                        shell.input_buf = format!("tag:add:{handle} ");
                        shell.insert_mode = true;
                        shell.focus = FocusTarget::Input;
                        vec![]
                    }
                    4 => {
                        shell.note_overlay_active = true;
                        shell.note_viewing_handle = Some(handle.clone());
                        shell.note_edit_buf = model.get_note(&handle).cloned().unwrap_or_default();
                        vec![]
                    }
                    5 => {
                        model.toggle_pin(&handle);
                        let pinned = model.is_pinned(&handle);
                        shell.push_toast(if pinned { format!("📌 pinned: {handle}") } else { format!("unpinned: {handle}") });
                        if pinned { vec![Cmd::PersistPinAdd { handle }] } else { vec![Cmd::PersistPinRemove { handle }] }
                    }
                    6 => vec![Cmd::SwitchTerminal { handle }],
                    7 => vec![Cmd::ReadTerminal { handle }],
                    8 => vec![Cmd::CloseTerminal { handle }],
                    9 => {
                        model.toggle_watch(&handle);
                        let watching = model.is_watched(&handle);
                        shell.push_toast(if watching { format!("👁 watching: {handle}") } else { format!("unwatched: {handle}") });
                        if watching { vec![Cmd::PersistWatchAdd { handle }] } else { vec![Cmd::PersistWatchRemove { handle }] }
                    }
                    _ => vec![],
                }
            }
            _ => vec![],
        };
    }
    match (k.code, k.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            shell.dashboard_active = false;
            shell.overlay_content = None;
            shell.overlay_scroll = 0;
            shell.worktree_ps_active = false;
            shell.rule_overlay_active = false;
            shell.cheatsheet_active = false;
            shell.config_overlay_active = false;
            shell.orch_tasks_active = false;
            shell.activity_active = false;
            shell.snippet_overlay_active = false;
            shell.macro_overlay_active = false;
            shell.views_overlay_active = false;
            shell.note_overlay_active = false;
            shell.note_viewing_handle = None;
            shell.note_edit_buf.clear();
            shell.metrics_overlay_active = false;
            shell.quick_actions_active = false;
            shell.alias_overlay_active = false;
            shell.hotkeys_overlay_active = false;
            shell.theme_overlay_active = false;
            vec![]
        }
        (KeyCode::Char('c'), KeyModifiers::NONE) => {
            if shell.activity_active {
                model.clear_events();
            } else if shell.history_overlay_active {
                model.history.clear();
            }
            vec![]
        }
        (KeyCode::Enter, KeyModifiers::NONE) if shell.history_overlay_active => {
            shell.history_overlay_active = false;
            let idx = model.history.len().saturating_sub(1).saturating_sub(shell.overlay_scroll);
            if let Some(entry) = model.history.get(idx) {
                shell.insert_mode = true;
                shell.focus = FocusTarget::Input;
                shell.input_buf = entry.text.clone();
                shell.history_cursor = None;
            }
            shell.overlay_scroll = 0;
            vec![]
        }
        (KeyCode::Enter, KeyModifiers::NONE) if shell.snippet_overlay_active => {
            shell.snippet_overlay_active = false;
            let mut names: Vec<String> = model.snippets.keys().cloned().collect();
            names.sort();
            let idx = shell.overlay_scroll.min(names.len().saturating_sub(1));
            if let Some(name) = names.get(idx) {
                shell.insert_mode = true;
                shell.focus = FocusTarget::Input;
                shell.input_buf = format!("run:{}", name);
            }
            shell.overlay_scroll = 0;
            vec![]
        }

        (KeyCode::Enter, KeyModifiers::NONE) if shell.rule_overlay_active => {
            shell.rule_overlay_active = false;
            let idx = shell.overlay_scroll.min(model.alert_rules.len().saturating_sub(1));
            if let Some(rule) = model.alert_rules.get(idx) {
                let id = rule.id;
                model.remove_alert_rule(id);
                shell.push_toast("alert rule removed".into());
                shell.overlay_scroll = 0;
                return vec![Cmd::RemoveAlertRule { id }];
            }
            shell.overlay_scroll = 0;
            vec![]
        }

        // ── Macro overlay: Enter runs macro ──
        (KeyCode::Enter, KeyModifiers::NONE) if shell.macro_overlay_active => {
            shell.macro_overlay_active = false;
            let mut names: Vec<String> = model.macros.keys().cloned().collect();
            names.sort();
            let idx = shell.overlay_scroll.min(names.len().saturating_sub(1));
            if let Some(name) = names.get(idx) {
                if let Some(macro_obj) = model.get_macro(name) {
                    let events = crate::model::deserialize_key_events(&macro_obj.key_events_json);
                    shell.replay_queue = events.clone();
                    shell.push_toast(format!("\u{25b6} replaying: {name} ({} keys)", events.len()));
                }
            }
            shell.overlay_scroll = 0;
            vec![]
        }
        // ── Macro overlay: d deletes macro ──
        (KeyCode::Char('d'), KeyModifiers::NONE) if shell.macro_overlay_active => {
            let mut names: Vec<String> = model.macros.keys().cloned().collect();
            names.sort();
            let idx = shell.overlay_scroll.min(names.len().saturating_sub(1));
            if let Some(name) = names.get(idx).cloned() {
                model.remove_macro(&name);
                shell.push_toast(format!("macro removed: {name}"));
                shell.overlay_scroll = 0;
                return vec![Cmd::RemoveMacro { name }];
            }
            vec![]
        }
        // ── Views overlay: Enter loads view ──
        (KeyCode::Enter, KeyModifiers::NONE) if shell.views_overlay_active => {
            shell.views_overlay_active = false;
            let mut names: Vec<String> = model.saved_views.keys().cloned().collect();
            names.sort();
            let idx = shell.overlay_scroll.min(names.len().saturating_sub(1));
            if let Some(name) = names.get(idx).cloned() {
                if let Some(snapshot) = model.get_saved_view(&name).cloned() {
                    shell.push_toast(format!("view loaded: {name}"));
                    shell.overlay_scroll = 0;
                    return apply_view_snapshot(shell, snapshot);
                }
            }
            shell.overlay_scroll = 0;
            vec![]
        }
        // ── Views overlay: d deletes view ──
        (KeyCode::Char('d'), KeyModifiers::NONE) if shell.views_overlay_active => {
            let mut names: Vec<String> = model.saved_views.keys().cloned().collect();
            names.sort();
            let idx = shell.overlay_scroll.min(names.len().saturating_sub(1));
            if let Some(name) = names.get(idx).cloned() {
                model.remove_saved_view(&name);
                shell.push_toast(format!("view removed: {name}"));
                shell.overlay_scroll = 0;
                return vec![Cmd::RemoveView { name }];
            }
            vec![]
        }
        // ── Metrics overlay: w cycles time window ──
        (KeyCode::Char('w'), KeyModifiers::NONE) if shell.metrics_overlay_active => {
            shell.metrics_window = shell.metrics_window.cycle();
            shell.overlay_scroll = 0;
            vec![]
        }
        // ── Note overlay: Enter saves note ──
        (KeyCode::Enter, KeyModifiers::NONE) if shell.note_overlay_active => {
            if let Some(handle) = shell.note_viewing_handle.clone() {
                let text = shell.note_edit_buf.trim().to_string();
                if text.is_empty() {
                    model.remove_note(&handle);
                    shell.push_toast(format!("note removed: {handle}"));
                    shell.note_overlay_active = false;
                    shell.note_viewing_handle = None;
                    shell.note_edit_buf.clear();
                    return vec![Cmd::RemoveNote { handle }];
                } else {
                    model.add_note(&handle, &text);
                    shell.push_toast(format!("note saved: {handle}"));
                    shell.note_overlay_active = false;
                    shell.note_viewing_handle = None;
                    shell.note_edit_buf.clear();
                    return vec![Cmd::PersistNote { handle, text }];
                }
            }
            shell.note_overlay_active = false;
            vec![]
        }
        // ── Note overlay: char input ──
        (KeyCode::Char(c), KeyModifiers::NONE) if shell.note_overlay_active => {
            shell.note_edit_buf.push(c);
            return vec![];
        }
        // ── Note overlay: backspace ──
        (KeyCode::Backspace, KeyModifiers::NONE) if shell.note_overlay_active => {
            shell.note_edit_buf.pop();
            return vec![];
        }
        (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, KeyModifiers::NONE) => {
            shell.overlay_scroll = shell.overlay_scroll.saturating_add(1);
            vec![]
        }
        (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, KeyModifiers::NONE) => {
            shell.overlay_scroll = shell.overlay_scroll.saturating_sub(1);
            vec![]
        }
        _ => vec![],
    }
}

// ───────────────────────── 辅助 ─────────────────────────

/// 当前 tab 对应的列表长度(用于 cursor 边界)。
/// Directory tab: 按 directory_sorted_handles 排序(worktreePath 分组 + 最近活跃)。
#[must_use]
fn list_len<'a>(model: &'a Model, shell: &Shell) -> std::borrow::Cow<'a, [String]> {
    match shell.tab {
        Tab::Directory => {
            let sorted = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
            if shell.filter_active {
                let q = shell.filter_query.as_deref().unwrap_or("");
                std::borrow::Cow::Owned(crate::model::directory_filter_handles(
                    &sorted,
                    &model.directory,
                    q,
                    &model.tags,
                ))
            } else {
                std::borrow::Cow::Owned(sorted)
            }
        }
        Tab::Groups => std::borrow::Cow::Owned(
            model.groups.keys().cloned().collect::<Vec<_>>(),
        ),
        Tab::Messages => {
            let all_ids: Vec<String> = model.messages.iter().map(|m| m.id.clone()).collect();
            if shell.filter_active {
                let q = shell.filter_query.as_deref().unwrap_or("");
                std::borrow::Cow::Owned(crate::model::messages_filter_ids(
                    &model.messages, q,
                ))
            } else {
                std::borrow::Cow::Owned(all_ids)
            }
        }
    }
}

/// 当前选中 agent 的 handle(用于发送)。pub 供 command.rs 使用。
pub fn selected_agent_handle_public(model: &Model, shell: &Shell) -> Option<String> {
    selected_agent_handle(model, shell)
}

fn selected_agent_handle(model: &Model, shell: &Shell) -> Option<String> {
    let handles: Vec<String> = if shell.filter_active && shell.tab == Tab::Directory {
        let q = shell.filter_query.as_deref().unwrap_or("");
        let sorted = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
        crate::model::directory_filter_handles(&sorted, &model.directory, q, &model.tags)
    } else {
        directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned)
    };
    handles.get(shell.cursor).cloned()
}

/// 当前选中群组名(用于群组操作)。pub 供 command.rs 使用。
pub fn selected_group_name_public(model: &Model, shell: &Shell) -> Option<String> {
    selected_group_name(model, shell)
}

fn selected_group_name(model: &Model, shell: &Shell) -> Option<String> {
    if shell.tab != Tab::Groups {
        return None;
    }
    let mut names: Vec<String> = model.groups.keys().cloned().collect();
    names.sort();
    names.get(shell.cursor).cloned()
}

/// 鼠标点击命中测试: (x, y) 是否落在某个 agent card 上,返回 sorted index。
/// 使用 view::directory_layout + directory_scroll, 与 draw_directory 完全一致。
fn hit_test_card(model: &Model, shell: &Shell, x: u16, y: u16) -> Option<usize> {
    if !matches!(shell.tab, Tab::Directory) || model.directory.is_empty() {
        return None;
    }

    // 可用区域 = shell.size 减去 TabBar(1) + border(2) + input(1) + status(1)
    let inner_x = 1u16;
    let inner_y = 2u16;
    let inner_w = shell.size.0.saturating_sub(2);
    let inner_h = shell.size.1.saturating_sub(5);

    let sorted = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
    let layout = directory_layout(&sorted, model, inner_x, inner_w);
    let scroll_y = directory_scroll(shell.cursor, &layout, inner_h);

    for entry in &layout {
        if let LayoutItem::Card { sorted_idx } = entry.item {
            // 跳过完全滚出视口顶部的卡片(saturating_sub 钳位会导致误命中)
            if entry.y + entry.h <= scroll_y {
                continue;
            }
            let adj_y = entry.y.saturating_sub(scroll_y) + inner_y;
            // 点击落在卡片矩形内?
            if x >= entry.x
                && x < entry.x + entry.w
                && y >= adj_y
                && y < adj_y + entry.h
            {
                return Some(sorted_idx);
            }
        }
    }
    None
}

/// Yank text to system clipboard via pbcopy/xclip/xsel (zero deps).
fn yank_to_clipboard(text: &str) {
    use std::io::Write;
    // Try pbcopy (macOS) → xclip (Linux X11) → xsel (Linux)
    for (cmd, args) in [
        ("pbcopy", &[][..]),
        ("xclip", &["-selection", "clipboard"][..]),
        ("xsel", &["--clipboard", "--input"][..]),
    ] {
        if let Ok(mut child) = std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return; // success, stop trying
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn make_test_msg(id: &str, from: &str, to: &str, subject: &str) -> crate::model::OrchMessage {
        crate::model::OrchMessage {
            id: id.to_string(),
            from_handle: from.to_string(),
            to_handle: to.to_string(),
            subject: subject.to_string(),
            body: String::new(),
            msg_type: "status".to_string(),
            priority: String::new(),
            thread_id: None,
            payload: None,
            read: 0,
            sequence: 0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn update_quit_on_q() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('q'))));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::Quit)));
    }

    #[test]
    fn update_quit_on_ctrl_c() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(
            &mut model,
            &mut shell,
            AppMsg::Key(make_ctrl_key('c')),
        );
        assert!(cmds.iter().any(|c| matches!(c, Cmd::Quit)));
    }

    #[test]
    fn update_tab_cycles() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        assert_eq!(shell.tab, Tab::Directory);

        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Tab)));
        assert_eq!(shell.tab, Tab::Groups);

        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Tab)));
        assert_eq!(shell.tab, Tab::Messages);

        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Tab)));
        assert_eq!(shell.tab, Tab::Directory);
    }

    #[test]
    fn update_insert_mode() {
        let mut model = Model::new();
        let mut shell = Shell::new();

        // i 进入 insert
        let _ = update(
            &mut model,
            &mut shell,
            AppMsg::Key(make_key(KeyCode::Char('i'))),
        );
        assert!(shell.insert_mode);
        assert_eq!(shell.focus, FocusTarget::Input);

        // Esc 退出 insert
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Esc)));
        assert!(!shell.insert_mode);
        assert!(shell.input_buf.is_empty());
    }

    #[test]
    fn update_tick_increments_spinner() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let frame0 = shell.spinner_frame;

        let cmds = update(&mut model, &mut shell, AppMsg::Tick);
        assert_eq!(shell.spinner_frame, frame0 + 1);
        // 每次 tick 都 RefreshStatus
        assert!(cmds.iter().any(|c| matches!(c, Cmd::RefreshStatus)));
    }

    #[test]
    fn update_resize() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(
            &mut model,
            &mut shell,
            AppMsg::Resize { width: 120, height: 40 },
        );
        assert_eq!(shell.size, (120, 40));
        assert!(cmds.is_empty());
    }

    #[test]
    fn update_send_ok_toast() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(
            &mut model,
            &mut shell,
            AppMsg::SendOk("msg-123".to_string()),
        );
        assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistActivityEvent(_))));
        assert_eq!(shell.toasts.len(), 1);
        assert!(shell.toasts[0].0.contains("msg-123"));
    }

    #[test]
    fn update_toast_drain() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        shell.push_toast("old".to_string());
        assert_eq!(shell.toasts.len(), 1);

        // drain with 0s = 清理所有
        shell.drain_toasts(0);
        assert!(shell.toasts.is_empty());
    }

    #[test]
    fn update_agents_loaded_triggers_write() {
        let mut model = Model::new();
        let mut shell = Shell::new();

        let agents = vec![crate::model::Agent {
            handle: "h1".into(),
            pty_id: None,
            cwd: "/tmp".into(),
            worktree_id: "w1".into(),
            branch: "main".into(),
            tab_id: "t1".into(),
            leaf_id: "l1".into(),
            pane_key: "t1:l1".into(),
            title: None,
            connected: true,
            writable: true,
            source: None,
            state: None,
            last_output_at: None,
            prompt: None,
            tool_name: None,
            tool_input: None,
            last_assistant_msg: None,
            preview: None,
        }];

        let cmds = update(
            &mut model,
            &mut shell,
            AppMsg::AgentsLoaded(agents),
        );
        assert!(model.directory.contains_key("h1"));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::WriteDirectory)));
    }

    #[test]
    fn update_error_toast() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(
            &mut model,
            &mut shell,
            AppMsg::Error("boom".to_string()),
        );
        assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistActivityEvent(_))));
        assert_eq!(shell.toasts.len(), 1);
    }

    #[test]
    fn update_messages_delete_single() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        shell.tab = Tab::Messages;

        // 添加 3 条消息
        model.push_message(make_test_msg("m1", "alice", "bob", "hello"));
        model.push_message(make_test_msg("m2", "bob", "alice", "world"));
        model.push_message(make_test_msg("m3", "carol", "alice", "test"));

        // cursor=0 → 最新消息(m3, 因为 rev)
        shell.cursor = 0;
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('d'))));
        assert_eq!(model.messages.len(), 2);
        // m3 应该被删除
        assert!(model.messages.iter().all(|m| m.id != "m3"));
    }

    #[test]
    fn update_messages_clear_all() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        shell.tab = Tab::Messages;

        model.push_message(make_test_msg("m1", "alice", "bob", "hello"));
        model.push_message(make_test_msg("m2", "bob", "alice", "world"));

        let _ = update(&mut model, &mut shell, AppMsg::Key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)));
        assert!(model.messages.is_empty());
        assert_eq!(shell.cursor, 0);
    }

    #[test]
    fn update_messages_mark_read() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        shell.tab = Tab::Messages;

        model.push_message(make_test_msg("m1", "alice", "bob", "hello"));
        shell.cursor = 0;

        let cmds = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('m'))));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::MarkRead { delivery_id } if delivery_id == "m1")));
        // 本地也标记已读
        assert_eq!(model.messages[0].read, 1);
    }

    #[test]
    fn update_messages_filter_key() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        shell.tab = Tab::Messages;

        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('/'))));
        assert!(shell.filter_active);
        assert!(shell.filter_query.is_some());
    }

    #[test]
    fn update_ack_ok_toast() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(&mut model, &mut shell, AppMsg::AckOk("m1".to_string()));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::RefreshUnread)));
        assert_eq!(shell.toasts.len(), 1);
        assert!(shell.toasts[0].0.contains("Marked read"));
    }

    #[test]
    fn update_ack_failed_toast() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(&mut model, &mut shell, AppMsg::AckFailed("err".to_string()));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistActivityEvent(_))));
        assert_eq!(shell.toasts.len(), 1);
    }

    #[test]
    fn messages_filter_ids_by_from() {
        let mut model = Model::new();
        model.push_message(make_test_msg("m1", "alice", "bob", "hello"));
        model.push_message(make_test_msg("m2", "bob", "alice", "world"));
        model.push_message(make_test_msg("m3", "carol", "alice", "test"));

        let ids = crate::model::messages_filter_ids(&model.messages, "from:alice");
        assert_eq!(ids, vec!["m1"]);

        let all = crate::model::messages_filter_ids(&model.messages, "");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn messages_filter_ids_fuzzy() {
        let mut model = Model::new();
        model.push_message(make_test_msg("m1", "alice", "bob", "hello world"));
        model.push_message(make_test_msg("m2", "bob", "alice", "goodbye"));

        let ids = crate::model::messages_filter_ids(&model.messages, "hello");
        assert_eq!(ids, vec!["m1"]);
    }

    // ── quick-jump digit 1-9 tests ──

    fn make_agent(handle: &str, cwd: &str, last_output_at: Option<i64>) -> Agent {
        Agent {
            handle: handle.into(),
            pty_id: None,
            cwd: cwd.into(),
            worktree_id: format!("w-{handle}"),
            branch: "main".into(),
            tab_id: format!("t-{handle}"),
            leaf_id: format!("l-{handle}"),
            pane_key: String::new(),
            title: None,
            connected: true,
            writable: true,
            source: None,
            state: None,
            last_output_at,
            prompt: None,
            tool_name: None,
            tool_input: None,
            last_assistant_msg: None,
            preview: None,
        }
    }

    #[test]
    fn digit_jump_to_group_1() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        // group A (most active) at /home/yy/proj-a, group B at /home/yy/proj-b
        model.apply_agents(vec![
            make_agent("a1", "/home/yy/proj-a", Some(100)),
            make_agent("a2", "/home/yy/proj-a", Some(50)),
            make_agent("b1", "/home/yy/proj-b", Some(10)),
        ]);
        // sorted: a1(100), a2(50) → group 1; b1(10) → group 2
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('1'))));
        assert_eq!(shell.cursor, 0); // first agent in group 1
        assert!(shell.toasts.iter().any(|t| t.0.contains("Jumped to group 1")));
    }

    #[test]
    fn digit_jump_to_group_2() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        model.apply_agents(vec![
            make_agent("a1", "/home/yy/proj-a", Some(100)),
            make_agent("b1", "/home/yy/proj-b", Some(10)),
        ]);
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('2'))));
        assert_eq!(shell.cursor, 1); // b1 is at sorted index 1
        assert!(shell.toasts.iter().any(|t| t.0.contains("Jumped to group 2")));
    }

    #[test]
    fn digit_jump_out_of_range_ignored() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        model.apply_agents(vec![
            make_agent("a1", "/home/yy/proj-a", Some(100)),
        ]);
        shell.cursor = 0;
        // only 1 group; pressing 9 should be ignored
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('9'))));
        assert_eq!(shell.cursor, 0); // unchanged
        assert!(shell.toasts.is_empty()); // no toast
    }

    #[test]
    fn digit_jump_ignored_on_non_directory_tab() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        shell.tab = Tab::Groups;
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('1'))));
        assert!(shell.toasts.is_empty());
    }

    #[test]
    fn digit_jump_toast_shows_tilde_for_home() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        model.apply_agents(vec![
            make_agent("a1", "/home/yy/.orca", Some(100)),
        ]);
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('1'))));
        assert!(shell.toasts.iter().any(|t| t.0.contains("~/yy/.orca")));
    }

    #[test]
    fn digit_jump_non_home_path_shows_full() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        model.apply_agents(vec![
            make_agent("a1", "/opt/project", Some(100)),
        ]);
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('1'))));
        assert!(shell.toasts.iter().any(|t| t.0.contains("/opt/project")));
    }

    #[test]
    fn activity_log_note_event() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        let cmds = update(&mut model, &mut shell, AppMsg::Error("boom".to_string()));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistActivityEvent(_))));
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].severity, EventSeverity::Error);
    }

    #[test]
    fn activity_log_status_transition() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        // seed an agent in Working state
        let agent = crate::model::Agent {
            handle: "h1".to_string(),
            pty_id: None, cwd: "/tmp".to_string(), worktree_id: String::new(),
            branch: String::new(), tab_id: String::new(), leaf_id: String::new(),
            pane_key: String::new(), title: None, connected: true, writable: true,
            source: Some("claude".to_string()), state: Some("working".to_string()),
            prompt: None, tool_name: None, tool_input: None, last_assistant_msg: None,
            preview: None, last_output_at: None,
        };
        model.directory.insert("h1".to_string(), agent);
        // transition to error
        let status = crate::msg::AgentStatus {
            pane_key: String::new(), source: "claude".to_string(), state: "error".to_string(),
            worktree_id: String::new(),
            prompt: None, tool_name: None, tool_input: None, last_assistant_msg: None,
        };
        let cmds = update(&mut model, &mut shell, AppMsg::StatusUpdated(vec![status]));
        assert!(cmds.iter().any(|c| matches!(&c, Cmd::PersistActivityEvent(e) if e.category == EventCategory::State)));
    }

    #[test]
    fn history_recall_up_down() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        model.push_history("to:h1 hello".to_string());
        model.push_history("to:h2 world".to_string());
        // Enter input mode, type something
        shell.insert_mode = true;
        shell.input_buf = "draft".to_string();
        // Up: recall newest
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Up)));
        assert_eq!(shell.input_buf, "to:h2 world");
        assert_eq!(shell.history_cursor, Some(1));
        // Up again: older
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Up)));
        assert_eq!(shell.input_buf, "to:h1 hello");
        assert_eq!(shell.history_cursor, Some(0));
        // Down: newer
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Down)));
        assert_eq!(shell.input_buf, "to:h2 world");
        // Down past newest: restore draft
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Down)));
        assert_eq!(shell.input_buf, "draft");
        assert!(shell.history_cursor.is_none());
    }

    #[test]
    fn history_record_on_enter() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        shell.insert_mode = true;
        shell.input_buf = "to:h1 hello".to_string();
        let cmds = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Enter)));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistHistoryEntry(_))));
        assert_eq!(model.history.len(), 1);
    }

    #[test]
    fn search_open_and_close() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        // Ctrl-S opens
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_ctrl_key('s')));
        assert!(shell.search_active);
        // Esc closes
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Esc)));
        assert!(!shell.search_active);
    }

    #[test]
    fn search_jump_to_agent() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        // seed agent
        let mut agent = crate::model::Agent {
            handle: "term_abc".to_string(), pty_id: None, cwd: "/tmp".to_string(),
            worktree_id: String::new(), branch: String::new(), tab_id: String::new(),
            leaf_id: String::new(), pane_key: String::new(), title: None,
            connected: true, writable: true, source: Some("claude".to_string()),
            state: Some("working".to_string()), prompt: None, tool_name: None,
            tool_input: None, last_assistant_msg: None, preview: None, last_output_at: None,
        };
        model.directory.insert("term_abc".to_string(), agent);
        // open search, type query matching the handle
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_ctrl_key('s')));
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('a'))));
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('b'))));
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('c'))));
        // Enter → jump
        let cmds = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Enter)));
        assert!(!shell.search_active);
        assert_eq!(shell.tab, Tab::Directory);
    }
    #[test]
    fn pin_toggle_test() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        // seed an agent
        let agent = crate::model::Agent {
            handle: "term_test".to_string(), pty_id: None, cwd: "/tmp".to_string(),
            worktree_id: String::new(), branch: String::new(), tab_id: String::new(),
            leaf_id: String::new(), pane_key: String::new(), title: None,
            connected: true, writable: true, source: Some("claude".to_string()),
            state: Some("working".to_string()), prompt: None, tool_name: None,
            tool_input: None, last_assistant_msg: None, preview: None, last_output_at: None,
        };
        model.directory.insert("term_test".to_string(), agent);
        shell.tab = Tab::Directory;
        shell.cursor = 0;
        // Press @ to pin
        let cmds = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('@'))));
        assert!(model.is_pinned("term_test"));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistPinAdd { .. })));
        // Press @ again to unpin
        let cmds2 = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Char('@'))));
        assert!(!model.is_pinned("term_test"));
        assert!(cmds2.iter().any(|c| matches!(c, Cmd::PersistPinRemove { .. })));
    }

    #[test]
    fn dashboard_toggle_test() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        // D key (Shift+d) opens dashboard (Directory tab)
        shell.tab = Tab::Directory;
        let _ = update(&mut model, &mut shell, AppMsg::Key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)));
        assert!(shell.dashboard_active);
        // Esc closes
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Esc)));
        assert!(!shell.dashboard_active);
    }
    #[test]
    fn tag_add_and_remove() {
        let mut model = Model::new();
        model.directory.insert("term_a".to_string(), crate::model::Agent {
            handle: "term_a".to_string(), pty_id: None, cwd: "/tmp".to_string(),
            worktree_id: String::new(), branch: String::new(), tab_id: String::new(),
            leaf_id: String::new(), pane_key: String::new(), title: None,
            connected: true, writable: true, source: None, state: None,
            prompt: None, tool_name: None, tool_input: None, last_assistant_msg: None,
            preview: None, last_output_at: None,
        });
        model.add_tag("term_a", "frontend");
        assert!(model.has_tag("term_a", "frontend"));
        model.remove_tag("term_a", "frontend");
        assert!(!model.has_tag("term_a", "frontend"));
    }
    #[test]
    fn snippet_save_and_run() {
        let mut model = Model::new();
        let mut shell = Shell::new();
        // Enter input mode and save a snippet
        shell.insert_mode = true;
        shell.input_buf = "snip:greet to:term_abc Hello".to_string();
        let cmds = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Enter)));
        assert!(model.get_snippet("greet").is_some());
        assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistSnippet { .. })));
        // Run the snippet
        shell.insert_mode = true;
        shell.input_buf = "run:greet".to_string();
        let _ = update(&mut model, &mut shell, AppMsg::Key(make_key(KeyCode::Enter)));
        assert!(shell.insert_mode);
        assert_eq!(shell.input_buf, "to:term_abc Hello");
    }

    #[test]
    fn alert_rule_state_match() {
        let mut model = Model::new();
        model.add_alert_rule(crate::model::AlertRule {
            id: 1, rule_type: crate::model::AlertRuleType::State,
            value: "Error".to_string(), created_at_ms: 0,
        });
        let ctx = crate::model::CheckContext { new_state: Some("Error"), ..Default::default() };
        let toasts = crate::model::check_alert_rules(&model.alert_rules, &ctx);
        assert_eq!(toasts.len(), 1);
        // Non-matching state
        let ctx2 = crate::model::CheckContext { new_state: Some("Done"), ..Default::default() };
        assert!(crate::model::check_alert_rules(&model.alert_rules, &ctx2).is_empty());
    }
}
