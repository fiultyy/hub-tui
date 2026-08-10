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
    /// join key: tabId:leafId(对齐 last-status.json paneKey)。apply_agents 时计算。
    #[serde(skip)]
    pub pane_key: String,
    pub title: Option<String>,
    pub connected: bool,
    pub writable: bool,
    /// join 后补充(来自 last-status.json)
    pub source: Option<String>,
    /// join 后补充(来自 last-status.json)
    pub state: Option<String>,
    /// join 后补充(来自 last-status.json)
    pub prompt: Option<String>,
    /// join 后补充(来自 last-status.json)
    pub tool_name: Option<String>,
    /// join 后补充(来自 last-status.json)
    pub tool_input: Option<String>,
    /// join 后补充(来自 last-status.json)
    pub last_assistant_msg: Option<String>,
    /// PTY 屏幕快照(来自 terminal list preview,每 5s 刷新)。
    #[serde(default)]
    pub preview: Option<String>,
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

// ───────────────────────── 活动日志(Activity Log)─────────────────────────

/// 事件严重级别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventSeverity {
    Info,
    Warn,
    Error,
}

impl EventSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::Warn => "Warn",
            Self::Error => "Error",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "warn" => Self::Warn,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }

    /// 单字宽图标。
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Error => "✖",
            Self::Warn => "⚠",
            Self::Info => "·",
        }
    }
}

/// 事件类别(用于过滤/着色)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventCategory {
    /// agent 生命周期: 出现/消失/创建。
    Agent,
    /// StatusCategory 状态转移。
    State,
    /// 消息: send/ack/receive。
    Message,
    /// 群组操作。
    Group,
    /// 系统: PTY/通用错误。
    System,
}

impl EventCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::State => "State",
            Self::Message => "Message",
            Self::Group => "Group",
            Self::System => "System",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "state" => Self::State,
            "message" => Self::Message,
            "group" => Self::Group,
            "system" => Self::System,
            _ => Self::Agent,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Agent => "👤",
            Self::State => "↻",
            Self::Message => "✉",
            Self::Group => "👥",
            Self::System => "⚙",
        }
    }
}

/// 活动日志事件。纯数据, 无 IO。
#[derive(Clone, Debug)]
pub struct Event {
    /// DB 自增 id; 0 = 未持久化。
    pub id: i64,
    /// epoch 毫秒。
    pub timestamp_ms: i64,
    pub severity: EventSeverity,
    pub category: EventCategory,
    /// agent handle 或 "system"。
    pub source: String,
    pub text: String,
}

/// 当前 epoch 毫秒(SystemTime, 无 IO 语义)。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 输入栏历史条目。
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: i64,
    pub timestamp_ms: i64,
    pub text: String,
    /// 匹配的输入前缀(to:/pty:/create:等), 无匹配则空。
    pub prefix: String,
}

/// 历史上限(内存; DB 保留更多)。
pub const HISTORY_CAP: usize = 500;

/// 编排消息(对齐 orca orchestration inbox --json)。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct OrchMessage {
    pub id: String,
    pub from_handle: String,
    pub to_handle: String,
    pub subject: String,
    pub body: String,
    #[serde(rename = "type", default = "default_msg_type")]
    pub msg_type: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub payload: Option<String>,
    #[serde(default)]
    pub read: i64,
    #[serde(default)]
    pub sequence: i64,
    #[serde(default, rename = "created_at")]
    pub created_at: String,
}

fn default_msg_type() -> String {
    "status".to_string()
}

// ───────────────────────── Model ─────────────────────────

/// inbox 消息上限。
pub const MESSAGES_CAP: usize = 5000;
/// 活动日志事件上限(内存; DB 保留更多)。
pub const EVENTS_CAP: usize = 2000;

/// last-status.json join 数据(通过 worktreeId join 到 Agent)。
/// 用于 pending_status 缓存 + apply_status 合并。
#[derive(Clone, Debug, Default)]
pub struct StatusJoin {
    pub source: String,
    pub state: String,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub last_assistant_msg: Option<String>,
}

