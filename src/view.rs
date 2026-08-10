//! view.rs —— 范式 5 纯渲染层(immediate-mode, draw 读 &Model + &Shell,绝不 &mut)。
//!
//! 布局(从上到下):
//! - TabBar(1 行): Directory / Groups / Messages
//! - 主区(bodyH = H - 6): 按当前 tab 渲染面板
//! - 输入栏(1 行): insert_mode 显示 input_buf + 光标
//! - 状态栏(1 行): spinner + 连接状态 + 快捷键提示
//! - 余量(1 行)
//! - Toast 层: 浮在底部上方

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::model::Model;
use crate::render::blocks;
use crate::render::theme::Theme;
use crate::shell::{ConnState, Shell, Tab};

/// 主 draw 入口。immediate-mode: 不 &mut Model/Shell。
pub fn draw(f: &mut Frame, model: &Model, shell: &Shell) {
    let theme = Theme::default();
    let area = f.area();

    // 终端太小: 不渲染
    if area.height < 8 || area.width < 20 {
        draw_too_small(f, &theme);
        return;
    }

    // bodyH = H - 6 (踩坑清单 #1)
    // 分配: TabBar(1) + body(H-6) + input(1) + status(1) + margin(1) = H
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // TabBar
            Constraint::Min(1),   // body (bodyH = H - 6 由框架自动计算)
            Constraint::Length(1), // 输入栏
            Constraint::Length(1), // 状态栏
        ])
        .split(area);

    draw_tabbar(f, shell, outer[0], &theme);
    draw_tab_body(f, model, shell, outer[1], &theme);
    draw_input_bar(f, shell, outer[2], &theme);
    draw_status_bar(f, shell, outer[3], &theme);

    // Toast 层(浮在底部上方)
    for (i, (msg, _)) in shell.toasts.iter().enumerate().rev() {
        if i >= 3 {
            break; // 最多 3 条
        }
        let toast_y = outer[3].y.saturating_sub(1 + i as u16);
        let toast_area = Rect {
            x: outer[3].x,
            y: toast_y,
            width: outer[3].width,
            height: 1,
        };
        let toast = Paragraph::new(Span::styled(
            msg.as_str(),
            Style::default().fg(theme.error).bg(theme.bg),
        ));
        f.render_widget(toast, toast_area);
    }
}

/// 终端太小提示。
fn draw_too_small(f: &mut Frame, theme: &Theme) {
    let msg = Paragraph::new(Span::styled(
        "Terminal too small (need ≥ 8 rows × 20 cols)",
        Style::default().fg(theme.muted),
    ));
    f.render_widget(msg, f.area());
}

/// 顶部 TabBar: 3 tab, 当前高亮 + 数字标号 + 连接灯。
fn draw_tabbar(f: &mut Frame, shell: &Shell, area: Rect, theme: &Theme) {
    let tabs = Tab::ALL;
    let mut spans: Vec<Span> = Vec::new();

    // 连接灯
    let conn = match shell.conn_state {
        ConnState::Connected => Span::styled("● ", Style::default().fg(theme.success)),
        ConnState::Disconnected => Span::styled("○ ", Style::default().fg(theme.muted)),
    };
    spans.push(conn);

    for (i, t) in tabs.iter().enumerate() {
        let active = *t == shell.tab;
        let label = if active {
            format!(" ▶{} {} ", i + 1, t.label())
        } else {
            format!(" {} {} ", i + 1, t.label())
        };
        let style = if active {
            Style::default()
                .fg(theme.bg)
                .bg(theme.tab_active)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.tab_inactive)
        };
        spans.push(Span::styled(label, style));
        if i + 1 < tabs.len() {
            spans.push(Span::styled("│", Style::default().fg(theme.border)));
        }
    }
    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

/// 主区: 按当前 tab 路由。
fn draw_tab_body(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    match shell.tab {
        Tab::Directory => draw_directory(f, model, shell, area, theme),
        Tab::Groups => draw_groups(f, model, shell, area, theme),
        Tab::Messages => draw_messages(f, model, shell, area, theme),
    }
}

