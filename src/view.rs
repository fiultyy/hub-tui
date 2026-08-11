//! view.rs —— 范式 5 纯渲染层(immediate-mode, draw 读 &Model + &Shell,绝不 &mut)。
//!
//! 布局(从上到下):
//! - TabBar(1 行): Directory / Messages
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

use crate::model::{directory_sorted_with_mode, AgentMetrics, EventCategory, EventSeverity, Model, StatusCategory};
use crate::render::blocks;
use crate::render::theme::Theme;
use crate::shell::{ConnState, Shell, Tab};

/// 主 draw 入口。immediate-mode: 不 &mut Model/Shell。
pub fn draw(f: &mut Frame, model: &Model, shell: &Shell) {
    let theme = Theme::from_name_with_overrides(&shell.theme_name, &model.config);
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
    // Autocomplete dropdown (insert mode + active): floats above input bar
    if shell.insert_mode && shell.autocomplete_active {
        draw_autocomplete_dropdown(f, model, shell, outer[2], &theme);
    }
    draw_status_bar(f, shell, outer[3], &theme);


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

    // 活动日志浮层(a 键激活)
    if shell.activity_active {
        draw_activity_overlay(f, model, shell, area, &theme);
    }
    // 命令历史浮层(H 键激活)
    if shell.history_overlay_active {
        draw_history_overlay(f, model, shell, area, &theme);
    }
    // 全局搜索浮层(Ctrl-S 激活)
    if shell.search_active {
        draw_search_overlay(f, model, shell, area, &theme);
    }
    // Dashboard 浮层(D 键激活)
    if shell.dashboard_active {
        draw_dashboard_overlay(f, model, shell, area, &theme);
    }
    // Snippet library 浮层(S 键激活)
    if shell.snippet_overlay_active {
        draw_snippet_overlay(f, model, shell, area, &theme);
    }
    // Alert Rules 浮层
    if shell.rule_overlay_active {
        draw_rule_overlay(f, model, shell, area, &theme);
    }
    // Macro 浮层
    if shell.macro_overlay_active {
        draw_macro_overlay(f, model, shell, area, &theme);
    }
    // Saved Views 浮层
    if shell.views_overlay_active {
        draw_views_overlay(f, model, shell, area, &theme);
    }
    if shell.metrics_overlay_active {
        draw_metrics_overlay(f, model, shell, area, &theme);
    }
    // Agent Note 浮层
    if shell.note_overlay_active {
        draw_note_overlay(f, model, shell, area, &theme);
    }
    // Quick Actions 浮层(o 键激活)
    if shell.quick_actions_active {
        draw_quick_actions_overlay(f, model, shell, area, &theme);
    }
    // Aliases 浮层
    if shell.alias_overlay_active {
        draw_alias_overlay(f, model, shell, area, &theme);
    }
    // Hotkeys 浮层
    if shell.hotkeys_overlay_active {
        draw_hotkeys_overlay(f, model, shell, area, &theme);
    }

    // Theme 定制浮层
    if shell.theme_overlay_active {
        draw_theme_overlay(f, model, shell, area, &theme);
    }

    // Templates 浮层
    if shell.template_overlay_active {
        draw_template_overlay(f, model, shell, area, &theme);
    }

    // Scheduler 浮层
    if shell.sched_overlay_active {
        draw_sched_overlay(f, model, shell, area, &theme);
    }

    if shell.quickswitch_active {
        draw_quickswitch_overlay(f, model, shell, area, &theme);
    }

    // Group wiring 浮层(G 键激活)
    if shell.group_overlay_active {
        draw_group_overlay(f, model, shell, area, &theme);
    }

    // Toast 层(浮在底部上方, 渲染在所有 overlay 之后以保持可见)
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

/// 顶部 TabBar: 2 tab, 当前高亮 + 数字标号 + 连接灯。
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
    }
}

// ───────────────────────── 布局常量 + 类型(draw + hit_test 共享) ─────────────────────────

/// 卡片宽度(字符)。
pub const CARD_W: u16 = 36;
/// 卡片高度(行): identity(1) + recap(5) + tool(1) + status(1) + bottom_pad(1)。
pub const CARD_H: u16 = 9;
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

/// 统一 scroll_y 计算: draw_directory 和 hit_test_card 共用, 保证点击坐标一致。
pub fn compute_scroll_y(shell: &crate::shell::Shell, layout: &[LayoutEntry], visible_h: u16) -> u16 {
    match shell.manual_scroll {
        Some(offset) => offset.min(layout.iter().map(|e| e.y + e.h).max().unwrap_or(0)),
        None => directory_scroll(shell.cursor, layout, visible_h),
    }
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
        let full = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
        crate::model::directory_filter_handles(&full, &model.directory, q, &model.tags)
    } else {
        directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned)
    };
    let sorted = crate::model::apply_focus_filter(sorted, shell.focus_mode, &shell.selected_set);
    let layout = directory_layout(&sorted, model, inner.x, inner.width);
    let scroll_y = compute_scroll_y(shell, &layout, inner.height);

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
                    let max_h = (scroll_y + inner.height).saturating_sub(entry.y);
                    let card_area = Rect { x: entry.x, y: adj_y, width: entry.w, height: entry.h.min(max_h) };
                    let unread: usize = 0;
                    let tags: Vec<String> = model.tags.get(&agent.handle).map(|s| {
                        let mut v: Vec<String> = s.iter().cloned().collect();
                        v.sort();
                        v
                    }).unwrap_or_default();
                    draw_agent_card(f, agent, card_area, theme, is_selected, false, model.pinned.contains(&agent.handle), &tags, unread, model.notes.contains_key(&agent.handle), model.watched.contains(&agent.handle), shell.spinner_frame);
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

/// Extract real cwd from agent data. Sources tried in order:
/// 1. PTY preview: scan for "cwd=" or "cwd:" pattern (from shell echoes)
/// 2. Title: shell prompt pattern "yy@host: ~/path"
/// 3. worktreePath (agent.cwd) — Orca's aggregated field, always present
fn extract_real_cwd(agent: &crate::model::Agent) -> String {
    // 1. Parse preview for cwd= or cwd: patterns
    if let Some(preview) = agent.preview.as_deref() {
        for line in preview.lines().rev().take(10) {
            // Match patterns: cwd=/path/to, cwd:/path/to, cwd=/home/...
            if let Some(idx) = line.find("cwd=") {
                let rest = &line[idx + 4..];
                let path = rest.split_whitespace().next().unwrap_or("").trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '~' && c != '.' && c != '-');
                if path.starts_with('/') || path.starts_with('~') {
                    return expand_tilde(path);
                }
            }
            if let Some(idx) = line.find("cwd:") {
                let rest = line[idx + 4..].trim_start();
                let path = rest.split_whitespace().next().unwrap_or("");
                if path.starts_with('/') || path.starts_with('~') {
                    return expand_tilde(path);
                }
            }
        }
    }
    // 2. Shell terminals have title like "yy@host: ~/path"
    if let Some(title) = agent.title.as_deref() {
        if let Some(idx) = title.rfind(": ") {
            let path_part = &title[idx + 2..].trim();
            if !path_part.is_empty() && (path_part.starts_with('~') || path_part.starts_with('/')) {
                return expand_tilde(path_part);
            }
        }
    }
    // 3. Fall back to worktreePath
    agent.cwd.clone()
}