/// 纯数据投影。无 IO、无运行态、无渲染缓存。
pub struct Model {
    /// handle → Agent (通信录)
    pub directory: HashMap<String, Agent>,
    /// group name → handle set (群组)
    pub groups: HashMap<String, HashSet<String>>,
    /// inbox 消息队列(cap 5000)
    pub messages: VecDeque<OrchMessage>,
    /// 活动日志事件队列(cap EVENTS_CAP, 最新在尾部)。
    pub events: VecDeque<Event>,
    /// 输入栏历史(cap HISTORY_CAP, 最新在尾部)。
    pub history: VecDeque<HistoryEntry>,
    /// 异步 generation guard(范式 3: 防陈旧回调)。
    pub generation: u64,
    /// pending status 缓存: AgentsLoaded 和 StatusUpdated 是异步的,
    /// 可能 StatusUpdated 先到但 directory 为空。缓存到这,AgentsLoaded 时合并。
    pub pending_status: HashMap<String, StatusJoin>,
    pub unread_counts: HashMap<String, usize>,
    pub worktree_ps: Vec<WorktreePsEntry>,
    /// 编排快照(按需刷新,t 键触发)。
    pub orch_snapshot: Option<OrchSnapshot>,
    /// 配置 key-value store(启动从 DB 加载,运行时变更持久化)。
    pub config: HashMap<String, String>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            directory: HashMap::new(),
            groups: HashMap::new(),
            messages: VecDeque::new(),
            history: VecDeque::new(),
            events: VecDeque::new(),
            generation: 0,
            pending_status: HashMap::new(),
            unread_counts: HashMap::new(),
            worktree_ps: Vec::new(),
            orch_snapshot: None,
            config: HashMap::new(),
        }
    }

    /// 增量更新: terminal list 全量结果。handle 为 key,保留已有 source/state join 数据。
    /// 同时合并 pending_status(解决 StatusUpdated 先于 AgentsLoaded 的竞态)。
    pub fn apply_agents(&mut self, agents: Vec<Agent>) {
        let mut incoming: HashMap<String, Agent> = agents
            .into_iter()
            .map(|a| (a.handle.clone(), a))
            .collect();

        // 保留已有 agent 的 join 数据, 计算 pane_key, 合并 pending_status
        for (handle, incoming_agent) in incoming.iter_mut() {
            if let Some(old) = self.directory.get(handle) {
                incoming_agent.source = old.source.clone();
                incoming_agent.state = old.state.clone();
                incoming_agent.prompt = old.prompt.clone();
                incoming_agent.tool_name = old.tool_name.clone();
                incoming_agent.tool_input = old.tool_input.clone();
                incoming_agent.last_assistant_msg = old.last_assistant_msg.clone();
            }
            // 合并 pending_status(paneKey join)
            if let Some(sj) = self.pending_status.get(&incoming_agent.pane_key) {
                incoming_agent.source = Some(sj.source.clone());
                incoming_agent.state = Some(sj.state.clone());
                incoming_agent.prompt = sj.prompt.clone();
                incoming_agent.tool_name = sj.tool_name.clone();
                incoming_agent.tool_input = sj.tool_input.clone();
                incoming_agent.last_assistant_msg = sj.last_assistant_msg.clone();
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

    /// 增量更新: last-status.json 结果。join key = paneKey(tabId:leafId)。
    /// 缓存到 pending_status,apply_agents 时合并(解决竞态)。
    pub fn apply_status(&mut self, statuses: Vec<crate::msg::AgentStatus>) {
        let status_map: HashMap<String, StatusJoin> = statuses
            .into_iter()
            .map(|s| {
                (
                    s.pane_key.clone(),
                    StatusJoin {
                        source: s.source,
                        state: s.state,
                        prompt: s.prompt,
                        tool_name: s.tool_name,
                        tool_input: s.tool_input,
                        last_assistant_msg: s.last_assistant_msg,
                    },
                )
            })
            .collect();

        // 缓存(供后续 apply_agents 合并)
        self.pending_status.extend(status_map.clone());

        // 立即尝试 join 到现有 directory(paneKey join)
        for agent in self.directory.values_mut() {
            if let Some(sj) = status_map.get(&agent.pane_key) {
                agent.source = Some(sj.source.clone());
                agent.state = Some(sj.state.clone());
                agent.prompt = sj.prompt.clone();
                agent.tool_name = sj.tool_name.clone();
                agent.tool_input = sj.tool_input.clone();
                agent.last_assistant_msg = sj.last_assistant_msg.clone();
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

    /// 追加活动日志事件, cap EVENTS_CAP, 溢出弹头。timestamp_ms=0 时自动填 now_ms。
    pub fn push_event(&mut self, mut event: Event) {
        if self.events.len() >= EVENTS_CAP {
            self.events.pop_front();
        }
        if event.timestamp_ms == 0 {
            event.timestamp_ms = now_ms();
        }
        self.events.push_back(event);
        self.generation += 1;
    }

    /// 启动时从 DB 批量加载历史事件(替换队列, 截断到 EVENTS_CAP)。
    pub fn apply_events(&mut self, events: Vec<Event>) {
        let mut q: VecDeque<Event> = events.into_iter().collect();
        if q.len() > EVENTS_CAP {
            let drop_n = q.len() - EVENTS_CAP;
            q.drain(..drop_n);
        }
        self.events = q;
        self.generation += 1;
    }

    /// 清空活动日志(overlay `c` 键)。
    pub fn clear_events(&mut self) {
        self.events.clear();
        self.generation += 1;
    }

    /// 追加输入历史, cap HISTORY_CAP。前缀从 text 自动提取(首个 ':')。
    pub fn push_history(&mut self, text: String) {
        let prefix = text.split_once(':').map(|(p, _)| format!("{p}:")).unwrap_or_default();
        if self.history.len() >= HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(HistoryEntry {
            id: 0,
            timestamp_ms: now_ms(),
            text,
            prefix,
        });
        self.generation += 1;
    }

    /// 启动时从 DB 批量加载历史(替换队列, 截断到 HISTORY_CAP)。
    pub fn apply_history(&mut self, entries: Vec<HistoryEntry>) {
        let mut q: VecDeque<HistoryEntry> = entries.into_iter().collect();
        if q.len() > HISTORY_CAP {
            let drop_n = q.len() - HISTORY_CAP;
            q.drain(..drop_n);
        }
        self.history = q;
    }

    /// 更新未读消息计数(来自 orchestration inbox)。
    pub fn apply_unread(&mut self, counts: HashMap<String, usize>) {
        self.unread_counts = counts;
        self.generation += 1;
    }

    /// 更新 worktree ps 编排摘要。
    pub fn apply_worktree_ps(&mut self, entries: Vec<WorktreePsEntry>) {
        self.worktree_ps = entries;
        self.generation += 1;
    }
    /// 加载/覆盖配置(启动时从 DB 加载)。
    pub fn apply_config(&mut self, config: HashMap<String, String>) {
        self.config = config;
    }

    /// 更新单个配置项(key-value)。
    pub fn set_config(&mut self, key: String, value: String) {
        self.config.insert(key, value);
    }

    /// 获取配置值,带默认。
    pub fn get_config(&self, key: &str, default: &str) -> String {
        self.config.get(key).cloned().unwrap_or_else(|| default.to_string())
    }

    /// 获取 refresh_interval_ms(默认 5000)。
    pub fn refresh_interval_ms(&self) -> u64 {
        self.get_config("refresh_interval_ms", "5000")
            .parse()
            .unwrap_or(5000)
    }

    /// 获取 theme(默认 "default")。
    pub fn get_theme(&self) -> &str {
        self.config.get("theme").map(|s| s.as_str()).unwrap_or("default")
    }

    /// 获取 default_filter(默认空)。
    pub fn get_default_filter(&self) -> &str {
        self.config.get("default_filter").map(|s| s.as_str()).unwrap_or("")
    }

    /// 获取排序模式(默认 by-worktree)。
    pub fn sort_mode(&self) -> SortMode {
        SortMode::from_str(self.get_config("sort", "by-worktree").as_str())
    }
    /// 更新编排快照。
    pub fn apply_orch_snapshot(&mut self, snapshot: OrchSnapshot) {
        self.orch_snapshot = Some(snapshot);
        self.generation += 1;
    }
}

/// worktree ps 条目(对齐 orca-ide worktree ps --json 输出)。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct WorktreePsEntry {
    #[serde(rename = "worktreePath", default)]
    pub path: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub agent_count: usize,
    #[serde(default)]
    pub status: Option<String>,
}

/// 编排快照(run-list + task-list + gate-list 三合一)。
#[derive(Clone, Debug, Default)]
pub struct OrchSnapshot {
    pub runs: Vec<OrchRunEntry>,
    pub tasks: Vec<OrchTaskEntry>,
    pub gates: Vec<OrchGateEntry>,
}

/// orchestration run 条目。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct OrchRunEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
}

/// orchestration task 条目。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct OrchTaskEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub assignee: Option<String>,
}

/// orchestration gate 条目。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct OrchGateEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
}

/// Directory 排序模式(从 model.config["sort"] 读)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    ByWorktree,
    ByState,
    BySource,
    ByName,
}