/// Directory tab: agent card 网格。
///
/// 每个 card 5 行高(border×2 + content×3):
///   ┌──────────────────────────────┐
///   │ ● term_fca57171…        Pi   │  ← 连接灯 + handle + title
///   │ ~/.orca                     │  ← cwd(global→floating)
///   │ omp · running               │  ← source · state(branch)
///   └──────────────────────────────┘
/// card 宽 36, 列数自适应终端宽度。
fn draw_directory(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    let focused = matches!(shell.focus, crate::shell::FocusTarget::Directory);
    let block = blocks::bordered_block("Directory", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if model.directory.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "(no agents — waiting for orca-ide?)",
            Style::default().fg(theme.muted),
        ));
        f.render_widget(empty, inner);
        return;
    }

    // 排序: 按字母序
    let mut handles: Vec<&String> = model.directory.keys().collect();
    handles.sort();
    let total = handles.len();

    // card 尺寸
    let card_w: u16 = 36;
    let card_h: u16 = 5; // border(1) + content(3) + border(1)
    let gap: u16 = 1;
    let cols = ((inner.width + gap) / (card_w + gap)).max(1) as usize;
    let rows_per_col = (inner.height / card_h) as usize;
    let visible = cols * rows_per_col;

    // 滚动偏移: 让选中 card 可见
    let scroll = if total <= visible {
        0
    } else {
        let page = shell.cursor / visible;
        page * visible
    };

    for (i, handle) in handles.iter().enumerate() {
        if i < scroll || i >= scroll + visible {
            continue;
        }
        let idx = i - scroll;
        let col = idx % cols;
        let row = idx / cols;
        let x = inner.x + col as u16 * (card_w + gap);
        let y = inner.y + row as u16 * card_h;
        let card_area = Rect { x, y, width: card_w, height: card_h };

        if card_area.bottom() > inner.bottom() {
            break;
        }

        let agent = &model.directory[*handle];
        let is_selected = i == shell.cursor;
        draw_agent_card(f, agent, card_area, theme, is_selected);
    }
}

/// 渲染单个 agent card。
fn draw_agent_card(
    f: &mut Frame,
    agent: &crate::model::Agent,
    area: Rect,
    theme: &Theme,
    selected: bool,
) {
    // card border
    let border_color = if selected {
        theme.accent
    } else if agent.connected {
        theme.border
    } else {
        theme.muted
    };
    let border_style = Style::default().fg(border_color);

    // card title = 短 handle(后 8 位)
    let short = if agent.handle.len() >= 12 {
        &agent.handle[agent.handle.len().saturating_sub(8)..]
    } else {
        &agent.handle
    };
    let title_str = format!(" {} ", short);
    let title = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            title_str,
            if selected {
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            },
        ));

    let inner = title.inner(area);
    f.render_widget(title, area);

    // line 1: 连接灯 + handle(截断) + title
    let conn_span = Span::styled(
        if agent.connected { "● " } else { "○ " },
        Style::default().fg(if agent.connected { theme.success } else { theme.muted }),
    );
    let handle_display = crate::render::truncate_width(&agent.handle, 20);
    let handle_span = Span::styled(handle_display, Style::default().fg(theme.fg));

    // title 在右侧(如果有)
    let title_text = agent.title.as_deref().unwrap_or("").trim();
    let used = 2 + crate::render::truncate_width(&agent.handle, 20).len();
    let avail = inner.width as usize;
    let title_truncated = if !title_text.is_empty() && avail > used + 3 {
        crate::render::truncate_width(title_text, avail.saturating_sub(used + 2))
    } else {
        String::new()
    };
    let padding = avail.saturating_sub(used + title_truncated.chars().count());
    let title_span = Span::styled(title_truncated, Style::default().fg(theme.muted));
    let pad_span = Span::raw(" ".repeat(padding));

    let line1 = Line::from(vec![conn_span, handle_span, pad_span, title_span]);

    // line 2: cwd(global-floating 特殊处理)
    let cwd_display = if agent.cwd.is_empty() {
        "(global)".to_string()
    } else {
        // 缩短: ~/ 替换 home
        let home = std::env::var("HOME").unwrap_or_default();
        let cwd = if !home.is_empty() && agent.cwd.starts_with(&home) {
            format!("~{}", &agent.cwd[home.len()..])
        } else {
            agent.cwd.clone()
        };
        cwd
    };
    let cwd_str = crate::render::truncate_width(&cwd_display, inner.width as usize);
    let cwd_span = Span::styled(
        format!("📁 {}", cwd_str),
        Style::default().fg(theme.muted),
    );
    let line2 = Line::from(vec![cwd_span]);

    // line 3: source · state(branch)
    let source = agent.source.as_deref().unwrap_or("-");
    let state = agent.state.as_deref().unwrap_or("-");
    let branch = if !agent.branch.is_empty() {
        format!(" ({})", agent.branch)
    } else {
        String::new()
    };
    let meta_str = crate::render::truncate_width(
        &format!("{} · {}{}", source, state, branch),
        inner.width as usize,
    );
    let meta_span = Span::styled(
        meta_str,
        Style::default().fg(theme.state_color(agent.state.as_deref())),
    );
    let line3 = Line::from(vec![meta_span]);

    let content = ratatui::widgets::Paragraph::new(vec![line1, line2, line3]);
    f.render_widget(content, inner);
}

