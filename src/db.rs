//! db.rs — SQLite persistence layer for hub-tui.
//!
//! Tables (schema v1):
//! - groups: custom group definitions (persists across restarts)
//! - group_members: group ↔ handle junction
//! - messages: local cache of orchestration messages (offline browse)
//! - agent_activity: agent presence snapshots (last-seen tracking)
//! - config: key-value store (db_version, misc settings)
//!
//! Concurrency: Arc<Mutex<Connection>> + WAL mode. Socket thread can read
//! concurrently while main thread writes. All methods are infallible
//! (errors logged to stderr, never crash the TUI).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};

const DB_VERSION: i64 = 10;

/// SQLite handle. Cloneable (Arc<Mutex>), thread-safe.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

/// Agent row from DB (for fast startup before first CLI poll).
#[derive(Debug, Clone)]
pub struct AgentRow {
    pub handle: String,
    pub cwd: String,
    pub title: Option<String>,
    pub connected: bool,
    pub state: Option<String>,
    pub source: Option<String>,
    pub last_output_at: Option<i64>,
}

impl AgentRow {
    /// Convert to Agent for Model bootstrap.
    pub fn into_agent(self) -> crate::model::Agent {
        crate::model::Agent {
            handle: self.handle,
            pty_id: None,
            cwd: self.cwd,
            worktree_id: String::new(),
            branch: String::new(),
            tab_id: String::new(),
            leaf_id: String::new(),
            pane_key: String::new(),
            title: self.title,
            connected: self.connected,
            writable: false,
            source: self.source,
            state: self.state,
            prompt: None,
            tool_name: None,
            tool_input: None,
            last_assistant_msg: None,
            preview: None,
            last_output_at: None,
        }
    }
}

