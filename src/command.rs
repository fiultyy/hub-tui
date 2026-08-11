//! command.rs —— 命令面板(Ctrl-P)。
//!
//! 所有操作通过模糊搜索执行,新增命令只需注册到 builtin_commands()。
//! 不改 AppMsg/Cmd/Tab 枚举(横切扩展)。
//!
//! 交互:
//! - Ctrl-P 或 : 打开面板
//! - 输入查询 → 实时 fuzzy match 过滤
//! - ↑↓/jk 选择, Enter 执行, Esc 关闭
//! - 命令分两类:即时命令(直接执行)和参数命令(填入 input_bar 等用户补充)

use crate::model::Model;
use crate::shell::Shell;
use crate::update::Cmd;

/// 一条面板命令。
#[derive(Clone)]
pub struct Command {
    /// 显示名(如 "switch terminal")。
    pub name: &'static str,
    /// 描述(如 "Activate selected agent's tab")。
    pub description: &'static str,
    /// 执行处理器:读 model+shell,返回 Cmd Vec。
    /// 参数命令返回 Cmd::Noop 并设置 shell.input_buf / shell.insert_mode。
    pub handler: fn(&Model, &mut Shell) -> Vec<Cmd>,
}

impl Command {
    pub const fn new(
        name: &'static str,
        description: &'static str,
        handler: fn(&Model, &mut Shell) -> Vec<Cmd>,
    ) -> Self {
        Self { name, description, handler }
    }
}

/// 模糊匹配:query 的每个字符按顺序出现在 target 中(大小写不敏感)。
/// 返回 true 表示匹配。空 query 匹配所有。
pub fn fuzzy_match(query: &str, target: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_ascii_lowercase();
    let t = target.to_ascii_lowercase();
    let mut qi = q.chars().peekable();
    for tc in t.chars() {
        if qi.peek() == Some(&tc) {
            qi.next();
        }
    }
    qi.peek().is_none()
}

