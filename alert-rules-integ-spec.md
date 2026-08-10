# Alert Rules Integration Spec (update.rs)

## 1. `rule:` prefix in dispatch_input (insert before `tag:` at L886)

```rust
// rule:add [field:value ...] message  / rule:rm:<id> / rule:ls
if let Some(rest) = buf.strip_prefix("rule:") {
    if let Some(id_str) = rest.strip_prefix("rm:") {
        let id = id_str.trim().to_string();
        if model.remove_alert_rule(&id) {
            shell.push_toast(format!("Rule removed: {id}"));
            return vec![Cmd::PersistAlertRuleRemove { id }];
        }
        shell.push_toast(format!("Rule not found: {id}"));
        return vec![];
    }
    if rest.trim() == "ls" {
        let rules = model.alert_rules_summary();
        shell.push_toast(rules);
        return vec![];
    }
    if let Some(spec) = rest.strip_prefix("add ") {
        // spec = optional "state:X source:Y severity:Z" fields + message text
        let mut state = None; let mut source = None; let mut sev = None;
        let mut msg_parts = Vec::new();
        for tok in spec.split_whitespace() {
            if let Some(v) = tok.strip_prefix("state:")   { state  = Some(v.to_string()); }
            else if let Some(v) = tok.strip_prefix("source:") { source = Some(v.to_string()); }
            else if let Some(v) = tok.strip_prefix("severity:") { sev = Some(v.to_string()); }
            else { msg_parts.push(tok); }
        }
        let message = msg_parts.join(" ");
        if message.is_empty() {
            shell.push_toast("rule:add needs a message".into());
            return vec![];
        }
        let rule = AlertRule { id: model.next_rule_id(), state, source, severity: sev, message, enabled: true };
        model.add_alert_rule(rule.clone());
        shell.push_toast(format!("Rule added: #{}", rule.id));
        return vec![Cmd::PersistAlertRuleAdd { rule }];
    }
    shell.push_toast("rule: usage: rule:add [state:X source:Y severity:Z] message | rule:rm:<id> | rule:ls".into());
    return vec![];
}
```

## 2. Hook Points — `check_alert_rules(ctx)` called after existing flows

**CheckContext struct** (new, in model.rs):
```rust
pub struct CheckContext<'a> {
    pub handle: Option<&'a str>,
    pub new_state: Option<StatusCategory>,
    pub event_severity: Option<&'a EventSeverity>,
    pub event_source: Option<&'a str>,
    pub is_new_message: bool,
}
```

### 2a. StatusUpdated transition loop (L199-208) — AFTER note_event + pinned toast

Existing loop already does `note_event` then pinned toast. Alert rules generalize this:
```rust
for (handle, sev, text) in transitions {
    cmds.push(note_event(model, sev, EventCategory::State, &handle, text));
    // ── Pin alert: proactive toast for pinned agents ──
    if model.pinned.contains(&handle) {
        if let Some(agent) = model.directory.get(&handle) {
            let new_cat = StatusCategory::from_agent(agent);
            shell.push_toast(format!("📌 {} → {}", handle, new_cat.label()));
        }
    }
    // ── Alert rules (generalization of pinned toast) ──
    let ctx = CheckContext {
        handle: Some(&handle),
        new_state: Some(new_cat_from_agent(model, &handle)),
        event_severity: Some(&sev),
        event_source: None,
        is_new_message: false,
    };
    for toast in model.check_alert_rules(&ctx) {
        shell.push_toast(toast);
    }
}
```
**Note**: `new_cat` is already computed inside the pinned block. Extract before the loop entry or recompute via helper. The pinned toast stays untouched — rule check is additive.

### 2b. MessagesDrained arm (L243-254) — after persist cmds

```rust
AppMsg::MessagesDrained(msgs) => {
    let n = msgs.len();
    let persist = msgs.clone();
    for msg in msgs { model.push_message(msg); }
    let mut cmds = vec![Cmd::PersistMessages(persist)];
    if n > 0 {
        cmds.push(note_event(model, EventSeverity::Info, EventCategory::Message, "system", format!("Received {n} messages")));
        // ── Alert rules: new message arrived ──
        let ctx = CheckContext { handle: None, new_state: None, event_severity: None, event_source: None, is_new_message: true };
        for toast in model.check_alert_rules(&ctx) { shell.push_toast(toast); }
    }
    cmds
}
```

### 2c. Error arms — after existing push_toast + note_event

