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
        if msg.read {
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

// ───────────────────────── 测试 ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_last_status_path_non_empty() {
        let p = last_status_path();
        assert!(p.to_string_lossy().contains("last-status.json"));
    }
}