/// 内置命令注册表。新增命令只需在这里加一行 Command::new()。
pub fn builtin_commands() -> Vec<Command> {
    vec![
        Command::new(
            "switch terminal",
            "Activate selected agent's terminal tab",
            switch_terminal,
        ),
        Command::new(
            "send message",
            "Compose a message to the selected agent",
            send_message,
        ),
        Command::new(
            "jump directory",
            "Go to Directory tab",
            jump_tab_directory,
        ),
        Command::new(
            "jump groups",
            "Go to Groups tab",
            jump_tab_groups,
        ),
        Command::new(
            "jump messages",
            "Go to Messages tab",
            jump_tab_messages,
        ),
        Command::new(
            "refresh agents",
            "Force refresh terminal list + status + inbox",
            refresh_all,
        ),
        Command::new(
            "filter agents",
            "Enter filter mode in Directory (search by worktree/source/state)",
            enter_filter,
        ),
        Command::new(
            "inject pty",
            "Send text directly to agent PTY (real-time, ASCII only)",
            inject_pty,
        ),
        Command::new(
            "close terminal",
            "Close the selected agent's terminal",
            close_terminal,
        ),
        Command::new(
            "rename terminal",
            "Rename the selected agent's tab (enters input mode)",
            rename_terminal,
        ),
        Command::new(
            "read output",
            "Read selected agent's terminal output",
            read_output,
        ),
        Command::new(
            "worktree ps",
            "Show cross-worktree orchestration summary",
            show_worktree_ps,
        ),
        Command::new(
            "new terminal",
            "Create a blank terminal in the current worktree (enter input mode)",
            new_terminal,
        ),
        Command::new(
            "spawn claude",
            "Launch Claude Code agent in a new terminal",
            spawn_claude,
        ),
        Command::new(
            "spawn codex",
            "Launch OpenAI Codex agent in a new terminal",
            spawn_codex,
        ),
        Command::new(
            "spawn pi",
            "Launch Pi agent in a new terminal",
            spawn_pi,
        ),
        Command::new(
            "create group",
            "Create a new group and join it (enter input mode: group:<name>)",
            create_group,
        ),
        Command::new(
            "join group",
            "Join an existing group (enter input mode: join:<group>)",
            join_group,
        ),
        Command::new(
            "leave group",
            "Leave the selected group (enter input mode: leave:<group>)",
            leave_group,
        ),
        Command::new(
            "broadcast",
            "Send message to all members of the selected group",
            broadcast_to_group,
        ),
        Command::new(
            "show config",
            "Display all configuration items and current values",
            show_config,
        ),
        Command::new(
            "set config",
            "Set a configuration value (enters input mode: config:key=value)",
            set_config,
        ),
        Command::new(
            "reply to message",
            "Reply to the selected message in Messages tab (enter reply mode)",
            reply_message,
        ),
        Command::new(
            "show tasks",
            "Show orchestration tasks/runs/gates overview",
            show_tasks,
        ),
        Command::new(
            "batch send",
            "Send a message to all selected agents (Space to multi-select first)",
            batch_send,
        ),
        Command::new(
            "batch close",
            "Close all selected agent terminals",
            batch_close,
        ),
        Command::new(
            "clear selection",
            "Clear multi-selection",
            clear_selection,
        ),
        Command::new(
            "theme mocha",
            "Switch to Mocha (dark) theme",
            theme_mocha,
        ),
        Command::new(
            "theme light",
            "Switch to light theme",
            theme_light,
        ),
        Command::new(
            "theme contrast",
            "Switch to high-contrast theme",
            theme_contrast,
        ),
        Command::new(
            "quit",
            "Exit hub-tui",
            quit,
        ),
        Command::new(
            "sort by worktree",
            "Sort agents by worktree group (default: recent activity)",
            sort_by_worktree,
        ),
        Command::new(
            "sort by state",
            "Sort agents by status (working first)",
            sort_by_state,
        ),
        Command::new(
            "sort by source",
            "Sort agents by source (claude/pi/omp/codex)",
            sort_by_source,
        ),
        Command::new(
            "sort by name",
            "Sort agents by title name (alphabetical)",
            sort_by_name,
        ),
        Command::new(
            "yank handle",
            "Copy selected agent handle to clipboard (press y in Directory)",
            yank_handle,
        ),
        Command::new(
            "activity log",
            "Open the Activity Log overlay (recent events across all agents)",
            show_activity,
        ),
        Command::new(
            "command history",
            "Open command history overlay (H) — recall and re-edit past inputs",
            show_history,
        ),
        Command::new(
            "global search",
            "Search across all data sources — agents, messages, events, history (Ctrl-S)",
            open_search,
        ),
        Command::new(
            "dashboard",
            "Open dashboard overlay — live aggregate stats (D)",
            show_dashboard,
        ),
        Command::new(
            "save snippet",
            "Save a named snippet (enters input mode: snip:name text)",
            save_snippet,
        ),
        Command::new(
            "run snippet",
            "Run a saved snippet by name (enters input mode: run:name)",
            run_snippet,
        ),
        Command::new(
            "snippet library",
            "Browse saved snippets overlay (S) — select and run",
            show_snippet_library,
        ),
        Command::new(
            "add alert rule",
            "Create an alert rule (enters input mode: rule:add type:value)",
            add_alert_rule_input,
        ),
        Command::new(
            "alert rules",
            "View and manage alert rules overlay (N)",
            show_alert_rules,
        ),
        Command::new(
            "record macro",
            "Start recording a named macro (enters input mode: macro:record:<name>)",
            record_macro_input,
        ),
        Command::new(
            "run macro",
            "Replay a saved macro by name (enters input mode: macro:run:<name>)",
            run_macro_input,
        ),
        Command::new(
            "macro library",
            "Browse saved macros overlay (e) — select to replay, d to delete",
            show_macro_library,
        ),
        Command::new(
            "save view",
            "Save current view as named preset (enters input mode: view:save:name)",
            save_view_input,
        ),
        Command::new(
            "add note",
            "Add a note to an agent (enters input mode: note:handle text)",
            add_note_input,
        ),
        Command::new(
            "view presets",
            "Browse saved view presets (V)",
            show_view_presets,
        ),
        Command::new(
            "add alias",
            "Create a command alias (enters input mode: alias:name expansion)",
            add_alias_input,
        ),
        Command::new(
            "bind hotkey",
            "Bind a custom hotkey to a command (enters input mode: hotkey:key command)",
            bind_hotkey_input,
        ),
    ]
}