impl SortMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "by-state" | "state" => Self::ByState,
            "by-source" | "source" => Self::BySource,
            "by-name" | "name" => Self::ByName,
            _ => Self::ByWorktree, // "by-worktree" / "worktree" / unknown
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ByWorktree => "by-worktree",
            Self::ByState => "by-state",
            Self::BySource => "by-source",
            Self::ByName => "by-name",
        }
    }
}

/// Directory 排序: 按 sort_mode 选择策略, 保证 cursor/导航/hit_test 一致性。
pub fn directory_sorted_handles(directory: &HashMap<String, Agent>) -> Vec<String> {
    directory_sorted_with_mode(directory, SortMode::ByWorktree)
}

/// 带排序模式的版本。
pub fn directory_sorted_with_mode(
    directory: &HashMap<String, Agent>,
    mode: SortMode,
) -> Vec<String> {
    let mut entries: Vec<&Agent> = directory.values().collect();

    match mode {
        SortMode::ByWorktree => {
            // worktreePath 分组 + 组间最近活跃 + 组内 lastOutputAt 降序
            let mut groups: HashMap<&str, Vec<&Agent>> = HashMap::new();
            for a in directory.values() {
                groups.entry(a.cwd.as_str()).or_default().push(a);
            }
            let group_activity = |agents: &[&Agent]| -> i64 {
                agents.iter().filter_map(|a| a.last_output_at).max().unwrap_or(0)
            };
            let mut group_list: Vec<(&str, Vec<&Agent>)> = groups.into_iter().collect();
            group_list.sort_by(|a, b| {
                group_activity(&b.1)
                    .cmp(&group_activity(&a.1))
                    .then_with(|| a.0.cmp(b.0))
            });
            for (_, agents) in group_list.iter_mut() {
                agents.sort_by(|a, b| {
                    b.last_output_at.unwrap_or(0)
                        .cmp(&a.last_output_at.unwrap_or(0))
                        .then_with(|| a.handle.cmp(&b.handle))
                });
            }
            group_list
                .into_iter()
                .flat_map(|(_, agents)| agents)
                .map(|a| a.handle.clone())
                .collect()
        }
        SortMode::ByState => {
            // 按状态优先级(working → waiting → blocked → error → done → unknown)
            entries.sort_by(|a, b| {
                StatusCategory::from_agent(a)
                    .cmp(&StatusCategory::from_agent(b))
                    .then_with(|| {
                        b.last_output_at.unwrap_or(0).cmp(&a.last_output_at.unwrap_or(0))
                    })
                    .then_with(|| a.handle.cmp(&b.handle))
            });
            entries.into_iter().map(|a| a.handle.clone()).collect()
        }
        SortMode::BySource => {
            // 按 source 分组, 组内最近活跃
            let mut groups: HashMap<&str, Vec<&Agent>> = HashMap::new();
            for a in directory.values() {
                let key = a.source.as_deref().unwrap_or("unknown");
                groups.entry(key).or_default().push(a);
            }
            let mut group_list: Vec<(&str, Vec<&Agent>)> = groups.into_iter().collect();
            group_list.sort_by(|a, b| a.0.cmp(b.0));
            for (_, agents) in group_list.iter_mut() {
                agents.sort_by(|a, b| {
                    b.last_output_at.unwrap_or(0)
                        .cmp(&a.last_output_at.unwrap_or(0))
                        .then_with(|| a.handle.cmp(&b.handle))
                });
            }
            group_list
                .into_iter()
                .flat_map(|(_, agents)| agents)
                .map(|a| a.handle.clone())
                .collect()
        }
        SortMode::ByName => {
            // 按 title 字母序, title 空的按 handle
            entries.sort_by(|a, b| {
                let na = a.title.as_deref().unwrap_or(&a.handle);
                let nb = b.title.as_deref().unwrap_or(&b.handle);
                na.cmp(nb).then_with(|| a.handle.cmp(&b.handle))
            });
            entries.into_iter().map(|a| a.handle.clone()).collect()
        }
    }
}

