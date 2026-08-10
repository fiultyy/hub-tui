# Agent Tags — Render Layer Spec

## 1. Card Tag Badge (`draw_agent_card`, view.rs ~L499)

### Current row-0 layout (L518–553):
```
Row 0 spans: [icon] [space] [title_trunc] [title_pad] [{badge}] [space] [right_part]
```
- `icon`: pinned/check/source_icon (L519)
- `title_trunc`: truncated agent title (L529–533)
- `badge_str`: unread count `(N)` in `theme.error` (L522, L542–547)
- `right_part`: status icon + elapsed, right-aligned (L524)

### Insertion point: after unread badge, before right_part
After the existing `if !badge_str.is_empty()` block (L542–547) and the gap span (L548), add tag badge spans:

```rust
// After L548 (row0_spans.push(Span::styled(" ", ...)))
if let Some(tag) = tags.first() {
    let tag_label = format!("[{tag}]");
    row0_spans.push(Span::styled(
        tag_label,
        Style::default().fg(theme.accent),
    ));
    row0_spans.push(Span::styled(" ", Style::default()));
}
```

### Width budget: `title_max` must also account for tag width
At L528, after computing `badge_w`, add:
```rust
let tag_w = tags.first().map_or(0, |t| t.len() + 3); // "[tag] "
let title_max = avail.saturating_sub(icon_w + badge_w + tag_w + right_w);
```

### Convention: match existing Span style
- Use `theme.accent` fg (consistent with tab headers, section labels)
- No BOLD (badge is metadata, not the primary identity)
- Format: `[tagname]` — brackets distinguish from unread `(N)` parens

## 2. Parameter Passing: `tags: &[String]`

Add `tags: &[String]` parameter to `draw_agent_card` signature (after `unread: usize`):

```rust
fn draw_agent_card(
    f: &mut Frame,
    agent: &crate::model::Agent,
    area: Rect,
    theme: &Theme,
    selected: bool,
    shell_selected: bool,
    pinned: bool,
    unread: usize,
    tags: &[String],  // NEW — Option A: lightweight, consistent with pinned:bool
)
```

### Rationale
- **Option A** (chosen): `tags: &[String]` — lighter, matches the `pinned: bool` / `unread: usize` pattern of passing pre-computed scalars from `model`.
- Option B (`model: &Model`) would pull the entire data model into a render-only function, violating the immediate-mode principle ("draw reads Model at the call site, not callee").

### Caller update (draw_directory, L359):
```rust
let tags = model.agent_tags.get(&agent.handle)
    .map(|v| v.as_slice())
    .unwrap_or_default();
draw_agent_card(f, agent, card_area, theme, is_selected, shell_sel,
    model.pinned.contains(&agent.handle), unread, tags);
```

## 3. Dashboard Tag Stats (`compute_snapshot`, model.rs)

### ModelSnapshot addition (after `pinned_count: usize`, L1021):
```rust
/// top-5 tags by agent count, descending.
pub tag_counts: Vec<(String, usize)>,
```

### compute_snapshot computation (inside the `for agent in model.directory.values()` loop, ~L1044–1052):
```rust
let mut tag_counts: HashMap<String, usize> = HashMap::new();
for agent in model.directory.values() {
    // ... existing status/source/pinned counting ...
    if let Some(agent_tags) = model.agent_tags.get(&agent.handle) {
        for t in agent_tags {
            *tag_counts.entry(t.clone()).or_insert(0) += 1;
        }
    }
}
let mut tag_top5: Vec<(String, usize)> = tag_counts.into_iter().collect();
tag_top5.sort_by(|a, b| b.1.cmp(a.1));
tag_top5.truncate(5);
```

## 4. Dashboard Tags Section (`draw_dashboard_overlay`, view.rs)

Insert after the 🏷 Sources section (after L1724 `lines.push(Line::from(""));`), mirroring the Sources pattern:

```rust
// 🏷 Tags section
lines.push(Line::from(Span::styled(
    "🏷 Tags", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))));
let tag_line = if snap.tag_counts.is_empty() {
    Line::from(Span::styled("  (none)", Style::default().fg(theme.muted)))
} else {
    Line::from(snap.tag_counts.iter().map(|(tag, cnt)| {
        Span::styled(format!("  {}: {} ", tag, cnt), Style::default().fg(theme.accent))
    }).collect::<Vec<_>>())
};
lines.push(tag_line);
lines.push(Line::from(""));
```

## 5. Section Header Tags — Skipped

Section headers (`draw_section_header`, L367–403) show `📂 worktreePath (N)` with a separator line. Aggregating per-section tag stats requires a group-level join that adds complexity for marginal value. Skip for v1; can revisit if tag-based grouping becomes a feature.

## Summary of Changes

| File | Location | Change |
|------|----------|--------|
| `view.rs` L499 | `draw_agent_card` sig | Add `tags: &[String]` param |
| `view.rs` L528 | row-0 width budget | Add `tag_w` to `title_max` subtraction |
| `view.rs` L548 | row-0 spans | Insert `[tag]` badge Span after gap, before `right_part` |
| `view.rs` L359 | caller in `draw_directory` | Resolve tags from `model.agent_tags`, pass through |
| `view.rs` L1724 | `draw_dashboard_overlay` | Add 🏷 Tags section after Sources |
| `model.rs` L1021 | `ModelSnapshot` struct | Add `tag_counts: Vec<(String, usize)>` |
| `model.rs` L1041 | `compute_snapshot` loop | Accumulate per-tag counts, sort, truncate(5) |