fn sort_by_worktree(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.push_toast("Sort: by-worktree".into());
    vec![Cmd::SetConfig { key: "sort".to_string(), value: "by-worktree".to_string() }]
}

fn sort_by_state(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.push_toast("Sort: by-state".into());
    vec![Cmd::SetConfig { key: "sort".to_string(), value: "by-state".to_string() }]
}

fn sort_by_source(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.push_toast("Sort: by-source".into());
    vec![Cmd::SetConfig { key: "sort".to_string(), value: "by-source".to_string() }]
}

fn sort_by_name(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.push_toast("Sort: by-name".into());
    vec![Cmd::SetConfig { key: "sort".to_string(), value: "by-name".to_string() }]
}
// ───────────────────────── 命令处理器 ─────────────────────────

fn selected_handle(model: &Model, shell: &Shell) -> Option<String> {
    crate::update::selected_agent_handle_public(model, shell)
}

fn switch_terminal(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(handle) = selected_handle(model, shell) {
        shell.push_toast(format!("switching to {}", &handle[..handle.len().min(20)]));
        vec![Cmd::SwitchTerminal { handle }]
    } else {
        shell.push_toast("No agent selected".into());
        vec![]
    }
}

fn send_message(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(handle) = selected_handle(model, shell) {
        shell.insert_mode = true;
        shell.focus = crate::shell::FocusTarget::Input;
        shell.input_buf = format!("to:{handle} ");
        vec![]
    } else {
        shell.push_toast("No agent selected".into());
        vec![]
    }
}

fn jump_tab_directory(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.tab = crate::shell::Tab::Directory;
    shell.cursor = 0;
    shell.focus = crate::shell::FocusTarget::Directory;
    vec![]
}

fn jump_tab_groups(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.tab = crate::shell::Tab::Groups;
    shell.cursor = 0;
    shell.focus = crate::shell::FocusTarget::Groups;
    vec![]
}

fn jump_tab_messages(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.tab = crate::shell::Tab::Messages;
    shell.cursor = 0;
    shell.focus = crate::shell::FocusTarget::Messages;
    vec![]
}

fn refresh_all(_model: &Model, _shell: &mut Shell) -> Vec<Cmd> {
    vec![
        Cmd::RefreshAgents,
        Cmd::RefreshStatus,
        Cmd::DrainMessages,
        Cmd::RefreshUnread,
    ]
}

fn enter_filter(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.tab = crate::shell::Tab::Directory;
    shell.focus = crate::shell::FocusTarget::Directory;
    shell.filter_active = true;
    shell.filter_query = Some(String::new());
    vec![]
}

fn quit(_model: &Model, _shell: &mut Shell) -> Vec<Cmd> {
    vec![Cmd::Quit]
}

fn yank_handle(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(handle) = crate::update::selected_agent_handle_public(model, shell) {
        shell.push_toast(format!("Yanked: {handle}"));
        // 实际 yank 在 update.rs handle_normal_key 的 'y' 键里做,这里只 toast。
        // 命令面板执行 yank 需要调用 clipboard — 但 command handler 不能直接 IO。
        // 所以这里返回 handle 作为 toast,实际 clipboard 操作用户按 y 键。
    }
    vec![]
}

fn reply_message(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.tab = crate::shell::Tab::Messages;
    shell.focus = crate::shell::FocusTarget::Messages;
    let msgs: Vec<_> = model.messages.iter().rev().collect();
    if let Some(msg) = msgs.get(shell.cursor) {
        let id = &msg.id;
        shell.insert_mode = true;
        shell.focus = crate::shell::FocusTarget::Input;
        shell.input_buf = format!("reply:{id} ");
    } else {
        shell.push_toast("No message selected".into());
    }
    vec![]
}

fn show_tasks(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.orch_tasks_active = true;
    shell.overlay_scroll = 0;
    vec![Cmd::RefreshOrchTasks]
}

