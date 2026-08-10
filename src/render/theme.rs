//! theme.rs —— 统一 truecolor 色阶(Catppuccin Mocha 风格)。
//!
//! 所有渲染模块共享此 palette,保证色阶一致。

use ratatui::style::{Color, Modifier, Style};

/// 统一主题。Copy + Clone,值传递,无生命周期。
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// 主前景色(文本)。
    pub fg: Color,
    /// 背景色。
    pub bg: Color,
    /// 强调色(tab/选中)。
    pub accent: Color,
    /// 次要色(hint/placeholder)。
    pub muted: Color,
    /// 工作中状态(青色)。
    pub working: Color,
    /// 空闲状态(灰色)。
    pub idle: Color,
    /// 错误状态(红色)。
    pub error: Color,
    /// 边框色。
    pub border: Color,
    /// 聚焦边框色。
    pub border_focus: Color,
    /// 选中行背景。
    pub selection_bg: Color,
    /// 选中行前景。
    pub selection_fg: Color,
    /// 成功色。
    pub success: Color,
    /// tab 激活背景。
    pub tab_active: Color,
    /// tab 未激活前景。
    pub tab_inactive: Color,
}

/// Catppuccin Mocha palette — 成熟、柔和不刺眼。
impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: Color::Rgb(205, 214, 244),       // Catppuccin text
            bg: Color::Rgb(30, 30, 46),          // Catppuccin base
            accent: Color::Rgb(137, 180, 250),    // Catppuccin blue
            muted: Color::Rgb(147, 153, 178),    // Catppuccin overlay0
            working: Color::Rgb(166, 227, 161),   // Catppuccin green
            idle: Color::Rgb(147, 153, 178),      // Catppuccin overlay0
            error: Color::Rgb(243, 139, 168),     // Catppuccin red
            border: Color::Rgb(88, 91, 112),       // Catppuccin surface0
            border_focus: Color::Rgb(137, 180, 250), // Catppuccin blue
            selection_bg: Color::Rgb(49, 50, 68),   // Catppuccin surface0
            selection_fg: Color::Rgb(205, 214, 244), // Catppuccin text
            success: Color::Rgb(166, 227, 161),     // Catppuccin green
            tab_active: Color::Rgb(137, 180, 250),  // Catppuccin blue
            tab_inactive: Color::Rgb(88, 91, 112),  // Catppuccin surface0
        }
    }
}

impl Theme {
    /// 状态色: 根据 agent state 返回对应色。
    pub fn state_color(&self, state: Option<&str>) -> Color {
        match state {
            Some(s) if s.contains("run") || s.contains("work") || s.contains("busy") => self.working,
            Some(s) if s.contains("error") || s.contains("fail") => self.error,
            Some(s) if s.contains("idle") || s.contains("done") || s.contains("ok") => self.idle,
            _ => self.muted,
        }
    }

    /// 状态 Style(带 bold)。
    pub fn state_style(&self, state: Option<&str>) -> Style {
        Style::default().fg(self.state_color(state)).add_modifier(Modifier::BOLD)
    }
}
