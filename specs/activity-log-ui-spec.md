# Activity Log Overlay — UI/UX Spec

## 1. Shell state additions (shell.rs, inside struct Shell after `config_overlay_active` ~L97)

```rust
/// Activity log overlay activated (`a` key).
pub activity_active: bool,
/// Activity overlay scroll position (reuses overlay_scroll pattern).
pub activity_scroll: usize,
/// Filter: substring match on text, or category: prefix (e.g. "category:Error").
pub activity_filter: Option<String>,
```

Shell::new() — add after `config_overlay_active: false`:
```rust
activity_active: false,
activity_scroll: 0,
activity_filter: None,
```

## 2. Key bindings (update.rs)

### Toggle — handle_key, insert into the overlay-toggle block at ~L297 (after `t:` orch arm, before `?:` cheatsheet):

```rust
// a: activity log overlay (toggle)
(KeyCode::Char('a'), KeyModifiers::NONE) if !shell.insert_mode => {
    shell.activity_active = !shell.activity_active;
    if shell.activity_active {
        shell.activity_scroll = 0;
        shell.activity_filter = None;
    }
    vec![]
}
```

### Overlay routing — handle_key ~L264, add `shell.activity_active` to the guard:

```rust
if shell.overlay_content.is_some() || shell.worktree_ps_active
    || shell.group_detail_active || shell.cheatsheet_active
    || shell.config_overlay_active || shell.orch_tasks_active
    || shell.activity_active {          // ← add
```

### Scroll — handle_overlay_key ~L870, no changes needed; the existing `j/k/Up/Down` arms already mutate `overlay_scroll`. But since activity uses `activity_scroll`, add two arms after the `k` arm (~L888):

```rust
// Ctrl-d / Ctrl-u: page scroll (same half-screen logic as main ~L347)
(KeyCode::Char('d'), KeyModifiers::CONTROL) => {
    shell.overlay_scroll = shell.overlay_scroll.saturating_add((shell.size.1 as usize / 2).max(1));
    vec![]
}
(KeyCode::Char('u'), KeyModifiers::CONTROL) => {
    shell.overlay_scroll = shell.overlay_scroll.saturating_sub((shell.size.1 as usize / 2).max(1));
    vec![]
}
```

### Close — handle_overlay_key Esc/q arm ~L872, add:
```rust
shell.activity_active = false;
```

### Filter — handle_overlay_key, add new arm after Ctrl-u:
```rust
(KeyCode::Char('f'), KeyModifiers::NONE) => {
    shell.activity_filter = Some(String::new());
    vec![Cmd::EnterActivityFilter]   // reuses filter_active input-bar UX, or inline
}
```

Simpler: in handle_overlay_key, when `activity_filter.is_some()`, route chars to the filter string, Esc commits filter. When `activity_filter.is_none()`, `f` enters filter-input mode.

## 3. view.rs — draw_activity_overlay()

### Dispatch — draw() ~L101 (after orch_tasks_active block):
```rust
if shell.activity_active {
    draw_activity_overlay(f, model, shell, area, &theme);
}
```

### Draw function — mirrors `draw_config_overlay` / `draw_worktree_ps_overlay` pattern:

