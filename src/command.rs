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
            "quit",
            "Exit hub-tui",
            quit,
        ),
    ]
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
}
