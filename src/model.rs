//! model.rs —— 范式 1/5 数据投影层(ADR-1 范式 5: 数据态/运行态分离)。
//!
//! Model 只持纯数据态(directory/groups/events)。无 IO、无运行态、无渲染缓存。
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

/// Scheduled task for batch scheduler (transient, not persisted).
#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub id: usize,
    pub command: String,
    pub fire_at: std::time::Instant,
    pub repeat_interval: Option<std::time::Duration>,
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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

// ───────────────────────── Model ─────────────────────────

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
    /// 活动日志事件队列(cap EVENTS_CAP, 最新在尾部)。
    pub events: VecDeque<Event>,
    /// 输入栏历史(cap HISTORY_CAP, 最新在尾部)。
    pub history: VecDeque<HistoryEntry>,
    /// 异步 generation guard(范式 3: 防陈旧回调)。
    pub generation: u64,
    /// pending status 缓存: AgentsLoaded 和 StatusUpdated 是异步的,
    /// 可能 StatusUpdated 先到但 directory 为空。缓存到这,AgentsLoaded 时合并。
    pub pending_status: HashMap<String, StatusJoin>,
    pub worktree_ps: Vec<WorktreePsEntry>,
    /// 编排快照(按需刷新,t 键触发)。
    pub orch_snapshot: Option<OrchSnapshot>,
    /// 配置 key-value store(启动从 DB 加载,运行时变更持久化)。
    pub config: HashMap<String, String>,
    /// 置顶(pinned) agent handle 集合(持久化到 DB)。
    pub pinned: HashSet<String>,
    /// Agent 标签: handle → tag set(持久化到 DB)。
    pub tags: HashMap<String, HashSet<String>>,
    /// 代码片段: name → command text(持久化到 DB)。
    pub snippets: HashMap<String, String>,
    /// 告警规则列表(持久化到 DB)。
    pub alert_rules: Vec<AlertRule>,
    /// 录制的宏: name → RecordedMacro(持久化到 DB)。
    pub macros: HashMap<String, RecordedMacro>,
    /// 视图预设: name → ViewSnapshot(持久化到 DB)。
    pub saved_views: HashMap<String, ViewSnapshot>,
    /// Agent 笔记: handle → note text(持久化到 DB)。
    pub notes: HashMap<String, String>,
    /// 命令别名: alias_name → expansion(持久化到 DB)。
    pub aliases: HashMap<String, String>,
    /// 自定义热键: key_char → dispatch_input_text(持久化到 DB)。
    pub hotkeys: HashMap<String, String>,
    /// Watch(监控) agent handle 集合(持久化到 DB)。
    pub watched: HashSet<String>,
    /// 命令模板: name → body with $N variables(持久化到 DB)。
    pub templates: HashMap<String, String>,
    /// 定时任务队列(transient,重启清空)。
    pub scheduled_tasks: Vec<ScheduledTask>,
    /// 定时任务 ID 计数器。
    pub next_sched_id: usize,
}



impl Model {
    pub fn new() -> Self {
        Self {
            directory: HashMap::new(),
            groups: HashMap::new(),
            history: VecDeque::new(),
            events: VecDeque::new(),
            generation: 0,
            pending_status: HashMap::new(),
            worktree_ps: Vec::new(),
            tags: HashMap::new(),
            alert_rules: Vec::new(),
            snippets: HashMap::new(),
            orch_snapshot: None,
            saved_views: HashMap::new(),
            notes: HashMap::new(),
            macros: HashMap::new(),
            config: HashMap::new(),
            pinned: HashSet::new(),
            aliases: HashMap::new(),
            hotkeys: HashMap::new(),
            templates: HashMap::new(),
            watched: HashSet::new(),
            scheduled_tasks: Vec::new(),
            next_sched_id: 1,
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
                incoming_agent.last_assistant_msg = sj.last_assistant_msg.clone().or(incoming_agent.last_assistant_msg.take());
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
                agent.state = Some(sj.state.clone());
                agent.prompt = sj.prompt.clone();
                agent.tool_name = sj.tool_name.clone();
                agent.tool_input = sj.tool_input.clone();
                agent.last_assistant_msg = sj.last_assistant_msg.clone().or(agent.last_assistant_msg.take());
            }

        }
        self.generation += 1;
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

    // ──── Pin(置顶)────

    /// agent 是否被置顶。
    pub fn is_pinned(&self, handle: &str) -> bool {
        self.pinned.contains(handle)
    }

    /// 切换置顶状态(insert/remove)。
    pub fn toggle_pin(&mut self, handle: &str) {
        if !self.pinned.remove(handle) {
            self.pinned.insert(handle.to_string());
        }
    }