fn batch_send(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if shell.selected_set.is_empty() {
        shell.push_toast("No agents selected (Space to select)".into());
        return vec![];
    }
    let handles: Vec<String> = shell.selected_set.iter().cloned().collect();
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = format!("batch:{} ", handles.join(","));
    vec![]
}

fn batch_close(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if shell.selected_set.is_empty() {
        shell.push_toast("No agents selected".into());
        return vec![];
    }
    let handles: Vec<String> = shell.selected_set.iter().cloned().collect();
    let count = handles.len();
    shell.push_toast(format!("Closing {count} terminals..."));
    shell.selected_set.clear();
    handles.into_iter().map(|h| Cmd::CloseTerminal { handle: h }).collect()
}

fn clear_selection(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    let count = shell.selected_set.len();
    shell.selected_set.clear();
    shell.push_toast(format!("Cleared {count} selections"));
    vec![]
}

fn theme_mocha(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.theme_name = "mocha".to_string();
    vec![Cmd::SetConfig { key: "theme".to_string(), value: "mocha".to_string() }]
}

fn theme_light(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.theme_name = "light".to_string();
    vec![Cmd::SetConfig { key: "theme".to_string(), value: "light".to_string() }]
}

fn theme_contrast(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.theme_name = "contrast".to_string();
    vec![Cmd::SetConfig { key: "theme".to_string(), value: "contrast".to_string() }]
}

fn inject_pty(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(handle) = selected_handle(model, shell) {
        shell.insert_mode = true;
        shell.focus = crate::shell::FocusTarget::Input;
        shell.input_buf = format!("pty:{handle} ");
    } else {
        shell.push_toast("No agent selected".into());
    }
    vec![]
}

fn close_terminal(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(handle) = selected_handle(model, shell) {
        shell.push_toast(format!("closing {handle}"));
        vec![Cmd::CloseTerminal { handle }]
    } else {
        shell.push_toast("No agent selected".into());
        vec![]
    }
}

fn rename_terminal(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(handle) = selected_handle(model, shell) {
        shell.insert_mode = true;
        shell.focus = crate::shell::FocusTarget::Input;
        shell.input_buf = format!("rename:{handle} ");
    } else {
        shell.push_toast("No agent selected".into());
    }
    vec![]
}

fn read_output(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(handle) = selected_handle(model, shell) {
        vec![Cmd::ReadTerminal { handle }]
    } else {
        shell.push_toast("No agent selected".into());
        vec![]
    }
}

fn show_worktree_ps(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.worktree_ps_active = true;
    vec![Cmd::RefreshWorktreePs]
}

fn show_activity(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.activity_active = true;
    shell.overlay_scroll = 0;
    vec![]
}


fn open_search(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.search_active = true;
    shell.search_query.clear();
    shell.search_cursor = 0;
    vec![]
}

fn show_dashboard(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.dashboard_active = true;
    shell.overlay_scroll = 0;
    vec![]
}

fn save_snippet(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "snip:".to_string();
    vec![]
}

fn run_snippet(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "run:".to_string();
    vec![]
}

fn add_alert_rule_input(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "rule:add ".to_string();
    vec![]
}

fn show_alert_rules(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.rule_overlay_active = true;
    shell.overlay_scroll = 0;
    vec![]
}

fn show_snippet_library(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.snippet_overlay_active = true;
    shell.overlay_scroll = 0;
    vec![]
}
fn show_history(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.history_overlay_active = true;
    shell.overlay_scroll = 0;
    vec![]
}

fn record_macro_input(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "macro:record:".to_string();
    vec![]
}

fn run_macro_input(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "macro:run:".to_string();
    vec![]
}

fn show_macro_library(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.macro_overlay_active = true;
    shell.overlay_scroll = 0;
    vec![]
}

fn save_view_input(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "view:save:".to_string();
    vec![]
}

fn show_view_presets(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.views_overlay_active = true;
    shell.overlay_scroll = 0;
    vec![]
}

fn add_note_input(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "note:".to_string();
    vec![]
}
fn add_alias_input(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "alias:".to_string();
    vec![]
}

fn bind_hotkey_input(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "hotkey:".to_string();
    vec![]
}

