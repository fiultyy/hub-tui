//! model.rs —— 范式 1/5 数据投影层(ADR-1 范式 5: 数据态/运行态分离)。
//!
//! Model 只持纯数据态(directory/groups/messages)。无 IO、无运行态、无渲染缓存。
//! view 读 &Model 不 &mut。增量更新 apply_agents / apply_status 就地改 HashMap。

use std::collections::{HashMap, HashSet, VecDeque};

// ───────────────────────── 领域类型 ─────────────────────────

/// Orca 终端条目。字段对齐 `orca-ide terminal list --json` 输出。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    #[serde(rename = "handle")]
    pub handle: String,
    #[serde(rename = "ptyId")]
    pub pty_id: Option<String>,
    #[serde(rename = "worktreePath")]
    pub cwd: String,
    #[serde(rename = "worktreeId")]
    pub worktree_id: String,
    pub branch: String,
    #[serde(rename = "tabId")]
    pub tab_id: String,
    #[serde(rename = "leafId")]
    pub leaf_id: String,
    pub title: Option<String>,
    pub connected: bool,
    pub writable: bool,
    /// join 后补充(来自 last-status.json)
    pub source: Option<String>,
    /// join 后补充(来自 last-status.json)
    pub state: Option<String>,
}

/// 编排消息(对齐 orca orchestration inbox --json)。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct OrchMessage {
    pub id: String,
    #[serde(rename = "fromHandle")]
    pub from_handle: String,
    #[serde(rename = "toHandle")]
    pub to_handle: String,
    pub subject: String,
    pub body: String,
    #[serde(rename = "msgType")]
    pub msg_type: String,
    pub priority: u8,
    #[serde(rename = "threadId")]
    pub thread_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

// ───────────────────────── Model ─────────────────────────

/// inbox 消息上限。
pub const MESSAGES_CAP: usize = 5000;

/// 纯数据投影。无 IO、无运行态、无渲染缓存。
pub struct Model {
    /// handle → Agent (通信录)
    pub directory: HashMap<String, Agent>,
    /// group name → handle set (群组)
    pub groups: HashMap<String, HashSet<String>>,
    /// inbox 消息队列(cap 5000)
    pub messages: VecDeque<OrchMessage>,
    /// 异步 generation guard(范式 3: 防陈旧回调)
    pub generation: u64,
}

impl Model {
    pub fn new() -> Self {
        Self {
            directory: HashMap::new(),
            groups: HashMap::new(),
            messages: VecDeque::new(),
            generation: 0,
        }
    }

    /// 增量更新: terminal list 全量结果。handle 为 key,保留已有 source/state join 数据。
    pub fn apply_agents(&mut self, agents: Vec<Agent>) {
        let mut incoming: HashMap<String, Agent> = agents
            .into_iter()
            .map(|a| (a.handle.clone(), a))
            .collect();

        // 保留已有 agent 的 source/state(join 数据),避免被全量覆盖丢失
        for (handle, incoming_agent) in incoming.iter_mut() {
            if let Some(old) = self.directory.get(handle) {
                incoming_agent.source = old.source.clone();
                incoming_agent.state = old.state.clone();
            }
        }

        // 移除已关闭的 handle
        let incoming_handles: HashSet<&str> = incoming.keys().map(|s| s.as_str()).collect();
        self.directory.retain(|h, _| incoming_handles.contains(h.as_str()));

        // 合入新数据
        for (handle, agent) in incoming {
            self.directory.insert(handle, agent);
        }

        self.generation += 1;
    }

    /// 增量更新: last-status.json 结果。join key = worktreeId(两源都有且稳定,ADR-5 实测验证)。
    pub fn apply_status(&mut self, statuses: Vec<crate::msg::AgentStatus>) {
        let status_map: HashMap<String, (String, String)> = statuses
            .into_iter()
            .map(|s| (s.worktree_id, (s.source, s.state)))
            .collect();

        for agent in self.directory.values_mut() {
            if let Some((source, state)) = status_map.get(&agent.worktree_id) {
                agent.source = Some(source.clone());
                agent.state = Some(state.clone());
            }
        }

        self.generation += 1;
    }

    /// 追加 inbox 消息, cap 5000, 溢出弹头。
    pub fn push_message(&mut self, msg: OrchMessage) {
        if self.messages.len() >= MESSAGES_CAP {
            self.messages.pop_front();
        }
        self.messages.push_back(msg);
    }
}