    /// 启动时从 DB 加载置顶集合(替换)。
    pub fn apply_pinned(&mut self, handles: Vec<String>) {
        self.pinned = handles.into_iter().collect();
    }
    // ──── Watch(监控)────

    /// agent 是否被监控。
    pub fn is_watched(&self, handle: &str) -> bool {
        self.watched.contains(handle)
    }

    /// 切换监控状态(insert/remove)。
    pub fn toggle_watch(&mut self, handle: &str) {
        if !self.watched.remove(handle) {
            self.watched.insert(handle.to_string());
        }
    }

    /// 启动时从 DB 加载监控集合(替换)。
    pub fn apply_watched(&mut self, handles: Vec<String>) {
        self.watched = handles.into_iter().collect();
    }

    // ──── Templates(命令模板)────

    /// 保存/覆盖模板。
    pub fn add_template(&mut self, name: &str, body: &str) {
        self.templates.insert(name.to_string(), body.to_string());
    }

    /// 移除模板。
    pub fn remove_template(&mut self, name: &str) {
        self.templates.remove(name);
    }

    /// 获取模板 body。
    pub fn get_template(&self, name: &str) -> Option<&String> {
        self.templates.get(name)
    }

    /// 启动时从 DB 加载模板(替换)。
    pub fn apply_templates(&mut self, templates: HashMap<String, String>) {
        self.templates = templates;
    }

    // ──── Scheduler(定时任务)────

    /// 添加定时任务,返回分配的 ID。
    pub fn add_scheduled_task(&mut self, command: String, fire_at: std::time::Instant, repeat: Option<std::time::Duration>) -> usize {
        let id = self.next_sched_id;
        self.next_sched_id = self.next_sched_id.wrapping_add(1);
        self.scheduled_tasks.push(ScheduledTask { id, command, fire_at, repeat_interval: repeat });
        id
    }

    /// 按 ID 移除定时任务。
    pub fn remove_scheduled_task(&mut self, id: usize) {
        self.scheduled_tasks.retain(|t| t.id != id);
    }


    // ──── Tags(标签)────

    /// 启动时从 DB 加载标签(替换)。
    pub fn apply_tags(&mut self, tags: HashMap<String, HashSet<String>>) {
        self.tags = tags;
        self.generation += 1;
    }

    /// 为 agent 添加标签(去重)。
    pub fn add_tag(&mut self, handle: &str, tag: &str) {
        self.tags.entry(handle.to_string()).or_default().insert(tag.to_string());
        self.generation += 1;
    }

    /// 移除 agent 的标签; 标签集空时移除 handle 条目。
    pub fn remove_tag(&mut self, handle: &str, tag: &str) {
        if let Some(set) = self.tags.get_mut(handle) {
            set.remove(tag);
            if set.is_empty() {
                self.tags.remove(handle);
            }
        }
        self.generation += 1;
    }

    /// agent 是否拥有指定标签。
    pub fn has_tag(&self, handle: &str, tag: &str) -> bool {
        self.tags.get(handle).map_or(false, |s| s.contains(tag))
    }

    // ──── Snippets(代码片段)────

    /// 保存/覆盖代码片段。cap SNIPPETS_CAP。
    pub fn add_snippet(&mut self, name: &str, text: &str) {
        self.snippets.insert(name.to_string(), text.to_string());
        self.generation += 1;
    }

    /// 移除代码片段。
    pub fn remove_snippet(&mut self, name: &str) {
        self.snippets.remove(name);
        self.generation += 1;
    }

    /// 获取代码片段文本。
    pub fn get_snippet(&self, name: &str) -> Option<&String> {
        self.snippets.get(name)
    }

    /// 启动时从 DB 加载代码片段(替换)。
    pub fn apply_snippets(&mut self, snippets: HashMap<String, String>) {
        self.snippets = snippets;
    }

    // ──── Alert Rules(告警规则)────

    /// 添加告警规则(cap ALERT_RULES_CAP)。
    pub fn add_alert_rule(&mut self, rule: AlertRule) {
        if self.alert_rules.len() < ALERT_RULES_CAP {
            self.alert_rules.push(rule);
            self.generation += 1;
        }
    }

    /// 按 id 移除告警规则。
    pub fn remove_alert_rule(&mut self, id: i64) {
        self.alert_rules.retain(|r| r.id != id);
        self.generation += 1;
    }

    /// 启动时从 DB 加载规则(替换)。
    pub fn apply_alert_rules(&mut self, rules: Vec<AlertRule>) {
        self.alert_rules = rules;
    }
    // ──── Macros(宏录制)────

