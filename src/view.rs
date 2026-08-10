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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::model::{directory_sorted_handles, Model, StatusCategory};
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

    // 命令面板浮层(Ctrl-P 激活时覆盖在主内容上方)
    if shell.palette_active {
        draw_command_palette(f, shell, area, &theme);
    }

    // 过滤指示器(Directory tab 过滤模式)
    if shell.filter_active {
        draw_filter_indicator(f, shell, outer[2], &theme);
    }

    // terminal read 浮层(overlay_content 有内容时弹出)
    if let Some(content) = &shell.overlay_content {
        draw_output_overlay(f, content, shell, area, &theme);
    }

    // worktree ps 浮层
    if shell.worktree_ps_active {
        draw_worktree_ps_overlay(f, model, shell, area, &theme);
    }

    // cheatsheet 浮层(? 键激活)
    if shell.cheatsheet_active {
        draw_cheatsheet(f, shell, area, &theme);
    }

    // config overlay (show config 命令激活)
    if shell.config_overlay_active {
        draw_config_overlay(f, model, shell, area, &theme);
    }

    // 编排任务浮层(t 键激活)
    if shell.orch_tasks_active {
        draw_orch_tasks_overlay(f, model, shell, area, &theme);
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

/// 主区: 按当前 tab 路由。group_detail_active 时叠加浮层。
fn draw_tab_body(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    match shell.tab {
        Tab::Directory => draw_directory(f, model, shell, area, theme),
        Tab::Groups => draw_groups(f, model, shell, area, theme),
        Tab::Messages => draw_messages(f, model, shell, area, theme),
    }
    // 群组详情浮层(叠加在 Groups tab 之上)
    if shell.group_detail_active {
        draw_group_detail(f, model, shell, area, theme);
    }
}

// ───────────────────────── 布局常量 + 类型(draw + hit_test 共享) ─────────────────────────

/// 卡片宽度(字符)。
pub const CARD_W: u16 = 36;
/// 卡片内容高度(行)。
pub const CARD_H: u16 = 4; // identity(1) + prompt(1) + tool+action(1) + meta(1)
/// 卡片间距(行/列)。
pub const CARD_GAP: u16 = 1;
/// 分区标题高度。
pub const SECTION_HEADER_H: u16 = 1;
/// 分区间距。
pub const SECTION_GAP: u16 = 1;

/// 布局项: 分区标题或卡片。
#[derive(Debug)]
pub enum LayoutItem {
    SectionHeader { group: String, count: usize },
    Card { sorted_idx: usize },
}

/// 布局项 + 绝对位置(y 从内容顶部起算, 非 screen 坐标)。
#[derive(Debug)]
pub struct LayoutEntry {
    pub item: LayoutItem,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// 计算完整 Directory 布局(所有分区标题 + 卡片)。
/// sorted 必须已按 directory_sorted_handles 排序。
/// 返回内容顶部的绝对 y 坐标列表(不含 scroll 偏移)。
pub fn directory_layout(
    sorted: &[String],
    model: &Model,
    inner_x: u16,
    inner_w: u16,
) -> Vec<LayoutEntry> {
    let cols = (((inner_w + CARD_GAP) / (CARD_W + CARD_GAP)).max(1)) as usize;
    let mut entries = Vec::new();
    let mut y: u16 = 0;

    let mut i = 0;
    while i < sorted.len() {
        let agent = &model.directory[&sorted[i]];
        let group = agent.cwd.clone();

        // 收集同 worktreePath 的连续句柄(sorted 保证连续)
        let group_start = i;
        while i < sorted.len() {
            let a = &model.directory[&sorted[i]];
            if a.cwd != group {
                break;
            }
            i += 1;
        }
        let count = i - group_start;

        // 分区标题
        entries.push(LayoutEntry {
            item: LayoutItem::SectionHeader { group, count },
            x: inner_x,
            y,
            w: inner_w,
            h: SECTION_HEADER_H,
        });
        y += SECTION_HEADER_H;

        // 卡片网格
        for card_i in 0..count {
            let col = (card_i % cols) as u16;
            let row = (card_i / cols) as u16;
            let sorted_idx = group_start + card_i;
            entries.push(LayoutEntry {
                item: LayoutItem::Card { sorted_idx },
                x: inner_x + col * (CARD_W + CARD_GAP),
                y: y + row * (CARD_H + CARD_GAP),
                w: CARD_W,
                h: CARD_H,
            });
        }

        // 推进 y 跳过所有卡片行(含 gap)
        let rows_needed = ((count + cols - 1) / cols).max(1) as u16;
        y += rows_needed * (CARD_H + CARD_GAP);
        y += SECTION_GAP;
    }

    entries
}

/// 根据 cursor 位置计算 scroll 偏移(纯函数, 无存储状态)。
/// 策略: 尽量把 cursor 所在分区的标题对齐到视口顶部;
/// 若分区太高导致 cursor 超出视口, 则滚动到刚好露出 cursor 底部。
pub fn directory_scroll(cursor: usize, layout: &[LayoutEntry], visible_h: u16) -> u16 {
    let content_h = layout.iter().map(|e| e.y + e.h).max().unwrap_or(0);
    let max_scroll = content_h.saturating_sub(visible_h);

    let mut section_y = 0u16;
    for entry in layout {
        match &entry.item {
            LayoutItem::SectionHeader { .. } => {
                section_y = entry.y;
            }
            LayoutItem::Card { sorted_idx } if *sorted_idx == cursor => {
                let cursor_bottom = entry.y + entry.h;
                // 分区对齐: 若 cursor 在视口内, 用 section_y
                if cursor_bottom <= section_y + visible_h {
                    return section_y.min(max_scroll);
                }
                // 分区太高: 滚动到刚好露出 cursor
                return cursor_bottom.saturating_sub(visible_h).min(max_scroll);
            }
            _ => {}
        }
    }
    0
}

/// 状态分类 → 主题色映射。
fn category_color(cat: StatusCategory, theme: &Theme) -> Color {
    match cat {
        StatusCategory::Working => theme.working,
        StatusCategory::Waiting => theme.accent,
        StatusCategory::Blocked => theme.error,
        StatusCategory::Error => theme.error,
        StatusCategory::Done => theme.idle,
        StatusCategory::Unknown => theme.muted,
    }
}

/// Directory tab: agent card 按 worktreePath 分组垂直堆叠。
///
/// 布局(从上到下): 每个 worktreePath 一个分区, 先标题行(📂 path (N))后卡片网格。
/// 分区按最近活跃排序(最近在前)。卡片 5 行高(色块底色)。
/// 滚动: cursor 所在分区对齐视口顶部。
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

    let sorted = if shell.filter_active {
        let q = shell.filter_query.as_deref().unwrap_or("");
        let full = directory_sorted_handles(&model.directory);
        crate::model::directory_filter_handles(&full, &model.directory, q)
    } else {
        directory_sorted_handles(&model.directory)
    };
    let layout = directory_layout(&sorted, model, inner.x, inner.width);
    let scroll_y = directory_scroll(shell.cursor, &layout, inner.height);

    for entry in &layout {
        // 内容空间剔除: 完全在视口上方/下方的项跳过
        // (saturating_sub 会把上方项钳到 inner.y, 不加此守卫会导致重叠渲染)
        if entry.y + entry.h <= scroll_y || entry.y >= scroll_y + inner.height {
            continue;
        }
        let adj_y = entry.y.saturating_sub(scroll_y) + inner.y;
        match &entry.item {
            LayoutItem::SectionHeader { group, count } => {
                draw_section_header(f, group, *count, entry.x, adj_y, entry.w, theme);
            }
            LayoutItem::Card { sorted_idx } => {
                if let Some(agent) = sorted.get(*sorted_idx).and_then(|h| model.directory.get(h)) {
                    let is_selected = *sorted_idx == shell.cursor;
                    let card_area = Rect { x: entry.x, y: adj_y, width: entry.w, height: entry.h };
                    let unread = *model.unread_counts.get(&agent.handle).unwrap_or(&0);
                    draw_agent_card(f, agent, card_area, theme, is_selected, unread);
                }
            }
        }
    }
}

/// 渲染分区标题行: 📂 worktreePath + (数量) + 分隔线。
fn draw_section_header(
    f: &mut Frame,
    group: &str,
    count: usize,
    x: u16,
    y: u16,
    w: u16,
    theme: &Theme,
) {
    use unicode_width::UnicodeWidthStr;
    let home = std::env::var("HOME").unwrap_or_default();
    let display = if group.is_empty() {
        "(global)".to_string()
    } else if !home.is_empty() && group.starts_with(&home) {
        format!("~{}", &group[home.len()..])
    } else {
        group.to_string()
    };
    let prefix = format!(" \u{1f4c2} {} ({}) ", display, count);
    let prefix_w = UnicodeWidthStr::width(prefix.as_str()) as u16;
    let divider_w = w.saturating_sub(prefix_w);

    let mut spans = vec![
        Span::styled(prefix, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
    ];
    if divider_w > 0 {
        spans.push(Span::styled(
            "\u{2500}".repeat(divider_w as usize),
            Style::default().fg(theme.border),
        ));
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x, y, width: w, height: 1 },
    );
}

// ───────────────────────── 卡片模板组件 ─────────────────────────

/// 从 handle 提取 8 位 tag: `term_fca57171-...` → `fca57171`。
fn handle_tag(handle: &str) -> &str {
    let rest = handle.strip_prefix("term_").unwrap_or(handle);
    &rest[..8.min(rest.len())]
}

/// 格式化 elapsed: `now - lastOutputAt(ms)` → "2s"/"3m"/"1h"/"2d"。
fn format_elapsed(last_output_at: Option<i64>) -> String {
    let ms = match last_output_at {
        Some(v) if v > 0 => v,
        _ => return String::new(),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let secs = ((now - ms) / 1000).max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// 卡片样式参数(从 agent+selected 派生)。
struct CardStyle {
    bg: Color,
    bar_fg: Color,
    tag_bg: Color,
}

impl CardStyle {
    fn compute(agent: &crate::model::Agent, selected: bool, theme: &Theme) -> Self {
        let cat = StatusCategory::from_agent(agent);
        if selected {
            Self { bg: Color::Rgb(49, 62, 96), bar_fg: theme.accent, tag_bg: theme.accent }
        } else if agent.connected {
            Self {
                bg: Color::Rgb(40, 41, 58),
                bar_fg: category_color(cat, theme),
                tag_bg: category_color(cat, theme),
            }
        } else {
            Self { bg: Color::Rgb(24, 24, 37), bar_fg: theme.muted, tag_bg: theme.muted }
        }
    }
}

/// source → 单字符图标映射(辨识度 >> hex tag)。
fn source_icon(source: Option<&str>) -> &'static str {
    match source.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some(s) if s.contains("pi") || s.contains("omp") => "\u{03c0}", // π
        Some(s) if s.contains("claude") || s.contains("cc") => "c",
        Some(s) if s.contains("codex") => "o",
        Some(s) if s.contains("grok") => "x",
        Some(s) if s.contains("cursor") => "\u{25c8}",                 // ◈
        _ => ">",
    }
}

/// 取 preview 最后一个非空行(过滤装饰线), 给裸终端提供辨识。
fn preview_tail(preview: Option<&str>) -> String {
    let raw = preview.unwrap_or("");
    raw.lines()
        .rev()
        .map(|l| l.trim())
        .find(|l| {
            !l.is_empty()
                && !l.starts_with('\u{2500}') // ─
                && !l.starts_with('\u{2550}') // ═
                && !l.starts_with('\u{2551}') // ║
                && !l.starts_with('\u{256d}') // ╭
                && !l.starts_with('\u{2570}') // ╰
                && !l.starts_with('\u{2502}') // │
        })
        .unwrap_or("")
        .to_string()
}

/// 渲染单个 agent card(4 行紧凑布局, 无空白行)。
///
/// 布局:
/// ```text
/// π title (N)                    ⠋ 2s   ← row 0: source图标+title+badge | 状态+elapsed右对齐
/// ▌ prompt 或 ↳lastMsg 或 preview        ← row 1: 主辨识行(fallback 链)
/// ▌ 🔧tool  toolInput截断                ← row 2: 工具+动作
/// ▌ source · state (branch)              ← row 3: 元信息
/// ```
fn draw_agent_card(
    f: &mut Frame,
    agent: &crate::model::Agent,
    area: Rect,
    theme: &Theme,
    selected: bool,
    unread: usize,
) {
    use unicode_width::UnicodeWidthStr;

    let cs = CardStyle::compute(agent, selected, theme);
    let bg_style = Style::default().bg(cs.bg);
    let avail = area.width as usize;
    let indent = 2usize; // 竖条(1) + gap(1)

    f.render_widget(ratatui::widgets::Clear, area);

    // ── row 0: source图标 + title + badge | 状态图标 + elapsed(右对齐) ──
    let icon = source_icon(agent.source.as_deref());
    let cat = StatusCategory::from_agent(agent);
    let title_text = agent.title.as_deref().unwrap_or("").trim();
    let badge_str = if unread > 0 { format!(" ({unread})") } else { String::new() };
    let elapsed = format_elapsed(agent.last_output_at);
    let right_part = format!("{} {}", cat.icon(), if elapsed.is_empty() { String::new() } else { elapsed });
    let right_w = UnicodeWidthStr::width(right_part.as_str()) + 1; // +1 for gap
    let badge_w = UnicodeWidthStr::width(badge_str.as_str());
    let icon_w = UnicodeWidthStr::width(icon) + 1; // icon + space
    let title_max = avail.saturating_sub(icon_w + badge_w + right_w);
    let title_trunc = if !title_text.is_empty() && title_max > 2 {
        crate::render::truncate_width(title_text, title_max)
    } else {
        String::new()
    };
    let title_display_w = UnicodeWidthStr::width(title_trunc.as_str());
    let title_pad = title_max.saturating_sub(title_display_w);
    let mut row0_spans = vec![
        Span::styled(icon, Style::default().fg(cs.bar_fg).add_modifier(Modifier::BOLD)),
        Span::styled(" ", Style::default()),
        Span::styled(title_trunc, Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(title_pad), Style::default()),
    ];
    if !badge_str.is_empty() {
        row0_spans.push(Span::styled(
            badge_str,
            Style::default().fg(theme.error).add_modifier(Modifier::BOLD),
        ));
    }
    row0_spans.push(Span::styled(" ", Style::default()));
    row0_spans.push(Span::styled(
        right_part,
        Style::default().fg(theme.state_color(agent.state.as_deref())),
    ));
    let row0 = Line::from(row0_spans);

    // ── row 1: prompt > lastAssistantMsg > preview_tail (fallback 链) ──
    let prompt_raw = agent.prompt.as_deref().unwrap_or("").trim();
    let msg_raw = agent.last_assistant_msg.as_deref().unwrap_or("").trim();
    let pv_raw = preview_tail(agent.preview.as_deref());
    let (row1_text, row1_prefix) = if !prompt_raw.is_empty() {
        (prompt_raw.to_string(), "")
    } else if !msg_raw.is_empty() {
        (msg_raw.to_string(), "\u{21b3}") // ↳
    } else if !pv_raw.is_empty() {
        (pv_raw, "\u{2243}")               // ≃ (preview fallback 标记)
    } else {
        (String::new(), "")
    };
    let prefix_w = UnicodeWidthStr::width(row1_prefix);
    let row1_str = crate::render::truncate_width(&row1_text, avail.saturating_sub(indent + prefix_w));
    let row1_w = UnicodeWidthStr::width(row1_str.as_str());
    let row1 = Line::from(vec![
        Span::styled("▌", Style::default().fg(cs.bar_fg).bg(cs.bg)),
        Span::styled(" ", bg_style),
        Span::styled(row1_prefix, Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(row1_str, Style::default().fg(theme.fg).bg(cs.bg)),
        Span::styled(" ".repeat(avail.saturating_sub(indent + prefix_w + row1_w)), bg_style),
    ]);

    // ── row 2: 🔧toolName  toolInput截断 ──
    let tool = agent.tool_name.as_deref().unwrap_or("");
    let tool_label = if !tool.is_empty() {
        format!("\u{1f527}{}", tool)
    } else {
        String::new()
    };
    let tool_input = agent.tool_input.as_deref().unwrap_or("").trim();
    let input_part = if !tool_input.is_empty() {
        format!(" {}", tool_input)
    } else {
        String::new()
    };
    let tool_label_w = UnicodeWidthStr::width(tool_label.as_str());
    let input_max = avail.saturating_sub(indent + tool_label_w);
    let input_trunc = crate::render::truncate_width(&input_part, input_max);
    let input_w = UnicodeWidthStr::width(input_trunc.as_str());
    let row2 = Line::from(vec![
        Span::styled("▌", Style::default().fg(cs.bar_fg).bg(cs.bg)),
        Span::styled(" ", bg_style),
        Span::styled(tool_label, Style::default().fg(theme.accent).bg(cs.bg)),
        Span::styled(input_trunc, Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(
            " ".repeat(avail.saturating_sub(indent + tool_label_w + input_w)),
            bg_style,
        ),
    ]);

    // ── row 3: source · state (branch) ──
    let source = agent.source.as_deref().unwrap_or("-");
    let state = agent.state.as_deref().unwrap_or("-");
    let branch = if !agent.branch.is_empty() {
        format!(" ({})", agent.branch)
    } else {
        String::new()
    };
    let meta_str = crate::render::truncate_width(
        &format!("{} \u{00b7} {}{}", source, state, branch),
        avail.saturating_sub(indent),
    );
    let meta_w = UnicodeWidthStr::width(meta_str.as_str());
    let row3 = Line::from(vec![
        Span::styled("▌", Style::default().fg(cs.bar_fg).bg(cs.bg)),
        Span::styled(" ", bg_style),
        Span::styled(meta_str, Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(
            " ".repeat(avail.saturating_sub(indent + meta_w)),
            bg_style,
        ),
    ]);

    let content = ratatui::widgets::Paragraph::new(vec![row0, row1, row2, row3]);
    f.render_widget(content, area);
}

/// Groups tab: 群组列表 + 成员。
/// Enter 选中后弹出成员详情浮层; 也可通过命令面板 create/join/leave/broadcast。
fn draw_groups(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    let focused = matches!(shell.focus, crate::shell::FocusTarget::Groups);
    let block = blocks::bordered_block("Groups", focused, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if model.groups.is_empty() {
        let empty = Paragraph::new(vec![
            Line::from(Span::styled("(no groups)", Style::default().fg(theme.muted))),
            Line::from(Span::styled(
                "  i: input  group:<name> to create",
                Style::default().fg(theme.muted),
            )),
        ]);
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

    let list_height = inner.height.saturating_sub(1); // 底部留一行给 hint
    let list_area = Rect::new(inner.x, inner.y, inner.width, list_height);

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme.selection_bg)
            .fg(theme.selection_fg),
    );

    let mut state = ListState::default();
    if !names.is_empty() {
        state.select(Some(shell.cursor.min(names.len() - 1)));
    }

    f.render_stateful_widget(list, list_area, &mut state);

    // 底部提示行
    let hint_y = inner.y + list_height;
    if hint_y < inner.y + inner.height {
        let hint = Paragraph::new(Span::styled(
            " Enter:details  i:input  Ctrl-P:commands",
            Style::default().fg(theme.muted),
        ));
        f.render_widget(hint, Rect::new(inner.x, hint_y, inner.width, 1));
    }
}

/// Groups 成员详情浮层: 显示选中群组的全部成员列表。
fn draw_group_detail(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    let mut names: Vec<&String> = model.groups.keys().collect();
    names.sort();

    let group_name = match names.get(shell.cursor) {
        Some(n) => *n,
        None => return,
    };
    let members = match model.groups.get(group_name) {
        Some(m) => m,
        None => return,
    };

    // 浮层居中
    let overlay_w = (area.width.min(60)).max(30);
    let overlay_h = (members.len() as u16 + 4).min(area.height.saturating_sub(4)).max(5);
    let x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let overlay_area = Rect::new(x, y, overlay_w, overlay_h);

    // 半透明背景
    let bg = Block::default()
        .style(Style::default().bg(theme.bg));
    f.render_widget(bg, area);

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(format!(" Group: {} ", group_name));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // 成员列表
    let mut member_lines: Vec<Line> = Vec::new();
    member_lines.push(Line::from(Span::styled(
        format!(" {} members (j/k scroll, q close)", members.len()),
        Style::default().fg(theme.muted),
    )));
    member_lines.push(Line::from("")); // 分隔线

    let mut handles: Vec<&String> = members.iter().collect();
    handles.sort();

    for (i, handle) in handles.iter().enumerate() {
        let truncated = crate::render::truncate_width(handle, overlay_w.saturating_sub(4) as usize);
        // 查找 agent 对应的 title
        let title = model.directory.get(*handle)
            .and_then(|a| a.title.as_deref())
            .unwrap_or("?");
        let title_trunc = crate::render::truncate_width(title, 20);
        let is_self = std::env::var("ORCA_TERMINAL_HANDLE")
            .map(|h| h == **handle)
            .unwrap_or(false);

        let style = if is_self {
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };

        member_lines.push(Line::from(vec![
            Span::styled(format!(" {:>2}. ", i + 1), style),
            Span::styled(format!("{truncated} "), style),
            Span::styled(format!("({title_trunc})"), Style::default().fg(theme.muted)),
        ]));
    }

    // 渲染带滚动
    let needs_scroll = member_lines.len() as u16 > inner.height;
    let scroll_offset = if needs_scroll {
        shell.overlay_scroll.min(member_lines.len() - inner.height as usize)
    } else {
        0
    };
    let content = Paragraph::new(member_lines);
    if needs_scroll {
        f.render_widget(content.scroll((scroll_offset as u16, 0)), inner);
    } else {
        f.render_widget(content, inner);
    }
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
            Tab::Directory => "i:send  j/k:nav  Enter:select  s:switch  g:groups",
            Tab::Groups => "i:send  j/k:navigate  g:messages",
            Tab::Messages => "i:send  j/k:navigate  g:directory",
        };
        let para = Paragraph::new(Span::styled(hint, Style::default().fg(theme.muted)));
        f.render_widget(para, area);
    }
}
/// 状态栏: spinner + 连接状态 + 动态快捷键提示。
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

    // 右侧: 根据当前 tab + 模式显示不同快捷键提示
    let right_hint = status_hint(shell);
    // 用 pad 到右边
    let total_text_width: usize = spans.iter().map(|s| s.width()).sum();
    let space_left = (area.width as usize).saturating_sub(total_text_width + right_hint.len());
    spans.push(Span::raw(" ".repeat(space_left)));
    spans.push(Span::styled(right_hint, Style::default().fg(theme.muted)));

    let para = Paragraph::new(Line::from(spans));
    f.render_widget(para, area);
}

/// 根据当前 tab + 模式返回右侧快捷键提示。
fn status_hint(shell: &Shell) -> String {
    use crate::shell::Tab;
    if shell.cheatsheet_active
        || shell.overlay_content.is_some()
        || shell.worktree_ps_active
        || shell.group_detail_active
        || shell.config_overlay_active
    {
        return " Esc/q:close j/k:scroll ".to_string();
    }
    if shell.palette_active {
        return " Esc:close Enter:exec j/k:nav ".to_string();
    }
    if shell.filter_active {
        return " Esc:close Enter:confirm j/k:nav ?:help ".to_string();
    }
    if shell.insert_mode {
        return " Esc:cancel Enter:send ".to_string();
    }
    match shell.tab {
        Tab::Directory => " j/k:nav i:input s:switch p:pty w:worktree /:filter ?:help ".to_string(),
        Tab::Groups => " j/k:nav Enter:detail g:next Tab:switch ?:help ".to_string(),
        Tab::Messages => " j/k:nav g:next Tab:switch ?:help ".to_string(),
    }
}

/// spinner 字符集(简化版: 4 帧)。
fn spinner_char(frame: usize) -> char {
    const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[frame % FRAMES.len()]
}

// ───────────────────────── 命令面板浮层 ─────────────────────────

/// 命令面板: 居中浮层 + 输入框 + 过滤列表 + 选择高亮。
fn draw_command_palette(f: &mut Frame, shell: &Shell, area: Rect, theme: &Theme) {
    use unicode_width::UnicodeWidthStr;

    let commands = crate::command::filter_commands(&shell.palette_query);
    let list_h = commands.len().min(10) as u16;
    let palette_h = 1 + 1 + list_h + 1; // border(1) + input(1) + list + border(1)
    let palette_w = (area.width / 2).max(40).min(60);
    let palette_x = area.x + (area.width - palette_w) / 2;
    let palette_y = area.y + 2;

    // 半透明背景(用 Clear + 纯色覆盖)
    let bg_area = Rect {
        x: palette_x.saturating_sub(1),
        y: palette_y.saturating_sub(1),
        width: palette_w + 2,
        height: palette_h + 2,
    };
    f.render_widget(ratatui::widgets::Clear, bg_area);
    let bg = ratatui::widgets::Block::default()
        .style(Style::default().bg(theme.bg));
    f.render_widget(bg, bg_area);

    // border
    let border = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Command Palette (Esc: close) ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(Rect {
        x: palette_x.saturating_sub(1),
        y: palette_y.saturating_sub(1),
        width: palette_w + 2,
        height: palette_h + 2,
    });
    f.render_widget(border, Rect {
        x: palette_x.saturating_sub(1),
        y: palette_y.saturating_sub(1),
        width: palette_w + 2,
        height: palette_h + 2,
    });

    // 输入行: > query
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(&shell.palette_query, Style::default().fg(theme.fg)),
        Span::styled(" ", Style::default().fg(theme.fg)),
    ]);
    f.render_widget(
        Paragraph::new(input_line),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );

    // 命令列表
    for (i, cmd) in commands.iter().enumerate().take(10) {
        let selected = i == shell.palette_cursor;
        let style = if selected {
            Style::default().fg(theme.bg).bg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let line = Line::from(vec![
            Span::styled(format!("  {} ", cmd.name), style),
            Span::styled(
                crate::render::truncate_width(cmd.description, inner.width as usize - cmd.name.len() - 4),
                if selected { style } else { Style::default().fg(theme.muted) },
            ),
        ]);
        f.render_widget(
            Paragraph::new(line),
            Rect { x: inner.x, y: inner.y + 1 + i as u16, width: inner.width, height: 1 },
        );
    }
}