**SendFailed (L225-228):**
```rust
AppMsg::SendFailed(e) => {
    shell.push_toast(format!("Send failed: {e}"));
    let mut cmds = vec![note_event(model, EventSeverity::Error, EventCategory::Message, "system", format!("Send failed: {e}"))];
    let ctx = CheckContext { handle: None, new_state: None, event_severity: Some(&EventSeverity::Error), event_source: Some("system"), is_new_message: false };
    for toast in model.check_alert_rules(&ctx) { shell.push_toast(toast); }
    cmds
}
```
**AckFailed (L238-241):** — identical pattern, event_source: Some("system").
**InjectFailed (L264-267):** — identical pattern, category: System.
**AppMsg::Error (if exists):** — same ctx with event_severity: Error.

## 3. N key toggle + overlay routing + handle_overlay_key

### 3a. N key toggle in handle_normal_key (insert after S toggle at L427-434)

```rust
// N(Shift+n): alert rules overlay (toggle)
(KeyCode::Char('N'), KeyModifiers::SHIFT) if !shell.insert_mode => {
    shell.alert_rules_active = !shell.alert_rules_active;
    if shell.alert_rules_active { shell.overlay_scroll = 0; }
    return vec![];
}
```

### 3b. Shell field: `pub alert_rules_active: bool` (init false)
Add to overlay guard at L357: `|| shell.alert_rules_active`

### 3c. handle_overlay_key — add Enter arm for rule removal (after snippet Enter at L1298-1310)

```rust
(KeyCode::Enter, KeyModifiers::NONE) if shell.alert_rules_active => {
    shell.alert_rules_active = false;
    // overlay_scroll indexes into sorted rules list
    let mut ids: Vec<u64> = model.alert_rules.keys().cloned().collect();
    ids.sort();
    let idx = shell.overlay_scroll.min(ids.len().saturating_sub(1));
    if let Some(id) = ids.get(idx) {
        let rid = *id;
        model.remove_alert_rule(&rid.to_string());
        shell.push_toast(format!("Rule removed: #{rid}"));
        shell.alert_rules_active = true; // re-open to show updated list
        shell.overlay_scroll = 0;
        return vec![Cmd::PersistAlertRuleRemove { id: rid.to_string() }];
    }
    shell.overlay_scroll = 0;
    vec![]
}
```

### 3d. handle_overlay_key Esc/q — add `shell.alert_rules_active = false;`

## 4. StatusUpdated Loop Modification Detail

The existing flow at L199-208 is:
1. `note_event(...)` — always fires for state transitions
2. Pinned check — fires only for pinned agents
3. **NEW**: `check_alert_rules(ctx)` — fires for ALL transitions, generalizing pinned

The rule check is strictly additive. It does NOT replace the pinned toast. Pinned agents get BOTH the 📌 toast and any matching rule toasts. The `CheckContext` carries `handle` + `new_state` so a rule with `state:Error` and `source:term_abc` matches precisely.

## 5. Test: alert_rule_state_match_test

```rust
#[test]
fn alert_rule_state_match_test() {
    let mut model = Model::new();
    // Add rule: match state:Error
    model.add_alert_rule(AlertRule {
        id: 1, state: Some("Error".into()), source: None,
        severity: None, message: "agent errored".into(), enabled: true,
    });
    // Simulate transition to Error state
    let agent = Agent { title: "test".into(), handle: "h1".into(), state: "error".into(), ..Default::default() };
    model.directory.insert("h1".into(), agent);
    let ctx = CheckContext {
        handle: Some("h1"),
        new_state: Some(StatusCategory::Error),
        event_severity: Some(&EventSeverity::Warn),
        event_source: None,
        is_new_message: false,
    };
    let toasts = model.check_alert_rules(&ctx);
    assert_eq!(toasts.len(), 1);
    assert!(toasts[0].contains("agent errored"));

    // Non-matching state should produce no toast
    let ctx2 = CheckContext {
        handle: Some("h1"), new_state: Some(StatusCategory::Idle),
        event_severity: Some(&EventSeverity::Info), event_source: None, is_new_message: false,
    };
    assert!(model.check_alert_rules(&ctx2).is_empty());

    // is_new_message only matches rules with state=None (broad rules)
    let ctx3 = CheckContext {
        handle: None, new_state: None, event_severity: None,
        event_source: None, is_new_message: true,
    };
    assert!(model.check_alert_rules(&ctx3).is_empty()); // rule has state:Some, no match
}
```

### Cmd enum additions needed:
```rust
Cmd::PersistAlertRuleAdd { rule: AlertRule },
Cmd::PersistAlertRuleRemove { id: String },
```