    /// 保存/覆盖宏(cap MACROS_CAP, FIFO 溢出)。
    pub fn add_macro(&mut self, m: RecordedMacro) {
        if self.macros.len() >= MACROS_CAP && !self.macros.contains_key(&m.name) {
            if let Some(oldest) = self.macros.keys().next().cloned() {
                self.macros.remove(&oldest);
            }
        }
        self.macros.insert(m.name.clone(), m);
        self.generation += 1;
    }

    /// 移除宏。
    pub fn remove_macro(&mut self, name: &str) {
        self.macros.remove(name);
        self.generation += 1;
    }

    /// 获取宏。
    pub fn get_macro(&self, name: &str) -> Option<&RecordedMacro> {
        self.macros.get(name)
    }

    /// 启动时从 DB 加载宏(替换)。
    pub fn apply_macros(&mut self, macros: Vec<RecordedMacro>) {
        self.macros = macros.into_iter().map(|m| (m.name.clone(), m)).collect();
    }
    // ──── Saved Views(视图预设)────

    /// 保存/覆盖视图预设(cap SAVED_VIEWS_CAP, FIFO 溢出)。
    pub fn add_saved_view(&mut self, name: String, snapshot: ViewSnapshot) {
        if self.saved_views.len() >= SAVED_VIEWS_CAP && !self.saved_views.contains_key(&name) {
            if let Some(oldest) = self.saved_views.keys().next().cloned() {
                self.saved_views.remove(&oldest);
            }
        }
        self.saved_views.insert(name, snapshot);
        self.generation += 1;
    }

    /// 移除视图预设, 返回是否存在。
    pub fn remove_saved_view(&mut self, name: &str) -> bool {
        let existed = self.saved_views.remove(name).is_some();
        if existed { self.generation += 1; }
        existed
    }

    /// 获取视图预设。
    pub fn get_saved_view(&self, name: &str) -> Option<&ViewSnapshot> {
        self.saved_views.get(name)
    }

    /// 启动时从 DB 加载视图预设(替换)。
    pub fn apply_saved_views(&mut self, views: Vec<(String, ViewSnapshot)>) {
        self.saved_views = views.into_iter().collect();
    }
    // ──── Notes(Agent 笔记)────

    /// 保存/覆盖 agent 笔记。
    pub fn add_note(&mut self, handle: &str, text: &str) {
        self.notes.insert(handle.to_string(), text.to_string());
        self.generation += 1;
    }

    /// 移除 agent 笔记。
    pub fn remove_note(&mut self, handle: &str) {
        self.notes.remove(handle);
        self.generation += 1;
    }

    /// 获取 agent 笔记。
    pub fn get_note(&self, handle: &str) -> Option<&String> {
        self.notes.get(handle)
    }

    /// 启动时从 DB 加载笔记(替换)。
    pub fn apply_notes(&mut self, notes: HashMap<String, String>) {
        self.notes = notes;
    }
    // ──── Aliases(命令别名)────

    /// 保存/覆盖别名。
    pub fn add_alias(&mut self, name: &str, expansion: &str) {
        self.aliases.insert(name.to_string(), expansion.to_string());
        self.generation += 1;
    }

    /// 移除别名。
    pub fn remove_alias(&mut self, name: &str) {
        self.aliases.remove(name);
        self.generation += 1;
    }

    /// 获取别名展开。
    pub fn get_alias(&self, name: &str) -> Option<&String> {
        self.aliases.get(name)
    }

    /// 启动时从 DB 加载别名(替换)。
    pub fn apply_aliases(&mut self, aliases: HashMap<String, String>) {
        self.aliases = aliases;
    }
    // ──── Hotkeys(自定义热键)────

    /// 保存/覆盖热键绑定。
    pub fn add_hotkey(&mut self, key: &str, command: &str) {
        self.hotkeys.insert(key.to_string(), command.to_string());
        self.generation += 1;
    }

    /// 移除热键绑定。
    pub fn remove_hotkey(&mut self, key: &str) {
        self.hotkeys.remove(key);
        self.generation += 1;
    }

    /// 获取热键绑定。
    pub fn get_hotkey(&self, key: &str) -> Option<&String> {
        self.hotkeys.get(key)
    }