// ───────────────────────── 过滤指示器 ─────────────────────────

/// 过滤模式指示器: 替换 input_bar 显示过滤查询。
fn draw_filter_indicator(f: &mut Frame, shell: &Shell, area: Rect, theme: &Theme) {
    let query = shell.filter_query.as_deref().unwrap_or("");
    let line = Line::from(vec![
        Span::styled("/filter ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(query, Style::default().fg(theme.fg)),
        Span::styled(
            "  (Esc: close, Enter: confirm)",
            Style::default().fg(theme.muted),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// ───────────────────────── terminal read 浮层 ─────────────────────────

/// 浮层显示 terminal read 输出(可滚动)。
fn draw_output_overlay(f: &mut Frame, content: &str, shell: &Shell, area: Rect, theme: &Theme) {
    let overlay_h = area.height.saturating_sub(4).max(10);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;

    f.render_widget(ratatui::widgets::Clear, Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });

    let border = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Terminal Output (j/k: scroll, Esc: close) ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });
    f.render_widget(border, Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });

    let lines: Vec<&str> = content.lines().collect();
    let visible_h = inner.height as usize;
    let start = shell.overlay_scroll.min(lines.len().saturating_sub(visible_h));
    let visible: Vec<Line> = lines[start..start.min(lines.len()).min(start + visible_h)]
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::default().fg(theme.fg))))
        .collect();
    f.render_widget(
        ratatui::widgets::Paragraph::new(visible),
        inner,
    );
}

