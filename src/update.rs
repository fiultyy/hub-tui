//! update.rs —— 范式 2 纯函数 reducer + 范式 3 异步派发命令(ADR-1 + ADR-7)。
//!
//! `fn update(model, shell, msg) -> Vec<Cmd>`: 纯函数, **绝不 IO**。
//! - Model/Shell 改在原地(&mut); Cmd 返回 Vec 给 service.rs 执行(范式 3 fire-and-forget)。
//! - 状态转移穷尽 match AppMsg 所有 variant。
//! - send 回灌: Cmd::OrchestrationSend → service spawn → AppMsg::SendOk/SendFailed 回来(ADR-7)。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::msg::AppMsg;
use crate::model::{directory_sorted_handles, Agent, OrchMessage, Model};
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
    /// spawn: 并行刷新 run-list + task-list + gate-list 编排快照。
    RefreshOrchTasks,
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

// ───────────────────────── update(纯 reducer) ─────────────────────────

/// 纯函数 reducer。绝不 IO, 改 Model/Shell + 返 Cmd Vec。
pub fn update(model: &mut Model, shell: &mut Shell, msg: AppMsg) -> Vec<Cmd> {
    match msg {
        AppMsg::Key(k) => handle_key(model, shell, k),

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
            cmds
        }

        // ──── terminal list 加载完成 ────
        AppMsg::AgentsLoaded(agents) => {
            let persist = agents.clone();
            model.apply_agents(agents);
            vec![Cmd::PersistAgents(persist), Cmd::WriteDirectory]
        }

        // ──── last-status.json 刷新结果 ────
        AppMsg::StatusUpdated(statuses) => {
            model.apply_status(statuses);
            vec![Cmd::WriteDirectory]
        }

        // ──── inbox 未读数刷新 ────
        AppMsg::UnreadUpdated(counts) => {
            model.apply_unread(counts);
            vec![Cmd::WriteDirectory]
        }

        // ──── 编排发送成功(ADR-7 回灌) ────
        AppMsg::SendOk(id) => {
            shell.push_toast(format!("Sent: {id}"));
            vec![]
        }

        // ──── 编排发送失败(ADR-7 回灌) ────
        AppMsg::SendFailed(e) => {
            shell.push_toast(format!("Send failed: {e}"));
            vec![]
        }

        AppMsg::MessagesDrained(msgs) => {
            let persist = msgs.clone();
            for msg in msgs {
                model.push_message(msg);
            }
            vec![Cmd::PersistMessages(persist)]
        }

        AppMsg::SocketQuery(req) => {
            vec![Cmd::QuerySocket { req }]
        }

        // ──── PTY 注入结果 ────
        AppMsg::InjectOk(n) => {
            shell.push_toast(format!("PTY: sent {n} bytes"));
            vec![]
        }
        AppMsg::InjectFailed(e) => {
            shell.push_toast(format!("PTY failed: {e}"));
            vec![]
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
            let label = title.as_deref().unwrap_or(&handle);
            shell.push_toast(format!("Created: {label}"));
            // 刷新通信录,让新终端出现在 Directory
            vec![Cmd::RefreshAgents, Cmd::WriteDirectory]
        }
        // ──── 群组操作成功 ────
        AppMsg::GroupActionOk(msg) => {
            shell.push_toast(msg);
            vec![Cmd::RefreshAgents]
        }

        // ──── 信息 toast ────
        AppMsg::Info(msg) => {
            shell.push_toast(msg);
            vec![Cmd::RefreshAgents]
        }

        // ──── 通用错误 ────
        AppMsg::Error(e) => {
            shell.push_toast(e);
            vec![]
        }

        // ──── 配置更新回灌 ────
        AppMsg::ConfigUpdated { key, value } => {
            model.set_config(key.clone(), value);
            shell.push_toast(format!("config: {key} updated"));
            vec![]
        }

        // ──── 编排快照回灌 ────
        AppMsg::OrchSnapshotLoaded(snapshot) => {
            model.apply_orch_snapshot(*snapshot);
            vec![]
        }

        // ──── 退出 ────
        AppMsg::Quit => vec![Cmd::Quit],
    }
}

// ───────────────────────── 键盘处理 ─────────────────────────

