//! shell.rs —— 范式 5 运行态(view 读, update 改)。
//!
//! Shell 持运行时 UI 状态(tab/focus/cursor/insert_mode/spinner/toasts)。
//! 数据态在 Model(物理分离)。generation guard 防陈旧回调(ADR-7)。

use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Directory,
    Groups,
    Messages,
}

impl Tab {
    /// 所有 tab 变体(按显示顺序)。
    pub const ALL: [Tab; 3] = [Tab::Directory, Tab::Groups, Tab::Messages];

    /// 显示标签。
    pub fn label(self) -> &'static str {
        match self {
            Tab::Directory => "Directory",
            Tab::Groups => "Groups",
            Tab::Messages => "Messages",
        }
    }

    /// 下一 tab(循环)。
    pub fn next(self) -> Self {
        match self {
            Tab::Directory => Tab::Groups,
            Tab::Groups => Tab::Messages,
            Tab::Messages => Tab::Directory,
        }
    }
}

/// 焦点区域。决定键盘输入目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Directory,
    Groups,
    Messages,
    Input,
}

/// Socket 连接状态(ADR-3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connected,
}

/// Shell 运行态。view 只读 &Shell, update 改 &mut Shell。
/// 无 IO(无 std::process/std::net/std::fs)。
#[derive(Debug)]
pub struct Shell {
    /// 当前 tab。
    pub tab: Tab,
    /// 当前焦点。
    pub focus: FocusTarget,
    /// 列表选中索引。
    pub cursor: usize,
    /// spinner 动画帧索引。
    pub spinner_frame: usize,
    /// socket 连接状态(ADR-3)。
    pub conn_state: ConnState,
    /// 是否处于输入模式(按 i 进入)。
    pub insert_mode: bool,
    /// 输入栏缓冲。
    pub input_buf: String,
    /// 终端尺寸 (width, height)。
    pub size: (u16, u16),
    pub toasts: Vec<(String, Instant)>,
    /// 命令面板激活(Ctrl-P)。
    pub palette_active: bool,
    /// 命令面板查询输入。
    pub palette_query: String,
    /// 命令面板选中索引。
    pub palette_cursor: usize,
    /// 过滤模式激活(Directory tab 按 / 进入)。
    pub filter_active: bool,
    pub filter_query: Option<String>,
    /// 浮层内容(read output / worktree ps)。
    pub overlay_content: Option<String>,
    /// 浮层滚动位置。
    pub overlay_scroll: usize,
    /// Groups tab: 选中群组后显示成员详情浮层。
    pub group_detail_active: bool,
    /// cheatsheet 浮层激活(? 键)。
    pub cheatsheet_active: bool,
    /// 编排任务浮层激活(t 键)。
    pub orch_tasks_active: bool,
    /// worktree ps 浮层激活(w 键)。
    pub worktree_ps_active: bool,
    /// config overlay 激活(show config 浮层)。
    pub config_overlay_active: bool,
    /// 活动日志浮层激活(a 键)。
    pub activity_active: bool,
    /// 输入历史回溯游标(None=未导航, Some(i)=回溯 history[i])。
    pub history_cursor: Option<usize>,
    /// 回溯时保存的草稿(Down 越过最新时恢复)。
    pub saved_input: String,
    /// 命令历史浮层激活(H 键)。
    pub history_overlay_active: bool,
    /// Dashboard 浮层激活(D 键)。
    pub dashboard_active: bool,
    /// Snippet library 浮层激活(S 键)。
    pub snippet_overlay_active: bool,
    /// Alert Rules 浮层激活(N 键)。
    pub rule_overlay_active: bool,
    /// Macro library 浮层激活(e 键)。
    pub macro_overlay_active: bool,
    /// 宏录制中。
    pub recording_active: bool,
    /// 宏录制缓冲(累积 KeyEvent)。
    pub recording_buffer: Vec<crossterm::event::KeyEvent>,
    /// 正在录制的宏名(非录制时为空)。
    pub recording_name: String,
    /// 宏回放队列(非空时每 tick 回放一个键)。
    pub replay_queue: Vec<crossterm::event::KeyEvent>,
    /// Saved Views 浮层激活(V 键)。
    pub views_overlay_active: bool,
    /// Metrics 浮层激活(x 键)。
    pub metrics_overlay_active: bool,
    /// Metrics 时间窗口(浮层内 w 键切换)。
    pub metrics_window: crate::model::MetricsWindow,
    /// Agent Note 浮层激活(n 键)。
    pub note_overlay_active: bool,
    /// 正在查看笔记的 agent handle(None = 列表模式)。
    pub note_viewing_handle: Option<String>,
    /// 笔记编辑缓冲(overlay 内输入)。
    pub note_edit_buf: String,
    /// 全局搜索浮层激活(Ctrl-S)。
    pub search_active: bool,
    /// 搜索查询输入。
    pub search_query: String,
    /// 搜索结果选中索引。
    pub search_cursor: usize,
    /// 多选的 handle 集合(Space 键 toggle)。
    pub selected_set: std::collections::HashSet<String>,
    /// Focus 模式: 仅显示 selected_set 中的 agent(f 键 toggle)。
    pub focus_mode: bool,
    /// Quick Actions 浮层激活(o 键)。
    pub quick_actions_active: bool,
    /// Quick Actions 选中索引。
    pub quick_actions_cursor: usize,
    /// Alias library 浮层激活(l 键)。
    pub alias_overlay_active: bool,
    /// Hotkeys 浮层激活(r 键)。
    pub hotkeys_overlay_active: bool,
    /// Theme customization 浮层激活(z 键)。
    pub theme_overlay_active: bool,
    /// Quick-Switch 浮层激活(v 键)。
    pub quickswitch_active: bool,
    /// Quick-Switch 查询输入。
    pub quickswitch_query: String,
    /// Quick-Switch 选中索引。
    pub quickswitch_cursor: usize,
    /// Activity log 类别过滤(空=显示全部)。
    pub activity_filter_categories: std::collections::HashSet<crate::model::EventCategory>,
    /// Activity log 严重级别过滤(空=显示全部)。
    pub activity_filter_severity: std::collections::HashSet<crate::model::EventSeverity>,
    /// Autocomplete dropdown 激活(insert mode 中 Tab 触发)。
    pub autocomplete_active: bool,
    /// Autocomplete 选中索引。
    pub autocomplete_cursor: usize,
    /// Template library 浮层激活(tpl:list 触发)。
    pub template_overlay_active: bool,
    /// Scheduler 浮层激活(sched:list 触发)。
    pub sched_overlay_active: bool,
    /// 当前主题名(从 config 加载,draw() 每帧读取)。
    pub theme_name: String,
    /// generation guard(范式 3: 防陈旧回调)。
    generation: u64,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            tab: Tab::Directory,
            focus: FocusTarget::Directory,
            cursor: 0,
            spinner_frame: 0,
            conn_state: ConnState::Connected,
            insert_mode: false,
            input_buf: String::new(),
            size: (80, 24),
            toasts: Vec::new(),
            palette_active: false,
            palette_query: String::new(),
            palette_cursor: 0,
            rule_overlay_active: false,
            filter_active: false,
            snippet_overlay_active: false,
            filter_query: None,
            dashboard_active: false,
            overlay_content: None,
            overlay_scroll: 0,
            search_active: false,
            search_query: String::new(),
            search_cursor: 0,
            group_detail_active: false,
            cheatsheet_active: false,
            activity_active: false,
            history_cursor: None,
            saved_input: String::new(),
            history_overlay_active: false,
            orch_tasks_active: false,
            worktree_ps_active: false,
            config_overlay_active: false,
            macro_overlay_active: false,
            recording_active: false,
            recording_buffer: Vec::new(),
            recording_name: String::new(),
            replay_queue: Vec::new(),
            views_overlay_active: false,
            metrics_overlay_active: false,
            metrics_window: crate::model::MetricsWindow::OneHour,
            note_overlay_active: false,
            note_viewing_handle: None,
            note_edit_buf: String::new(),
            selected_set: std::collections::HashSet::new(),
            focus_mode: false,
            quick_actions_active: false,
            quick_actions_cursor: 0,
            alias_overlay_active: false,
            hotkeys_overlay_active: false,
            theme_overlay_active: false,
            quickswitch_active: false,
            quickswitch_query: String::new(),
            sched_overlay_active: false,
            template_overlay_active: false,
            autocomplete_active: false,
            autocomplete_cursor: 0,
            quickswitch_cursor: 0,
            activity_filter_categories: std::collections::HashSet::new(),
            activity_filter_severity: std::collections::HashSet::new(),
            theme_name: "default".to_string(),
            generation: 0,
        }
    }

    /// generation guard: 发起异步操作前递增,回调时比对丢弃陈旧结果(范式 3)。
    pub fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 追加 toast 通知(发送成功/失败/错误提示)。
    pub fn push_toast(&mut self, msg: String) {
        self.toasts.push((msg, Instant::now()));
    }

    /// 清理过期 toast(> max_age_secs 秒)。
    pub fn drain_toasts(&mut self, max_age_secs: u64) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(max_age_secs);
        self.toasts.retain(|(_, t)| *t >= cutoff);
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}
