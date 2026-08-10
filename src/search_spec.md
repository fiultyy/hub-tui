# Global Search Engine Spec

Lives in `model.rs`, placed after `messages_filter_ids` (after L815).

## Types

```rust
/// 搜索结果分类(决定分组顺序和 jump_target 语义)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchCategory {
    Agent,
    Message,
    Event,
    History,
    Command,
}

/// 搜索结果的导航目标(用于 Enter 跳转)。
#[derive(Debug, Clone)]
pub enum JumpTarget {
    AgentHandle(String),
    MessageId(String),
    EventIndex(usize),
    HistoryIndex(usize),
    CommandName(&'static str),
}

/// 一条搜索结果。
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub category: SearchCategory,
    pub primary: String,
    pub secondary: String,
    pub jump_target: JumpTarget,
}
```

## Unified Function

```rust
/// 全局搜索: 跨所有数据源 fuzzy match, 返回分组排序结果。
/// 纯计算, 无 IO。空 query → 空结果。
/// 分组顺序: Agent > Message > Event > History > Command。
/// 每组上限 5, 总上限 25。
pub fn global_search(model: &Model, query: &str) -> Vec<SearchResult> { … }
```

## Per-Source Logic (all use `crate::command::fuzzy_match`)

### Agents — iterate `model.directory.values()`
- Match: `fuzzy_match(q, handle) || fuzzy_match(q, title) || fuzzy_match(q, cwd) || fuzzy_match(q, source)`
- `primary` = `agent.handle.clone()`
- `secondary` = `format!("{source} · {state} · {cwd}")`  where source/state fall back to `""`/`"idle"`
- `jump_target` = `JumpTarget::AgentHandle(agent.handle.clone())`
- Cap: 5

### Messages — iterate `model.messages.iter()`
- Match: `fuzzy_match(q, from_handle) || fuzzy_match(q, subject) || fuzzy_match(q, body)`
- `primary` = `if !m.subject.is_empty() { m.subject.clone() } else { m.from_handle.clone() }`
- `secondary` = `format!("from {} · {}", m.from_handle, truncate(&m.body, 60))`
  where `truncate(s, n)` takes first `n` chars, appends "…" if longer.
- `jump_target` = `JumpTarget::MessageId(m.id.clone())`
- Cap: 5

### Events — iterate `model.events.iter()` (newest-last; search from back for recency priority)
- Match: `fuzzy_match(q, text) || fuzzy_match(q, source)`
- `primary` = `truncate(&ev.text, 80)`
- `secondary` = `format!("{} · {} · {}", ev.severity.as_str(), ev.category.as_str(), ev.source)`
- `jump_target` = `JumpTarget::EventIndex(idx)` where `idx` is the position in `model.events`
- Cap: 5

### History — iterate `model.history.iter()` (newest-last; search from back for recency priority)
- Match: `fuzzy_match(q, text) || fuzzy_match(q, prefix)`
- `primary` = `entry.text.clone()`
- `secondary` = `entry.prefix.clone()`
- `jump_target` = `JumpTarget::HistoryIndex(idx)` where `idx` is position in `model.history`
- Cap: 5

### Commands — call `crate::command::filter_commands(query)`
- Map each `Command` to: `primary = c.name.to_string()`, `secondary = c.description.to_string()`, `jump_target = JumpTarget::CommandName(c.name)`
- Empty query: `filter_commands("")` returns all → cap to 5
- Cap: 5

## Helper

```rust
/// 截断到 max_len 字符, 超出加 "…"。
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len])
    }
}
```

## Assembly

Collect each category's `Vec<SearchResult>`, extend into output in order: Agent, Message, Event, History, Command. Total cap 25 (enforced by per-group cap of 5 × 5 groups).

## Imports needed in model.rs

```rust
use crate::command::fuzzy_match;  // already used in directory_filter_handles/messages_filter_ids
use crate::command::filter_commands;
```

No new `use` statements needed — `fuzzy_match` is already imported at existing call sites. `filter_commands` needs one `use` at the top of `global_search` or a file-level import.

## Tests (in `model.rs` `#[cfg(test)]` block or inline)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, fuzzy_match as fm, filter_commands};

    #[test]
    fn test_global_search_empty_query_returns_empty() { … }
    #[test]
    fn test_global_search_agents_match() { … }
    #[test]
    fn test_global_search_messages_match() { … }
    #[test]
    fn test_global_search_commands_reuse() { … }
    #[test]
    fn test_truncate() { … }
}
```
