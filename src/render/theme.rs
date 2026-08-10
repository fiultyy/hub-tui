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
    /// 警告色(活动日志 Warn 严重级)。
    pub warn: Color,
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
        Self::mocha()
    }
}

impl Theme {
    /// 按名称选择主题。未知名称 fallback 到 mocha。
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "light" => Self::light(),
            "contrast" | "hc" | "high-contrast" => Self::contrast(),
            _ => Self::mocha(), // "default" / "dark" / "mocha" / unknown
        }
    }

    /// Catppuccin Mocha(默认深色)。
    pub fn mocha() -> Self {
        Self {
            fg: Color::Rgb(205, 214, 244),
            bg: Color::Rgb(30, 30, 46),
            accent: Color::Rgb(137, 180, 250),
            muted: Color::Rgb(147, 153, 178),
            warn: Color::Rgb(249, 226, 175),
            working: Color::Rgb(166, 227, 161),
            idle: Color::Rgb(147, 153, 178),
            error: Color::Rgb(243, 139, 168),
            border: Color::Rgb(88, 91, 112),
            border_focus: Color::Rgb(137, 180, 250),
            selection_bg: Color::Rgb(49, 50, 68),
            selection_fg: Color::Rgb(205, 214, 244),
            success: Color::Rgb(166, 227, 161),
            tab_active: Color::Rgb(137, 180, 250),
            tab_inactive: Color::Rgb(88, 91, 112),
        }
    }

    /// 浅色主题(暖白底,适合明亮环境)。
    pub fn light() -> Self {
        Self {
            fg: Color::Rgb(60, 56, 54),
            bg: Color::Rgb(245, 240, 235),
            accent: Color::Rgb(0, 123, 167),
            muted: Color::Rgb(146, 131, 116),
            working: Color::Rgb(46, 125, 50),
            warn: Color::Rgb(180, 130, 20),
            idle: Color::Rgb(146, 131, 116),
            error: Color::Rgb(198, 40, 40),
            border: Color::Rgb(189, 174, 147),
            border_focus: Color::Rgb(0, 123, 167),
            selection_bg: Color::Rgb(221, 214, 207),
            selection_fg: Color::Rgb(60, 56, 54),
            success: Color::Rgb(46, 125, 50),
            tab_active: Color::Rgb(0, 123, 167),
            tab_inactive: Color::Rgb(189, 174, 147),
        }
    }

    /// 高对比度主题(纯黑白,最大可读性)。
    pub fn contrast() -> Self {
        Self {
            fg: Color::Rgb(255, 255, 255),
            bg: Color::Rgb(0, 0, 0),
            accent: Color::Rgb(0, 255, 255),
            muted: Color::Rgb(160, 160, 160),
            warn: Color::Rgb(255, 255, 0),
            working: Color::Rgb(0, 255, 0),
            idle: Color::Rgb(128, 128, 128),
            error: Color::Rgb(255, 0, 0),
            border: Color::Rgb(96, 96, 96),
            border_focus: Color::Rgb(0, 255, 255),
            selection_bg: Color::Rgb(48, 48, 48),
            selection_fg: Color::Rgb(255, 255, 255),
            success: Color::Rgb(0, 255, 0),
            tab_active: Color::Rgb(0, 255, 255),
            tab_inactive: Color::Rgb(64, 64, 64),
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

/// 解析颜色字符串: hex (#RRGGBB) 或命名色 (red/green/blue/etc)。
/// 返回 None 表示无法解析(调用方保留默认值)。
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    // hex #RRGGBB
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
    }
    // named colors
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "white" => Color::White,
        "reset" | "default" => return None, // reset = use base theme
        _ => return None,
    })
}

/// 按 config 覆盖的 key 列表, 返回字段名 slice。
const THEME_COLOR_KEYS: &[&str] = &[
    "fg", "bg", "accent", "muted", "working", "idle", "error", "warn",
    "border", "border_focus", "selection_bg", "selection_fg", "success",
    "tab_active", "tab_inactive",
];

impl Theme {
    /// 按名称加载主题, 再用 config 中的 theme.* 键覆盖颜色。
    pub fn from_name_with_overrides(name: &str, config: &std::collections::HashMap<String, String>) -> Self {
        let mut theme = Self::from_name(name);
        for &key in THEME_COLOR_KEYS {
            let config_key = format!("theme.{key}");
            if let Some(val) = config.get(&config_key) {
                if let Some(color) = parse_color(val) {
                    match key {
                        "fg" => theme.fg = color,
                        "bg" => theme.bg = color,
                        "accent" => theme.accent = color,
                        "muted" => theme.muted = color,
                        "working" => theme.working = color,
                        "idle" => theme.idle = color,
                        "error" => theme.error = color,
                        "warn" => theme.warn = color,
                        "border" => theme.border = color,
                        "border_focus" => theme.border_focus = color,
                        "selection_bg" => theme.selection_bg = color,
                        "selection_fg" => theme.selection_fg = color,
                        "success" => theme.success = color,
                        "tab_active" => theme.tab_active = color,
                        "tab_inactive" => theme.tab_inactive = color,
                        _ => {}
                    }
                }
            }
        }
        theme
    }
}
