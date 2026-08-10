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
    /// PTY 最后输出时间(epoch ms)。来自 terminal list lastOutputAt。
    #[serde(rename = "lastOutputAt", default)]
    pub last_output_at: Option<i64>,
}

/// 活跃判定阈值: lastOutputAt 在此时间内 = working(推理中)。
const ACTIVE_THRESHOLD_MS: i64 = 10_000;

impl Agent {
    /// 返回 NOW 的时间戳(epoch ms)。
    fn now_ms() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// 推断显示状态。规则:
    /// 1. lastOutputAt < 10s → "working"(PTY 有输出 = agent 在推理/执行)
    /// 2. 否则用 hook state(working/waiting/blocked/done)
    /// 3. 都没有 → "idle"
    /// 解决 agent 长时间 LLM 推理不触发 hook 导致 state 冻结在 done 的问题。
    pub fn effective_state(&self) -> &str {
        if let Some(last) = self.last_output_at {
            if Self::now_ms() - last < ACTIVE_THRESHOLD_MS {
                return "working";
            }
        }
        self.state.as_deref().unwrap_or("idle")
    }
}
/// Directory 分区用: agent 状态分类。
/// 排序值(derive Ord) = 分区显示顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatusCategory {
    /// working/running/busy — 最高优先级显示
    Working = 0,
    /// waiting — 等待用户输入
    Waiting = 1,
    /// blocked — 被阻塞
    Blocked = 2,
    /// error/fail — 出错
    Error = 3,
    /// done/idle/ok — 已完成
    Done = 4,
    /// 无状态(join 未命中或 state=None) — 最低优先级
    Unknown = 5,
}

impl StatusCategory {
    /// 从 agent.state 字符串推断分类。
    pub fn from_state(state: Option<&str>) -> Self {
        match state {
            Some(s) => {
                let s = s.to_ascii_lowercase();
                if s.contains("run") || s.contains("work") || s.contains("busy") {
                    Self::Working
                } else if s.contains("wait") {
                    Self::Waiting
                } else if s.contains("block") {
                    Self::Blocked
                } else if s.contains("error") || s.contains("fail") {
                    Self::Error
                } else if s.contains("done") || s.contains("idle") || s.contains("ok") {
                    Self::Done
                } else {
                    Self::Unknown
                }
            }
            None => Self::Unknown,
        }
    }

    /// 状态图标(Unicode)。
    pub fn icon(self) -> &'static str {
        match self {
            Self::Working => "⠋",
            Self::Waiting => "?",
            Self::Blocked => "=",
            Self::Error => "x",
            Self::Done => "v",
            Self::Unknown => "-",
        }
    }

    /// 分区标题文本。
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Waiting => "Waiting",
            Self::Blocked => "Blocked",
            Self::Error => "Error",
            Self::Done => "Done",
            Self::Unknown => "No Status",
        }
    }

    /// 从 agent 直接取分类。
    pub fn from_agent(agent: &Agent) -> Self {
        Self::from_state(Some(agent.effective_state()))
    }
}

/// 编排消息(对齐 orca orchestration inbox --json)。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct OrchMessage {
    pub id: String,
    pub from_handle: String,
    pub to_handle: String,
    pub subject: String,
    pub body: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default)]
    pub priority: String,
    pub thread_id: Option<String>,
    #[serde(default)]
    pub payload: Option<String>,
    #[serde(default, deserialize_with = "de_read_flag")]
    pub read: bool,
    pub created_at: String,
    #[serde(default)]
    pub sequence: i64,
}

/// 反序列化 `read` 字段: CLI 输出用 int 0/1, Rust 内部统一为 bool。
/// 同时容忍 true/false 和缺失(null), 保证不会因该字段解析失败而丢掉整条消息。
fn de_read_flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    struct Vis;
    impl<'de> serde::de::Visitor<'de> for Vis {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bool or int 0/1")
        }
        fn visit_bool<E: serde::de::Error>(self, b: bool) -> Result<bool, E> { Ok(b) }
        fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<bool, E> { Ok(n != 0) }
        fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<bool, E> { Ok(n != 0) }
        fn visit_unit<E: serde::de::Error>(self) -> Result<bool, E> { Ok(false) }
    }
    d.deserialize_any(Vis)
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
    /// pending status 缓存: AgentsLoaded 和 StatusUpdated 是异步的,
    /// 可能 StatusUpdated 先到但 directory 为空。缓存到这,AgentsLoaded 时合并。
    pub pending_status: HashMap<String, (String, String)>,
    /// handle → 未读消息数(来自 orchestration inbox)
    pub unread_counts: HashMap<String, usize>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            directory: HashMap::new(),
            groups: HashMap::new(),
            messages: VecDeque::new(),
            generation: 0,
            pending_status: HashMap::new(),
            unread_counts: HashMap::new(),
        }
    }

    /// 增量更新: terminal list 全量结果。handle 为 key,保留已有 source/state join 数据。
    /// 同时合并 pending_status(解决 StatusUpdated 先于 AgentsLoaded 的竞态)。
    pub fn apply_agents(&mut self, agents: Vec<Agent>) {
        let mut incoming: HashMap<String, Agent> = agents
            .into_iter()
            .map(|a| (a.handle.clone(), a))
            .collect();

        // 保留已有 agent 的 source/state,并合并 pending_status
        for (handle, incoming_agent) in incoming.iter_mut() {
            if let Some(old) = self.directory.get(handle) {
                incoming_agent.source = old.source.clone();
                incoming_agent.state = old.state.clone();
            }
            // 合并 pending_status(worktreeId join)
            if let Some((source, state)) = self.pending_status.get(&incoming_agent.worktree_id) {
                incoming_agent.source = Some(source.clone());
                incoming_agent.state = Some(state.clone());
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

    /// 增量更新: last-status.json 结果。join key = worktreeId。
    /// 缓存到 pending_status,apply_agents 时合并(解决竞态)。
    pub fn apply_status(&mut self, statuses: Vec<crate::msg::AgentStatus>) {
        let status_map: HashMap<String, (String, String)> = statuses
            .into_iter()
            .map(|s| (s.worktree_id, (s.source, s.state)))
            .collect();

        // 缓存(供后续 apply_agents 合并)
        self.pending_status.extend(status_map.clone());

        // 立即尝试 join 到现有 directory
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

    /// 更新未读消息计数(来自 orchestration inbox)。
    pub fn apply_unread(&mut self, counts: HashMap<String, usize>) {
        self.unread_counts = counts;
        self.generation += 1;
    }
}

/// Directory 排序: 按 (状态分类 rank, handle) 排序。
/// 所有 cursor/导航/hit_test 逻辑共享此排序, 保证分区一致性。
pub fn directory_sorted_handles(directory: &HashMap<String, Agent>) -> Vec<String> {
    let mut entries: Vec<(&String, &Agent)> = directory.iter().collect();
    entries.sort_by(|a, b| {
        let ca = StatusCategory::from_agent(a.1);
        let cb = StatusCategory::from_agent(b.1);
        ca.cmp(&cb).then_with(|| a.0.cmp(b.0))
    });
    entries.into_iter().map(|(h, _)| h.clone()).collect()
}