/// Expand ~ to $HOME for display.
fn expand_tilde(path: &str) -> String {
    if path.starts_with("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            return format!("{}{}", home, &path[1..]);
        }
    } else if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
    }
    path.to_string()
}

/// Tail-anchor a path: keep the last N segments, prefix with … if truncated.
fn cwd_tail(path: &str, max_w: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return String::new();
    }
    for &keep in &[3usize, 2, 1] {
        if segments.len() <= keep {
            let full = segments.join("/");
            if UnicodeWidthStr::width(full.as_str()) <= max_w {
                return full;
            }
            break;
        }
        let tail: String = segments[segments.len() - keep..].join("/");
        let display = format!("\u{2026}/{}", tail); // …/
        if UnicodeWidthStr::width(display.as_str()) <= max_w {
            return display;
        }
    }
    crate::render::truncate_width(path, max_w)
}

/// Word-wrap text to fit within max_w display columns (CJK-aware).
fn word_wrap(text: &str, max_w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    if max_w == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            if UnicodeWidthStr::width(candidate.as_str()) <= max_w {
                current = candidate;
            } else {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                // Hard-break long words
                let mut remainder = word.to_string();
                while UnicodeWidthStr::width(remainder.as_str()) > max_w {
                    let mut cut = 0usize;
                    let mut w = 0usize;
                    for (i, ch) in remainder.char_indices() {
                        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                        if w + cw > max_w { break; }
                        w += cw;
                        cut = i + ch.len_utf8();
                    }
                    if cut == 0 { break; }
                    lines.push(remainder[..cut].to_string());
                    remainder = remainder[cut..].to_string();
                }
                current = remainder;
            }
        }
        lines.push(current);
    }
    if lines.is_empty() { lines.push(String::new()); }
    lines
}
/// 渲染单个 agent card(8 行布局)。
///
/// 布局:
/// ```text
/// π title (N) [tag] 📝 👁              ← row 0: source图标+title+badges
/// ▌ recap line 1 (last_assistant_msg)  ← row 1: recap body (wrapped)
/// ▌ recap line 2                       ← row 2
/// ▌ recap line 3                       ← row 3
/// ▌ recap line 4                       ← row 4
/// ▌ recap line 5                       ← row 5
/// ▌ 🔧tool  toolInput截断              ← row 6: 工具+动作
/// ▌ ⠋ Working 2s · …/projects/hub-tui  ← row 7: status line
/// ```
fn draw_agent_card(
    f: &mut Frame,
    agent: &crate::model::Agent,
    area: Rect,
    theme: &Theme,
    selected: bool,
    shell_selected: bool,
    pinned: bool,
    tags: &[String],
    unread: usize,
    has_note: bool,
    watched: bool,
    spinner_frame: usize,
) {
    use unicode_width::UnicodeWidthStr;

    let cs = CardStyle::compute(agent, selected, theme);
    let bg_style = Style::default().bg(cs.bg);
    let avail = area.width as usize;
    let indent = 2usize; // 竖条(1) + gap(1)

    f.render_widget(ratatui::widgets::Clear, area);

    // ── row 0: termID tag only ──
    let id_tag = handle_tag(&agent.handle);
    let id_tag_str = format!(" {} ", id_tag);
    let id_tag_w = UnicodeWidthStr::width(id_tag_str.as_str());
    let msg_btn = " \u{279c}"; // ➤ (2 cols)
    let msg_btn_w = unicode_width::UnicodeWidthStr::width(msg_btn);
    let mid_pad = avail.saturating_sub(id_tag_w).saturating_sub(msg_btn_w + 1);
    let row0 = Line::from(vec![
        Span::styled(id_tag_str, Style::default().bg(cs.tag_bg).fg(theme.bg).add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(mid_pad), bg_style),
        Span::styled(msg_btn.to_string(), Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(" ".to_string(), bg_style),
    ]);

    // ── rows 1-3: recap body (last_assistant_msg > prompt > preview_tail) ──
    let msg_raw = agent.last_assistant_msg.as_deref().unwrap_or("").trim();
    let prompt_raw = agent.prompt.as_deref().unwrap_or("").trim();
    let pv_raw = preview_tail(agent.preview.as_deref());
    let recap_text = if !msg_raw.is_empty() {
        msg_raw.to_string()
    } else if !prompt_raw.is_empty() {
        prompt_raw.to_string()
    } else if !pv_raw.is_empty() {
        pv_raw
    } else {
        String::new()
    };
    let body_h = 5usize;
    let chars_per_line = avail.saturating_sub(indent);
    let wrapped = if recap_text.is_empty() {
        Vec::new()
    } else {
        word_wrap(&recap_text, chars_per_line)
    };
    let recap_fg = Style::default().fg(theme.fg).bg(cs.bg);
    let bar_span = Span::styled("\u{258c}", Style::default().fg(cs.bar_fg).bg(cs.bg)); // ▌
    let gap_span = Span::styled(" ", bg_style);
    let mut recap_lines: Vec<Line> = Vec::with_capacity(body_h);
    for i in 0..body_h {
        if i < wrapped.len() {
            let wl = &wrapped[i];
            let wl_w = UnicodeWidthStr::width(wl.as_str());
            let pad = chars_per_line.saturating_sub(wl_w);
            recap_lines.push(Line::from(vec![
                bar_span.clone(),
                gap_span.clone(),
                Span::styled(wl.clone(), recap_fg),
                Span::styled(" ".repeat(pad), bg_style),
            ]));
        } else {
            recap_lines.push(Line::from(vec![
                bar_span.clone(),
                Span::styled(" ".repeat(avail.saturating_sub(1)), bg_style),
            ]));
        }
    }

    // ── row 4: 🔧toolName  toolInput截断 ──
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
    let tool_row = Line::from(vec![
        bar_span.clone(),
        gap_span.clone(),
        Span::styled(tool_label, Style::default().fg(theme.accent).bg(cs.bg)),
        Span::styled(input_trunc, Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(" ".repeat(avail.saturating_sub(indent + tool_label_w + input_w)), bg_style),
    ]);

    // ── row 7: status line (spinner/icon + label + 响应:elapsed · cwd_tail) ──
    let cat = StatusCategory::from_agent(agent);
    let status_label = cat.label();
    let elapsed = format_elapsed(agent.last_output_at);
    let state_color = theme.state_color(agent.state.as_deref());
    let elapsed_display = if elapsed.is_empty() { "-".to_string() } else { elapsed };
    let elapsed_str = format!("last:{}", elapsed_display); // last:Ns
    let sep = " \u{00b7} "; // ·
    // Animated spinner for Working state, static icon otherwise
    let icon_str: String = if cat == StatusCategory::Working {
        spinner_char(spinner_frame).to_string()
    } else {
        cat.icon().to_string()
    };
    let icon_w = UnicodeWidthStr::width(icon_str.as_str());
    let label_w = UnicodeWidthStr::width(status_label);
    let elapsed_w = UnicodeWidthStr::width(elapsed_str.as_str());
    let sep_w = UnicodeWidthStr::width(sep);
    let fixed_w = indent + icon_w + 1 + label_w + 1 + elapsed_w + sep_w;
    let cwd_max = avail.saturating_sub(fixed_w);
    let real_cwd = extract_real_cwd(agent);
    let cwd_display = cwd_tail(&real_cwd, cwd_max);
    let cwd_w = UnicodeWidthStr::width(cwd_display.as_str());
    let status_row = Line::from(vec![
        bar_span,
        gap_span,
        Span::styled(icon_str, Style::default().fg(state_color).bg(cs.bg)),
        Span::styled(" ", bg_style),
        Span::styled(status_label, Style::default().fg(state_color).bg(cs.bg)),
        Span::styled(" ", bg_style),
        Span::styled(elapsed_str, Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(sep, Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(cwd_display, Style::default().fg(theme.muted).bg(cs.bg)),
        Span::styled(" ".repeat(avail.saturating_sub(fixed_w + cwd_w)), bg_style),
    ]);

    let bottom_row = Line::from(vec![
        Span::styled(" ".repeat(avail), bg_style),
    ]);

    let content = ratatui::widgets::Paragraph::new(vec![row0]
        .into_iter()
        .chain(recap_lines)
        .chain(std::iter::once(tool_row))
        .chain(std::iter::once(status_row))
        .chain(std::iter::once(bottom_row))
        .collect::<Vec<_>>());
    f.render_widget(content, area);
}


/// 输入栏: insert_mode 时显示 input_buf + 光标。
fn draw_input_bar(f: &mut Frame, shell: &Shell, area: Rect, theme: &Theme) {
    if shell.insert_mode {
        let input_display = if shell.input_buf.is_empty() {
            Span::styled("Type message (Enter send, Tab complete, Esc cancel)", Style::default().fg(theme.muted))
        } else {
            Span::styled(shell.input_buf.as_str(), Style::default().fg(theme.fg))
        };
        let recall = shell.history_cursor.is_some();
        let prompt = if recall { " ↑>" } else { " >" };
        let prompt_span = Span::styled(
            prompt.to_string(),
            Style::default().fg(if recall { theme.warn } else { theme.accent }),
        );
        let para = Paragraph::new(Line::from(vec![
            prompt_span,
            input_display,
        ]));
        f.render_widget(para, area);

        // 光标定位(buf 末尾 + prompt offset)
        use unicode_width::UnicodeWidthStr;
        let cursor_offset = prompt.len() as u16;
        let cursor_x = (UnicodeWidthStr::width(shell.input_buf.as_str()) as u16 + cursor_offset).min(area.width.saturating_sub(1));
        f.set_cursor_position((area.x + cursor_x, area.y));
    } else {
        // 非 insert_mode: 提示
        let hint = match shell.tab {
            Tab::Directory => "i:send  j/k:nav  Enter:select  s:switch  G:group",
        };
        let para = Paragraph::new(Span::styled(hint, Style::default().fg(theme.muted)));
        f.render_widget(para, area);
    }
}

/// Autocomplete dropdown: Tab-completion suggestions floating above input bar.
fn draw_autocomplete_dropdown(f: &mut Frame, model: &Model, shell: &Shell, input_area: Rect, theme: &Theme) {
    use ratatui::widgets::{Clear, Borders};

    let suggestions = crate::command::autocomplete_suggestions(&shell.input_buf, model);
    if suggestions.is_empty() {
        return;
    }

    let visible = suggestions.len().min(6) as u16;
    let dropdown_area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(visible),
        width: input_area.width,
        height: visible,
    };

    f.render_widget(Clear, dropdown_area);

    let items: Vec<ListItem> = suggestions
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, s)| {
            let style = if i == shell.autocomplete_cursor {
                Style::default().fg(theme.selection_fg).bg(theme.selection_bg)
            } else {
                Style::default().fg(theme.fg)
            };
            ListItem::new(Line::from(Span::styled(format!(" {}", s.label), style)))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border_focus))
            .style(Style::default().bg(theme.bg)),
    );
    f.render_widget(list, dropdown_area);
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
    // Recording indicator
    if shell.recording_active {
        spans.push(Span::styled(
            format!(" ●REC {} ", shell.recording_name),
            Style::default().fg(theme.error).add_modifier(Modifier::BOLD),
        ));
    }

    // Replay indicator
    if !shell.replay_queue.is_empty() {
        spans.push(Span::styled(
            format!(" ▶PLAY ({} left) ", shell.replay_queue.len()),
            Style::default().fg(theme.success),
        ));
    }

    // Focus mode indicator
    if shell.focus_mode {
        spans.push(Span::styled(
            format!(" ◉FOCUS({}) ", shell.selected_set.len()),
            Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        ));
    }

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
        || shell.rule_overlay_active
        || shell.orch_tasks_active
        || shell.history_overlay_active
        || shell.activity_active
        || shell.dashboard_active
        || shell.snippet_overlay_active
    {
        return " Esc/q:close j/k:scroll c:clear ".to_string();
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
        Tab::Directory => " j/k:nav i:input s:switch p:pty w:worktree G:group @:pin /:filter ?:help ".to_string(),
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
// ───────────────────────── 全局搜索浮层 ─────────────────────────

/// 全局搜索浮层: 输入查询 + 分类结果列表。
fn draw_search_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    let results = crate::model::global_search(model, &shell.search_query);
    let list_h = results.len().min(15) as u16;
    let overlay_h = 1 + 1 + list_h + 1; // border + input + list + border
    let overlay_w = (area.width * 3 / 4).max(50).min(70);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " Global Search (Esc:close Enter:jump j/k:nav) ",
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // 输入行
    let input_line = Line::from(vec![
        Span::styled("\u{1f50d} ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(&shell.search_query, Style::default().fg(theme.fg)),
    ]);
    f.render_widget(Paragraph::new(input_line), Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 });

    // 结果列表(分类标题 + 结果行)
    let mut y = inner.y + 1;
    let mut prev_cat: Option<&crate::model::SearchCategory> = None;
    for (i, result) in results.iter().enumerate() {
        if y >= inner.y + inner.height { break; }
        // 分类标题(首次出现时显示)
        if prev_cat != Some(&result.category) {
            let header = Line::from(vec![
                Span::styled(
                    format!(" {} ", result.category.label()),
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                ),
            ]);
            f.render_widget(Paragraph::new(header), Rect { x: inner.x, y, width: inner.width, height: 1 });
            y += 1;
            prev_cat = Some(&result.category);
        }
        if y >= inner.y + inner.height { break; }

        let selected = i == shell.search_cursor;
        let style = if selected {
            Style::default().fg(theme.selection_fg).bg(theme.selection_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let secondary_style = if selected { style } else { Style::default().fg(theme.muted) };

        let line = Line::from(vec![
            Span::styled(format!("  {} ", result.primary), style),
            Span::styled(
                crate::render::truncate_width(&result.secondary, (inner.width as usize).saturating_sub(result.primary.len() + 4)),
                secondary_style,
            ),
        ]);
        f.render_widget(Paragraph::new(line), Rect { x: inner.x, y, width: inner.width, height: 1 });
        y += 1;
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
        ("Tab           ", "Switch tab / Autocomplete (insert)"),
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
        ("refresh_interval_ms", "Agent refresh interval (ms)", "1500"),
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

/// epoch 毫秒 → HH:MM:SS (UTC)。
fn format_hms(ts_ms: i64) -> String {
    let secs = (ts_ms / 1000).max(0) as u64;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// 活动日志浮层: 最新事件在前, severity 着色, 可滚动。
fn draw_activity_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    // 过滤事件
    let filtered_events: Vec<&crate::model::Event> = model.events.iter().rev().filter(|e| {
        let cat_ok = shell.activity_filter_categories.is_empty() || !shell.activity_filter_categories.contains(&e.category);
        let sev_ok = shell.activity_filter_severity.is_empty() || !shell.activity_filter_severity.contains(&e.severity);
        cat_ok && sev_ok
    }).collect();

    // 构建行(最新在前)
    let mut lines: Vec<Line> = filtered_events.iter().map(|e| {
        let ts = format_hms(e.timestamp_ms);
        let sev_color = match e.severity {
            EventSeverity::Error => theme.error,
            EventSeverity::Warn => theme.warn,
            EventSeverity::Info => theme.muted,
        };
        let src: String = if e.source.len() > 16 {
            format!("{}…", &e.source[..16])
        } else {
            e.source.clone()
        };
        Line::from(vec![
            Span::styled(format!(" {ts} "), Style::default().fg(theme.muted)),
            Span::styled(format!("{} ", e.severity.icon()), Style::default().fg(sev_color)),
            Span::styled(format!("{} ", e.category.icon()), Style::default().fg(theme.accent)),
            Span::styled(format!("{src:<16} "), Style::default().fg(theme.muted)),
            Span::styled(e.text.clone(), Style::default().fg(theme.fg)),
        ])
    }).collect();

    // Filter status header
    let has_filters = !shell.activity_filter_categories.is_empty() || !shell.activity_filter_severity.is_empty();
    if has_filters {
        lines.insert(0, Line::from(Span::styled(
            format!(" ⚡ Filtered: {} of {} events shown (0:reset)", filtered_events.len(), model.events.len()),
            Style::default().fg(theme.warn),
        )));
        lines.insert(1, Line::from(""));
    }

    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(" (no events)", Style::default().fg(theme.muted)))]
    } else {
        lines
    };

    // 尺寸 + 居中(同 worktree_ps_overlay)
    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };

    f.render_widget(Clear, overlay_area);

    let title = " Activity Log (1-5:cat 6-8:sev 0:reset c:clear Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // 滚动裁剪
    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    let visible: Vec<Line> = lines[start..end].to_vec();
    f.render_widget(Paragraph::new(visible), inner);
}
/// Dashboard 浮层: 聚合统计视图, 可滚动。
fn draw_dashboard_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};
    let snap = crate::model::compute_snapshot(model);
    let mut lines: Vec<Line> = Vec::new();

    // 📊 Agents section
    lines.push(Line::from(Span::styled("📊 Agents", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
    let mut status_spans: Vec<Span> = vec![Span::styled(format!("  Total: {}  ", snap.agent_total), Style::default().fg(theme.fg))];
    for (cat, count) in &snap.status_counts {
        let color = match cat {
            crate::model::StatusCategory::Working => theme.working,
            crate::model::StatusCategory::Error => theme.error,
            crate::model::StatusCategory::Done => theme.idle,
            _ => theme.muted,
        };
        status_spans.push(Span::styled(format!("{} {}: {}  ", cat.icon(), cat.label(), count), Style::default().fg(color)));
    }
    lines.push(Line::from(status_spans));
    lines.push(Line::from(""));

    // 🏷 Sources section
    lines.push(Line::from(Span::styled("🏷 Sources", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
    let mut sorted_sources: Vec<_> = snap.source_counts.iter().collect();
    sorted_sources.sort_by(|a, b| b.1.cmp(a.1));
    let src_line = if sorted_sources.is_empty() {
        Line::from(Span::styled("  (none)", Style::default().fg(theme.muted)))
    } else {
        Line::from(sorted_sources.iter().map(|(src, cnt)| {
            Span::styled(format!("  {}: {} ", src, cnt), Style::default().fg(theme.fg))
        }).collect::<Vec<_>>())
    };
    lines.push(src_line);
    lines.push(Line::from(""));

    // ✉ Messages section
    lines.push(Line::from(Span::styled("✉ Messages", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(vec![
        Span::styled(format!("  Total: {}  ", snap.message_total), Style::default().fg(theme.fg)),
        Span::styled(format!("Unread: {}", snap.message_unread), Style::default().fg(if snap.message_unread > 0 { theme.warn } else { theme.muted })),
    ]));
    lines.push(Line::from(""));

    // ⚡ Events section
    lines.push(Line::from(Span::styled("⚡ Events", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(vec![
        Span::styled(format!("  Total: {}  ", snap.event_total), Style::default().fg(theme.fg)),
        Span::styled(format!("· Info: {}  ", snap.event_by_severity[0].1), Style::default().fg(theme.muted)),
        Span::styled(format!("⚠ Warn: {}  ", snap.event_by_severity[1].1), Style::default().fg(theme.warn)),
        Span::styled(format!("✖ Error: {}", snap.event_by_severity[2].1), Style::default().fg(theme.error)),
    ]));
    lines.push(Line::from(Span::styled(format!("  Rate: {} events/60s", snap.event_recent_60s), Style::default().fg(theme.muted))));
    lines.push(Line::from(""));
    // 🏷 Tags section
    if !snap.tag_counts.is_empty() {
        lines.push(Line::from(Span::styled("🏷 Tags", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
        let tag_line = Line::from(snap.tag_counts.iter().map(|(tag, cnt)| {
            Span::styled(format!("  {}: {} ", tag, cnt), Style::default().fg(theme.fg))
        }).collect::<Vec<_>>());
        lines.push(tag_line);
        lines.push(Line::from(""));
    }

    // Summary row: 📌 👥 ⌘
    lines.push(Line::from(vec![
        Span::styled("📌 Pinned: ", Style::default().fg(theme.accent)),
        Span::styled(format!("{}  ", snap.pinned_count), Style::default().fg(theme.fg)),
        Span::styled("👥 Groups: ", Style::default().fg(theme.accent)),
        Span::styled(format!("{}  ", snap.group_count), Style::default().fg(theme.fg)),
        Span::styled("⌘ History: ", Style::default().fg(theme.accent)),
        Span::styled(format!("{}  ", snap.history_count), Style::default().fg(theme.fg)),
        Span::styled("🚨 Rules: ", Style::default().fg(theme.accent)),
        Span::styled(format!("{}", snap.alert_rule_count), Style::default().fg(theme.fg)),
    ]));

    // 尺寸 + 居中(同 activity overlay)
    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };
    f.render_widget(Clear, overlay_area);
    let title = " Dashboard (j/k:scroll Esc:close) ";
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.accent))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);
    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

/// 命令历史浮层: 最新在前, prefix 着色, 可滚动, Enter 编辑选中项。
fn draw_history_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    let lines: Vec<Line> = model.history.iter().rev().map(|e| {
        let ts = format_hms(e.timestamp_ms);
        let prefix_display = if e.prefix.is_empty() { "(none)".to_string() } else { e.prefix.clone() };
        Line::from(vec![
            Span::styled(format!(" {ts} "), Style::default().fg(theme.muted)),
            Span::styled(format!("{prefix_display:<10} "), Style::default().fg(theme.accent)),
            Span::styled(e.text.clone(), Style::default().fg(theme.fg)),
        ])
    }).collect();

    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(" (no history)", Style::default().fg(theme.muted)))]
    } else {
        lines
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };

    f.render_widget(Clear, overlay_area);

    let title = " Command History (j/k:scroll Enter:edit c:clear Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    let visible: Vec<Line> = lines[start..end].to_vec();
    f.render_widget(Paragraph::new(visible), inner);
}

/// Snippet library overlay: sorted by name, shows command text preview, scrollable.
fn draw_snippet_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    let mut names: Vec<&String> = model.snippets.keys().collect();
    names.sort();

    let lines: Vec<Line> = names
        .iter()
        .map(|name| {
            let text = &model.snippets[*name];
            let text_display = if text.chars().count() > 60 {
                let mut t: String = text.chars().take(60).collect();
                t.push('\u{2026}');
                t
            } else {
                text.clone()
            };
            Line::from(vec![
                Span::styled(format!(" [{name}] "), Style::default().fg(theme.accent)),
                Span::styled(text_display, Style::default().fg(theme.fg)),
            ])
        })
        .collect();

    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(
            " (no snippets)",
            Style::default().fg(theme.muted),
        ))]
    } else {
        lines
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Snippets (Enter:run Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

/// Alert Rules 浮层: sorted by created_at_ms, shows rule type/value, scrollable.
fn draw_rule_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    let mut rules: Vec<_> = model.alert_rules.iter().collect();
    rules.sort_by_key(|r| r.created_at_ms);

    let lines: Vec<Line> = rules.iter().map(|r| {
        Line::from(vec![
            Span::styled(format!(" [{}:{}] ", r.rule_type.as_str(), r.value), Style::default().fg(theme.accent)),
            Span::styled(format!("rule #{}", r.id), Style::default().fg(theme.muted)),
        ])
    }).collect();

    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(" (no rules)", Style::default().fg(theme.muted)))]
    } else { lines };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };
    f.render_widget(Clear, overlay_area);
    let title = " Alert Rules (Enter:remove Esc:close) ";
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme.accent))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);
    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