/// Groups tab: 群组列表 + 成员。
fn draw_groups(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    let focused = matches!(shell.focus, crate::shell::FocusTarget::Groups);
    let block = blocks::bordered_block("Groups", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if model.groups.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "(no groups)",
            Style::default().fg(theme.muted),
        ));
        f.render_widget(empty, inner);
        return;
    }

    // 排序: 按群组名
    let mut names: Vec<&String> = model.groups.keys().collect();
    names.sort();

    let items: Vec<ListItem> = names
        .iter()
        .map(|name| {
            let members = model.groups[*name].len();
            // 列出前 3 个成员
            let member_preview: Vec<String> = model.groups[*name]
                .iter()
                .take(3)
                .map(|h| crate::render::truncate_width(h, 16))
                .collect();
            let extra = if members > 3 {
                format!(" +{}", members - 3)
            } else {
                String::new()
            };
            let preview = member_preview.join(", ") + &extra;

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", name),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("({}) ", members),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(preview, Style::default().fg(theme.fg)),
            ]))
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg),
    );

    let mut state = ListState::default();
    if !names.is_empty() {
        state.select(Some(shell.cursor.min(names.len() - 1)));
    }

    f.render_stateful_widget(list, inner, &mut state);
}

/// Messages tab: inbox 消息流(按 thread_id 分组, 最近在上)。
fn draw_messages(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    let focused = matches!(shell.focus, crate::shell::FocusTarget::Messages);
    let block = blocks::bordered_block("Messages", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if model.messages.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "(no messages — inbox empty)",
            Style::default().fg(theme.muted),
        ));
        f.render_widget(empty, inner);
        return;
    }

    // 消息列表: 最新在上(遍历 order 是 vecdeque, 前头=旧, 后尾=新)
    let items: Vec<ListItem> = model
        .messages
        .iter()
        .rev()
        .map(|msg| {
            let from = crate::render::truncate_width(&msg.from_handle, 16);
            let subject = crate::render::truncate_width(&msg.subject, 50);
            let time = crate::render::truncate_width(&msg.created_at, 19);

            let thread_mark = msg
                .thread_id
                .as_deref()
                .map(|_| "↩")
                .unwrap_or("·");

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", thread_mark),
                    Style::default().fg(theme.accent),
                ),
                Span::styled(
                    crate::render::pad_left(&from, 16),
                    Style::default().fg(theme.working),
                ),
                Span::styled(" → ", Style::default().fg(theme.muted)),
                Span::styled(
                    crate::render::pad_left(&subject, 50),
                    Style::default().fg(theme.fg),
                ),
                Span::styled(format!("  {}", time), Style::default().fg(theme.muted)),
            ]))
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg),
    );

    let mut state = ListState::default();
    let len = model.messages.len();
    if !model.messages.is_empty() {
        state.select(Some(shell.cursor.min(len - 1)));
    }

    f.render_stateful_widget(list, inner, &mut state);
}