impl Db {
    pub fn open(path: Option<&str>) -> Option<Self> {
        let path = path.unwrap_or("~/.orca/hub-tui.db");
        // Expand ~
        let expanded = if path.starts_with("~/") {
            if let Ok(home) = std::env::var("HOME") {
                format!("{}/{}", home, &path[2..])
            } else {
                path.to_string()
            }
        } else {
            path.to_string()
        };
        let conn = Connection::open(&expanded).ok()?;
        Self::init(&conn)?;
        Some(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open at explicit path.
    pub fn open_path(path: &str) -> Option<Self> {
        Self::open(Some(path))
    }

    /// Run WAL pragmas + migration.
    fn init(conn: &Connection) -> Option<()> {
        // WAL mode for concurrent reads from socket thread
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .ok()?;
        Self::migrate(conn)?;
        Some(())
    }

    /// Version-gated migration. Idempotent (CREATE IF NOT EXISTS).
    fn migrate(conn: &Connection) -> Option<()> {
        let current: i64 = conn
            .query_row("SELECT value FROM config WHERE key='db_version'", [], |r| r.get(0))
            .unwrap_or(0);

        if current >= DB_VERSION {
            return Some(());
        }

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS config (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS groups (
                name       TEXT PRIMARY KEY,
                created_by TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS group_members (
                group_name TEXT NOT NULL,
                handle     TEXT NOT NULL,
                joined_at  TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (group_name, handle),
                FOREIGN KEY (group_name) REFERENCES groups(name) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS messages (
                id          TEXT PRIMARY KEY,
                from_handle TEXT NOT NULL,
                to_handle   TEXT NOT NULL,
                subject     TEXT NOT NULL DEFAULT '',
                body        TEXT NOT NULL DEFAULT '',
                msg_type    TEXT NOT NULL DEFAULT 'status',
                priority    TEXT NOT NULL DEFAULT 'normal',
                thread_id   TEXT,
                payload     TEXT,
                read        INTEGER NOT NULL DEFAULT 0,
                created_at  TEXT NOT NULL DEFAULT '',
                sequence    INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_messages_created ON messages(created_at);
            CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id);

            CREATE TABLE IF NOT EXISTS agent_activity (
                handle         TEXT NOT NULL,
                cwd            TEXT NOT NULL DEFAULT '',
                title          TEXT,
                connected      INTEGER NOT NULL DEFAULT 0,
                state          TEXT,
                source         TEXT,
                last_output_at INTEGER,
                snapshot_at    TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (handle, snapshot_at)
            );

            CREATE TABLE IF NOT EXISTS events (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                ts       INTEGER NOT NULL,
                severity TEXT NOT NULL DEFAULT 'Info',
                category TEXT NOT NULL DEFAULT 'Agent',
                source   TEXT NOT NULL DEFAULT 'system',
                text     TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS input_history (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                text     TEXT NOT NULL,
                ts       INTEGER NOT NULL,
                prefix   TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_input_history_ts ON input_history(ts);

            CREATE TABLE IF NOT EXISTS pinned_agents (
                handle TEXT PRIMARY KEY
            );

            CREATE TABLE IF NOT EXISTS agent_tags (
                handle TEXT NOT NULL,
                tag    TEXT NOT NULL,
                PRIMARY KEY (handle, tag)
            );

            CREATE TABLE IF NOT EXISTS snippets (
                name       TEXT PRIMARY KEY,
                text       TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS alert_rules (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                rule_type  TEXT NOT NULL,
                value      TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS macros (
                name       TEXT PRIMARY KEY,
                key_events TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS saved_views (
                name       TEXT PRIMARY KEY,
                view_state TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS agent_notes (
                handle     TEXT PRIMARY KEY,
                note       TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );

            INSERT OR REPLACE INTO config (key, value) VALUES ('db_version', '10');
            ",
        )
        .ok()?;
        Some(())
    }

    // ──── Agent activity ────

    /// Upsert latest agent snapshot (one row per handle, latest snapshot_at).
    pub fn upsert_agent(
        &self,
        handle: &str,
        cwd: &str,
        title: Option<&str>,
        connected: bool,
        state: Option<&str>,
        source: Option<&str>,
        last_output_at: Option<i64>,
    ) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "INSERT OR REPLACE INTO agent_activity
             (handle, cwd, title, connected, state, source, last_output_at, snapshot_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
            params![handle, cwd, title, connected as i32, state, source, last_output_at],
        );
    }

    /// Load latest snapshot for each handle (for fast startup).
    pub fn get_all_agents(&self) -> Vec<AgentRow> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare(
            "SELECT a.handle, a.cwd, a.title, a.connected, a.state, a.source, a.last_output_at
             FROM agent_activity a
             INNER JOIN (
                 SELECT handle, MAX(snapshot_at) AS max_snap
                 FROM agent_activity
                 GROUP BY handle
             ) latest ON a.handle = latest.handle AND a.snapshot_at = latest.max_snap",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |row| {
                Ok(AgentRow {
                    handle: row.get(0)?,
                    cwd: row.get(1)?,
                    title: row.get(2)?,
                    connected: row.get::<_, i32>(3)? != 0,
                    state: row.get(4)?,
                    source: row.get(5)?,
                    last_output_at: row.get(6)?,
                })
            })
            .ok();
        rows.map(|r| r.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    /// Prune agent_activity entries older than `days` days.
    pub fn prune_activity(&self, days: i64) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "DELETE FROM agent_activity
             WHERE snapshot_at < datetime('now', ?1)",
            params![format!("-{days} days")],
        );
    }

    // ──── Messages ────

    /// Insert message (INSERT OR IGNORE dedup by id). Caps at 5000 rows.
    pub fn insert_message_raw(
        &self,
        id: &str,
        from_handle: &str,
        to_handle: &str,
        subject: &str,
        body: &str,
        msg_type: &str,
        priority: &str,
        thread_id: Option<&str>,
        payload: Option<&str>,
        read: i64,
        created_at: &str,
        sequence: i64,
    ) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "INSERT OR IGNORE INTO messages
             (id, from_handle, to_handle, subject, body, msg_type, priority, thread_id, payload, read, created_at, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                from_handle,
                to_handle,
                subject,
                body,
                msg_type,
                priority,
                thread_id,
                payload,
                read as i32,
                created_at,
                sequence
            ],
        );
        // Cap: delete oldest beyond 5000
        let _ = conn.execute(
            "DELETE FROM messages WHERE rowid IN (
                SELECT rowid FROM messages ORDER BY sequence DESC LIMIT -1 OFFSET 5000
            )",
            [],
        );
    }

    /// Load recent messages (oldest first, for display).
    pub fn get_recent_messages(&self, limit: usize) -> Vec<crate::model::OrchMessage> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare(
            "SELECT id, from_handle, to_handle, subject, body, msg_type, priority,
                    thread_id, payload, read, created_at, sequence
             FROM messages ORDER BY sequence DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(crate::model::OrchMessage {
                    id: row.get(0)?,
                    from_handle: row.get(1)?,
                    to_handle: row.get(2)?,
                    subject: row.get(3)?,
                    body: row.get(4)?,
                    msg_type: row.get(5)?,
                    priority: row.get(6)?,
                    thread_id: row.get(7)?,
                    payload: row.get(8)?,
                    read: row.get::<_, i64>(9)?,
                    created_at: row.get(10)?,
                    sequence: row.get(11)?,
                })
            })
            .ok();
        let mut result: Vec<_> = rows
            .map(|r| r.filter_map(|x| x.ok()).collect())
            .unwrap_or_default();
        result.reverse(); // oldest first for display
        result
    }

    // ──── Groups ────

    pub fn create_group(&self, name: &str, created_by: Option<&str>) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "INSERT OR IGNORE INTO groups (name, created_by) VALUES (?1, ?2)",
            params![name, created_by],
        );
    }

