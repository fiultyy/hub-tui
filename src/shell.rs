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
    pub worktree_ps_active: bool,
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
            filter_active: false,
            filter_query: None,
            overlay_content: None,
            overlay_scroll: 0,
            group_detail_active: false,
            cheatsheet_active: false,
            worktree_ps_active: false,
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