/// 输入栏: insert_mode 时显示 input_buf + 光标。
fn draw_input_bar(f: &mut Frame, shell: &Shell, area: Rect, theme: &Theme) {
    if shell.insert_mode {
        let input_display = if shell.input_buf.is_empty() {
            Span::styled("Type message (Enter to send, Esc to cancel)", Style::default().fg(theme.muted))
        } else {
            Span::styled(shell.input_buf.as_str(), Style::default().fg(theme.fg))
        };
        let para = Paragraph::new(Line::from(vec![
            Span::styled(" > ", Style::default().fg(theme.accent)),
            input_display,
        ]));
        f.render_widget(para, area);

        // 光标定位(buf 末尾 + "> " offset)
        let cursor_x = (shell.input_buf.len() as u16 + 3).min(area.width.saturating_sub(1));
        f.set_cursor_position((area.x + cursor_x, area.y));
    } else {
        // 非 insert_mode: 提示
        let hint = match shell.tab {
            Tab::Directory => "i:send  j/k:navigate  Enter:select  g:groups",
            Tab::Groups => "i:send  j/k:navigate  g:messages",
            Tab::Messages => "i:send  j/k:navigate  g:directory",
        };
        let para = Paragraph::new(Span::styled(hint, Style::default().fg(theme.muted)));
        f.render_widget(para, area);
    }
}

/// 状态栏: spinner + 连接状态 + 快捷键。
fn draw_status_bar(f: &mut Frame, shell: &Shell, area: Rect, theme: &Theme) {
    let mut spans: Vec<Span> = Vec::new();

    // spinner
    let ch = spinner_char(shell.spinner_frame);
    spans.push(Span::styled(
        format!("{ch} "),
        Style::default().fg(theme.accent),
    ));

    // Tab
    spans.push(Span::styled(
        shell.tab.label(),
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));

    // 连接状态
    let (conn_label, conn_color) = match shell.conn_state {
        ConnState::Connected => ("connected", theme.success),
        ConnState::Disconnected => ("disconnected", theme.error),
    };
    spans.push(Span::styled(conn_label, Style::default().fg(conn_color)));

    // 右侧: q:quit  Tab:switch  Ctrl-d/u:scroll
    let right_hint = " q:quit  Tab:switch  Ctrl-d/u:scroll ";
    // 用 pad 到右边
    let total_text_width: usize = spans.iter().map(|s| s.width()).sum();
    let space_left = (area.width as usize).saturating_sub(total_text_width + right_hint.len());
    spans.push(Span::raw(" ".repeat(space_left)));
    spans.push(Span::styled(right_hint, Style::default().fg(theme.muted)));

    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

/// spinner 字符集(简化版: 4 帧)。
fn spinner_char(frame: usize) -> char {
    const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[frame % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinner_char_cycles() {
        // 不 panic, 周期正确
        let c0 = spinner_char(0);
        let c1 = spinner_char(1);
        assert_ne!(c0, c1);
        // 10 帧后回到起点
        assert_eq!(spinner_char(10), c0);
    }

    #[test]
    fn test_truncate_width_ascii() {
        assert_eq!(crate::render::truncate_width("hello", 10), "hello");
        assert_eq!(crate::render::truncate_width("hello", 3), "he…");
        assert_eq!(crate::render::truncate_width("hello", 0), "");
    }

    #[test]
    fn test_pad_left_ascii() {
        assert_eq!(crate::render::pad_left("ab", 5), "ab   ");
        assert_eq!(crate::render::pad_left("abcdef", 3), "ab…");
    }
}