// ───────────────────────── worktree ps 浮层 ─────────────────────────

/// worktree ps 浮层: 显示跨 worktree 编排摘要。
fn draw_worktree_ps_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;

    f.render_widget(ratatui::widgets::Clear, Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });

    let border = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Worktree Ps (j/k: scroll, Esc: close) ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });
    f.render_widget(border, Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });

    if model.worktree_ps.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("(loading...)", Style::default().fg(theme.muted))),
            inner,
        );
        return;
    }

    let lines: Vec<Line> = model.worktree_ps.iter().map(|e| {
        let home = std::env::var("HOME").unwrap_or_default();
        let display = if !home.is_empty() && e.path.starts_with(&home) {
            format!("~{}", &e.path[home.len()..])
        } else if e.path.is_empty() {
            "(global)".to_string()
        } else {
            e.path.clone()
        };
        Line::from(vec![
            Span::styled(
                format!(" {} agents={}", display, e.agent_count),
                Style::default().fg(theme.fg),
            ),
            Span::styled(
                format!(" {}", e.branch),
                Style::default().fg(theme.muted),
            ),
        ])
    }).collect();

    let visible_h = inner.height as usize;
    let start = shell.overlay_scroll.min(lines.len().saturating_sub(visible_h));
    let visible: Vec<Line> = lines[start..start.min(lines.len()).min(start + visible_h)].to_vec();
    f.render_widget(ratatui::widgets::Paragraph::new(visible), inner);
}