    /// 启动时从 DB 加载热键(替换)。
    pub fn apply_hotkeys(&mut self, hotkeys: HashMap<String, String>) {
        self.hotkeys = hotkeys;
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

    /// 获取 refresh_interval_ms(默认 1500)。
    pub fn refresh_interval_ms(&self) -> u64 {
        self.get_config("refresh_interval_ms", "1500")
            .parse()
            .unwrap_or(1500)
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
    directory_sorted_with_mode(directory, SortMode::ByWorktree, &HashSet::new())
}

/// 带排序模式的版本(pinned 分区: 置顶 agent 排最前)。
pub fn directory_sorted_with_mode(
    directory: &HashMap<String, Agent>,
    mode: SortMode,
    pinned: &HashSet<String>,
) -> Vec<String> {
    let sorted = sort_directory_inner(directory, mode);
    if pinned.is_empty() {
        return sorted;
    }
    // 分区: pinned 在前, 其余在后, 各自保持原排序
    let mut result = Vec::with_capacity(sorted.len());
    result.extend(sorted.iter().filter(|h| pinned.contains(h.as_str())).cloned());
    result.extend(sorted.iter().filter(|h| !pinned.contains(h.as_str())).cloned());
    result
}

/// 原始排序逻辑(无 pinned 分区)。
fn sort_directory_inner(directory: &HashMap<String, Agent>, mode: SortMode) -> Vec<String> {
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
    tags: &HashMap<String, HashSet<String>>,
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
            match field {
                Some("source") => agent.source.as_deref().unwrap_or("").to_ascii_lowercase().contains(&value_lower),
                Some("state") => agent.effective_state().to_ascii_lowercase().contains(&value_lower),
                Some("cwd") | Some("path") | Some("worktree") => agent.cwd.to_ascii_lowercase().contains(&value_lower),
                Some("title") => agent.title.as_deref().unwrap_or("").to_ascii_lowercase().contains(&value_lower),
                Some("tag") => {
                    // tag:tagname → agent 拥有该标签(精确匹配, 大小写不敏感)
                    tags.get(*h).map_or(false, |s| s.iter().any(|t| t.eq_ignore_ascii_case(value)))
                }
                _ => {
                    // 无前缀: 跨维度模糊匹配
                    let combined = format!(
                        "{} {} {} {}",
                        agent.title.as_deref().unwrap_or(""),
                        agent.source.as_deref().unwrap_or(""),
                        agent.effective_state(),
                        agent.cwd,
                    );
                    crate::command::fuzzy_match(value, &combined)
                }
            }
        })
        .cloned()
        .collect()
}

// ───────────────────────── 全局搜索 ─────────────────────────

/// 搜索结果分类(决定分组顺序和跳转语义)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchCategory {
    Agent,
    Event,
    History,
    Command,
}

impl SearchCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Agent => "Agents",
            Self::Event => "Events",
            Self::History => "History",
            Self::Command => "Commands",
        }
    }
}
#[derive(Debug, Clone)]
pub enum JumpTarget {
    AgentHandle(String),
    EventIndex(usize),
    HistoryIndex(usize),
    CommandName(String),
}

/// 一条搜索结果。
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub category: SearchCategory,
    pub primary: String,
    pub secondary: String,
    pub jump_target: JumpTarget,
}

/// 截断到 max_len 字符(按 char 边界), 超出加 "…"。
fn truncate_search(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max_len).collect();
        t.push('…');
        t
    }
}