/// 从 sorted handles 中过滤出匹配 query 的 handles。
/// 支持命名空间前缀: source:claude / state:working / cwd:~/.orca
/// 无前缀 = 跨维度模糊匹配(title/source/state/cwd)。
pub fn directory_filter_handles(
    sorted: &[String],
    directory: &HashMap<String, Agent>,
    query: &str,
) -> Vec<String> {
    if query.is_empty() {
        return sorted.to_vec();
    }
    let query = query.trim();
    // 解析命名空间前缀
    let (field, value) = if let Some((f, v)) = query.split_once(':') {
        (Some(f), v)
    } else {
        (None, query)
    };
    let value_lower = value.to_ascii_lowercase();

    sorted
        .iter()
        .filter(|h| {
            let agent = match directory.get(*h) {
                Some(a) => a,
                None => return false,
            };
            let target = match field {
                Some("source") => agent.source.as_deref().unwrap_or(""),
                Some("state") => agent.effective_state(),
                Some("cwd") | Some("path") | Some("worktree") => &agent.cwd,
                Some("title") => agent.title.as_deref().unwrap_or(""),
                _ => {
                    // 无前缀: 跨维度模糊匹配
                    let combined = format!(
                        "{} {} {} {}",
                        agent.title.as_deref().unwrap_or(""),
                        agent.source.as_deref().unwrap_or(""),
                        agent.effective_state(),
                        agent.cwd,
                    );
                    return crate::command::fuzzy_match(value, &combined);
                }
            };
            target.to_ascii_lowercase().contains(&value_lower)
        })
        .cloned()
        .collect()
}