// ───────────────────────── cheatsheet 浮层 ─────────────────────────

/// cheatsheet: 快捷键 + 命令面板命令 + 输入前缀的统一帮助浮层(? 键)。
/// 命令列表从 builtin_commands() 动态读取,不硬编码。
fn draw_cheatsheet(f: &mut Frame, shell: &Shell, area: Rect, theme: &Theme) {
    let commands = crate::command::builtin_commands();

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(""));

    // ── Section 1: Keybindings ──
    lines.push(Line::from(vec![
        Span::styled(" KEYBINDINGS", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
    ]));

    let keybindings: &[(&str, &str)] = &[
        ("j/k ↓↑       ", "Navigate list"),
        ("i             ", "Enter input mode"),
        ("g             ", "Cycle tabs (Dir→Groups→Msg)"),
        ("s             ", "Switch to selected agent"),
        ("p             ", "PTY inject to selected agent"),
        ("Enter         ", "Select agent / send message"),
        ("Tab           ", "Switch tab"),
        ("/             ", "Filter agents (Directory)"),
        ("w             ", "Worktree overview"),
        ("Ctrl-P / :    ", "Command palette"),
        ("Ctrl-d / u    ", "Scroll half page"),
        ("q / Ctrl-C    ", "Quit"),
        ("Esc           ", "Exit insert mode / cancel"),
        ("?             ", "Toggle this help"),
    ];
    for (key, desc) in keybindings {
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", key), Style::default().fg(theme.accent)),
            Span::styled(*desc, Style::default().fg(theme.fg)),
        ]));
    }

    lines.push(Line::from(""));

    // ── Section 2: Commands (from builtin_commands()) ──
    lines.push(Line::from(vec![
        Span::styled(" COMMANDS (Ctrl-P)", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
    ]));

    for cmd in &commands {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:20}", cmd.name),
                Style::default().fg(theme.fg),
            ),
            Span::styled(cmd.description, Style::default().fg(theme.muted)),
        ]));
    }

    lines.push(Line::from(""));

    // ── Section 3: Input Prefixes ──
    lines.push(Line::from(vec![
        Span::styled(" INPUT PREFIXES", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
    ]));

    let prefixes: &[(&str, &str)] = &[
        ("to:handle msg     ", "Send orchestration message"),
        ("pty:handle txt    ", "Inject text to agent PTY"),
        ("rename:h name     ", "Rename agent terminal"),
        ("group:name        ", "Create group and join"),
        ("join:name         ", "Join existing group"),
        ("leave:name        ", "Leave group"),
        ("broadcast:g msg   ", "Broadcast to group"),
        ("create:cmd        ", "Create terminal in worktree"),
        ("config:k=v        ", "Set configuration (refresh/theme/filter)"),
    ];
    for (key, desc) in prefixes {
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", key), Style::default().fg(theme.accent)),
            Span::styled(*desc, Style::default().fg(theme.fg)),
        ]));
    }

    lines.push(Line::from(""));

    // ── Layout: centered overlay, scrollable ──
    let content_w: usize = lines.iter().map(|l| l.width()).max().unwrap_or(40);
    let max_w = area.width.saturating_sub(4) as usize;
    let display_w = content_w.min(max_w).max(40);
    let overlay_w = (display_w + 4) as u16; // +4: border(2) + padding(2)
    let max_h = area.height.saturating_sub(4);
    let content_h = lines.len() as u16;
    let overlay_h = content_h.min(max_h).max(10);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + 2;

    let bg_rect = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };
    f.render_widget(ratatui::widgets::Clear, bg_rect);

    let border = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Quick Reference (Esc/q: close) ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(bg_rect);
    f.render_widget(border, bg_rect);

    let visible_h = inner.height as usize;
    if visible_h == 0 {
        return;
    }
    let start = shell.overlay_scroll.min(lines.len().saturating_sub(visible_h));
    let visible: Vec<Line> = lines[start..start.min(lines.len()).min(start + visible_h)].to_vec();
    f.render_widget(ratatui::widgets::Paragraph::new(visible), inner);
}