/// 全局搜索: 跨所有数据源 fuzzy match。空 query → 空 vec。
/// 分组顺序: Agent > Message > Event > History > Command。每组上限 5。
pub fn global_search(model: &Model, query: &str) -> Vec<SearchResult> {
    if query.is_empty() {
        return vec![];
    }
    let q = query.to_ascii_lowercase();
    let mut results = Vec::new();

    // Agents (cap 5)
    let mut count = 0;
    for agent in model.directory.values() {
        if count >= 5 { break; }
        let title = agent.title.as_deref().unwrap_or("");
        let source = agent.source.as_deref().unwrap_or("");
        let state = agent.state.as_deref().unwrap_or("");
        let tags_str = model.tags.get(&agent.handle)
            .map(|s| s.iter().cloned().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        if crate::command::fuzzy_match(&q, &agent.handle)
            || crate::command::fuzzy_match(&q, title)
            || crate::command::fuzzy_match(&q, &agent.cwd)
            || crate::command::fuzzy_match(&q, source)
            || crate::command::fuzzy_match(&q, &tags_str)
        {
            results.push(SearchResult {
                category: SearchCategory::Agent,
                primary: agent.handle.clone(),
                secondary: format!("{source} · {state} · {}", agent.cwd),
                jump_target: JumpTarget::AgentHandle(agent.handle.clone()),
            });
            count += 1;
        }
    }

    // Events (cap 5, newest-first)
    let mut count = 0;
    for (idx, ev) in model.events.iter().rev().enumerate() {
        if count >= 5 { break; }
        if crate::command::fuzzy_match(&q, &ev.text) || crate::command::fuzzy_match(&q, &ev.source) {
            let real_idx = model.events.len().saturating_sub(1).saturating_sub(idx);
            results.push(SearchResult {
                category: SearchCategory::Event,
                primary: truncate_search(&ev.text, 80),
                secondary: format!("{} · {} · {}", ev.severity.as_str(), ev.category.as_str(), ev.source),
                jump_target: JumpTarget::EventIndex(real_idx),
            });
            count += 1;
        }
    }

    // History (cap 5, newest-first)
    let mut count = 0;
    for (idx, entry) in model.history.iter().rev().enumerate() {
        if count >= 5 { break; }
        if crate::command::fuzzy_match(&q, &entry.text) || crate::command::fuzzy_match(&q, &entry.prefix) {
            let real_idx = model.history.len().saturating_sub(1).saturating_sub(idx);
            results.push(SearchResult {
                category: SearchCategory::History,
                primary: entry.text.clone(),
                secondary: entry.prefix.clone(),
                jump_target: JumpTarget::HistoryIndex(real_idx),
            });
            count += 1;
        }
    }

    // Commands (cap 5)
    let commands = crate::command::filter_commands(query);
    for cmd in commands.iter().take(5) {
        results.push(SearchResult {
            category: SearchCategory::Command,
            primary: cmd.name.to_string(),
            secondary: cmd.description.to_string(),
            jump_target: JumpTarget::CommandName(cmd.name.to_string()),
        });
    }

    results
}

// ───────────────────────── Dashboard 仪表盘 ─────────────────────────

/// Dashboard 聚合快照。纯计算投影, 从 &Model 单次遍历得出。
#[derive(Debug, Clone)]
pub struct ModelSnapshot {
    pub agent_total: usize,
    /// 6 槽位, 对齐 StatusCategory 判别值顺序 (Working=0 .. Unknown=5)。
    pub status_counts: [(StatusCategory, usize); 6],
    /// source 字符串 → agent 计数。
    pub source_counts: HashMap<String, usize>,
    pub message_total: usize,
    /// read == 0 的消息数。
    pub message_unread: usize,
    pub event_total: usize,
    /// 3 槽位: [("Info",n), ("Warn",n), ("Error",n)]。
    pub event_by_severity: [(String, usize); 3],
    /// EventCategory::as_str() → 计数。
    pub event_by_category: HashMap<String, usize>,
    pub group_count: usize,
    pub pinned_count: usize,
    pub history_count: usize,
    /// 最近 60 秒事件数(活跃度指标)。
    pub event_recent_60s: usize,
    pub computed_at_ms: i64,
    /// Top-5 tags by agent count (Dashboard 标签分布)。
    pub tag_counts: Vec<(String, usize)>,
    /// Active alert rule count。
    pub alert_rule_count: usize,
}

/// 计算 Dashboard 快照。纯计算, 无 IO。
pub fn compute_snapshot(model: &Model) -> ModelSnapshot {
    let computed_at_ms = now_ms();
    let cutoff_60s = computed_at_ms - 60_000;

    let mut status_counts: [(StatusCategory, usize); 6] = [
        (StatusCategory::Working, 0),
        (StatusCategory::Waiting, 0),
        (StatusCategory::Blocked, 0),
        (StatusCategory::Error, 0),
        (StatusCategory::Done, 0),
        (StatusCategory::Unknown, 0),
    ];
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    let mut pinned_in_dir: usize = 0;

    for agent in model.directory.values() {
        let cat = StatusCategory::from_agent(agent);
        status_counts[cat as usize].1 += 1;
        let src = agent.source.as_deref().unwrap_or("unknown");
        *source_counts.entry(src.to_string()).or_insert(0) += 1;
        if model.pinned.contains(&agent.handle) {
            pinned_in_dir += 1;
        }
    }
    let message_total: usize = 0;
    let message_unread: usize = 0;

    let mut sev_counts: [usize; 3] = [0, 0, 0];
    let mut event_by_category: HashMap<String, usize> = HashMap::new();
    let mut event_recent_60s: usize = 0;
    for ev in &model.events {
        let sev_idx = match ev.severity {
            EventSeverity::Info => 0,
            EventSeverity::Warn => 1,
            EventSeverity::Error => 2,
        };
        sev_counts[sev_idx] += 1;
        *event_by_category.entry(ev.category.as_str().to_string()).or_insert(0) += 1;
        if ev.timestamp_ms > cutoff_60s {
            event_recent_60s += 1;
        }
    }

    // Tag 分布: top-5 by agent count
    let mut tag_counts_map: HashMap<String, usize> = HashMap::new();
    for tags_set in model.tags.values() {
        for tag in tags_set {
            *tag_counts_map.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut tag_counts: Vec<(String, usize)> = tag_counts_map.into_iter().collect();
    tag_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    tag_counts.truncate(5);

    ModelSnapshot {
        agent_total: model.directory.len(),
        status_counts,
        source_counts,
        message_total,
        message_unread,
        event_total: model.events.len(),
        event_by_severity: [
            ("Info".to_string(), sev_counts[0]),
            ("Warn".to_string(), sev_counts[1]),
            ("Error".to_string(), sev_counts[2]),
        ],
        event_by_category,
        group_count: model.groups.len(),
        tag_counts,
        pinned_count: pinned_in_dir,
        history_count: model.history.len(),
        alert_rule_count: model.alert_rules.len(),
        event_recent_60s,
        computed_at_ms,
    }
}

// ───────────────────────── Alert Rules 告警规则 ─────────────────────────

/// 告警规则上限。
pub const ALERT_RULES_CAP: usize = 20;

/// 告警规则类型。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlertRuleType {
    State,
    Source,
    Severity,
    Message,
}

impl AlertRuleType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Source => "source",
            Self::Severity => "severity",
            Self::Message => "message",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "state" => Some(Self::State),
            "source" => Some(Self::Source),
            "severity" => Some(Self::Severity),
            "message" => Some(Self::Message),
            _ => None,
        }
    }
}

