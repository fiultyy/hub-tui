# Activity Log — update.rs Integration Spec

## 1. Event-Emitting AppMsg Arms

| AppMsg Arm | Event Category | Severity | Notes |
|---|---|---|---|
| `SendOk(id)` | `Message` | `Info` | text: `"Sent {id}"` |
| `SendFailed(e)` | `Message` | `Error` | text: `"Send failed: {e}"` |
| `AckOk(id)` | `Message` | `Info` | text: `"Marked read: {id}"` |
| `AckFailed(e)` | `Message` | `Error` | text: `"Ack failed: {e}"` |
| `InjectOk(n)` | `System` | `Info` | text: `"PTY inject: {n} bytes"` |
| `InjectFailed(e)` | `System` | `Error` | text: `"PTY inject failed: {e}"` |
| `GroupActionOk(msg)` | `Group` | `Info` | text: the msg payload |
| `TerminalCreated { handle, title }` | `Agent` | `Info` | text: `"Created: {label}"` |
| `Error(e)` | `System` | `Error` | text: the e payload |
| `MessagesDrained(msgs)` | `Message` | `Info` | **Only if msgs.len() > 0**; text: `"Received {n} messages"` |
| `AgentsLoaded(agents)` | `Agent` | `Info` | **Diff only**: agents appeared/disappeared (see §3) |
| `StatusUpdated(statuses)` | `AgentState` | `Info`/`Warn` | **Diff only**: StatusCategory transitions (see §2) |

### Arms that do NOT emit events (internal/structural):
`Key`, `MouseLeftClick`, `Resize`, `Tick`, `Quit`, `SocketQuery`, `TerminalOutput`,
`UnreadUpdated`, `Info`, `ConfigUpdated`, `OrchSnapshotLoaded`, `WorktreePsLoaded`.

## 2. State-Transition Detection (StatusUpdated)

This is the core algorithm. Must run **before** `model.apply_status` mutates directory,
so we snapshot old categories first.

```rust
AppMsg::StatusUpdated(statuses) => {
    // ── Step 1: snapshot old StatusCategory per handle (by pane_key join) ──
    let old_cats: HashMap<&str, StatusCategory> = model.directory.iter()
        .map(|(h, a)| (h.as_str(), StatusCategory::from_agent(a)))
        .collect();

    // ── Step 2: apply the status update (mutates directory) ──
    model.apply_status(statuses);

    // ── Step 3: diff → collect transitions ──
    let mut transitions = Vec::new();
    for (handle, agent) in &model.directory {
        let new_cat = StatusCategory::from_agent(agent);
        if let Some(&old_cat) = old_cats.get(handle.as_str()) {
            if old_cat != new_cat {
                transitions.push((handle.clone(), old_cat, new_cat));
            }
        }
    }

    // ── Step 4: emit events from collected transitions (no borrow conflict) ──
    let mut cmds = vec![Cmd::WriteDirectory];
    for (handle, old_cat, new_cat) in transitions {
        let (severity, text) = transition_severity_text(&handle, old_cat, new_cat);
        cmds.push(Cmd::PersistActivityEvent {
            severity,
            category: EventCategory::AgentState,
            source: handle.clone(),
            text,
        });
    }
    cmds
}
```

### Severity rules for transitions:

| From \ To | Working | Waiting | Blocked | Error | Done | Unknown |
|---|---|---|---|---|---|---|
| **Working** | *(suppress)* | `Info` | `Warn` | `Warn` | `Info` | *(suppress)* |
| **Waiting** | `Info` | *(suppress)* | `Warn` | `Warn` | `Info` | *(suppress)* |
| **Blocked** | `Info` | `Info` | *(suppress)* | `Warn` | `Info` | *(suppress)* |
| **Error** | `Info` | `Info` | `Info` | *(suppress)* | `Info` | *(suppress)* |
| **Done** | `Info` | `Info` | `Warn` | `Warn` | *(suppress)* | *(suppress)* |
| **Unknown** | `Info` | `Info` | `Info` | `Warn` | `Info` | *(suppress)* |