/// Macro library overlay: sorted alphabetically, shows key count + relative time, scrollable.
fn draw_macro_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    let mut names: Vec<&String> = model.macros.keys().collect();
    names.sort();

    let now = crate::model::now_ms();

    let lines: Vec<Line> = names
        .iter()
        .map(|name| {
            let m = &model.macros[*name];
            let count = crate::model::count_key_events(&m.key_events_json);
            let elapsed_ms = now.saturating_sub(m.created_at_ms);
            let time_label = if elapsed_ms < 60_000 {
                format!("{}s ago", elapsed_ms / 1000)
            } else if elapsed_ms < 3_600_000 {
                format!("{}m ago", elapsed_ms / 60_000)
            } else {
                format!("{}h ago", elapsed_ms / 3_600_000)
            };
            Line::from(vec![
                Span::styled(format!(" {name}"), Style::default().fg(theme.accent)),
                Span::styled(format!("({count} keys)  "), Style::default().fg(theme.fg)),
                Span::styled(time_label, Style::default().fg(theme.muted)),
            ])
        })
        .collect();

    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(
            " No macros recorded yet. Use 'macro:record:name' to start.",
            Style::default().fg(theme.muted),
        ))]
    } else {
        lines
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Macros (Enter:run d:delete Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

// ───────────────────────── Saved Views 浮层 ─────────────────────────

/// Saved Views overlay: sorted alphabetically, shows tab/sort/filter preview, scrollable.
fn draw_views_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    let mut names: Vec<&String> = model.saved_views.keys().collect();
    names.sort();

    let lines: Vec<Line> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let snap = &model.saved_views[*name];
            let query = snap.filter_query.as_deref().unwrap_or("-");
            let name_style = if i == shell.overlay_scroll {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.fg)
            };
            Line::from(vec![
                Span::styled(format!("  {name}  "), name_style),
                Span::styled(
                    format!("[{}] sort:{} filter:{}", snap.tab, snap.sort_mode, query),
                    Style::default().fg(theme.muted),
                ),
            ])
        })
        .collect();

    let lines = if lines.is_empty() {
        vec![Line::from(Span::styled(
            " No saved views yet. Use 'view:save:name' to create one.",
            Style::default().fg(theme.muted),
        ))]
    } else {
        lines
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Saved Views (Enter:load d:delete Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}
// ───────────────────────── Agent Metrics 浮层 ─────────────────────────

/// Unicode block char for sparkline: maps val/max to ▁▂▃▄▅▆▇█.
fn sparkline_char(val: u64, max: u64) -> char {
    if max == 0 { return ' '; }
    let ratio = val as f64 / max as f64;
    match ratio {
        r if r > 0.875 => '█',
        r if r > 0.75 => '▇',
        r if r > 0.625 => '▆',
        r if r > 0.5 => '▅',
        r if r > 0.375 => '▄',
        r if r > 0.25 => '▃',
        r if r > 0.125 => '▂',
        r if r > 0.0 => '▁',
        _ => ' ',
    }
}

/// Agent Metrics & Trends overlay: timeline sparkline, category/severity breakdown, top agents.
fn draw_metrics_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    let snap = crate::model::compute_agent_metrics(model, shell.metrics_window);
    let total_events: usize = snap.category_totals.iter().sum();
    let mut lines: Vec<Line> = Vec::new();

    if total_events == 0 {
        lines.push(Line::from(Span::styled(
            " No events in this window yet.",
            Style::default().fg(theme.muted),
        )));
    } else {
        // Header
        lines.push(Line::from(vec![
            Span::styled("Window: ", Style::default().fg(theme.accent)),
            Span::styled(snap.window.as_label().to_string(), Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
            Span::styled("    Events: ", Style::default().fg(theme.accent)),
            Span::styled(format!("{total_events}"), Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
            Span::styled("    Agents active: ", Style::default().fg(theme.accent)),
            Span::styled(format!("{}", snap.agents.len()), Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(""));

        // Global timeline sparkline
        let gt_max = snap.global_timeline.iter().copied().max().unwrap_or(0);
        let sparkline: String = snap.global_timeline.iter().map(|&v| sparkline_char(v, gt_max)).collect();
        lines.push(Line::from(vec![
            Span::styled("Timeline: ", Style::default().fg(theme.accent)),
            Span::styled(sparkline, Style::default().fg(theme.fg)),
        ]));
        lines.push(Line::from(""));

        // Category breakdown
        let cat_names = ["Agent", "State", "Message", "Group", "System"];
        let cat_spans: Vec<Span> = cat_names.iter().zip(snap.category_totals.iter()).map(|(name, &count)| {
            Span::styled(format!("{name}: {count}  "), Style::default().fg(theme.fg))
        }).collect();
        lines.push(Line::from(vec![
            Span::styled("Categories  ", Style::default().fg(theme.accent)),
        ].into_iter().chain(cat_spans).collect::<Vec<_>>()));
        lines.push(Line::from(""));

        // Severity breakdown
        lines.push(Line::from(vec![
            Span::styled("Severity    ", Style::default().fg(theme.accent)),
            Span::styled(format!("Info: {}  ", snap.severity_totals[0]), Style::default().fg(theme.fg)),
            Span::styled(format!("Warn: {}  ", snap.severity_totals[1]), Style::default().fg(theme.warn)),
            Span::styled(format!("Error: {}", snap.severity_totals[2]), Style::default().fg(theme.error)),
        ]));
        lines.push(Line::from(""));

        // Top agents (already sorted desc by total_events in snapshot)
        lines.push(Line::from(Span::styled(
            "Top Agents", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
        let top: Vec<&AgentMetrics> = snap.agents.iter().take(5).collect();
        if top.is_empty() {
            lines.push(Line::from(Span::styled("  (none)", Style::default().fg(theme.muted))));
        } else {
            for a in top {
                let tl_max = a.timeline.iter().copied().max().unwrap_or(0);
                let mini: String = a.timeline.iter().map(|&v| sparkline_char(v, tl_max)).collect();
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}: ", a.handle), Style::default().fg(theme.accent)),
                    Span::styled(format!("{} events ", a.total_events), Style::default().fg(theme.fg)),
                    Span::styled(mini, Style::default().fg(theme.muted)),
                ]));
            }
        }
    }

    // 尺寸 + 居中(同 dashboard overlay pattern)
    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };
    f.render_widget(Clear, overlay_area);
    let title = " Metrics (w:window x/Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);
    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

// ───────────────────────── Agent Note 浮层 ─────────────────────────

/// Agent Note 编辑浮层: 显示/编辑指定 agent 的备注文本。
fn draw_note_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Borders, Clear};

    // Get the handle we're viewing
    let handle = match &shell.note_viewing_handle {
        Some(h) => h,
        None => return,
    };

    // Centered overlay area
    let overlay_h = (area.height.saturating_sub(4)).max(8);
    let overlay_w = (area.width.saturating_sub(4)).max(50);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };

    f.render_widget(Clear, overlay_area);

    // Title shows the handle
    let title = format!(" Note: {} (Enter:save Esc:cancel) ", handle);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // Show the edit buffer text, with empty state hint
    let lines: Vec<Line> = if shell.note_edit_buf.is_empty() {
        vec![Line::from(Span::styled(
            " (type to add a note, Enter to save, Esc to cancel)",
            Style::default().fg(theme.muted),
        ))]
    } else {
        shell
            .note_edit_buf
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(theme.fg))))
            .collect()
    };
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────── Quick Actions 浮层 ─────────────────────────