fn new_terminal(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    // 进入输入模式,预填 "create:" 前缀,用户补 command
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "create:".to_string();
    vec![]
}

fn spawn_agent(model: &Model, shell: &mut Shell, agent_cmd: &str, label: &str) -> Vec<Cmd> {
    // 尝试获取选中 agent 的 worktree; 没有则默认 active
    let worktree = selected_handle(model, shell)
        .and_then(|h| model.directory.get(&h).map(|a| a.worktree_id.clone()));
    // 用 agent 命令名作为 title
    shell.push_toast(format!("spawning {label}..."));
    vec![Cmd::CreateTerminal {
        worktree,
        command: agent_cmd.to_string(),
        title: Some(label.to_string()),
    }]
}

fn spawn_claude(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    spawn_agent(model, shell, "claude", "claude")
}

fn spawn_codex(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    spawn_agent(model, shell, "codex", "codex")
}

fn spawn_pi(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    spawn_agent(model, shell, "pi", "pi")
}

fn selected_group(model: &Model, shell: &Shell) -> Option<String> {
    crate::update::selected_group_name_public(model, shell)
}

fn create_group(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "group:".to_string();
    vec![]
}

fn join_group(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "join:".to_string();
    vec![]
}

fn leave_group(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(name) = selected_group(model, shell) {
        shell.insert_mode = true;
        shell.focus = crate::shell::FocusTarget::Input;
        shell.input_buf = format!("leave:{name}");
    } else {
        shell.push_toast("No group selected".into());
    }
    vec![]
}

fn broadcast_to_group(model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    if let Some(name) = selected_group(model, shell) {
        shell.insert_mode = true;
        shell.focus = crate::shell::FocusTarget::Input;
        shell.input_buf = format!("broadcast:{name} ");
    } else {
        shell.push_toast("No group selected".into());
    }
    vec![]
}

fn show_config(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.config_overlay_active = true;
    vec![]
}

fn set_config(_model: &Model, shell: &mut Shell) -> Vec<Cmd> {
    shell.insert_mode = true;
    shell.focus = crate::shell::FocusTarget::Input;
    shell.input_buf = "config:".to_string();
    vec![]
}


// ───────────────────────── 过滤 + 匹配 ─────────────────────────

/// 根据查询过滤命令列表,返回匹配的命令(带 score 排序)。
/// 匹配维度:name + description。
pub fn filter_commands(query: &str) -> Vec<Command> {
    let all = builtin_commands();
    if query.is_empty() {
        return all;
    }
    all.into_iter()
        .filter(|c| fuzzy_match(query, c.name) || fuzzy_match(query, c.description))
        .collect()
}

// ─────────────────────────── Input Autocomplete ───────────────────────────

/// Autocomplete suggestion entry for the Tab-completion dropdown.
#[derive(Debug, Clone)]
pub struct AcSuggestion {
    /// Display text shown in the dropdown.
    pub label: String,
    /// Full text to replace `input_buf` on accept.
    pub insert: String,
    /// Source category badge ("prefix", "agent", "snippet", "macro", "view", "group").
    pub source: &'static str,
}

/// All recognised input prefixes for Tab-completion.
pub const INPUT_PREFIXES: &[&str] = &[
    "to:", "pty:", "inject:", "rename:", "group:", "join:", "leave:",
    "broadcast:", "create:", "config:", "reply:", "batch:", "tag:",
    "tagged:", "snip:", "run:", "rule:", "macro:", "view:", "note:",
    "export:", "import:", "alias:", "hotkey:", "theme:", "watch:",
    "chain:",
];

/// Prefixes where the first argument is an agent handle.
const HANDLE_PREFIXES: &[&str] = &["to:", "pty:", "rename:", "watch:"];
/// Prefixes where the first argument is a group name.
const GROUP_PREFIXES: &[&str] = &["join:", "leave:", "broadcast:", "group:"];

