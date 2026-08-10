//! render/ —— 渲染辅助模块。
//!
//! - theme: 统一 truecolor 色阶(Catppuccin Mocha 风格)
//! - blocks: 边框/面板辅助函数

pub mod blocks;
pub mod theme;

pub use theme::Theme;

/// 显示宽度感知截断:按 unicode 显示宽度截断,末尾加省略号。
/// CJK=2cell, ASCII=1cell。
pub fn truncate_width(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = UnicodeWidthStr::width(s);
    if w <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let target = max_width.saturating_sub(1);
    let mut acc = 0usize;
    let mut result = String::new();
    for c in s.chars() {
        let cw = UnicodeWidthStr::width(c.to_string().as_str()).max(1);
        if acc + cw > target {
            break;
        }
        acc += cw;
        result.push(c);
    }
    result.push('…');
    result
}

/// 显示宽度感知左对齐填充:右侧补空格。
pub fn pad_left(s: &str, target_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = UnicodeWidthStr::width(s);
    if w >= target_width {
        truncate_width(s, target_width)
    } else {
        let pad = target_width - w;
        format!("{s}{}", " ".repeat(pad))
    }
}
