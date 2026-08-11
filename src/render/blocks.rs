//! blocks.rs —— 面板辅助函数(边框 Block 构造)。

use ratatui::style::{Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders};

use super::Theme;

/// 构造带边框的 Block。focused 时边框高亮。
pub fn bordered_block<'a>(title: &'a str, focused: bool, theme: &Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme.accent),
        ))
        .border_style(Style::default().fg(if focused {
            theme.border_focus
        } else {
            theme.border
        }))
}