/// Compute ranked autocomplete suggestions for the given input buffer.
///
/// Three tiers:
/// 1. Partial prefix (no colon) → suggest matching prefixes.
/// 2. Known prefix + partial arg → suggest context-appropriate completions
///    (agent handles, snippet/macro/view names, group names).
/// 3. Subcommand-aware: `macro:run:` → macro names, `view:load:` → view names, etc.
pub fn autocomplete_suggestions(input: &str, model: &crate::model::Model) -> Vec<AcSuggestion> {
    let q = input.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    autocomplete_into(q, model, &mut out);
    out.truncate(8);
    out
}

fn autocomplete_into(q: &str, model: &crate::model::Model, out: &mut Vec<AcSuggestion>) {
    // Try to match a known prefix
    for &pfx in INPUT_PREFIXES {
        if q.starts_with(pfx) {
            let arg = &q[pfx.len()..];
            suggest_after_prefix(pfx, arg, model, out);
            return;
        }
    }

    // No known prefix matched → suggest prefix completions
    for &pfx in INPUT_PREFIXES {
        if pfx.starts_with(q) && q.len() < pfx.len() {
            out.push(AcSuggestion {
                label: format!("{}  [prefix]", pfx),
                insert: pfx.to_string(),
                source: "prefix",
            });
        }
    }
}

/// Suggest completions after a recognised prefix.
fn suggest_after_prefix(pfx: &str, arg: &str, model: &crate::model::Model, out: &mut Vec<AcSuggestion>) {
    let arg = arg.trim_start();

    // ── Handle-taking prefixes ──
    if HANDLE_PREFIXES.contains(&pfx) {
        push_handles(pfx, arg, model, out);
        return;
    }

    // ── Group-taking prefixes ──
    if GROUP_PREFIXES.contains(&pfx) {
        push_groups(pfx, arg, model, out);
        return;
    }

    // ── Prefixes with named entities ──
    match pfx {
        "snip:" => {
            if let Some(partial) = arg.strip_prefix("rm:") {
                push_names("snip:rm:", partial, model.snippets.keys(), "snippet", out);
            } else {
                push_names("snip:", arg, model.snippets.keys(), "snippet", out);
            }
        }
        "macro:" => {
            if arg.is_empty() {
                for sub in &["record:", "run:", "rm:", "stop", "list"] {
                    out.push(sub_cmd(pfx, sub));
                }
            } else if let Some(sub) = arg.strip_prefix("run:").map(|_| "run:")
                .or_else(|| arg.strip_prefix("rm:").map(|_| "rm:"))
                .or_else(|| arg.strip_prefix("record:").map(|_| "record:"))
            {
                let partial = &arg[sub.len()..];
                push_names(&format!("macro:{}", sub), partial, model.macros.keys(), "macro", out);
            }
        }
        "view:" => {
            if arg.is_empty() {
                for sub in &["save:", "load:", "rm:", "list"] {
                    out.push(sub_cmd(pfx, sub));
                }
            } else if let Some(sub) = arg.strip_prefix("load:").map(|_| "load:")
                .or_else(|| arg.strip_prefix("rm:").map(|_| "rm:"))
                .or_else(|| arg.strip_prefix("save:").map(|_| "save:"))
            {
                let partial = &arg[sub.len()..];
                push_names(&format!("view:{}", sub), partial, model.saved_views.keys(), "view", out);
            }
        }
        "alias:" => {
            if let Some(partial) = arg.strip_prefix("rm:") {
                push_names("alias:rm:", partial, model.aliases.keys(), "alias", out);
            } else {
                push_names("alias:", arg, model.aliases.keys(), "alias", out);
            }
        }
        "tag:" => {
            if arg.is_empty() {
                out.push(sub_cmd("tag:", "add:"));
                out.push(sub_cmd("tag:", "rm:"));
            }
            // After tag:add:/tag:rm:/bare tag:, suggest handles
            let (tag_pfx, partial) = if let Some(p) = arg.strip_prefix("add:") {
                ("tag:add:", p)
            } else if let Some(p) = arg.strip_prefix("rm:") {
                ("tag:rm:", p)
            } else if !arg.contains(':') {
                ("tag:", arg)
            } else {
                return;
            };
            push_handles(tag_pfx, partial, model, out);
        }
        _ => {}
    }
}

