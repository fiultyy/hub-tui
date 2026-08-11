//! transport.rs —— CLI 子进程包装(同步, 无 async)。
//!
//! 三项外部 IO:
//! - `fetch_terminals()`: 调 `orca-ide terminal list --json`, 解析为 `Vec<Agent>`
//! - `read_last_status()`: 读 `~/.config/orca/agent-hooks/last-status.json`
//! - `last_status_mtime()`: stat 该文件 mtime(mtime poll, 非 inotify, ADR-5)

use std::fs;
use std::process::Command;
use std::time::SystemTime;

use crate::model::Agent;

// ───────────────────────── JSON 镜像(仅解析用) ─────────────────────────

/// 对齐 `orca-ide terminal list --json` 的顶层壳。
#[derive(serde::Deserialize)]
struct CliResult {
    result: CliTerminals,
}

#[derive(serde::Deserialize)]
struct CliTerminals {
    terminals: Vec<serde_json::Value>,
}

/// 对齐 `last-status.json` 顶层壳。
#[derive(serde::Deserialize)]
struct LastStatusFile {
    entries: std::collections::HashMap<String, LastStatusEntry>,
}

/// last-status.json 单条 entry(只取 join 需要的字段)。
#[derive(serde::Deserialize)]
struct LastStatusEntry {
    #[serde(rename = "paneKey")]
    pane_key: String,
    source: String,
    /// `payload.state` 里存放实际状态。
    payload: LastStatusPayload,
    #[serde(rename = "worktreeId")]
    worktree_id: String,
}

#[derive(serde::Deserialize)]
struct LastStatusPayload {
    state: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(rename = "toolName", default)]
    tool_name: Option<String>,
    #[serde(rename = "toolInput", default)]
    tool_input: Option<String>,
    #[serde(rename = "lastAssistantMessage", default)]
    last_assistant_msg: Option<String>,
}

// ───────────────────────── 公开函数 ─────────────────────────

/// 跑 `orca-ide terminal list --json`, 解析返回 `Vec<Agent>`。
pub fn fetch_terminals() -> Result<Vec<Agent>, String> {
    let output = Command::new("orca-ide")
        .args(["terminal", "list", "--json"])
        .output()
        .map_err(|e| format!("orca-ide terminal list failed: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "orca-ide terminal list exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let raw: CliResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("parse terminal list JSON: {e}"))?;

    raw.result
        .terminals
        .into_iter()
        .map(|v| {
            serde_json::from_value(v).map_err(|e| format!("parse agent entry: {e}"))
        })
        .collect()
}

/// 读 `~/.config/orca/agent-hooks/last-status.json`, 转为 `Vec<AgentStatus>`。
pub fn read_last_status() -> Result<Vec<crate::msg::AgentStatus>, String> {
    let path = last_status_path();
    let text = fs::read_to_string(&path)
        .map_err(|e| format!("read last-status.json: {e}"))?;
    let file: LastStatusFile = serde_json::from_str(&text)
        .map_err(|e| format!("parse last-status.json: {e}"))?;

    Ok(file
        .entries
        .into_values()
        .map(|e| crate::msg::AgentStatus {
            pane_key: e.pane_key,
            source: e.source,
            state: e.payload.state,
            worktree_id: e.worktree_id,
            prompt: e.payload.prompt,
            tool_name: e.payload.tool_name,
            tool_input: e.payload.tool_input,
            last_assistant_msg: e.payload.last_assistant_msg,
        })
        .collect())
}

/// stat `last-status.json` mtime。文件不存在返回 `None`(静默)。
pub fn last_status_mtime() -> Option<SystemTime> {
    fs::metadata(last_status_path())
        .ok()
        .and_then(|m| m.modified().ok())
}

// ───────────────────────── orchestration CLI 包装(ADR-4) ─────────────────────────

// ───────────────────────── 内部 ─────────────────────────

fn last_status_path() -> std::path::PathBuf {
    dirs_home()
        .join(".config")
        .join("orca")
        .join("agent-hooks")
        .join("last-status.json")
}


fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
    .unwrap_or_else(|_| "/tmp".into())
}

// ───────────────────────── terminal 操作 CLI 包装 ─────────────────────────