/// Get the currently selected agent handle from directory view.
fn current_directory_handle(model: &Model, shell: &Shell) -> Option<String> {
    let sorted = if shell.filter_active {
        let q = shell.filter_query.as_deref().unwrap_or("");
        let full = directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned);
        crate::model::directory_filter_handles(&full, &model.directory, q, &model.tags)
    } else {
        directory_sorted_with_mode(&model.directory, model.sort_mode(), &model.pinned)
    };
    let sorted = crate::model::apply_focus_filter(sorted, shell.focus_mode, &shell.selected_set);
    sorted.get(shell.cursor).cloned()
}

/// Quick Actions 浮层: 紧凑居中菜单, 9 个快捷操作。
fn draw_quick_actions_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    let handle = match current_directory_handle(model, shell) {
        Some(h) => h,
        None => return,
    };
    let is_pinned = model.pinned.contains(&handle);

    // 9 fixed menu items
    let items: Vec<String> = vec![
        "Send message".into(),
        "Inject PTY".into(),
        "Rename".into(),
        "Add tag".into(),
        "Add note".into(),
        if is_pinned { "📌 Unpin".into() } else { "Pin".into() },
        "Switch terminal".into(),
        "Read output".into(),
        "Close terminal".into(),
    ];

    // Compact centered overlay
    let item_count = items.len();
    let overlay_h = (item_count as u16 + 4).min(area.height.saturating_sub(4));
    let overlay_w = 50u16.min(area.width.saturating_sub(8));
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };

    f.render_widget(Clear, overlay_area);

    let title = format!(" Quick Actions — {} (o/Esc:close) ", handle);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // Render items with ▸ selection highlight
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let selected = i == shell.quick_actions_cursor;
            let prefix = if selected { "▸ " } else { "  " };
            if selected {
                Line::from(Span::styled(
                    format!("{prefix}{label}"),
                    Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(
                    format!("{prefix}{label}"),
                    Style::default().fg(theme.fg),
                ))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

// ───────────────────────── Aliases 浮层 ─────────────────────────

/// Aliases overlay: sorted alphabetically, shows name → expansion pairs, scrollable.
fn draw_alias_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    let mut names: Vec<&String> = model.aliases.keys().collect();
    names.sort();

    let lines: Vec<Line> = if names.is_empty() {
        vec![Line::from(Span::styled(
            " No aliases yet. Use 'alias:name expansion' to create one.",
            Style::default().fg(theme.muted),
        ))]
    } else {
        names
            .iter()
            .map(|name| {
                let expansion = &model.aliases[*name];
                Line::from(vec![
                    Span::styled(format!(" {name}"), Style::default().fg(theme.accent)),
                    Span::styled(format!(" → {expansion}"), Style::default().fg(theme.fg)),
                ])
            })
            .collect()
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(50);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Aliases (Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

// ───────────────────────── Hotkeys 浮层 ─────────────────────────

/// Hotkeys overlay: sorted alphabetically, shows key → command pairs, scrollable.
fn draw_hotkeys_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    let mut keys: Vec<&String> = model.hotkeys.keys().collect();
    keys.sort();

    let lines: Vec<Line> = if keys.is_empty() {
        vec![Line::from(Span::styled(
            " No hotkeys bound. Use 'hotkey:key command' to bind one.",
            Style::default().fg(theme.muted),
        ))]
    } else {
        keys.iter().map(|key| {
            let cmd = &model.hotkeys[*key];
            Line::from(vec![
                Span::styled(format!(" {key}"), Style::default().fg(theme.accent)),
                Span::styled(format!(" → {cmd}"), Style::default().fg(theme.fg)),
            ])
        }).collect()
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(50);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Hotkeys (Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

// ───────────────────────── Templates 浮层 ─────────────────────────

/// Templates overlay: sorted alphabetically, shows name → body (with $N placeholders), scrollable.
fn draw_template_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    let mut names: Vec<&String> = model.templates.keys().collect();
    names.sort();

    let lines: Vec<Line> = if names.is_empty() {
        vec![Line::from(Span::styled(
            " No templates yet. Use 'tpl:name body with $1 $2' to create one.",
            Style::default().fg(theme.muted),
        ))]
    } else {
        names
            .iter()
            .map(|name| {
                let body = &model.templates[*name];
                Line::from(vec![
                    Span::styled(format!(" {name}"), Style::default().fg(theme.accent)),
                    Span::styled(format!(" → {body}"), Style::default().fg(theme.fg)),
                ])
            })
            .collect()
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(50);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Templates (Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

// ───────────────────────── Scheduler 浮层 ─────────────────────────

/// Scheduler overlay: shows scheduled tasks with countdown, 🔄 for repeating.
fn draw_sched_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    let now = std::time::Instant::now();
    let mut tasks = model.scheduled_tasks.clone();
    tasks.sort_by_key(|t| t.fire_at);

    let lines: Vec<Line> = if tasks.is_empty() {
        vec![Line::from(Span::styled(
            " No scheduled tasks. Use 'sched:<N> <command>' or 'sched:repeat:<N> <command>'.",
            Style::default().fg(theme.muted),
        ))]
    } else {
        tasks
            .iter()
            .map(|t| {
                let remaining = t.fire_at.saturating_duration_since(now);
                let secs = remaining.as_secs();
                let countdown = if secs >= 3600 {
                    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
                } else if secs >= 60 {
                    format!("{}m {}s", secs / 60, secs % 60)
                } else {
                    format!("{}s", secs)
                };
                let repeat_icon = if t.repeat_interval.is_some() { "🔄 " } else { "" };
                let cmd_display: String = t.command.chars().take(50).collect();
                Line::from(vec![
                    Span::styled(format!(" #{} ", t.id), Style::default().fg(theme.muted)),
                    Span::styled(format!("{:>8} ", countdown), Style::default().fg(theme.warn)),
                    Span::styled(format!("{repeat_icon}{cmd_display}"), Style::default().fg(theme.fg)),
                ])
            })
            .collect()
    };

    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(50);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Scheduled Tasks (Esc:close) ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(
            Span::styled(
                title,
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

// ───────────────────────── Theme Customization 浮层 ─────────────────────────

/// Theme overlay: 15 color keys with █ swatches in actual colors, shows custom/base status.
fn draw_theme_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    // The 15 theme color keys with their current Color values
    let color_entries: Vec<(&str, Color)> = vec![
        ("fg", theme.fg),
        ("bg", theme.bg),
        ("accent", theme.accent),
        ("muted", theme.muted),
        ("working", theme.working),
        ("idle", theme.idle),
        ("error", theme.error),
        ("warn", theme.warn),
        ("border", theme.border),
        ("border_focus", theme.border_focus),
        ("selection_bg", theme.selection_bg),
        ("selection_fg", theme.selection_fg),
        ("success", theme.success),
        ("tab_active", theme.tab_active),
        ("tab_inactive", theme.tab_inactive),
    ];

    // Build lines: key name, color swatch, config override or base value
    let lines: Vec<Line> = color_entries
        .iter()
        .map(|(key, color)| {
            let config_key = format!("theme.{key}");
            let config_val = model.config.get(&config_key);
            let swatch = Span::styled("█", Style::default().fg(*color));
            let key_span = Span::styled(format!(" {key:<14}"), Style::default().fg(theme.fg));
            let val_str = if let Some(v) = config_val {
                format!(" {v} (custom)")
            } else {
                " (base)".to_string()
            };
            let val_span = Span::styled(val_str, Style::default().fg(theme.muted));
            Line::from(vec![key_span, swatch, Span::raw(" "), val_span])
        })
        .collect();

    // Centered overlay
    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(50);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect {
        x: overlay_x,
        y: overlay_y,
        width: overlay_w,
        height: overlay_h,
    };

    f.render_widget(Clear, overlay_area);

    let title = " Theme (z/Esc:close) — use 'theme:key color' to customize ";
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title,
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let visible_h = inner.height as usize;
    let total = lines.len();
    let start = shell.overlay_scroll.min(total.saturating_sub(visible_h));
    let end = (start + visible_h).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

/// Quick-Switch 浮层: fuzzy 搜索 agent, 选中后跳转 cursor。
fn draw_quickswitch_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
    let sorted = crate::model::directory_sorted_with_mode(
        &model.directory, model.sort_mode(), &model.pinned,
    );
    let q = shell.quickswitch_query.to_ascii_lowercase();
    let matches: Vec<&String> = if q.is_empty() {
        sorted.iter().collect()
    } else {
        sorted.iter().filter(|h| {
            let agent = match model.directory.get(*h) { Some(a) => a, None => return false };
            let title = agent.title.as_deref().unwrap_or("").to_ascii_lowercase();
            let cwd = agent.cwd.to_ascii_lowercase();
            h.to_ascii_lowercase().contains(&q) || title.contains(&q) || cwd.contains(&q)
        }).collect()
    };
    let overlay_h = (area.height / 2).min(20).max(8);
    let overlay_w = area.width.saturating_sub(8).max(50);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;
    let overlay_area = Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h };
    f.render_widget(Clear, overlay_area);
    let title = if shell.quickswitch_query.is_empty() {
        " Jump to Agent (type to search, Esc:cancel) ".to_string()
    } else {
        format!(" Jump to Agent: '{}' ({} matches, Esc:cancel) ", shell.quickswitch_query, matches.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(title, Style::default().fg(theme.accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);
    let items: Vec<ListItem> = matches.iter().map(|h| {
        let agent = match model.directory.get(*h) { Some(a) => a, None => return ListItem::new(h.to_string()) };
        let title = agent.title.as_deref().unwrap_or("");
        let state = agent.state.as_deref().unwrap_or("");
        ListItem::new(Line::from(vec![
            Span::styled(format!(" {}", h), Style::default().fg(theme.accent)),
            Span::styled(format!(" {}", title), Style::default().fg(theme.fg)),
            Span::styled(format!(" [{}]", state), Style::default().fg(theme.muted)),
        ]))
    }).collect();
    let mut list_state = ListState::default();
    list_state.select(Some(shell.quickswitch_cursor.min(matches.len().saturating_sub(1))));
    let list = List::new(items)
        .highlight_style(Style::default().bg(theme.selection_bg).fg(theme.selection_fg).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, inner, &mut list_state);
}

/// Group wiring overlay: 列出已有 groups, 可加入/退出, 可新建。
/// h 键: handshake → 向 group 成员群发通信信息。
fn draw_group_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Clear};

    let current_handle = crate::update::selected_agent_handle_public(model, shell);

    // 创建模式
    if shell.group_creating {
        let overlay_w = 50.min(area.width);
        let overlay_h = 5;
        let x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
        let y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
        let oa = Rect::new(x, y, overlay_w, overlay_h);
        f.render_widget(Clear, oa);
        let block = Block::default().borders(Borders::ALL)
            .border_style(Style::default().fg(theme.accent))
            .title(" New Group (type name, Enter=create, Esc=cancel) ");
        let inner = block.inner(oa);
        f.render_widget(block, oa);
        let line = if shell.group_create_buf.is_empty() {
            Line::from(Span::styled(" group name...", Style::default().fg(theme.muted)))
        } else {
            Line::from(Span::styled(format!(" {}", shell.group_create_buf), Style::default().fg(theme.fg)))
        };
        f.render_widget(line, inner);
        return;
    }

    let mut names: Vec<String> = model.groups.keys().cloned().collect();
    names.sort();
    let n = names.len();
    // +1 for "new group" option
    let list_len = n + 1;
    let cursor = shell.group_overlay_cursor.min(list_len.saturating_sub(1));

    let overlay_h = (list_len as u16 + 5).min(area.height.saturating_sub(4));
    let overlay_w = (area.width * 4 / 5).max(60);
    let x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let oa = Rect::new(x, y, overlay_w, overlay_h);

    f.render_widget(Clear, oa);
    let cur_name = current_handle.as_ref()
        .and_then(|h| crate::view::handle_tag(h).to_string().into())
        .unwrap_or_default();
    let title = format!(" Group Wiring — agent: {} (Enter:join/leave h:handshake Esc:close) ", cur_name);
    let block = Block::default().borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(title);
    let inner = block.inner(oa);
    f.render_widget(block, oa);

    let mut lines: Vec<Line> = Vec::new();

    // 列出 groups
    for (i, gname) in names.iter().enumerate() {
        let members = model.groups.get(gname).map(|s| s.len()).unwrap_or(0);
        let is_in = current_handle.as_ref()
            .map(|h| model.groups.get(gname).map(|s| s.contains(h)).unwrap_or(false))
            .unwrap_or(false);
        let mark = if is_in { "\u{25cf}" } else { " " }; // ● = joined
        let mark_color = if is_in { theme.success } else { theme.muted };

        // 成员预览: 前 5 个 tag
        let preview: String = model.groups.get(gname).map(|s| {
            let tags: Vec<String> = s.iter()
                .map(|h| handle_tag(h).to_string())
                .take(5)
                .collect();
            let extra = if s.len() > 5 { format!(" +{}", s.len() - 5) } else { String::new() };
            format!("{tags:?}{}", extra)
        }).unwrap_or_default();

        let selected = i == cursor;
        let style = if selected {
            Style::default().bg(theme.selection_bg).fg(theme.selection_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        // gname field width scales with overlay
        let name_w = (inner.width as usize / 3).max(12);
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", mark), Style::default().fg(mark_color)),
            Span::styled(format!("{:<width$}", gname, width=name_w), style),
            Span::styled(format!(" ({})  ", members), Style::default().fg(theme.muted)),
            Span::styled(preview, Style::default().fg(theme.muted)),
        ]));
    }

    // "+ new group" 选项
    let selected_new = cursor == n;
    let new_style = if selected_new {
        Style::default().bg(theme.selection_bg).fg(theme.selection_fg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.accent)
    };
    lines.push(Line::from(vec![
        Span::styled(" + ", new_style),
        Span::styled("new group", new_style),
    ]));

    // 空行 + hint
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k:navigate  Enter:join/leave  h:handshake(broadcast)  Esc:close",
        Style::default().fg(theme.muted),
    )));

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
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
