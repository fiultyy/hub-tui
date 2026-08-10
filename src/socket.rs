//! socket.rs —— Unix socket 服务端 (ADR-3 + ADR-6)。
//!
//! 接受 agent Unix socket 连接,处理群组低频只读查询。
//! 方案: SocketServer 持有 `Arc<RwLock<Model>>`,
//! handle_connection 直接 lock+读 model 回应查询,不走 fan-in channel。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::thread;

use parking_lot::RwLock;

use super::msg::SocketReq;
use super::model::Model;

/// Unix socket 文件路径(ADR-3: 固定路径,启动时清理残留)。
const SOCKET_PATH: &str = "/tmp/orca-hub.sock";

// ───────────────────────── SocketServer ─────────────────────────

/// Unix socket 服务端句柄。持有 accept 线程的 JoinHandle。
/// Drop 时清理 socket 文件。
pub struct SocketServer {
    handle: Option<thread::JoinHandle<()>>,
}

impl SocketServer {
    /// 启动 socket 服务: 清理残留 → bind → spawn accept 线程。
    ///
    /// `model`: `Arc<RwLock<Model>>` 共享引用,连接处理线程通过读锁查询群组数据。
    pub fn start(model: Arc<RwLock<Model>>) -> Self {
        // ADR-3: 启动时 remove_file 清理残留
        let _ = std::fs::remove_file(SOCKET_PATH);
        let listener = UnixListener::bind(SOCKET_PATH).expect("bind hub socket");

        let handle = thread::spawn(move || {
            accept_loop(listener, model);
        });

        Self {
            handle: Some(handle),
        }
    }

    /// 关闭: 清理 socket 文件 + drop JoinHandle(detach 线程)。
    pub fn shutdown(self) {
        let _ = std::fs::remove_file(SOCKET_PATH);
        // handle drop → 线程 detach(listener 在线程内 own,close 时线程自然退出)
    }
}

impl Drop for SocketServer {
    fn drop(&mut self) {
        // safety net: 即使没调 shutdown 也清理 socket 文件
        let _ = std::fs::remove_file(SOCKET_PATH);
    }
}

// ───────────────────────── accept loop ─────────────────────────

/// 主 accept 循环: 每个 incoming 连接 spawn 一个处理线程。
fn accept_loop(listener: UnixListener, model: Arc<RwLock<Model>>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let m = model.clone();
                thread::spawn(move || {
                    handle_connection(stream, m);
                });
            }
            Err(_) => continue,
        }
    }
}

// ───────────────────────── connection handler ─────────────────────────

/// 处理单个连接: 逐行读 newline-delimited JSON → 解析 SocketReq → 查询 model → 写回 JSON 响应。
///
/// 支持的 cmd(ADR-6: 仅群组低频操作):
/// - `group_join {name, handle}` — 将 handle 加入群组(写 model)
/// - `group_leave {name, handle}` — 将 handle 从群组移除(写 model)
/// - `group_members {name}` — 查询群组成员列表(只读)
/// - `broadcast` — 暂不支持,返回错误
///
/// 坏 JSON 行: 跳过并返回 `{"ok":false,"error":"bad json"}`。
/// 未知 cmd: 返回 `{"ok":false,"error":"unknown cmd: ..."}`。
fn handle_connection(stream: UnixStream, model: Arc<RwLock<Model>>) {
    let mut writer = stream.try_clone().expect("clone stream for write");
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        match line {
            Ok(json_str) if !json_str.is_empty() => {
                let resp = match serde_json::from_str::<SocketReq>(&json_str) {
                    Ok(req) => dispatch_cmd(req, &model),
                    Err(e) => SocketResp::err(format!("bad json: {e}")),
                };
                let _ = writeln!(writer, "{}", serde_json::to_string(&resp).unwrap_or_default());
            }
            _ => break, // EOF or read error → connection done
        }
    }
}

// ───────────────────────── 命令分发 ─────────────────────────────────

/// 分发 SocketReq 到对应操作,返回 JSON 响应。
fn dispatch_cmd(req: SocketReq, model: &Arc<RwLock<Model>>) -> SocketResp {
    match req.cmd.as_str() {
        "group_join" => cmd_group_join(&req, model),
        "group_leave" => cmd_group_leave(&req, model),
        "group_members" => cmd_group_members(&req, model),
        "broadcast" => SocketResp::err("broadcast not yet supported"),
        other => SocketResp::err(format!("unknown cmd: {other}")),
    }
}

// ───────────────────────── 命令实现 ─────────────────────────────────

/// `group_join`: 将 handle 加入群组。需要写锁。
fn cmd_group_join(req: &SocketReq, model: &Arc<RwLock<Model>>) -> SocketResp {
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

    // 写入群组
    {
        let mut m = model.write();
        m.groups.entry(name.clone()).or_default().insert(handle.clone());
    }

    SocketResp::ok(serde_json::json!({"action": "joined", "group": &name, "handle": &handle}))
}

/// `group_leave`: 将 handle 从群组移除。
fn cmd_group_leave(req: &SocketReq, model: &Arc<RwLock<Model>>) -> SocketResp {
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
            // 群组空了就清理
            if members.is_empty() {
                m.groups.remove(&name);
            }
            return SocketResp::ok(serde_json::json!({"action": "left", "group": &name, "handle": &handle}));
        }
    }
    SocketResp::err(format!("handle {handle} not in group {name}"))
}

/// `group_members`: 查询群组成员列表(只读)。
fn cmd_group_members(req: &SocketReq, model: &Arc<RwLock<Model>>) -> SocketResp {
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

// ───────────────────────── 响应类型 ─────────────────────────

/// Socket 响应(JSON 序列化后写回连接)。
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
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