/// 一条告警规则。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlertRule {
    pub id: i64,
    pub rule_type: AlertRuleType,
    pub value: String,
    pub created_at_ms: i64,
}

/// 规则匹配上下文(由 update.rs 构造)。
#[derive(Debug, Default)]
pub struct CheckContext<'a> {
    pub handle: Option<&'a str>,
    pub new_state: Option<&'a str>,
    pub event_severity: Option<&'a str>,
    pub event_source: Option<&'a str>,
    pub is_new_message: bool,
}

/// 评估告警规则。返回匹配规则的 toast 消息列表。纯计算。
pub fn check_alert_rules(rules: &[AlertRule], ctx: &CheckContext) -> Vec<String> {
    let mut toasts = Vec::new();
    for rule in rules {
        let matched = match rule.rule_type {
            AlertRuleType::State => {
                ctx.new_state.map_or(false, |s| s.eq_ignore_ascii_case(&rule.value))
            }
            AlertRuleType::Source => {
                let h = ctx.handle.unwrap_or("");
                let s = ctx.event_source.unwrap_or("");
                h.eq_ignore_ascii_case(&rule.value) || s.eq_ignore_ascii_case(&rule.value)
            }
            AlertRuleType::Severity => {
                ctx.event_severity.map_or(false, |s| s.eq_ignore_ascii_case(&rule.value))
            }
            AlertRuleType::Message => ctx.is_new_message,
        };
        if matched {
            toasts.push(format!("🔔 alert: {}:{}", rule.rule_type.as_str(), rule.value));
        }
    }
    toasts
}


// ───────────────────────── Macros(宏录制) ─────────────────────────

/// Macro 上限。
pub const MACROS_CAP: usize = 50;

/// crossterm KeyEvent 的可序列化包装(DB JSON 持久化用)。
/// 只保存 code + modifiers; replay 时 kind=Press, state=NONE。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SerializableKeyEvent {
    /// 按键代码: "a", "Enter", "Esc", "Up", "Tab", "Backspace", "F1", …
    pub code: String,
    /// 修饰键: "NONE", "CONTROL", "SHIFT", "ALT", "CONTROL|SHIFT", …
    pub modifiers: String,
}

/// KeyCode → 字符串(序列化方向)。
fn key_code_to_str(code: &crossterm::event::KeyCode) -> String {
    use crossterm::event::KeyCode;
    match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "BackTab".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::F(n) => format!("F{n}"),
        _ => "Null".into(),
    }
}

/// 字符串 → KeyCode(反序列化方向)。未知返回 Null。
fn str_to_key_code(s: &str) -> crossterm::event::KeyCode {
    use crossterm::event::KeyCode;
    match s {
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Insert" => KeyCode::Insert,
        "Delete" => KeyCode::Delete,
        "Null" => KeyCode::Null,
        other if other.starts_with('F') && other.len() > 1 => {
            other[1..].parse::<u8>().ok().map(KeyCode::F).unwrap_or(KeyCode::Null)
        }
        other => KeyCode::Char(other.chars().next().unwrap_or('\0')),
    }
}

