//! msg.rs —— 范式 2 单一 fan-in 事件类型(ADR-1)。
//!
//! 所有事件源(socket/CLI/键盘/tick/service 结果)send 成 AppMsg,
//! 主 loop 单 channel match。增删事件只改本文件 + update.rs 一个 match arm。

use crossterm::event::KeyEvent;

use crate::model::Agent;

// ───────────────────────── 辅助类型 ─────────────────────────

/// Agent 运行态(来自 last-status.json,通过 tabId:leafId join 到 Agent)。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentStatus {
    #[serde(rename = "paneKey")]
    pub pane_key: String,
    pub source: String,
    pub state: String,
    #[serde(rename = "worktreeId")]
    pub worktree_id: String,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub last_assistant_msg: Option<String>,
}

/// Unix socket 查询请求(ADR-3)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SocketReq {
    pub cmd: String,
    pub name: Option<String>,
    pub handle: Option<String>,
    pub cwd: Option<String>,
    /// broadcast 消息体。
    pub message: Option<String>,
}

// ───────────────────────── AppMsg(唯一 fan-in 出口)─────────────────────────

/// 唯一 fan-in 出口(范式 2)。所有事件源 send 成 AppMsg,主 loop 单 channel match。
#[derive(Debug)]
pub enum AppMsg {
    /// 键盘按键(crossterm KeyEvent)。
    Key(KeyEvent),
    /// 鼠标左键点击(终端坐标)。
    MouseLeftClick { x: u16, y: u16 },
    /// 终端尺寸变化。
    Resize { width: u16, height: u16 },
    /// 定时 tick(驱动 spinner/toast 超时/状态刷新)。
    Tick,
    /// 退出。
    Quit,
    /// terminal list 加载完成。
    AgentsLoaded(Vec<Agent>),
    /// last-status.json 刷新结果。
    StatusUpdated(Vec<AgentStatus>),
    /// orchestration send 成功(msg_id)。
    SendOk(String),
    /// orchestration send 失败(error)。
    SendFailed(String),
    /// orchestration inbox drain 完成(ADR-4)。
    MessagesDrained(Vec<crate::model::OrchMessage>),
    /// orchestration inbox 全量未读数刷新(handle → count)。
    /// mark-read 成功(delivery_id)。
    AckOk(String),
    /// mark-read 失败(error)。
    AckFailed(String),
    UnreadUpdated(std::collections::HashMap<String, usize>),
    /// socket 查询请求(来自 agent 连接)。
    SocketQuery(SocketReq),
    /// 信息 toast(非错误)。
    Info(String),
    InjectOk(usize),
    /// PTY 注入失败。
    InjectFailed(String),
    /// terminal read 结果回灌。
    TerminalOutput(String),
    /// 群组操作成功反馈(joined/left/broadcast)。
    GroupActionOk(String),
    /// worktree ps 结果回灌。
    WorktreePsLoaded(Vec<crate::model::WorktreePsEntry>),
    /// terminal create 成功: 返回新终端 handle + title(用于 toast + RefreshAgents)。
    TerminalCreated { handle: String, title: Option<String> },
    /// 配置项更新成功(key, value)。
    ConfigUpdated { key: String, value: String },
    /// 编排快照回灌(run-list + task-list + gate-list 三合一)。
    OrchSnapshotLoaded(Box<crate::model::OrchSnapshot>),
    /// 通用错误。
    Error(String),
}