```rust
fn draw_activity_overlay(f: &mut Frame, model: &Model, shell: &Shell, area: Rect, theme: &Theme) {
    use ratatui::widgets::{Block, Borders, Paragraph, Clear};
    use ratatui::style::Modifier;

    // Build lines from model.events (newest-first display)
    let filtered: Vec<&Event> = model.events.iter().rev().filter(|e| {
        match &shell.activity_filter {
            None => true,
            Some(q) if q.starts_with("category:") => {
                e.category.as_str().eq_ignore_ascii_case(&q[9..])
            }
            Some(q) => e.text.to_lowercase().contains(&q.to_lowercase()),
        }
    }).collect();

    let lines: Vec<Line> = filtered.iter().map(|e| {
        let ts = format_hms(e.timestamp_ms);  // HH:MM:SS helper
        let sev_icon = match e.severity { EventSeverity::Error => "✖", EventSeverity::Warn => "⚠", EventSeverity::Info => "·" };
        let cat_icon = match e.category { EventCategory::AgentState => "↻", EventCategory::Message => "💬", EventCategory::Error => "⚡", EventCategory::Group => "👥", EventCategory::Orch => "⟳" };
        let sev_color = match e.severity { EventSeverity::Error => theme.error, EventSeverity::Warn => theme.warn, EventSeverity::Info => theme.muted };
        Line::from(vec![
            Span::styled(format!(" {ts} "), Style::default().fg(theme.muted)),
            Span::styled(format!("{sev_icon} "),  Style::default().fg(sev_color)),
            Span::styled(format!("{cat_icon} "),  Style::default().fg(theme.accent)),
            Span::styled(e.text.clone(),         Style::default().fg(theme.fg)),
        ])
    }).collect();

    if lines.is_empty() { lines.push(Line::from(Span::styled(" (no events)", Style::default().fg(theme.muted)))); }

    // Size & centering — same as worktree_ps_overlay
    let overlay_h = area.height.saturating_sub(4).max(8);
    let overlay_w = area.width.saturating_sub(4).max(40);
    let overlay_x = area.x + (area.width - overlay_w) / 2;
    let overlay_y = area.y + 2;

    f.render_widget(Clear, Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h });

    let title_suffix = match &shell.activity_filter {
        None => String::new(),
        Some(q) => format!(" [filter: {}]", q),
    };
    let border = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!(" Activity Log{} (j/k: scroll, f: filter, Esc: close) ", title_suffix),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    let inner = border.inner(Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h });
    f.render_widget(border, Rect { x: overlay_x, y: overlay_y, width: overlay_w, height: overlay_h });

    let visible_h = inner.height as usize;
    let start = shell.overlay_scroll.min(lines.len().saturating_sub(visible_h));
    let visible: Vec<Line> = lines[start..start.min(lines.len()).min(start + visible_h)].to_vec();
    f.render_widget(Paragraph::new(visible), inner);
}
```

### Timestamp helper:
```rust
fn format_hms(ts_ms: i64) -> String {
    let secs = (ts_ms / 1000) as u64;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}
```

### Severity colors — Theme struct (theme.rs) already has `error`, `warn`, `muted`. If `warn` is missing, add:
```rust
pub warn: Color,  // yellow
```

## 4. Filter semantics

- `activity_filter: None` → show all events.
- `Some("keyword")` → case-insensitive substring match on `event.text`.
- `Some("category:Error")` → exact match on `EventCategory::as_str()` (case-insensitive).
- Entering filter mode: press `f` in overlay → sets `activity_filter = Some("")` → overlay key handler routes printable chars into the string, Backspace truncates, Esc commits (keeps filter) or clears (if empty), Enter same as Esc.
- Filter indicator rendered in overlay title bar (see title_suffix above).

## 5. Scroll integration

`overlay_scroll` is already shared across all overlays. When `activity_active` flips on, `overlay_scroll` resets to 0 (set in toggle arm). The `handle_overlay_key` j/k/Ctrl-d/Ctrl-u arms mutate `overlay_scroll` generically — no per-overlay arm needed for basic scrolling. This matches how worktree_ps, config, and cheatsheet overlays already work.

## 6. Toast dedup for high-severity events

In update.rs, wherever `model.push_event(event)` is called for Error/Warn severity:

```rust
/// In Shell — add field:
pub last_toast_severity_text: Option<(EventSeverity, String, Instant)>,

/// Shell method — call instead of raw push_toast for events:
pub fn push_event_toast(&mut self, sev: EventSeverity, text: &str) {
    let key = (sev.clone(), text.to_string());
    let now = Instant::now();
    // Dedup: same severity+text within 10s → suppress
    if let Some(ref last) = self.last_toast_severity_text {
        if last.0 == sev && last.1 == text && now.duration_since(last.2).as_secs() < 10 {
            return;
        }
    }
    self.last_toast_severity_text = Some((sev, text.to_string(), now));
    let prefix = match sev { EventSeverity::Error => "✖", EventSeverity::Warn => "⚠", EventSeverity::Info => return }; // Info → no toast
    self.push_toast(format!("{prefix} {text}"));
}
```

- Error → red toast (existing toast color is `theme.error`).
- Warn → could add a second toast color slot, but current toast rendering hardcodes `theme.error`. For now, Warn toasts also use error color; a future enhancement could color-code toast severity.
- **Info events never toast** — they're only visible in the activity overlay.
- 10-second dedup window prevents spam from repeated identical errors (e.g. reconnect failures).
