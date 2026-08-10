# Activity Log — Data Layer Spec

## 1. Event struct (model.rs, after OrchMessage ~L172)

```rust
/// Activity log event. Pure data, no IO.
#[derive(Clone, Debug)]
pub struct Event {
    pub id: i64,               // DB autoincrement; 0 = not-yet-persisted
    pub timestamp_ms: i64,     // epoch millis (std::time::SystemTime::now)
    pub severity: EventSeverity,
    pub category: EventCategory,
    pub source: String,        // agent handle or "system"
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventSeverity { Info, Warn, Error }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventCategory { AgentState, Message, Error, Group, Orch }
```

## 2. Model additions

```rust
pub const EVENTS_CAP: usize = 2000;

// Inside Model struct (after messages field, ~L202):
pub events: VecDeque<Event>,

// Model::new() — add:
events: VecDeque::new(),

// impl Model — new methods (after push_message ~L316):
/// Append event, enforce cap, evict oldest.
pub fn push_event(&mut self, event: Event) {
    if self.events.len() >= EVENTS_CAP {
        self.events.pop_front();
    }
    self.events.push_back(event);
}

/// Drain all events (for batch DB flush or view consumption).
pub fn drain_events(&mut self) -> Vec<Event> {
    self.events.drain(..).collect()
}
```

## 3. AppMsg additions (msg.rs, after OrchSnapshotLoaded ~L87)

```rust
/// Single event persisted (echo back for view redraw).
EventLogged(Event),
/// Bulk events loaded from DB on startup.
EventsLoaded(Vec<Event>),
```

## 4. db.rs additions

### Schema (append to migrate() CREATE block, before `INSERT OR REPLACE`)

```sql
CREATE TABLE IF NOT EXISTS events (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       INTEGER NOT NULL,
    severity TEXT NOT NULL DEFAULT 'Info',
    category TEXT NOT NULL,
    source   TEXT NOT NULL DEFAULT 'system',
    text     TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
```

### FIFO cap (after insert, idempotent)

```sql
DELETE FROM events WHERE id <= (
    SELECT id FROM events ORDER BY id DESC LIMIT 1 OFFSET 5000
);
```

### Methods on Db

```rust
/// Insert one event, return assigned id. Trims past cap.
pub fn insert_event(
    &self, timestamp_ms: i64, severity: &str,
    category: &str, source: &str, text: &str,
) -> i64 {
    let conn = match self.conn.lock() { Ok(c) => c, Err(_) => return 0 };
    let _ = conn.execute(
        "INSERT INTO events (ts, severity, category, source, text) VALUES (?1,?2,?3,?4,?5)",
        params![timestamp_ms, severity, category, source, text],
    );
    let id: i64 = conn.last_insert_rowid();
    // FIFO trim (cap 5000)
    let _ = conn.execute(
        "DELETE FROM events WHERE id <= (SELECT id FROM events ORDER BY id DESC LIMIT 1 OFFSET 5000)",
        [],
    );
    id
}

/// Load N most recent events (newest last for VecDeque push_back ordering).
pub fn load_recent_events(&self, limit: usize) -> Vec<Event> {
    let conn = match self.conn.lock() { Ok(c) => c, Err(_) => return vec![] };
    let mut stmt = match conn.prepare(
        "SELECT id, ts, severity, category, source, text FROM events ORDER BY ts ASC LIMIT ?1"
    ) { Ok(s) => s, Err(_) => return vec![] };
    let rows = stmt.query_map(params![limit as i64], |r| {
        Ok(Event {
            id: r.get(0)?,
            timestamp_ms: r.get(1)?,
            severity: parse_severity(r.get::<_, String>(2)?),
            category: parse_category(r.get::<_, String>(3)?),
            source: r.get(4)?,
            text: r.get(5)?,
        })
    });
    rows.filter_map(|r| r.ok()).collect()
}
```

Helper fns `parse_severity(s: &str) -> EventSeverity` and `parse_category(s: &str) -> EventCategory` — match on `"Info"/"Warn"/"Error"` and `"AgentState"/"Message"/"Error"/"Group"/"Orch"`, default `Info`/`AgentState`.

## 5. Event flow

```
[any update.rs arm]                  [main.rs startup]
  │                                      │
  ├─ model.push_event(event)            ├─ let events = db.load_recent_events(2000);
  ├─ db.insert_event(...);  // IO       ├─ for e in events { model.push_event(e); }
  └─ return AppMsg::EventLogged(event)  └─ (no AppMsg; model already populated)
```

- **MVU respected**: `Model::push_event` is pure mutation. DB writes happen in update.rs (or a dedicated arm) *after* the model mutation, never inside Model.
- `EventLogged` carries the persisted event back to the main loop so the view can redraw the activity panel without polling model diff.
- Startup: `load_recent_events` returns oldest-first; `push_event` appends to VecDeque back → correct chronological order.
- DB cap (5000) > in-memory cap (2000): DB retains more history; on restart the most recent 2000 fill the VecDeque.
