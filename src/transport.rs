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

/// 对齐 `orca orchestration send` stdout 格式。
#[derive(serde::Deserialize)]
struct SendOutput {
    result: SendResult,
}

#[derive(serde::Deserialize)]
struct SendResult {
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// 对齐 `orca orchestration check --json` 顶层壳。
#[derive(serde::Deserialize)]
struct CheckOutput {
    result: CheckResult,
}

#[derive(serde::Deserialize)]
struct CheckResult {
    messages: Vec<crate::model::OrchMessage>,
}


/// ADR-4: 发消息(orchestration send)。
///
/// 跑 `orca orchestration send --to <to> --type status --subject <subject> --body <body>`。
/// 解析 stdout: 含 `message_id`/`id` → Ok(id), 含 `error` 或解析失败 → Err(stderr)。
pub fn orchestration_send(to: &str, subject: &str, body: &str) -> Result<String, String> {
    let out = std::process::Command::new("orca")
        .args([
            "orchestration",
            "send",
            "--to", to,
            "--type", "status",
            "--subject", subject,
            "--body", body,
        ])
        .output()
        .map_err(|e| format!("failed to spawn orca: {e}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(stderr.trim().to_string());
    }

    // 尝试 JSON 解析
    if let Ok(parsed) = serde_json::from_str::<SendOutput>(&stdout) {
        if let Some(id) = parsed.result.message_id.or(parsed.result.id) {
            return Ok(id);
        }
        if let Some(err) = parsed.result.error {
            return Err(err);
        }
    }

    // fallback: 解析 "Sent msg_xxx" 文本
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Sent ") {
            return Ok(rest.trim().to_string());
        }
        // 也尝试直接在任意行找 msg_ 前缀
        for word in line.split_whitespace() {
            if word.starts_with("msg_") {
                return Ok(word.to_string());
            }
        }
    }

    Err(format!("unexpected send output: {stdout}"))
}

/// ADR-4: drain inbox(orchestration check)。
///
/// 跑 `orca orchestration check --json`, 解析 messages 数组。
pub fn orchestration_check() -> Result<Vec<crate::model::OrchMessage>, String> {
    let out = std::process::Command::new("orca")
        .args(["orchestration", "check", "--json"])
        .output()
        .map_err(|e| format!("failed to spawn orca: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(stderr.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let parsed: CheckOutput = serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse check output: {e}"))?;

    Ok(parsed.result.messages)
}

/// 跑 `orca-ide orchestration inbox --json`, 解析全量 inbox 获取每个 handle 的未读数。
/// 用于 TUI 显示 agent card 上的未读 badge。
pub fn orchestration_inbox_unread() -> Result<std::collections::HashMap<String, usize>, String> {
    let out = std::process::Command::new("orca-ide")
        .args(["orchestration", "inbox", "--json"])
        .output()
        .map_err(|e| format!("failed to spawn orca-ide inbox: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(stderr.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let parsed: CheckOutput = serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse inbox output: {e}"))?;

    let mut unread: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for msg in &parsed.result.messages {
        if msg.read != 0 {
            continue;
        }
        *unread.entry(msg.to_handle.clone()).or_default() += 1;
    }
    Ok(unread)
}

/// ADR-4: 群发(循环 send)。
///
/// 串行对每个 handle 调用 `orchestration_send`, 返回每个结果。
pub fn group_broadcast(
    handles: &[String],
    subject: &str,
    body: &str,
) -> Vec<Result<String, String>> {
    handles
        .iter()
        .map(|h| orchestration_send(h, subject, body))
        .collect()
}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_last_status_path_non_empty() {
        let p = last_status_path();
        assert!(p.to_string_lossy().contains("last-status.json"));
    }

    /// 回归: orchestration inbox --json 的真实 schema 是 snake_case + read 为 int 0/1。
    /// OrchMessage 之前用 camelCase rename + bool read, 导致反序列化整体失败。
    #[test]
    fn test_inbox_json_parses_snake_case_and_int_read() {
        let raw = r#"{
            "result": {
                "messages": [
                    {"id":"m1","from_handle":"a","to_handle":"hX","subject":"s","body":"","type":"status","read":0,"created_at":"2026-01-01T00:00:00Z","sequence":1},
                    {"id":"m2","from_handle":"b","to_handle":"hX","subject":"s","body":"","type":"status","read":0,"created_at":"2026-01-01T00:00:00Z","sequence":2},
                    {"id":"m3","from_handle":"c","to_handle":"hY","subject":"s","body":"","type":"status","read":1,"created_at":"2026-01-01T00:00:00Z","sequence":3}
                ]
            }
        }"#;
        let parsed: CheckOutput = serde_json::from_str(raw).expect("must parse snake_case inbox json");
        let mut unread = std::collections::HashMap::new();
        for msg in &parsed.result.messages {
        if msg.read != 0 { continue; }
            *unread.entry(msg.to_handle.clone()).or_default() += 1;
        }
        assert_eq!(unread.get("hX"), Some(&2), "hX has two unread");
        assert!(unread.get("hY").is_none(), "hY has no unread entries");
    }
}