Same-category transitions are suppressed (no event). Transitions to `Error` or `Blocked` are `Warn`.
All other real transitions are `Info`. Unknown→Unknown suppressed.

```rust
fn transition_severity_text(
    handle: &str, old: StatusCategory, new: StatusCategory,
) -> (EventSeverity, String) {
    let text = format!("{handle}: {old:?} → {new:?}");
    let sev = match new {
        StatusCategory::Error | StatusCategory::Blocked => EventSeverity::Warn,
        _ => EventSeverity::Info,
    };
    (sev, text)
}
```

## 3. Agent Appeared / Disappeared (AgentsLoaded)

Diff `model.directory` keys before and after `apply_agents`:

```rust
AppMsg::AgentsLoaded(agents) => {
    // ── Step 1: snapshot old handles ──
    let old_handles: HashSet<&str> = model.directory.keys().map(|s| s.as_str()).collect();
    let persist = agents.clone();
    model.apply_agents(agents);
    // ── Step 2: diff ──
    let new_handles: HashSet<&str> = model.directory.keys().map(|s| s.as_str()).collect();
    let appeared: Vec<_> = new_handles.difference(&old_handles).copied().collect();
    let disappeared: Vec<_> = old_handles.difference(&new_handles).copied().collect();
    // ── Step 3: emit ──
    let mut cmds = vec![Cmd::PersistAgents(persist), Cmd::WriteDirectory];
    for h in appeared {
        cmds.push(Cmd::PersistActivityEvent {
            severity: EventSeverity::Info,
            category: EventCategory::Agent,
            source: h.to_string(),
            text: format!("Agent appeared: {h}"),
        });
    }
    for h in disappeared {
        cmds.push(Cmd::PersistActivityEvent {
            severity: EventSeverity::Info,
            category: EventCategory::Agent,
            source: h.to_string(),
            text: format!("Agent disappeared: {h}"),
        });
    }
    cmds
}
```

## 4. Helper: `note_event` (pure-function, borrow-checker safe)

```rust
/// Central event emission helper. Returns Cmds to persist.
/// Call after model mutation (or before, depending on arm — see §2/§3).
fn note_event(
    severity: EventSeverity,
    category: EventCategory,
    source: String,
    text: String,
) -> Vec<Cmd> {
    vec![Cmd::PersistActivityEvent {
        severity, category, source, text,
    }]
}
```

The pure-reducer constraint (no IO in `update`) means we emit `Cmd::PersistActivityEvent`
and let `service.rs` handle the actual DB insert — exactly like `Cmd::PersistAgents`.

## 5. Open Questions / Risks

1. **Event storms during rapid polling**: `StatusUpdated` fires every tick (~50ms).
   If an agent oscillates between Working↔Unknown due to `lastOutputAt` edge effects,
   this generates 20 events/sec. **Mitigation**: debounce — only emit if the *previous
   persisted event* for the same handle+category is older than N seconds (e.g. 2s).
   Requires a lightweight in-model timestamp cache (`last_event_at: HashMap<String, Instant>`),
   checked inside the transition loop before pushing a Cmd.

2. **`effective_state()` uses wall-clock time** (`SystemTime::now()` inside `Agent`):
   Two calls within the same `update` invocation can return different categories if the
   clock crosses the 10s boundary mid-snapshot. **Mitigation**: use a single `now_ms()` call,
   pass it as a parameter, or accept the negligible race (single-threaded, ~microseconds).

3. **AgentsLoaded full-replace semantics**: `apply_agents` removes handles not in the
   incoming list. During Orca instability, a transient empty response would fire
   "disappeared" for ALL agents then "appeared" on next poll. **Mitigation**: suppress
   appeared/disappeared events when `old_handles.is_empty() || new_handles.is_empty()`.

4. **Cmd::PersistActivityEvent schema**: needs a new `Cmd` variant + `service.rs` handler
   + DB table. This is a separate deliverable from the integration wiring.
