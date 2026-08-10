# Session Handoff: hub-tui unread badge 不显示

## 问题
agent card 上的未读消息 badge `(N)` 没有显示。代码已提交 (19bffc2) 但运行时看不到。

## 已实现的完整数据流
```
orca-ide orchestration inbox --json (每 5s,在 Tick handler 里)
  → transport::orchestration_inbox_unread() → HashMap<handle, count>
  → AppMsg::UnreadUpdated(counts) → model.apply_unread(counts)
  → view.rs draw_agent_card(agent, ..., unread) → 红色 (N) badge
```

## 关键文件和行号
- `transport.rs:208-233` — `orchestration_inbox_unread()` 函数,调 `orca-ide orchestration inbox --json`,解析 read=false 的消息计数
- `msg.rs:57-58` — `AppMsg::UnreadUpdated(HashMap<String, usize>)` variant
- `update.rs:33-34` — `Cmd::RefreshUnread` variant
- `update.rs:105` — Tick handler 里 `cmds.push(Cmd::RefreshUnread)` (在 should_refresh_agents 的 if 块内,5s 频率)
- `update.rs:123-127` — `AppMsg::UnreadUpdated` arm → `model.apply_unread(counts)`
- `service.rs:179-191` — `RefreshUnread` arm → spawn thread → `transport::orchestration_inbox_unread()` → send `AppMsg::UnreadUpdated`
- `main.rs:78` — 启动时 `svc.execute(vec![..., Cmd::RefreshUnread])`
- `model.rs:259-263` — `apply_unread()` 方法
- `model.rs:182` — `unread_counts: HashMap<String, usize>` 字段
- `view.rs:295-296` — `let unread = *model.unread_counts.get(&agent.handle).unwrap_or(&0); draw_agent_card(f, agent, card_area, theme, is_selected, unread);`
- `view.rs:367-371` — badge_str: `if unread > 0 { format!(" ({unread})") } else { "" }`
- `view.rs:392-393` — `Span::styled(badge_str, Style::default().fg(theme.error).bg(bg).add_modifier(Modifier::BOLD))`

## 排查方向

### 1. 验证 transport 层是否真的拿到了数据
```bash
orca-ide orchestration inbox --json | python3 -c "
import json,sys
d=json.load(sys.stdin)
from collections import Counter
unread = Counter()
for m in d['result']['messages']:
    if m.get('read') == 0:
        unread[m.get('toHandle','')] += 1
for h,c in unread.most_common():
    print(f'{h}: {c}')
"
```
注意: `orchestration inbox --json` 用的是 `orca-ide` 还是 `orca`? transport.rs 里 `orchestration_check()` 用的是 `orca`(line 191),而 `orchestration_inbox_unread()` 用的是 `orca-ide`(line 211)。检查这两个 CLI 是否都可用。

### 2. 验证 service.rs 里 RefreshUnread 是否被执行
在 `service.rs:179` 的 `RefreshUnread` arm 里加一行 `eprintln!("[debug] RefreshUnread spawned");` 然后在终端看 stderr。

### 3. 验证 AppMsg::UnreadUpdated 是否到达 update
在 `update.rs:123` 的 arm 里加 `eprintln!("[debug] UnreadUpdated: {} entries", counts.len());`

### 4. JSON 字段名差异
`orchestration inbox --json` 的 message 字段用的是 `toHandle` (camelCase) 还是 `to_handle` (snake_case)?
transport.rs 里 `OrchMessage` 用 `#[serde(rename = "toHandle")]`,但 `orchestration_inbox_unread()` 复用了 `CheckOutput` 结构体解析。检查 inbox 输出格式是否跟 check 一样。

### 5. `read` 字段类型
inbox 里 `read` 可能是 `0/1` (int) 而不是 `true/false` (bool)。`OrchMessage.read` 是 `bool` + `#[serde(default)]`。如果 JSON 里是 `0/1`,bool 反序列化会失败,所有 read 都 default 到 false,导致全部算未读或者全算已读。

## 仓库状态
- repo: /home/yy/.orca/hub-tui (独立 git repo)
- remote: https://github.com/fiultyy/hub-tui.git
- branch: main
- last commit: 19bffc2
- build: clean (0 errors)
- test: 19/19 pass
- hub-tui 进程在 term_d5c794b9 运行中

## 已知的其他问题(不阻塞,记录)
1. `orchestration_check()` 里用 `orca` 而非 `orca-ide` — Linux 上 `orca` 是屏幕阅读器,应改为 `orca-ide`
2. `lastOutputAt` 的 effective_state 阈值是 10s,可能需要调
3. pending_status 竞态缓存已修但未充分测试
