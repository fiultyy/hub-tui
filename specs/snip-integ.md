# Spec: Snippets — update.rs Integration

## 1. Cmd enum additions (update.rs:83, before Noop)

```rust
/// 持久化 snippet (save/update)。service 同步写 snippets 表。
PersistSnippet { name: String, text: String },
/// 移除 snippet。service 同步删 snippets 行。
RemoveSnippet { name: String },
```

## 2. dispatch_input — new prefixes (insert BEFORE final `vec![]` at L932)

### Insertion point: after `tagged:` block (L930), before `vec![]` (L932)

```rust
    // snip:name text... — save snippet / snip:rm:name — remove
    if let Some(rest) = buf.strip_prefix("snip:") {
        if let Some(name) = rest.strip_prefix("rm:") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                model.remove_snippet(&name);
                shell.push_toast(format!("snippet removed: {name}"));
                return vec![Cmd::RemoveSnippet { name }];
            }
            return vec![];
        }
        if let Some((name, text)) = rest.split_once(' ') {
            let name = name.trim().to_string();
            let text = text.trim().to_string();
            if !name.is_empty() && !text.is_empty() {
                if model.snippets.len() >= 100 && !model.snippets.contains_key(&name) {
                    shell.push_toast("snippet limit (100) reached".into());
                    return vec![];
                }
                model.add_snippet(&name, &text);
                shell.push_toast(format!("snippet saved: {name}"));
                return vec![Cmd::PersistSnippet { name, text }];
            }
        }
        shell.push_toast("snip: usage: snip:name command_text".into());
        return vec![];
    }

    // run:name — replay snippet (enter insert mode with text pre-filled)
    if let Some(name) = buf.strip_prefix("run:") {
        let name = name.trim();
        if let Some(text) = model.get_snippet(name) {
            shell.insert_mode = true;
            shell.focus = FocusTarget::Input;
            shell.input_buf = text.to_string();
            shell.push_toast(format!("replaying: {name} (Enter to confirm)"));
        } else {
            shell.push_toast(format!("snippet not found: {name}"));
        }
        return vec![];
    }
```

### Key design decisions
- **Signature**: `dispatch_input(model: &mut Model, shell: &mut Shell, buf: String) -> Vec<Cmd>` — shell IS `&mut`, so `run:` setting `shell.insert_mode = true` and `shell.input_buf` is valid.
- **`run:` returns `vec![]`** — no Cmd needed; it only mutates shell state. The user presses Enter to confirm, which re-enters `handle_input_key → dispatch_input` with the expanded text. This is the same pattern as command palette pre-fills (see command.rs L298-301: `shell.insert_mode = true; shell.focus = FocusTarget::Input; shell.input_buf = ...; vec![]`).
- **`snip:` and `snip:rm:` DO return Cmds** for DB persistence, matching the pattern of `tag:`/`tag:rm:` (L873-897).

## 3. History recording decision

**Record snippet execution to history as `run:<name>`, NOT the expanded text.**

Rationale: The Enter arm (L948-952) records `buf` (which is `run:name`) to history. Since `run:` returns `vec![]` (empty cmds), the condition `!cmds.is_empty()` at L949 is FALSE, so **`run:name` is NOT recorded to history by default**. This is correct behavior — avoids noise.

To record for audit, the `run:` arm should return a non-empty vec so the Enter arm records it:
```rust
if let Some(text) = model.get_snippet(name) {
    // ... set shell state ...
    return vec![Cmd::Noop]; // trigger history recording of "run:name"
}
```
**Decision: use `Cmd::Noop` trick.** This records `run:name` (the snippet reference, not expanded text) to history for audit trail, without adding a new Cmd variant. Clean audit, no noise.

## 4. handle_input_key Enter arm — NO changes needed

The existing flow at L939-953 already handles the `run:` lifecycle:
1. User types `run:deploy` → Enter → `dispatch_input` called → sets `shell.insert_mode=true`, `shell.input_buf="deploy:main build"`, returns `vec![Cmd::Noop]`
2. Enter arm L949: `cmds.is_empty()` is false → records `run:deploy` to history
3. User reviews pre-filled text, presses Enter again → `dispatch_input` processes the actual command

## 5. Test: `snippet_save_and_run_test`

```rust
#[test]
fn snippet_save_and_run_test() {
    let mut model = Model::new();
    let mut shell = Shell::new();

    // Save a snippet
    let cmds = dispatch_input(&mut model, &mut shell, "snip:deploy to:main build && test".into());
    assert!(cmds.iter().any(|c| matches!(c, Cmd::PersistSnippet { name, .. } if name == "deploy")));
    assert_eq!(model.get_snippet("deploy"), Some("to:main build && test"));

    // Remove a snippet
    let cmds = dispatch_input(&mut model, &mut shell, "snip:rm:deploy".into());
    assert!(cmds.iter().any(|c| matches!(c, Cmd::RemoveSnippet { name, .. } if name == "deploy")));
    assert_eq!(model.get_snippet("deploy"), None);

    // Re-save and test run: replay
    let _ = dispatch_input(&mut model, &mut shell, "snip:deploy to:main build".into());
    let cmds = dispatch_input(&mut model, &mut shell, "run:deploy".into());
    assert!(cmds.iter().any(|c| matches!(c, Cmd::Noop))); // triggers history recording
    assert!(shell.insert_mode);
    assert_eq!(shell.input_buf, "to:main build");

    // run: non-existent
    shell.insert_mode = false;
    let cmds = dispatch_input(&mut model, &mut shell, "run:nosuch".into());
    assert!(cmds.is_empty());
    assert!(!shell.insert_mode); // no state change on miss
}
```

## 6. Edge cases handled
- `snip:` with no space → usage toast + `vec![]`
- `snip:rm:` with empty name → `vec![]` (no-op)
- `run:` with empty name → no match, not-found toast
- `run:` with non-existent name → not-found toast, no state change
- Cap 100: rejects NEW snippets at limit; allows overwrite of existing
- Name collision: `snip:name` overwrites silently (same as `model.add_snippet` semantics)
