//! socket.rs —— Unix socket 服务端 (ADR-3 + ADR-6)。
//!
//! 接受 agent Unix socket 连接,处理群组低频只读查询。
//! 方案: SocketServer 持有 `Arc<RwLock<Model>>` + `Db` (Clone,内部 Arc 共享),
//! handle_connection 直接 lock+读 model 回应查询,不走 fan-in channel。
//! Group mutations persist to Db for restart survival.

use std::io::{BufRead, BufReader, Write};
use std::thread;
use std::os::unix::net::{UnixListener, UnixStream};

use parking_lot::RwLock;
use std::sync::Arc;

use super::model::Model;
use super::db::Db;

/// Unix socket 文件路径(ADR-3: 固定路径,启动时清理残留)。
const SOCKET_PATH: &str = "/tmp/orca-hub.sock";

// ───────────────────────── SocketServer ─────────────────────────

/// Unix socket 服务端句柄(RAII guard)。
/// Drop 时清理 socket 文件。accept 线程 detached。
pub struct SocketServer;

impl SocketServer {
    /// 启动 socket 服务: 清理残留 → bind → spawn accept 线程。
    pub fn start(model: Arc<RwLock<Model>>, db: Db) -> Self {
        // ADR-3: 启动时 remove_file 清理残留
        let _ = std::fs::remove_file(SOCKET_PATH);
        let listener = UnixListener::bind(SOCKET_PATH).expect("bind hub socket");

        let _handle = thread::spawn(move || {
            accept_loop(listener, model, db);
        });

        Self
    }
}


impl Drop for SocketServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(SOCKET_PATH);
    }
}

// ───────────────────────── accept loop ─────────────────────────

/// 主 accept 循环: 每个 incoming 连接 spawn 一个处理线程。
fn accept_loop(listener: UnixListener, model: Arc<RwLock<Model>>, db: Db) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let m = model.clone();
                let d = db.clone();
                thread::spawn(move || {
                    handle_connection(stream, m, d);
                });
            }
            Err(_) => continue,
        }
    }
}

// ───────────────────────── connection handler ─────────────────────────

/// 处理单个连接: 逐行读 newline-delimited JSON → 解析 SocketReq → 查询 model → 写回 JSON 响应。
fn handle_connection(stream: UnixStream, model: Arc<RwLock<Model>>, db: Db) {
    let mut writer = stream.try_clone().expect("clone stream for write");
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        match line {
            Ok(json_str) if !json_str.is_empty() => {
                let resp = match serde_json::from_str::<crate::msg::SocketReq>(&json_str) {
                    Ok(req) => dispatch_cmd(req, &model, &db),
                    Err(e) => SocketResp::err(format!("bad json: {e}")),
                };
                let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap_or_default());
            }
            _ => break,
        }
    }
}

// ───────────────────────── 命令分发 ─────────────────────────────────

fn dispatch_cmd(
    req: crate::msg::SocketReq,
    model: &Arc<RwLock<Model>>,
    db: &Db,
) -> SocketResp {
    match req.cmd.as_str() {
        "group_join" => cmd_group_join(&req, model, db),
        "group_leave" => cmd_group_leave(&req, model, db),
        "group_members" => cmd_group_members(&req, model),
        "broadcast" => cmd_broadcast(&req, model),
        other => SocketResp::err(format!("unknown cmd: {other}")),
    }
}

// ───────────────────────── 命令实现 ─────────────────────────────────