/// KeyModifiers → 字符串。
fn key_modifiers_to_str(m: &crossterm::event::KeyModifiers) -> String {
    use crossterm::event::KeyModifiers;
    let mut parts = Vec::new();
    if m.contains(KeyModifiers::SHIFT) { parts.push("SHIFT"); }
    if m.contains(KeyModifiers::CONTROL) { parts.push("CONTROL"); }
    if m.contains(KeyModifiers::ALT) { parts.push("ALT"); }
    if parts.is_empty() { "NONE".into() } else { parts.join("|") }
}

/// 字符串 → KeyModifiers。
fn str_to_key_modifiers(s: &str) -> crossterm::event::KeyModifiers {
    use crossterm::event::KeyModifiers;
    let mut m = KeyModifiers::NONE;
    for part in s.split('|') {
        match part.trim() {
            "SHIFT" => m |= KeyModifiers::SHIFT,
            "CONTROL" => m |= KeyModifiers::CONTROL,
            "ALT" => m |= KeyModifiers::ALT,
            _ => {}
        }
    }
    m
}

impl SerializableKeyEvent {
    pub fn from_key_event(k: &crossterm::event::KeyEvent) -> Self {
        Self {
            code: key_code_to_str(&k.code),
            modifiers: key_modifiers_to_str(&k.modifiers),
        }
    }

    pub fn to_key_event(&self) -> crossterm::event::KeyEvent {
        let code = str_to_key_code(&self.code);
        let modifiers = str_to_key_modifiers(&self.modifiers);
        crossterm::event::KeyEvent::new(code, modifiers)
    }
}

/// 序列化 KeyEvent 列表为 JSON 字符串。
pub fn serialize_key_events(events: &[crossterm::event::KeyEvent]) -> String {
    let serializable: Vec<SerializableKeyEvent> = events.iter().map(SerializableKeyEvent::from_key_event).collect();
    serde_json::to_string(&serializable).unwrap_or_else(|_| "[]".into())
}

/// 反序列化 JSON 字符串为 KeyEvent 列表。
pub fn deserialize_key_events(json: &str) -> Vec<crossterm::event::KeyEvent> {
    serde_json::from_str::<Vec<SerializableKeyEvent>>(json)
        .unwrap_or_default()
        .iter()
        .map(|s| s.to_key_event())
        .collect()
}

/// 一条录制的宏: 命名 + KeyEvent 序列(JSON)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecordedMacro {
    pub name: String,
    pub key_events_json: String,
    pub created_at_ms: i64,
}

/// 统计 JSON 中保存的按键数(轻量解析, 不完整反序列化)。
pub fn count_key_events(json: &str) -> usize {
    serde_json::from_str::<Vec<SerializableKeyEvent>>(json)
        .map(|v| v.len())
        .unwrap_or(0)
}

// ───────────────────────── Saved Views(视图预设) ─────────────────────────

/// Saved View 上限。
pub const SAVED_VIEWS_CAP: usize = 50;

/// 可序列化的视图状态快照(存储为 JSON)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ViewSnapshot {
    /// 活跃 tab: "directory" | "groups" | "messages"。
    pub tab: String,
    /// 过滤查询(可能含 tag:xxx 前缀)。
    pub filter_query: Option<String>,
    /// 排序模式: "by-worktree" | "by-state" | "by-source" | "by-name"。
    pub sort_mode: String,
    /// 多选 handle 集合(恢复 cursor 上下文)。
    #[serde(default)]
    pub selected_set: Vec<String>,
    /// 创建时间(epoch ms)。
    pub created_at_ms: i64,
}

// ───────────────────────── Agent Metrics(指标趋势) ─────────────────────────

/// 指标时间窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricsWindow {
    FiveMin,
    ThirtyMin,
    OneHour,
}

impl MetricsWindow {
    pub fn as_ms(&self) -> i64 {
        match self {
            Self::FiveMin => 300_000,
            Self::ThirtyMin => 1_800_000,
            Self::OneHour => 3_600_000,
        }
    }

    pub fn as_label(&self) -> &'static str {
        match self {
            Self::FiveMin => "5m",
            Self::ThirtyMin => "30m",
            Self::OneHour => "1h",
        }
    }

    pub fn cycle(&self) -> Self {
        match self {
            Self::FiveMin => Self::ThirtyMin,
            Self::ThirtyMin => Self::OneHour,
            Self::OneHour => Self::FiveMin,
        }
    }

    /// (bucket_count, bucket_ms) — 用于时间线分桶。
    pub fn buckets(&self) -> (usize, i64) {
        match self {
            Self::FiveMin => (10, 30_000),    // 10 buckets × 30s
            Self::ThirtyMin => (30, 60_000),  // 30 buckets × 1m
            Self::OneHour => (60, 60_000),    // 60 buckets × 1m
        }
    }
}