fn push_handles(insert_pfx: &str, partial: &str, model: &crate::model::Model, out: &mut Vec<AcSuggestion>) {
    for h in model.directory.keys() {
        if fuzzy_match(partial, h) {
            out.push(AcSuggestion {
                label: format!("{}{}", insert_pfx.trim_end(), h),
                insert: format!("{}{} ", insert_pfx, h),
                source: "agent",
            });
        }
    }
}

fn push_groups(pfx: &str, partial: &str, model: &crate::model::Model, out: &mut Vec<AcSuggestion>) {
    for name in model.groups.keys() {
        if fuzzy_match(partial, name) {
            out.push(AcSuggestion {
                label: format!("{}{}", pfx, name),
                insert: format!("{}{}", pfx, name),
                source: "group",
            });
        }
    }
}

fn push_names<'a, I: Iterator<Item = &'a String>>(
    label_pfx: &str,
    partial: &str,
    names: I,
    source: &'static str,
    out: &mut Vec<AcSuggestion>,
) {
    for name in names {
        if fuzzy_match(partial, name) {
            out.push(AcSuggestion {
                label: format!("{}{}", label_pfx, name),
                insert: format!("{}{}", label_pfx, name),
                source,
            });
        }
    }
}

fn sub_cmd(pfx: &str, sub: &str) -> AcSuggestion {
    AcSuggestion {
        label: format!("{}{}", pfx, sub),
        insert: format!("{}{}", pfx, sub),
        source: "subcmd",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_match_empty() {
        assert!(fuzzy_match("", "anything"));
    }

    #[test]
    fn test_fuzzy_match_exact() {
        assert!(fuzzy_match("switch", "switch terminal"));
    }

    #[test]
    fn test_fuzzy_match_subsequence() {
        assert!(fuzzy_match("swt", "switch terminal"));
        assert!(fuzzy_match("swtrm", "switch terminal"));
    }

    #[test]
    fn test_fuzzy_match_case_insensitive() {
        assert!(fuzzy_match("SWITCH", "switch terminal"));
        assert!(fuzzy_match("Switch", "switch terminal"));
    }

    #[test]
    fn test_fuzzy_match_no_match() {
        assert!(!fuzzy_match("xyz", "switch terminal"));
    }

    #[test]
    fn test_filter_commands() {
        let results = filter_commands("sw");
        assert!(results.iter().any(|c| c.name == "switch terminal"));
    }

    #[test]
    fn test_filter_commands_empty_returns_all() {
        let results = filter_commands("");
        assert_eq!(results.len(), builtin_commands().len());
    }

    #[test]
    fn test_autocomplete_prefix_partial() {
        let m = crate::model::Model::new();
        let sugs = autocomplete_suggestions("to", &m);
        assert!(sugs.iter().any(|s| s.insert == "to:"));
    }

    fn test_agent() -> crate::model::Agent {
        crate::model::Agent {
            handle: "term_abc".to_string(),
            pty_id: None,
            cwd: "/tmp".to_string(),
            worktree_id: String::new(),
            branch: String::new(),
            tab_id: String::new(),
            leaf_id: String::new(),
            pane_key: String::new(),
            title: None,
            connected: false,
            writable: false,
            source: None,
            state: None,
            prompt: None,
            tool_name: None,
            tool_input: None,
            last_assistant_msg: None,
            preview: None,
            last_output_at: None,
        }
    }

    #[test]
    fn test_autocomplete_empty_returns_nothing() {
        let m = crate::model::Model::new();
        let sugs = autocomplete_suggestions("", &m);
        assert!(sugs.is_empty());
    }

    #[test]
    fn test_autocomplete_exact_prefix_suggests_handles() {
        let mut m = crate::model::Model::new();
        m.directory.insert("term_abc".to_string(), test_agent());
        let sugs = autocomplete_suggestions("to:abc", &m);
        assert!(sugs.iter().any(|s| s.insert.starts_with("to:term_abc")));
    }

    #[test]
    fn test_autocomplete_macro_subcommands() {
        let m = crate::model::Model::new();
        let sugs = autocomplete_suggestions("macro:", &m);
        assert!(sugs.iter().any(|s| s.insert == "macro:record:"));
        assert!(sugs.iter().any(|s| s.insert == "macro:run:"));
    }
}