fn cmd_group_join(
    req: &crate::msg::SocketReq,
    model: &Arc<RwLock<Model>>,
    db: &Db,
) -> SocketResp {
    let name = match &req.name {
        Some(n) if !n.is_empty() => n.clone(),
        _ => return SocketResp::err("group_join requires \"name\""),
    };
    let handle = match &req.handle {
        Some(h) if !h.is_empty() => h.clone(),
        _ => return SocketResp::err("group_join requires \"handle\""),
    };

    // 检查 handle 是否存在于 directory
    {
        let m = model.read();
        if !m.directory.contains_key(&handle) {
            return SocketResp::err(format!("unknown handle: {handle}"));
        }
    }

    // 写入群组 (in-memory)
    {
        let mut m = model.write();
        m.groups.entry(name.clone()).or_default().insert(handle.clone());
    }

    // 持久化到 DB (survives restart)
    db.join_group(&name, &handle);

    SocketResp::ok(serde_json::json!({"action": "joined", "group": &name, "handle": &handle}))
}

fn cmd_group_leave(
    req: &crate::msg::SocketReq,
    model: &Arc<RwLock<Model>>,
    db: &Db,
) -> SocketResp {
    let name = match &req.name {
        Some(n) if !n.is_empty() => n.clone(),
        _ => return SocketResp::err("group_leave requires \"name\""),
    };
    let handle = match &req.handle {
        Some(h) if !h.is_empty() => h.clone(),
        _ => return SocketResp::err("group_leave requires \"handle\""),
    };

    let mut m = model.write();
    if let Some(members) = m.groups.get_mut(&name) {
        if members.remove(&handle) {
            if members.is_empty() {
                m.groups.remove(&name);
            }
            drop(m); // release write lock before DB write
            db.leave_group(&name, &handle);
            return SocketResp::ok(serde_json::json!({"action": "left", "group": &name, "handle": &handle}));
        }
    }
    SocketResp::err(format!("handle {handle} not in group {name}"))
}

fn cmd_group_members(req: &crate::msg::SocketReq, model: &Arc<RwLock<Model>>) -> SocketResp {
    let name = match &req.name {
        Some(n) if !n.is_empty() => n.clone(),
        _ => return SocketResp::err("group_members requires \"name\""),
    };

    let m = model.read();
    match m.groups.get(&name) {
        Some(members) => {
            let list: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
            SocketResp::ok(serde_json::json!({"group": &name, "members": list}))
        }
        None => SocketResp::err(format!("group not found: {name}")),
    }
}

/// broadcast: 向群组所有成员发送编排消息。
/// 从 model 读取成员列表, 逐个调用 transport::orchestration_send。
fn cmd_broadcast(req: &crate::msg::SocketReq, model: &Arc<RwLock<Model>>) -> SocketResp {
    let name = match &req.name {
        Some(n) if !n.is_empty() => n.clone(),
        _ => return SocketResp::err("broadcast requires \"name\""),
    };
    let message = match &req.message {
        Some(m) if !m.is_empty() => m.clone(),
        _ => return SocketResp::err("broadcast requires \"message\""),
    };

    let members: Vec<String> = {
        let m = model.read();
        match m.groups.get(&name) {
            Some(set) => set.iter().cloned().collect(),
            None => return SocketResp::err(format!("group not found: {name}")),
        }
    };

    if members.is_empty() {
        return SocketResp::ok(serde_json::json!({
            "action": "broadcast",
            "group": &name,
            "sent": 0,
            "failed": 0,
        }));
    }

    // 逐个发送, 收集结果

    let mut ok_count = 0usize;
    let mut fail_count = 0usize;
    for handle in &members {
        let text = format!("[{}] {}", name, message);
        match crate::transport::terminal_send(handle, &text) {
            Ok(_) => ok_count += 1,
            Err(_) => fail_count += 1,
        }
    }

    SocketResp::ok(serde_json::json!({
        "action": "broadcast",
        "group": &name,
        "sent": ok_count,
        "failed": fail_count,
    }))
}

// ───────────────────────── 响应类型 ─────────────────────────

#[derive(Debug, serde::Serialize)]
struct SocketResp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl SocketResp {
    fn ok(data: serde_json::Value) -> Self {
        Self { ok: true, data: Some(data), error: None }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self { ok: false, data: None, error: Some(msg.into()) }
    }
}