    pub fn join_group(&self, name: &str, handle: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        // Ensure group exists
        let _ = conn.execute(
            "INSERT OR IGNORE INTO groups (name) VALUES (?1)",
            params![name],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO group_members (group_name, handle) VALUES (?1, ?2)",
            params![name, handle],
        );
    }

    pub fn leave_group(&self, name: &str, handle: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "DELETE FROM group_members WHERE group_name = ?1 AND handle = ?2",
            params![name, handle],
        );
    }

    pub fn get_groups(&self) -> HashMap<String, Vec<String>> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };
        let mut stmt = match conn.prepare(
            "SELECT group_name, handle FROM group_members ORDER BY group_name, joined_at",
        ) {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        };
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                ))
            })
            .ok();
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        if let Some(rows) = rows {
            for r in rows.flatten() {
                map.entry(r.0).or_default().push(r.1);
            }
        }
        map
    }

    pub fn get_group_members(&self, name: &str) -> Vec<String> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare(
            "SELECT handle FROM group_members WHERE group_name = ?1 ORDER BY joined_at",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(params![name], |row| row.get::<_, String>(0))
            .ok()
            .map(|r| r.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    // ──── Config ────

    pub fn set_config(&self, key: &str, value: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
            params![key, value],
        );
    }

    pub fn get_config(&self, key: &str) -> Option<String> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return None,
        };
        conn.query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .ok()
    }

    /// Load all config key-value pairs.
    pub fn get_all_config(&self) -> HashMap<String, String> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return HashMap::new(),
        };
        let mut stmt = match conn.prepare("SELECT key, value FROM config") {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        };
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok()
        .map(|r| r.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
    }

    // ──── Activity Log events ────

    /// 插入活动日志事件, 返回分配的 id。FIFO 截断到 5000 行。
    pub fn insert_event(&self, event: &crate::model::Event) -> i64 {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return 0,
        };
        let _ = conn.execute(
            "INSERT INTO events (ts, severity, category, source, text) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.timestamp_ms,
                event.severity.as_str(),
                event.category.as_str(),
                event.source,
                event.text,
            ],
        );
        // FIFO trim (cap 5000)
        let _ = conn.execute(
            "DELETE FROM events WHERE id IN (
                SELECT id FROM events ORDER BY id DESC LIMIT -1 OFFSET 5000
            )",
            [],
        );
        conn.last_insert_rowid()
    }

    /// 加载最近 N 条事件(时间升序, 最旧在前 — 便于 push_back 入队)。
    pub fn load_recent_events(&self, limit: usize) -> Vec<crate::model::Event> {
        use crate::model::{Event, EventCategory, EventSeverity};
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare(
            "SELECT id, ts, severity, category, source, text
             FROM events ORDER BY ts ASC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(Event {
                id: r.get(0)?,
                timestamp_ms: r.get(1)?,
                severity: EventSeverity::from_str(&r.get::<_, String>(2)?),
                category: EventCategory::from_str(&r.get::<_, String>(3)?),
                source: r.get(4)?,
                text: r.get(5)?,
            })
        });
        rows.map(|r| r.filter_map(|x| x.ok()).collect::<Vec<_>>()).unwrap_or_default()
    }

    // ──── Input history ────

    /// 插入输入历史(连续去重: 与最后一行相同则跳过)。FIFO 截断到 2000 行。
    pub fn insert_history(&self, text: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        // 连续去重: 与最后一行相同则跳过
        let last: Option<String> = conn
            .query_row(
                "SELECT text FROM input_history ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        if last.as_deref() == Some(text) {
            return;
        }
        let prefix = text.split_once(':').map(|(p, _)| format!("{p}:")).unwrap_or_default();
        let ts = crate::model::now_ms();
        let _ = conn.execute(
            "INSERT INTO input_history (text, ts, prefix) VALUES (?1, ?2, ?3)",
            params![text, ts, prefix],
        );
        // FIFO trim (cap 2000)
        let _ = conn.execute(
            "DELETE FROM input_history WHERE id IN (
                SELECT id FROM input_history ORDER BY id DESC LIMIT -1 OFFSET 2000
            )",
            [],
        );
    }

    /// 加载最近 N 条输入历史(时间升序, 最旧在前 — 便于 push_back)。
    pub fn load_recent_history(&self, limit: usize) -> Vec<crate::model::HistoryEntry> {
        use crate::model::HistoryEntry;
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare(
            "SELECT id, ts, prefix, text FROM input_history ORDER BY ts ASC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(HistoryEntry {
                id: r.get(0)?,
                timestamp_ms: r.get(1)?,
                prefix: r.get(2)?,
                text: r.get(3)?,
            })
        });
        rows.map(|r| r.filter_map(|x| x.ok()).collect::<Vec<_>>()).unwrap_or_default()
    }

    /// 清空输入历史。
    pub fn clear_history(&self) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute("DELETE FROM input_history", []);
    }

    // ──── Pinned agents ────

    /// 添加置顶 agent。
    pub fn upsert_pinned(&self, handle: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "INSERT OR IGNORE INTO pinned_agents (handle) VALUES (?1)",
            params![handle],
        );
    }

    /// 移除置顶 agent。
    pub fn remove_pinned(&self, handle: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "DELETE FROM pinned_agents WHERE handle = ?1",
            params![handle],
        );
    }

    /// 加载所有置顶 agent handles。
    pub fn load_pinned(&self) -> Vec<String> {
        let conn = match self.conn.lock() {
 Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare("SELECT handle FROM pinned_agents") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .ok()
            .map(|r| r.filter_map(|x| x.ok()).collect())
            .unwrap_or_default()
    }

    // ──── Agent tags ────

    /// 添加标签(幂等)。
    pub fn upsert_tag(&self, handle: &str, tag: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "INSERT OR IGNORE INTO agent_tags (handle, tag) VALUES (?1, ?2)",
            params![handle, tag],
        );
    }

    /// 移除标签。
    pub fn remove_tag(&self, handle: &str, tag: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "DELETE FROM agent_tags WHERE handle = ?1 AND tag = ?2",
            params![handle, tag],
        );
    }

    /// 加载所有标签 → handle → tag set。
    pub fn load_tags(&self) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return std::collections::HashMap::new(),
        };
        let mut stmt = match conn.prepare("SELECT handle, tag FROM agent_tags") {
            Ok(s) => s,
            Err(_) => return std::collections::HashMap::new(),
        };
        let mut map: std::collections::HashMap<String, std::collections::HashSet<String>> = std::collections::HashMap::new();
        let _ = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map(|rows| {
                for row in rows.flatten() {
                    map.entry(row.0).or_default().insert(row.1);
                }
            });
        map
    }


    // ──── Snippets ────

    /// 保存/覆盖代码片段(幂等)。
    pub fn upsert_snippet(&self, name: &str, text: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let ts = crate::model::now_ms();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO snippets (name, text, created_at) VALUES (?1, ?2, ?3)",
            params![name, text, ts],
        );
    }

    /// 移除代码片段。
    pub fn remove_snippet(&self, name: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute("DELETE FROM snippets WHERE name = ?1", params![name]);
    }

    /// 加载所有代码片段 → name → text。
    pub fn load_snippets(&self) -> std::collections::HashMap<String, String> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return std::collections::HashMap::new(),
        };
        let mut stmt = match conn.prepare("SELECT name, text FROM snippets") {
            Ok(s) => s,
            Err(_) => return std::collections::HashMap::new(),
        };
        let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let _ = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map(|rows| {
                for row in rows.flatten() {
                    map.insert(row.0, row.1);
                }
            });
        map
    }

    // ──── Alert rules ────

    /// 添加告警规则, 返回分配的 id。
    pub fn upsert_alert_rule(&self, rule_type: &str, value: &str) -> i64 {
        let conn = match self.conn.lock() { Ok(c) => c, Err(_) => return 0 };
        let ts = crate::model::now_ms();
        let _ = conn.execute(
            "INSERT INTO alert_rules (rule_type, value, created_at) VALUES (?1, ?2, ?3)",
            params![rule_type, value, ts],
        );
        conn.last_insert_rowid()
    }

    /// 按 id 移除告警规则。
    pub fn remove_alert_rule(&self, id: i64) {
        let conn = match self.conn.lock() { Ok(c) => c, Err(_) => return };
        let _ = conn.execute("DELETE FROM alert_rules WHERE id = ?1", params![id]);
    }

    /// 加载所有告警规则。
    pub fn load_alert_rules(&self) -> Vec<crate::model::AlertRule> {
        use crate::model::{AlertRule, AlertRuleType};
        let conn = match self.conn.lock() { Ok(c) => c, Err(_) => return vec![] };
        let mut stmt = match conn.prepare("SELECT id, rule_type, value, created_at FROM alert_rules ORDER BY created_at") {
            Ok(s) => s, Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| {
            Ok(AlertRule {
                id: r.get(0)?,
                rule_type: AlertRuleType::from_str(&r.get::<_, String>(1)?).unwrap_or(AlertRuleType::Message),
                value: r.get(2)?,
                created_at_ms: r.get(3)?,
            })
        });
        rows.map(|r| r.filter_map(|x| x.ok()).collect::<Vec<_>>()).unwrap_or_default()
    }
    // ──── Macros ────

    /// 保存/覆盖宏(幂等)。
    pub fn upsert_macro(&self, name: &str, key_events_json: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let ts = crate::model::now_ms();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO macros (name, key_events, created_at) VALUES (?1, ?2, ?3)",
            params![name, key_events_json, ts],
        );
    }

    /// 移除宏。
    pub fn remove_macro(&self, name: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute("DELETE FROM macros WHERE name = ?1", params![name]);
    }

    /// 加载所有宏 → Vec<RecordedMacro>。
    pub fn load_macros(&self) -> Vec<crate::model::RecordedMacro> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare("SELECT name, key_events, created_at FROM macros ORDER BY created_at") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| {
            Ok(crate::model::RecordedMacro {
                name: r.get(0)?,
                key_events_json: r.get(1)?,
                created_at_ms: r.get(2)?,
            })
        });
        rows.map(|r| r.filter_map(|x| x.ok()).collect::<Vec<_>>()).unwrap_or_default()
    }
    // ──── Saved Views ────

    /// 保存/覆盖视图预设(幂等)。
    pub fn upsert_saved_view(&self, name: &str, view_state_json: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let ts = crate::model::now_ms();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO saved_views (name, view_state, created_at) VALUES (?1, ?2, ?3)",
            params![name, view_state_json, ts],
        );
    }

    /// 移除视图预设。
    pub fn remove_saved_view(&self, name: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute("DELETE FROM saved_views WHERE name = ?1", params![name]);
    }

    /// 加载所有视图预设 → (name, ViewSnapshot)。
    pub fn load_saved_views(&self) -> Vec<(String, crate::model::ViewSnapshot)> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let mut stmt = match conn.prepare("SELECT name, view_state FROM saved_views ORDER BY created_at") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            let json: String = r.get(1)?;
            let snapshot: crate::model::ViewSnapshot = serde_json::from_str(&json).unwrap_or(crate::model::ViewSnapshot {
                tab: "directory".into(),
                filter_query: None,
                sort_mode: "by-worktree".into(),
                selected_set: vec![],
                created_at_ms: 0,
            });
            Ok((name, snapshot))
        });
        rows.map(|r| r.filter_map(|x| x.ok()).collect::<Vec<_>>()).unwrap_or_default()
    }
    // ──── Agent Notes ────

    /// 保存/覆盖 agent 笔记(幂等)。
    pub fn upsert_note(&self, handle: &str, note: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let ts = crate::model::now_ms();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO agent_notes (handle, note, updated_at) VALUES (?1, ?2, ?3)",
            params![handle, note, ts],
        );
    }

    /// 移除 agent 笔记。
    pub fn remove_note(&self, handle: &str) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute("DELETE FROM agent_notes WHERE handle = ?1", params![handle]);
    }

    /// 加载所有 agent 笔记 → handle → note。
    pub fn load_notes(&self) -> std::collections::HashMap<String, String> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return std::collections::HashMap::new(),
        };
        let mut stmt = match conn.prepare("SELECT handle, note FROM agent_notes") {
            Ok(s) => s,
            Err(_) => return std::collections::HashMap::new(),
        };
        let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let _ = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map(|rows| {
                for row in rows.flatten() {
                    map.insert(row.0, row.1);
                }
            });
        map
    }
    // ──── Service-compatible aliases ────

    /// Alias: upsert with snapshot_at param (service.rs compatibility).
    pub fn upsert_agent_activity(
        &self,
        handle: &str,
        cwd: &str,
        title: Option<&str>,
        connected: bool,
        state: Option<&str>,
        source: Option<&str>,
        _snapshot_at: &str,
    ) {
        self.upsert_agent(handle, cwd, title, connected, state, source, None);
    }

    /// Insert from OrchMessage ref (service.rs calls insert_message(msg, bool)).
    pub fn insert_message(&self, msg: &crate::model::OrchMessage, _mark_read: bool) {
        self.insert_message_raw(
            &msg.id,
            &msg.from_handle,
            &msg.to_handle,
            &msg.subject,
            &msg.body,
            &msg.msg_type,
            &msg.priority,
            msg.thread_id.as_deref(),
            msg.payload.as_deref(),
            msg.read,
            &msg.created_at,
            msg.sequence,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Db {
        Db::open(Some(":memory:")).expect("failed to open in-memory db")
    }

    #[test]
    fn db_open_and_migrate() {
        let db = test_db();
        assert_eq!(db.get_config("db_version"), Some("10".to_string()));
    }

    #[test]
    fn groups_roundtrip() {
        let db = test_db();
        db.join_group("team-a", "term_001");
        db.join_group("team-a", "term_002");
        db.join_group("team-b", "term_003");

        let groups = db.get_groups();
        assert_eq!(groups.len(), 2);
        assert!(groups["team-a"].contains(&"term_001".to_string()));
        assert!(groups["team-a"].contains(&"term_002".to_string()));
        assert_eq!(groups["team-b"], vec!["term_003".to_string()]);

        db.leave_group("team-a", "term_001");
        let groups2 = db.get_groups();
        assert_eq!(groups2["team-a"], vec!["term_002".to_string()]);
    }

    #[test]
    fn messages_dedup_and_cap() {
        let db = test_db();
        for i in 0..10 {
            db.insert_message_raw(
                &format!("msg_{i:04}"),
                "term_a",
                "term_b",
                "test",
                "body",
                "status",
                "normal",
                None,
                None,
                0,
                "2026-01-01T00:00:00Z",
                i,
            );
        }
        // Insert duplicate (should be ignored)
        db.insert_message_raw(
            "msg_0000",
            "term_a",
            "term_b",
            "dup",
            "",
            "status",
            "normal",
            None,
            None,
                0,
            "",
            0,
        );
        let msgs = db.get_recent_messages(100);
        assert_eq!(msgs.len(), 10); // no dup
        assert_eq!(msgs[0].id, "msg_0000"); // oldest first
    }

    #[test]
    fn agent_activity_upsert() {
        let db = test_db();
        db.upsert_agent("term_001", "/home/test", Some("Pi"), true, Some("working"), Some("omp"), Some(1700000000));
        let agents = db.get_all_agents();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].handle, "term_001");
        assert_eq!(agents[0].cwd, "/home/test");
        assert!(agents[0].connected);
    }

    #[test]
    fn config_roundtrip() {
        let db = test_db();
        db.set_config("socket_path", "/tmp/orca-hub.sock");
        assert_eq!(
            db.get_config("socket_path"),
            Some("/tmp/orca-hub.sock".to_string())
        );
    }
}