/// 单 agent 指标摘要。
#[derive(Debug, Clone)]
pub struct AgentMetrics {
    pub handle: String,
    pub total_events: usize,
    pub by_category: [usize; 5],
    pub by_severity: [usize; 3],
    pub timeline: Vec<u64>,
}

/// 全局指标快照(浮层渲染用)。
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub window: MetricsWindow,
    pub agents: Vec<AgentMetrics>,
    pub category_totals: [usize; 5],
    pub severity_totals: [usize; 3],
    pub global_timeline: Vec<u64>,
}

fn cat_to_idx(cat: &EventCategory) -> usize {
    match cat {
        EventCategory::Agent => 0,
        EventCategory::State => 1,
        EventCategory::Message => 2,
        EventCategory::Group => 3,
        EventCategory::System => 4,
    }
}

fn sev_to_idx(sev: &EventSeverity) -> usize {
    match sev {
        EventSeverity::Info => 0,
        EventSeverity::Warn => 1,
        EventSeverity::Error => 2,
    }
}

/// 从内存事件队列计算指标快照。纯函数, 无 IO。
pub fn compute_agent_metrics(model: &Model, window: MetricsWindow) -> MetricsSnapshot {
    let now = now_ms();
    let window_ms = window.as_ms();
    let cutoff = now - window_ms;
    let (bucket_count, bucket_ms) = window.buckets();

    let window_events: Vec<&Event> = model.events.iter()
        .filter(|e| e.timestamp_ms > cutoff)
        .collect();

    // 全局时间线
    let mut global_timeline = vec![0u64; bucket_count];
    for ev in &window_events {
        let age = now.saturating_sub(ev.timestamp_ms);
        let idx = ((window_ms - age) / bucket_ms) as usize;
        if idx < bucket_count {
            global_timeline[idx] += 1;
        }
    }

    // 收集 agent handle
    let mut handles: std::collections::HashSet<String> = window_events.iter()
        .filter(|e| e.source != "system")
        .map(|e| e.source.clone())
        .collect();
    for h in model.directory.keys() {
        handles.insert(h.clone());
    }

    // 每 agent 指标
    let mut agents: Vec<AgentMetrics> = handles.into_iter().map(|handle| {
        let mut m = AgentMetrics {
            handle: handle.clone(),
            total_events: 0,
            by_category: [0; 5],
            by_severity: [0; 3],
            timeline: vec![0u64; bucket_count],
        };
        for ev in &window_events {
            if ev.source == handle {
                m.total_events += 1;
                m.by_category[cat_to_idx(&ev.category)] += 1;
                m.by_severity[sev_to_idx(&ev.severity)] += 1;
                let age = now.saturating_sub(ev.timestamp_ms);
                let idx = ((window_ms - age) / bucket_ms) as usize;
                if idx < bucket_count {
                    m.timeline[idx] += 1;
                }
            }
        }
        m
    }).collect();

    // 按活跃度降序
    agents.sort_by(|a, b| b.total_events.cmp(&a.total_events));

    // 全局 totals
    let mut category_totals = [0usize; 5];
    let mut severity_totals = [0usize; 3];
    for ev in &window_events {
        category_totals[cat_to_idx(&ev.category)] += 1;
        severity_totals[sev_to_idx(&ev.severity)] += 1;
    }

    MetricsSnapshot {
        window, agents, category_totals, severity_totals, global_timeline,
    }
}

// ───────────────────────── Focus Mode(聚焦模式) ─────────────────────────

/// 聚焦模式过滤: focus_mode=true 时仅保留 selected_set 中的 handle。
/// focus_mode=false 时原样返回(零开销 clone)。
pub fn apply_focus_filter(
    sorted: Vec<String>,
    focus_mode: bool,
    selected: &std::collections::HashSet<String>,
) -> Vec<String> {
    if !focus_mode || selected.is_empty() {
        return sorted;
    }
    sorted.into_iter().filter(|h| selected.contains(h)).collect()
}

// ───────────────────────── Export/Import(导出导入) ─────────────────────────

/// 用户数据导出包(JSON 序列化)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportBundle {
    pub config: HashMap<String, String>,
    pub tags: HashMap<String, HashSet<String>>,
    pub snippets: HashMap<String, String>,
    pub macros: Vec<RecordedMacro>,
    pub saved_views: Vec<(String, ViewSnapshot)>,
    pub pinned: Vec<String>,
    pub alert_rules: Vec<AlertRule>,
    pub notes: HashMap<String, String>,
    pub aliases: HashMap<String, String>,
    pub hotkeys: HashMap<String, String>,
    pub watched: Vec<String>,
    pub templates: HashMap<String, String>,
}