/// 键盘处理。全局快捷键优先(q/Ctrl+C 退出, Tab 切 tab), 其余按 insert_mode 分流。
fn handle_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    // 命令面板激活时,所有键盘事件走面板处理
    if shell.palette_active {
        return handle_palette_key(model, shell, k);
    }
    // 过滤模式激活时,键盘事件走过滤输入处理
    if shell.filter_active {
        return handle_filter_key(model, shell, k);
    }
    // 浮层激活时(overlay_content / worktree_ps / group_detail / cheatsheet / config),键盘走浮层处理
    if shell.overlay_content.is_some() || shell.worktree_ps_active || shell.group_detail_active || shell.cheatsheet_active || shell.config_overlay_active || shell.orch_tasks_active {
        return handle_overlay_key(model, shell, k);
    }


    match (k.code, k.modifiers) {
        // Ctrl-P 或 : 打开命令面板
        (KeyCode::Char('p'), KeyModifiers::CONTROL) | (KeyCode::Char(':'), KeyModifiers::NONE) => {
            shell.palette_active = true;
            shell.palette_query.clear();
            shell.palette_cursor = 0;
            return vec![];
        }

        // / 进入过滤模式(Directory tab)
        (KeyCode::Char('/'), KeyModifiers::NONE)
            if !shell.insert_mode && shell.tab == Tab::Directory =>
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

        _ => vec![],
    }
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

            // 解析 "to:handle subject body" 格式(编排 inbox)
            if let Some((to, rest)) = buf.strip_prefix("to:").and_then(|s| s.split_once(' ')) {
                let subject = rest.to_string();
                let body = String::new();
                return vec![Cmd::OrchestrationSend { to: to.to_string(), subject, body }];
            }

            // 解析 "pty:handle text" 格式(PTY 直接注入)
            if let Some((handle, text)) = buf.strip_prefix("pty:").and_then(|s| s.split_once(' ')) {
                if !text.is_ascii() {
                    _shell.push_toast("PTY: non-ASCII may fail".into());
                }
                return vec![Cmd::TerminalSend { handle: handle.to_string(), text: text.to_string() }];
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
                _shell.push_toast(format!("Joined group: {name}"));
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
                    _shell.push_toast("ORCA_TERMINAL_HANDLE not set".into());
                    return vec![];
                }
                model.groups.entry(group_name.clone()).or_default().insert(self_handle.clone());
                _shell.push_toast(format!("Joined group: {group_name}"));
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
                _shell.push_toast(format!("Left group: {name}"));
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
                    _shell.push_toast(format!("group '{group_name}' has no members"));
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
                        _shell.push_toast("config: key cannot be empty".into());
                        return vec![];
                    }
                    // 验证已知 key
                    match key.as_str() {
                        "refresh_interval_ms" => {
                            if value.parse::<u64>().is_err() {
                                _shell.push_toast("config: refresh_interval_ms must be a number".into());
                                return vec![];
                            }
                        }
                        "theme" | "default_filter" => {}
                        _ => {
                            _shell.push_toast(format!("config: unknown key '{key}'"));
                            return vec![];
                        }
                    }
                    return vec![Cmd::SetConfig { key, value }];
                }
                // 无 = → 无效
                _shell.push_toast("config: use format config:key=value".into());
                return vec![];
            }

            // 解析 "reply:<msg_id> <body>" 格式(回复消息)
            if let Some((id, body)) = buf.strip_prefix("reply:").and_then(|s| s.split_once(' ')) {
                if body.is_empty() {
                    return vec![];
                }
                return vec![Cmd::OrchestrationReply { id: id.to_string(), body: body.to_string() }];
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

// ───────────────────────── 浮层键盘处理 ─────────────────────────

/// 浮层键盘: j/k 滚动, Esc/q 关闭。
fn handle_overlay_key(_model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    match (k.code, k.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
            shell.overlay_content = None;
            shell.overlay_scroll = 0;
            shell.worktree_ps_active = false;
            shell.group_detail_active = false;
            shell.cheatsheet_active = false;
            shell.config_overlay_active = false;
            shell.orch_tasks_active = false;
            vec![]
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
            let sorted = directory_sorted_handles(&model.directory);
            if shell.filter_active {
                let q = shell.filter_query.as_deref().unwrap_or("");
                std::borrow::Cow::Owned(crate::model::directory_filter_handles(
                    &sorted,
                    &model.directory,
                    q,
                ))
            } else {
                std::borrow::Cow::Owned(sorted)
            }
        }
        Tab::Groups => std::borrow::Cow::Owned(
            model.groups.keys().cloned().collect::<Vec<_>>(),
        ),
        Tab::Messages => std::borrow::Cow::Owned(
            model.messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        ),
    }
}

/// 当前选中 agent 的 handle(用于发送)。pub 供 command.rs 使用。
pub fn selected_agent_handle_public(model: &Model, shell: &Shell) -> Option<String> {
    selected_agent_handle(model, shell)
}

fn selected_agent_handle(model: &Model, shell: &Shell) -> Option<String> {
    let handles: Vec<String> = if shell.filter_active && shell.tab == Tab::Directory {
        let q = shell.filter_query.as_deref().unwrap_or("");
        let sorted = directory_sorted_handles(&model.directory);
        crate::model::directory_filter_handles(&sorted, &model.directory, q)
    } else {
        directory_sorted_handles(&model.directory)
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

    let sorted = directory_sorted_handles(&model.directory);
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
        assert!(cmds.is_empty());
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
        assert!(cmds.is_empty());
        assert_eq!(shell.toasts.len(), 1);
    }
}