/// PTY 直接注入: `orca-ide terminal send --text <text> --enter`。
/// 踩坑: 非 ASCII 可能被 crossterm 拆成无效 key event(见 orca-com skill)。
pub fn terminal_send(handle: &str, text: &str) -> Result<usize, String> {
    let output = Command::new("orca-ide")
        .args(["terminal", "send", "--terminal", handle, "--text", text, "--enter"])
        .output()
        .map_err(|e| format!("terminal send failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "terminal send exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(text.len())
}

/// 关闭终端: `orca-ide terminal close --terminal <handle>`。
pub fn terminal_close(handle: &str) -> Result<(), String> {
    let output = Command::new("orca-ide")
        .args(["terminal", "close", "--terminal", handle])
        .output()
        .map_err(|e| format!("terminal close failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "terminal close exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// 重命名终端 tab: `orca-ide terminal rename --terminal <handle> --title <name>`。
pub fn terminal_rename(handle: &str, title: &str) -> Result<(), String> {
    let output = Command::new("orca-ide")
        .args(["terminal", "rename", "--terminal", handle, "--title", title])
        .output()
        .map_err(|e| format!("terminal rename failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "terminal rename exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// 读终端输出: `orca-ide terminal read --terminal <handle>`。
pub fn terminal_read_output(handle: &str) -> Result<String, String> {
    let output = Command::new("orca-ide")
        .args(["terminal", "read", "--terminal", handle])
        .output()
        .map_err(|e| format!("terminal read failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "terminal read exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// terminal create 结果: `orca-ide terminal create --json` 返回 handle 等字段。
#[derive(serde::Deserialize)]
#[allow(dead_code)] // worktree_id 对齐 orca-ide JSON, 只用 handle/title
pub struct CreateTerminalResult {
    /// 新终端 handle(用于后续 send/switch/close 等操作)。
    pub handle: String,
    /// 终端 title(可能为空)。
    #[serde(default)]
    pub title: Option<String>,
    /// worktreeId。
    #[serde(default)]
    pub worktree_id: Option<String>,
}

/// 对齐 `orca-ide terminal create --json` 顶层壳。
#[derive(serde::Deserialize)]
struct CreateTerminalOutput {
    result: CreateTerminalData,
}

#[derive(serde::Deserialize)]
struct CreateTerminalData {
    terminal: CreateTerminalResult,
}

/// 创建终端: `orca-ide terminal create --worktree <sel> --command <cmd> --title <name> --json`。
/// 返回 handle(用于后续操作)。
pub fn terminal_create(
    worktree: Option<&str>,
    command: &str,
    title: Option<&str>,
) -> Result<CreateTerminalResult, String> {
    let mut args = vec!["terminal", "create", "--json"];
    if let Some(wt) = worktree {
        args.extend_from_slice(&["--worktree", wt]);
    }
    args.extend_from_slice(&["--command", command]);
    if let Some(t) = title {
        args.extend_from_slice(&["--title", t]);
    }
    let output = Command::new("orca-ide")
        .args(&args)
        .output()
        .map_err(|e| format!("terminal create failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "terminal create exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let parsed: CreateTerminalOutput = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("parse terminal create JSON: {e}"))?;
    Ok(parsed.result.terminal)
}

/// worktree ps: `orca-ide worktree ps --json`。
pub fn fetch_worktree_ps() -> Result<Vec<crate::model::WorktreePsEntry>, String> {
    let output = Command::new("orca-ide")
        .args(["worktree", "ps", "--json"])
        .output()
        .map_err(|e| format!("worktree ps failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "worktree ps exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let raw: WorktreePsResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("parse worktree ps JSON: {e}"))?;
    Ok(raw.result.worktrees)
}

/// worktree ps JSON 壳。
#[derive(serde::Deserialize)]
struct WorktreePsResult {
    result: WorktreePsData,
}

#[derive(serde::Deserialize)]
struct WorktreePsData {
    #[serde(default)]
    worktrees: Vec<crate::model::WorktreePsEntry>,
}

/// fetch run-list: `orca-ide orchestration run-list --json`.
pub fn fetch_run_list() -> Result<Vec<crate::model::OrchRunEntry>, String> {
    let output = Command::new("orca-ide")
        .args(["orchestration", "run-list", "--json"])
        .output()
        .map_err(|e| format!("run-list failed: {e}"))?;
    if !output.status.success() { return Ok(Vec::new()); }
    let raw: GenericListResult = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| GenericListResult { result: GenericListData { tasks: None, runs: None, gates: None, items: None } });
    raw.result.tasks
        .or(raw.result.items)
        .or(raw.result.runs)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect::<Vec<_>>()
        .pipe(Ok)
}

/// fetch task-list。
pub fn fetch_task_list() -> Result<Vec<crate::model::OrchTaskEntry>, String> {
    let output = Command::new("orca-ide")
        .args(["orchestration", "task-list", "--json"])
        .output()
        .map_err(|e| format!("task-list failed: {e}"))?;
    if !output.status.success() { return Ok(Vec::new()); }
    let raw: GenericListResult = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| GenericListResult { result: GenericListData { tasks: None, runs: None, gates: None, items: None } });
    raw.result.tasks
        .or(raw.result.items)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect::<Vec<_>>()
        .pipe(Ok)
}

/// fetch gate-list。
pub fn fetch_gate_list() -> Result<Vec<crate::model::OrchGateEntry>, String> {
    let output = Command::new("orca-ide")
        .args(["orchestration", "gate-list", "--json"])
        .output()
        .map_err(|e| format!("gate-list failed: {e}"))?;
    if !output.status.success() { return Ok(Vec::new()); }
    let raw: GenericListResult = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| GenericListResult { result: GenericListData { tasks: None, runs: None, gates: None, items: None } });
    raw.result.gates
        .or(raw.result.items)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect::<Vec<_>>()
        .pipe(Ok)
}
/// 泛用 list 结果壳(兼容 tasks/runs/gates/items 字段名)。
#[derive(serde::Deserialize)]
struct GenericListResult {
    result: GenericListData,
}

#[derive(serde::Deserialize)]
struct GenericListData {
    #[serde(default)]
    tasks: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    runs: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    gates: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    items: Option<Vec<serde_json::Value>>,
}

/// Pipe trait(Rust 没有 std pipe,简化链式)。
trait Pipe: Sized { fn pipe<F, R>(self, f: F) -> R where F: FnOnce(Self) -> R { f(self) } }
impl<T> Pipe for T {}