// ───────────────────────── config overlay ─────────────────────────

/// config overlay: 显示所有已知配置项 + 当前值。
/// 已知配置项: refresh_interval_ms, theme, default_filter。
/// 未在 model.config 中的项显示默认值。
fn draw_config_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Paragraph, Clear};
    use ratatui::style::Modifier;

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(" CONFIGURATION", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));

    // 已知配置项(带描述和默认值)
    let known: &[(&str, &str, &str)] = &[
        ("refresh_interval_ms", "Agent refresh interval (ms)", "5000"),
        ("theme", "UI color theme (dark/default)", "default"),
        ("default_filter", "Default directory filter query", ""),
    ];

    for (key, desc, default) in known {
        let value = model.config.get(*key).map(|s| s.as_str()).unwrap_or(*default);
        let is_custom = model.config.contains_key(*key);
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", key), Style::default().fg(theme.accent)),
            Span::styled(
                format!("= {} ", value),
                if is_custom {
                    Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted)
                },
            ),
            Span::styled(format!("({})", if is_custom { "custom" } else { "default" }), Style::default().fg(theme.muted)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("    {}", desc), Style::default().fg(theme.muted)),
        ]));
        lines.push(Line::from(""));
    }

    // 显示未知配置项(用户自定义的)
    let known_keys: std::collections::HashSet<&str> = known.iter().map(|(k, _, _)| *k).collect();
    let custom_keys: Vec<&str> = model.config.keys().filter(|k| !known_keys.contains(k.as_str())).map(|s| s.as_str()).collect();
    if !custom_keys.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" CUSTOM", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(""));
        for key in custom_keys {
            if let Some(value) = model.config.get(key) {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {} ", key), Style::default().fg(theme.accent)),
                    Span::styled(format!("= {}", value), Style::default().fg(theme.fg)),
                ]));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  config:key=value", Style::default().fg(theme.accent)),
        Span::styled(" to change (e.g. config:refresh_interval_ms=3000)", Style::default().fg(theme.muted)),
    ]));

    // Layout: centered overlay
    let content_w: usize = lines.iter().map(|l| l.width()).max().unwrap_or(40);
    let max_w = area.width.saturating_sub(4) as usize;
    let display_w = content_w.min(max_w).max(40);
    let overlay_w = (display_w + 4) as u16;
    let max_h = area.height.saturating_sub(4);
    let content_h = lines.len() as u16;
    let overlay_h = content_h.min(max_h).max(8);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + 2;

    let bg_rect = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };
    f.render_widget(Clear, bg_rect);

    let border = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Config (Esc/q: close) ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(bg_rect);
    f.render_widget(border, bg_rect);

    let visible_h = inner.height as usize;
    if visible_h == 0 {
        return;
    }
    let start = shell.overlay_scroll.min(lines.len().saturating_sub(visible_h));
    let visible: Vec<Line> = lines[start..start.min(lines.len()).min(start + visible_h)].to_vec();
    f.render_widget(Paragraph::new(visible), inner);
}

