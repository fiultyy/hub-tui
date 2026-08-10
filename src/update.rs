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
    /// 持久化群组离开。
    PersistGroupLeave { name: String, handle: String },
    /// 无操作。
    Noop,
    /// 退出。
    Quit,
}

/// 周期性刷新间隔(tick 数)。tick=50ms → 5 tick=250ms 用于 agents 刷新(ADR-5: 5s)。
/// 精确 5s = 100 tick, 但这里用较小值让首屏更快加载。
const REFRESH_AGENTS_INTERVAL: usize = 100; // 50ms * 100 = 5s

/// Tick 计数器(追踪周期性任务)。
/// 存在 Shell 上不合适(数据态), 这里用 thread-local-free 方案:
/// update 每次收到 Tick 时判断 model.generation 变化次数。
/// 简化: 用 shell.spinner_frame 翻转频率间接计时。
/// 更简洁: 在 update 内维护 tick_count, 传入 model/shell 改太侵入。
/// 最终: 每次 Tick 都返回 RefreshStatus(轻量 stat), RefreshAgents 用固定间隔。
fn should_refresh_agents(shell: &Shell) -> bool {
    // spinner_frame 在每次 Tick +1, 约 50ms/帧。
    // 100 frames ≈ 5s
    shell.spinner_frame % REFRESH_AGENTS_INTERVAL == 0
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
            // 周期性刷新 agents(5s)
            if should_refresh_agents(shell) {
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

        // ──── socket 查询请求(ADR-3) ────
        AppMsg::SocketQuery(req) => {
            vec![Cmd::QuerySocket { req }]
        }

        // ──── 通用错误 ────
        AppMsg::Error(e) => {
            shell.push_toast(e);
            vec![]
        }

        // ──── 退出 ────
        AppMsg::Quit => vec![Cmd::Quit],
    }
}

// ───────────────────────── 键盘处理 ─────────────────────────

/// 键盘处理。全局快捷键优先(q/Ctrl+C 退出, Tab 切 tab), 其余按 insert_mode 分流。
fn handle_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    // 全局快捷键(不受 insert_mode 影响)
    match (k.code, k.modifiers) {
        // 退出
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

        // Enter(in Directory tab): 选中 agent, 进入输入模式准备发送
        (KeyCode::Enter, KeyModifiers::NONE) => {
            let selected_handle = selected_agent_handle(model, shell);
            if let Some(handle) = selected_handle {
                shell.insert_mode = true;
                shell.focus = FocusTarget::Input;
                shell.input_buf = format!("to:{handle} ");
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

            // 解析 "to:handle subject body" 格式
            if let Some((to, rest)) = buf.strip_prefix("to:").and_then(|s| s.split_once(' ')) {
                let subject = rest.to_string();
                let body = String::new(); // 简化: 单行 subject 作 body
                return vec![Cmd::OrchestrationSend { to: to.to_string(), subject, body }];
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

// ───────────────────────── 辅助 ─────────────────────────

/// 当前 tab 对应的列表长度(用于 cursor 边界)。
/// Directory tab: 按 directory_sorted_handles 排序(状态分区顺序)。
#[must_use]
fn list_len<'a>(model: &'a Model, shell: &Shell) -> std::borrow::Cow<'a, [String]> {
    match shell.tab {
        Tab::Directory => {
            std::borrow::Cow::Owned(directory_sorted_handles(&model.directory))
        }
        Tab::Groups => std::borrow::Cow::Owned(
            model.groups.keys().cloned().collect::<Vec<_>>(),
        ),
        Tab::Messages => std::borrow::Cow::Owned(
            model.messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        ),
    }
}

/// 当前选中 agent 的 handle(用于发送)。
/// Directory tab: 按 directory_sorted_handles 排序(与 cursor 索引一致)。
fn selected_agent_handle(model: &Model, shell: &Shell) -> Option<String> {
    match shell.tab {
        Tab::Directory => directory_sorted_handles(&model.directory)
            .get(shell.cursor)
            .cloned(),
        _ => None,
    }
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
            title: None,
            connected: true,
            writable: true,
            source: None,
            state: None,
            last_output_at: None,
            prompt: None,
            tool_name: None,
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
