# Search Integration Spec (update.rs key routing + jump dispatch)

## 1. Intercept position in handle_key (update.rs ~L315)

Search intercepts BEFORE palette/filter/overlay. Insert as the FIRST check:

```rust
fn handle_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    // ── Global Search: highest priority intercept ──
    if shell.search_active {
        return handle_search_key(model, shell, k);
    }
    // Ctrl-S: toggle search
    if !shell.insert_mode {
        if let (KeyCode::Char('s'), KeyModifiers::CONTROL) = (k.code, k.modifiers) {
            if shell.search_active {
                shell.search_active = false;
                shell.search_query.clear();
                shell.search_cursor = 0;
            } else {
                shell.search_active = true;
                shell.search_query.clear();
                shell.search_cursor = 0;
            }
            return vec![];
        }
    }
    // ... existing palette/filter/overlay intercepts (L317-327) ...
```

## 2. handle_search_key — mirrors handle_palette_key pattern

```rust
fn handle_search_key(model: &mut Model, shell: &mut Shell, k: KeyEvent) -> Vec<Cmd> {
    match (k.code, k.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) => {
            shell.search_active = false;
            shell.search_query.clear();
            shell.search_cursor = 0;
            vec![]
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            dispatch_search_jump(model, shell)
        }
        (KeyCode::Down, KeyModifiers::NONE) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            let results = crate::model::global_search(model, &shell.search_query);
            if shell.search_cursor + 1 < results.len() {
                shell.search_cursor += 1;
            }
            vec![]
        }
        (KeyCode::Up, KeyModifiers::NONE) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            shell.search_cursor = shell.search_cursor.saturating_sub(1);
            vec![]
        }
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            shell.search_query.pop();
            shell.search_cursor = 0;
            vec![]
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            shell.search_query.push(c);
            shell.search_cursor = 0;
            vec![]
        }
        _ => vec![],
    }
}
```

## 3. dispatch_search_jump — the core integration

```rust
fn dispatch_search_jump(model: &mut Model, shell: &mut Shell) -> Vec<Cmd> {
    let results = crate::model::global_search(model, &shell.search_query);
    let result = match results.get(shell.search_cursor) {
        Some(r) => r.clone(),
        None => { /* close search, no-op */ return search_close(shell); }
    };
    // Always clear search state before jumping
    search_close(shell);
    match result.jump {
        JumpTarget::AgentHandle(handle) => {
            // Compute cursor from sorted handles (same pattern as digit-jump L622-648)
            let sorted = directory_sorted_with_mode(&model.directory, model.sort_mode());
            if let Some(idx) = sorted.iter().position(|h| h == &handle) {
                shell.tab = Tab::Directory;
                shell.cursor = idx;
                shell.focus = FocusTarget::Directory;
                shell.filter_active = false;
                shell.filter_query = None;
            }
            vec![]
        }
        JumpTarget::MessageId(id) => {
            // Messages tab cursor maps to reversed list (see L526-533, L538-541)
            let msgs: Vec<_> = model.messages.iter().rev().collect();
            if let Some(idx) = msgs.iter().position(|m| m.id == id) {
                shell.tab = Tab::Messages;
                shell.cursor = idx;
                shell.focus = FocusTarget::Messages;
                shell.filter_active = false;
                shell.filter_query = None;
            }
            vec![]
        }
        JumpTarget::CommandName(name) => {
            // Find command by name and invoke its handler (same as palette Enter L917-926)
            let all = crate::command::builtin_commands();
            if let Some(cmd) = all.iter().find(|c| c.name == name) {
                let handler = cmd.handler;
                return handler(model, shell);
            }
            vec![]
        }
        JumpTarget::EventIndex(eidx) => {
            // Open activity overlay, scroll to event position
            // Events are newest-last in VecDeque; scroll 0 = bottom (newest)
            shell.activity_active = true;
            // eidx is 0-based from top of results = offset into model.events
            // overlay_scroll counts from top of rendered list
            shell.overlay_scroll = eidx;
            vec![]
        }
        JumpTarget::HistoryIndex(hidx) => {
            // Open history overlay, scroll to entry
            // History overlay: scroll 0 = bottom (newest entry), see L1022-1024
            shell.history_overlay_active = true;
            // hidx is position from top; overlay_scroll measures from bottom
            // total_len - 1 - hidx = scroll-from-bottom
            let total = model.history.len();
            shell.overlay_scroll = total.saturating_sub(1).saturating_sub(hidx);
            vec![]
        }
    }
}

fn search_close(shell: &mut Shell) -> Vec<Cmd> {
    shell.search_active = false;
    shell.search_query.clear();
    shell.search_cursor = 0;
    vec![]
}
```

## 4. Key patterns used (from existing code)

- **Agent cursor from handle** (digit-jump L622-648): `directory_sorted_with_mode()` returns `Vec<String>` sorted handles; `position(|h| h == &target)` gives cursor index.
- **Message cursor from id** (L526-533): `model.messages.iter().rev().collect()` gives newest-first Vec; `position(|m| m.id == target)` gives cursor index. This is identical to how Enter-on-Messages works.
- **Command execution** (palette L917-926): `filter_commands` → `cmds_filtered.get(cursor)` → `cmd.handler(model, shell)`. We do `builtin_commands().find(|c| c.name == name)` instead since SearchResult already identified the command.
- **History overlay scroll** (L1022-1024): `overlay_scroll = history.len() - 1 - overlay_scroll` maps scroll position to deque index. We invert this.

## 5. Shell state additions

```rust
// In shell.rs Shell struct, after palette fields (~L80):
pub search_active: bool,
pub search_query: String,
pub search_cursor: usize,
```

## 6. Performance note

`global_search()` is called on every j/k navigation keypress inside search mode. With <100 agents, <5000 messages, <2000 events/history entries, the full scan takes <1ms. No caching. The results list is a plain `Vec<SearchResult>` allocated each call — cheap to drop and rebuild.

## 7. Edge cases

- **Empty query**: global_search returns all items (like palette/filter). Cursor 0 = first result.
- **No results on Enter**: search_close(), no jump.
- **Agent handle not found in sorted list**: no cursor change, search still closes.
- **Message id not found**: same — search closes, no jump.
- **Search active + Ctrl-S**: toggles off (handled in handle_search_key's caller, but Ctrl-S won't reach there since search_active → handle_search_key first). Move the Ctrl-S toggle OUTSIDE the search_active intercept (as shown in §1), or handle Ctrl-S inside handle_search_key as an Esc-like close. The spec above puts Ctrl-S BEFORE the search_active check.