/// 从消息列表中过滤出匹配 query 的消息(按 id 列表返回,用于 cursor 导航)。
/// 支持命名空间前缀: from:handle / subject:text / type:status
/// 无前缀 = 跨维度模糊匹配(from/subject/body/type)。
pub fn messages_filter_ids(
    messages: &std::collections::VecDeque<OrchMessage>,
    query: &str,
) -> Vec<String> {
    if query.is_empty() {
        return messages.iter().map(|m| m.id.clone()).collect();
    }
    let query = query.trim();
    let (field, value) = if let Some((f, v)) = query.split_once(':') {
        (Some(f), v)
    } else {
        (None, query)
    };
    let value_lower = value.to_ascii_lowercase();

    messages
        .iter()
        .filter(|m| {
            let target = match field {
                Some("from") => m.from_handle.as_str(),
                Some("subject") => m.subject.as_str(),
                Some("type") => m.msg_type.as_str(),
                Some("body") => m.body.as_str(),
                Some("thread") => m.thread_id.as_deref().unwrap_or(""),
                _ => {
                    // 无前缀: 跨维度模糊匹配
                    let combined = format!(
                        "{} {} {} {}",
                        m.from_handle, m.subject, m.body, m.msg_type
                    );
                    return crate::command::fuzzy_match(value, &combined);
                }
            };
            target.to_ascii_lowercase().contains(&value_lower)
        })
        .map(|m| m.id.clone())
        .collect()
}