// ───────────────────────── 编排任务浮层 ─────────────────────────

/// 编排任务浮层: Runs / Tasks / Gates 三段统一视图。
fn draw_orch_tasks_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    let overlay_h = area.height.saturating_sub(4).max(10);
    let overlay_w = area.width.saturating_sub(4).max(50);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;

    f.render_widget(ratatui::widgets::Clear, Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });
    let border = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Orchestration (j/k: scroll, Esc: close) ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });
    f.render_widget(border, Rect {
        x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h,
    });

    let snap = match &model.orch_snapshot {
        Some(s) => s,
        None => {
            f.render_widget(
                Paragraph::new(Span::styled("(loading...)", Style::default().fg(theme.muted))),
                inner,
            );
            return;
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    // Runs 段
    lines.push(Line::from(Span::styled(
        format!(" Runs ({})", snap.runs.len()),
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    for r in &snap.runs {
        let color = status_color(&r.status, theme);
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", status_icon(&r.status)), Style::default().fg(color)),
            Span::styled(&r.title, Style::default().fg(theme.fg)),
            Span::styled(format!(" ({})", r.status), Style::default().fg(theme.muted)),
        ]));
    }
    lines.push(Line::from(""));

    // Tasks 段
    lines.push(Line::from(Span::styled(
        format!(" Tasks ({})", snap.tasks.len()),
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    for t in &snap.tasks {
        let color = status_color(&t.status, theme);
        let assignee = t.assignee.as_deref().unwrap_or("-");
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", status_icon(&t.status)), Style::default().fg(color)),
            Span::styled(&t.title, Style::default().fg(theme.fg)),
            Span::styled(format!(" \u{2192} {}", assignee), Style::default().fg(theme.muted)),
        ]));
    }
    lines.push(Line::from(""));

    // Gates 段
    lines.push(Line::from(Span::styled(
        format!(" Gates ({})", snap.gates.len()),
        Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    for g in &snap.gates {
        let color = status_color(&g.status, theme);
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", status_icon(&g.status)), Style::default().fg(color)),
            Span::styled(&g.title, Style::default().fg(theme.fg)),
        ]));
    }

    let visible_h = inner.height as usize;
    let start = shell.overlay_scroll.min(lines.len().saturating_sub(visible_h));
    let visible: Vec<Line> = lines[start..start.min(lines.len()).min(start + visible_h)].to_vec();
    f.render_widget(Paragraph::new(visible), inner);
}

/// 状态 → 图标。
fn status_icon(status: &str) -> &'static str {
    match status.to_ascii_lowercase().as_str() {
        s if s.contains("run") || s.contains("work") || s.contains("busy") => "\u{23f3}", // ⏳
        s if s.contains("done") || s.contains("ok") || s.contains("complete") => "\u{2713}", // ✓
        s if s.contains("block") || s.contains("wait") || s.contains("pending") => "\u{23f4}", // ⏴
        s if s.contains("fail") || s.contains("error") => "\u{2717}", // ✗
        _ => "\u{00b7}", // ·
    }
}

/// 状态 → 颜色。
fn status_color(status: &str, theme: &Theme) -> Color {
    match status.to_ascii_lowercase().as_str() {
        s if s.contains("run") || s.contains("work") || s.contains("busy") => theme.working,
        s if s.contains("done") || s.contains("ok") => theme.idle,
        s if s.contains("block") || s.contains("fail") => theme.error,
        _ => theme.muted,
    }
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
