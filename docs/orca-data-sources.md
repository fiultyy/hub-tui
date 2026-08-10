# Orca 数据源全字段深度分析

> 基于 `stablyai/orca` v1.4.151+ 源码 (`~/tools/orca/`) 的逐字段 trace。
> 分析方法：3 个 scout 并行 fan-out，逐字段 trace TypeScript 类型定义 → RPC handler → 运行时 build 方法 → coordinator poll-loop 消费。
> 分析日期：2026-08-10。

## 数据源总览

```
terminal list ──────► terminal 元数据(PTY/连接/标题/预览) ── 每 5s poll
last-status.json ───► hook 事件快照(agent 状态/工具/prompt) ── mtime poll
orchestration.db ───► 消息总线/任务 DAG/dispatch/决策门 ── 按需查
```

三源通过 **worktreeId** join（terminal.list ↔ last-status.json），通过 **handle** join（terminal.list ↔ orchestration.db dispatch.assignee_handle）。

---

## 一、terminal list — `orca-ide terminal list --json`

**RPC 流**: CLI → `terminal.list` RPC (`rpc/methods/terminal.ts:1110`) → `runtime.listTerminals()` (`orca-runtime.ts:12471`) → 双路径产出：
- `buildTerminalSummary()` (`orca-runtime.ts:25421`) — renderer graph 绑定的 leaf，`orphaned=false`
- `buildPtyTerminalSummary()` (`orca-runtime.ts:26706`) — graph-unbound PTY，`orphaned` 实时算

| # | 字段 | 类型 | 来源 (file:line) | 写入时机 | Orca 业务用途 | 过期风险 | hub-tui 当前 | hub-tui 可加 |
|---|---|---|---|---|---|---|---|---|
| 1 | **handle** | `string` `term_<uuid>` | `issueHandle()` `orca-runtime.ts:26815` | PTY 绑定时分配一次，graph epoch bump 前 stable | 所有操作的身份 key：send/close/switch/dispatch/orphan adoption/hibernation | 无(stable,显式失效) | ✅ | — |
| 2 | **ptyId** | `string\|null` | `RuntimeSyncedLeaf.ptyId` `runtime-types.ts:140` | renderer graph sync 写入 | liveness filter、orphan recovery、hibernation、remote sleep | 低(sync 更新) | ❌ | ★ 内部用 |
| 3 | **incarnationId** | `string\|null` | `pty.incarnationId` `orca-runtime.ts:25433` | daemon controller 报告时 | orphan 领养 claim、topology 校验 | 低 | ❌ | ★ 内部用 |
| 4 | **orphaned** | `boolean` | leaf 路径恒 `false`；PTY 路径实时计算 `orca-runtime.ts:26713` | **每次 list 实时计算** | 孤儿恢复：web/mobile 发现失去 renderer 绑定的 PTY 并重新领养 | 无(实时) | ❌ | ★★ 孤儿标识 |
| 5 | **worktreeId** | `string` `<repoId>::<path>` | `RuntimeSyncedLeaf.worktreeId` `runtime-types.ts:135` | PTY 注册时一次，不可变 | worktree 过滤、orchestration 路由、拓扑修订、join key | 无(不可变) | ✅(join) | — |
| 6 | **worktreePath** | `string` | `worktreesById.get().path` `orca-runtime.ts:25436` | list 时从 worktree 缓存解析 | CLI 展示、visual layouts | 低(带过期缓存+fallback) | ✅ (cwd) | — |
| 7 | **branch** | `string` | `worktreesById.get().branch` `orca-runtime.ts:25437` | list 时解析；空=未追踪 git | CLI 展示 | 低(同缓存) | ✅ | — |
| 8 | **tabId** | `string` | leaf: renderer；orphan PTY: 合成 `pty:<ptyId>` `orca-runtime.ts:26722` | sync/spawn 时 | handle 身份组成、orphan 领养、visual layouts | 无(不可变) | ❌ | ★ 内部用 |
| 9 | **leafId** | `string` | leaf: renderer；orphan: 合成 `orca-runtime.ts:26723` | sync/spawn 时 | handle 身份、visual layouts | 无(不可变) | ❌ | ★ 内部用 |
| 10 | **title** | `string\|null` | `getLatestLeafTitle()` `orca-runtime.ts:32304` 取 paneTitle/oscTitle/tabTitle 中**时间最新的** | **实时更新**：OSC escape、pane title change、controller update | agent 检测 (classifyAgentTitle)、dashboard 展示、worktree 状态推断 | 中(OSC 驱动,块间有滞后) | ✅ | — |
| 11 | **connected** | `boolean` | leaf: `leaf.connected`；PTY: 每次 list `refreshPtyWorktreeRecordsFromController()` 查 daemon `orca-runtime.ts:25206` | **每次 list 调用刷新** | liveness gate、orchestration dispatch、hibernation、remote sleep | 低(每次 list 查 daemon) | ✅ | — |
| 12 | **writable** | `boolean` | `graphStatus===ready && connected && ptyId!==null` `orca-runtime.ts:26917` | list 时派生 | `terminal send` 的写入门控 | 低(派生) | ❌ | ★★ 能否发消息 |
| 13 | **lastOutputAt** | `number\|null` epoch ms | 每次 PTY data chunk 时 `Date.now()` `orca-runtime.ts:7493,7530` | **每个输出字节都更新**（单调递增） | staleness 检测 (`AGENT_STATUS_STALE_AFTER_MS`)、worktree active/inactive 判定、terminal wait readiness | 无(单调) | ❌ | ★★★ **最近活动时间** |
| 14 | **preview** | `string` | `buildPreview()` `orca-runtime.ts:30593` 从 tail buffer 取最多 6 行 300 字符，ANSI/OSC 已剥离 | 每次 data chunk 更新 | CLI 展示、worktree status | 低(per-chunk) | ❌ | ★★★ **终端末尾画面** |

### 关键结论

- **没有独立 tab name 字段**。`title` 就是 tab title（agent 通过 OSC escape 设置："Pi"、"OMP ready"、shell prompt）。一个 tab 可有多 pane（leaf），tabId 是 UUID 非人类可读。
- **lastOutputAt** 是 PTY 最后输出字节的时间（毫秒精度），不是 agent 响应时间。agent 思考时吐 token = 有输出 = 时间在更新。**不依赖 hook，每次 list 都有**。
- **connected** 每次 list 调用时查 daemon controller，不是缓存值。
- 两 leaf 共享同一 PTY → 同一 handle。

---

## 二、last-status.json — hook 事件快照

**文件位置**: `~/.config/orca/agent-hooks/last-status.json`
**写入管线**: hook POST → `resolveHookSource()` 路由 → `normalizeHookPayload()` 归一化 → `applyNormalizedStatus()` enrich → `cacheStatusEntry()` upsert → `scheduleStatusPersist()` **250ms debounce** → `runStatusPersist()` **原子写** (tmp+rename)
**启动恢复**: `hydrateLastStatusFromDisk()` (`server.ts:2006`) 丢弃 7 天以上条目 (`HYDRATE_MAX_AGE_MS`)
**上限**: 文件 16MB、500 panes (`MAX_AGENT_HOOK_STATUS_CACHE_PANES`)、1M 结构 token

### 2.1 顶层字段

| # | 字段 | 类型 | 来源 (file:line) | 用途 |
|---|---|---|---|---|
| T1 | **version** | `number` | `LAST_STATUS_FILE_VERSION=2` `server.ts:138` | schema 版本控制，mismatch 视为损坏 |
| T2 | **entries** | `Record<paneKey, Entry>` | `server.ts:139` | 所有 pane 的最新 hook 事件快照 |

### 2.2 entry 级字段（每个 paneKey）

| # | 字段 | 类型 | 来源 (file:line) | 写入时机 | Orca 业务用途 | hub-tui 当前 | hub-tui 可加 |
|---|---|---|---|---|---|---|---|
| E1 | **paneKey** | `string` `tabId:leafUuid` | `agent-hook-listener.ts:275` | 每次 hook 事件 | 稳定复合 key 标识 layout leaf（≤200 字符） | ✅(join) | — |
| E2 | **source** | `string` (17 值，见下) | `resolveHookSource()` 从 URL pathname 推导 `agent-hook-listener.ts:4026` `HOOK_SOURCE_BY_PATHNAME:4006` | 每次 hook 事件 | 路由到对应 agent normalizer | ✅ | — |
| E3 | **tabId** | `string` | hook body `agent-hook-listener.ts:280` | 每次 hook 事件 | tab 归属、关闭 tab 时清理状态 | ❌ | ★ |
| E4 | **worktreeId** | `string` (≤16KB) | hook body `agent-hook-listener.ts:281` | 每次 hook 事件 | worktree 归属 | ✅(join) | — |
| E5 | **connectionId** | `string\|null` (≤1KB) | `ingestRemote()` `server.ts:1514` | relay/SSH 事件时设置 | SSH 连接归属，本地 hook 恒 null | ❌ | ★ 远程用 |
| E6 | **hookEventName** | `string` | hook body: `hook_event_name`/`hookEventName`/`hook_type` `agent-hook-listener.ts:3992` | 每次 hook 事件 | transition guard: permission stickiness、interrupt suppression | ❌ | ★★ 最近事件 |
| E7 | **providerSession** | `{key,id,transcriptPath?}` | `extractAgentProviderSession()` `agent-session-resume.ts:171` | 每次 hook 事件 | **agent resume**: `claude --resume <id>` | ❌ | ★★ 恢复 session |

#### source 全部 17 值

| source | URL pathname | agent CLI |
|---|---|---|
| claude | /hook/claude | Claude Code |
| codex | /hook/codex | Codex CLI |
| gemini | /hook/gemini | Gemini CLI |
| antigravity | /hook/antigravity | Antigravity |
| amp | /hook/amp | Amp |
| opencode | /hook/opencode | OpenCode |
| mimo-code | /hook/mimo-code | Mimo Code |
| cursor | /hook/cursor | Cursor |
| pi | /hook/pi | Pi |
| omp | /hook/omp | OMP |
| droid | /hook/droid | Droid |
| command-code | /hook/command-code | Command Code |
| grok | /hook/grok | Grok |
| copilot | /hook/copilot | Copilot |
| hermes | /hook/hermes | Hermes |
| devin | /hook/devin | Devin |
| kimi | /hook/kimi | Kimi |

### 2.3 payload 字段（`ParsedAgentStatusPayload` `agent-status-types.ts:154`）

| # | 字段 | 类型 | 来源 (file:line) | 写入时机 | Orca 业务用途 | hub-tui 当前 | hub-tui 可加 |
|---|---|---|---|---|---|---|---|
| P1 | **state** | `enum` 4 值 | per-agent normalizer `agent-status-types.ts:17` | 每次 hook 事件 | dashboard 状态色、staleness 判定 | ✅(原始值) | ★★★ **状态图标** |
| P2 | **prompt** | `string` | `extractPromptText()` `agent-hook-listener.ts:398` | 新 turn 时设置；tool-use 省略时持久化 | dashboard 最近用户输入 | ❌ | ★★ 最近 prompt |
| P3 | **agentType** | `string` (20 值，≤40 字符) | per-agent normalizer `agent-status-types.ts:20` | 每次 hook 事件 | agent 分类 | ✅(=source) | — |
| P4 | **toolName** | `string` (≤60 字符) | `extractToolFields()` `agent-hook-listener.ts:2305` | tool 事件时；新 turn reset | dashboard 当前工具 | ❌ | ★★★ **最近工具** |
| P5 | **toolInput** | `string` (≤160 字符) | `extractToolFields()` | tool 事件时 | dashboard 工具输入预览 | ❌ | ★★ 工具输入 |
| P6 | **lastAssistantMessage** | `string` (≤8000 字符) | hook body `message`/`assistant_message` | done 事件；缺失时 retry 5×50ms | dashboard AI 最近回复 | ❌ | ★★ AI 回复摘要 |
| P7 | **model** | `string` (≤120 字符) | hook body `model`（Codex/Devin） | 有 model 字段时 | dashboard 模型名 | ❌ | ★ 模型显示 |
| P8 | **interrupted** | `boolean?` | done 事件 `is_interrupt=true`；fallback `inferInterrupt()` `server.ts:831` | done 事件 | interrupt 后抑制 15s late tool hooks | ❌ | ★ 中断标识 |
| P9 | **subagents** | `AgentSubagentSnapshot[]?` (≤32) | Claude/Codex roster `agent-status-types.ts:63` | subagent 事件 | dashboard 子 agent 面板 | ❌ | ★★ 子 agent |

### state 状态机（`AgentStatusState` `agent-status-types.ts:17`）

```
正常流:     working → (waiting/blocked)* → working → done
被打断:     working → done (interrupted=true)
权限粘性:   working → waiting → working (同工具时保持 waiting,Claude 专属)
Done 门控:  done + 活跃子 agent → working (Claude/Codex)
中断抑制:   interrupted done 后 15s 内 late tool hooks 被抑制
```

| state | 含义 | 触发事件示例 |
|---|---|---|
| **`working`** | agent 正在执行 | UserPromptSubmit / PreToolUse / PostToolUse / agent_start / tool_call |
| **`waiting`** | 需要用户注意（权限/问题） | PermissionRequest / AskUserQuestion |
| **`blocked`** | 阻塞等待用户输入 | ask_user_question 工具 (Pi/Copilot) |
| **`done`** | agent 完成一轮 | Stop / agent_end / SessionIdle / sessionEnd |

### 内部字段

| # | 字段 | 类型 | 来源 | 用途 |
|---|---|---|---|---|
| I1 | **receivedAt** | `number` | `attachStatusTiming()` `server.ts:1058` | 最新事件到达时间 |
| I2 | **stateStartedAt** | `number` | `attachStatusTiming()` | 当前状态首次出现的时刻 |

---

## 三、orchestration.db — 5 表

**位置**: `~/.config/orca/orchestration.db` (SQLite WAL, schema v6)

### 3.1 messages 表

| # | 列名 | 类型 | Source(file:line) | 用途 | hub-tui |
|---|---|---|---|---|---|
| M1 | **id** | TEXT PK | `db.ts:104` | `msg_<hex12>` | ★★ |
| M2 | **from_handle** | TEXT NOT NULL | `db.ts:105` | 发送者 handle；lifecycle 权威检查 | ★★★ |
| M3 | **to_handle** | TEXT NOT NULL | `db.ts:106` | 接收者 handle 或 `@all`；inbox 索引 | ★★★ |
| M4 | **subject** | TEXT NOT NULL | `db.ts:107` | 简短摘要 | ★★★ |
| M5 | **body** | TEXT DEFAULT '' | `db.ts:108` | 详细内容 | ★★★ |
| M6 | **type** | TEXT DEFAULT 'status' | `db.ts:109` | 8 种类型 | ★★★ |
| M7 | **priority** | TEXT DEFAULT 'normal' | `db.ts:114` | normal/high/urgent | ★★ |
| M8 | **thread_id** | TEXT | `db.ts:116` | 线程关联 | ★★★ |
| M9 | **payload** | TEXT (JSON) | `db.ts:117` | 结构化数据 | ★★★ |
| M10 | **read** | INT DEFAULT 0 | `db.ts:118` | 0=未读 1=已读 | ★★★ |
| M11 | **sequence** | INT AUTOINCREMENT | `db.ts:119` | FIFO 排序 | ★ |
| M12 | **created_at** | TEXT | `db.ts:120` | UTC 时间戳 | ★★ |
| M13 | **delivered_at** | TEXT | `db.ts:121` | 投递时间戳 | ★★ |
| M14 | **sender_pane_key** | TEXT | `db.ts:122` | remint-stable pane 身份 | ★ |

#### message type 详解

| type | 谁发 | 何时 | payload |
|---|---|---|---|
| **status** | 任意 agent | 进度更新 | — |
| **worker_done** | worker | 任务完成（成功/失败） | `{taskId,dispatchId,filesModified[],completedAt}` |
| **heartbeat** | worker | 存活信号，每 5min，phase ∈ investigating/implementing/reviewing/waiting | `{taskId,dispatchId,phase}` |
| **escalation** | worker | 阻塞/失败通知 | `{taskId}` |
| **decision_gate** | worker (via ask) | 需要人工输入 | `{question,options[]}` |
| dispatch/handoff/merge_ready | — | 预留 | — |

### 3.2 tasks 表

| # | 列名 | 类型 | 用途 | hub-tui |
|---|---|---|---|---|
| T1 | **id** | TEXT PK | `task_<hex12>` | ★★★ |
| T2 | **parent_id** | TEXT | 父任务层级 | ★★ |
| T3 | **created_by_terminal_handle** | TEXT | 创建者 | ★★ |
| T4 | **task_title** | TEXT | 人类可读标题 | ★★★ |
| T5 | **display_name** | TEXT | UI worker 标签 | ★★★ |
| T6 | **spec** | TEXT NOT NULL | 完整任务规格 | ★★★ |
| T7 | **status** | TEXT DEFAULT 'pending' | 6 种状态 | ★★★ |
| T8 | **deps** | TEXT DEFAULT '[]' | JSON 依赖数组 | ★★★ |
| T9 | **result** | TEXT | 完成结果 JSON | ★★★ |
| T10 | **created_at** | TEXT | 创建时间 | ★★ |
| T11 | **completed_at** | TEXT | 完成时间 | ★★ |

#### task status 生命周期

| status | 含义 | 下一步 |
|---|---|---|
| `pending` | 等待依赖 | → ready |
| `ready` | 可派发 | → dispatched |
| `dispatched` | 已分配 terminal | → completed/failed/blocked |
| `completed` | 成功完成 | 终态 |
| `failed` | 永久失败（3 次熔断） | 终态 |
| `blocked` | 等待人工决策 | → ready (gate resolved) |

### 3.3 dispatch_contexts 表

| # | 列名 | 类型 | 用途 | hub-tui |
|---|---|---|---|---|
| D1 | **id** | TEXT PK | `ctx_<hex12>` dispatch 实例 | ★★★ |
| D2 | **task_id** | TEXT NOT NULL | FK → tasks.id | ★★★ |
| D3 | **assignee_handle** | TEXT | 被分配 terminal | ★★★ |
| D4 | **assignee_pane_key** | TEXT | remint-stable pane | ★★ |
| D5 | **status** | TEXT DEFAULT 'pending' | 5 种 | ★★★ |
| D6 | **failure_count** | INT DEFAULT 0 | **熔断器** ≥3 circuit_broken | ★★★ |
| D7 | **last_failure** | TEXT | 失败原因 | ★★ |
| D8 | **dispatched_at** | TEXT | 派发时间 | ★★ |
| D9 | **completed_at** | TEXT | 完成时间 | ★★ |
| D11 | **last_heartbeat_at** | TEXT | 最近心跳（10min stale 检测） | ★★★ |

#### 心跳机制
- worker 每 **5min** 发 heartbeat
- coordinator 检查 **10min 无心跳** → warn（不自动 fail）

#### 熔断器
- failure_count **跨重试累积**
- 阈值 **3 次** → circuit_broken → task 永久 failed

### 3.4 decision_gates 表

| # | 列名 | 类型 | 用途 | hub-tui |
|---|---|---|---|---|
| G1 | id | TEXT PK | `gate_<hex12>` | ★★★ |
| G2 | **task_id** | TEXT NOT NULL | 关联任务 | ★★★ |
| G3 | **question** | TEXT NOT NULL | 待决问题 | ★★★ |
| G4 | options | TEXT DEFAULT '[]' | JSON 选项 | ★★ |
| G5 | **status** | TEXT DEFAULT 'pending' | pending/resolved/timeout | ★★★ |
| G6 | **resolution** | TEXT | 人类回答 | ★★★ |
| G7 | created_at | TEXT | 创建时间 | ★★ |
| G8 | resolved_at | TEXT | 解决时间 | ★★ |

### 3.5 coordinator_runs 表

| # | 列名 | 类型 | 用途 | hub-tui |
|---|---|---|---|---|
| R1 | id | TEXT PK | `run_<hex12>` | ★★★ |
| R2 | spec | TEXT NOT NULL | 编排规格 | ★★ |
| R3 | **status** | TEXT DEFAULT 'idle' | idle/running/completed/failed | ★★★ |
| R4 | coordinator_handle | TEXT NOT NULL | 协调者 handle | ★★ |
| R5 | poll_interval_ms | INT DEFAULT 2000 | tick 间隔 | ★★ |

---

## 四、Coordinator poll-loop 字段消费

每 **2s** 一轮 `tick()` (`coordinator.ts:237`)，6 个原子子步：

| 步骤 | 读 | 写 | 关键字段 |
|---|---|---|---|
| 1. processMessages | `messages WHERE to_handle=coord AND read=0` | messages.read=1; tasks.status; dispatch.status | msg.type 分发 |
| 2. processEscalations | — | — | no-op |
| 3. processDecisionGates | `gates WHERE status=pending` | tasks → blocked | gate.task_id |
| 4. warnStaleDispatches | `dispatch WHERE status=dispatched AND old` | — (warn only) | dispatched_at; last_heartbeat_at |
| 5. dispatchReadyTasks | `tasks WHERE status=ready` | dispatch INSERT; tasks=dispatched | deps; spec→preamble |
| 6. checkConvergence | `getTaskStatusCounts()` | run.status | total === completed + failed |

---

## 五、hub-tui 字段增强优先级

### P0（最有价值，数据已验证有值）

| 字段 | 数据源 | 效果 |
|---|---|---|
| **elapsed** | terminal.lastOutputAt → `now - lastOutputAt` | 哪个 agent 最近活跃 |
| **toolName** | last-status.payload.toolName | agent 在干什么 |
| **state 图标** | last-status.payload.state → ⠋/✓/⚠/⏸ | 替代原始字符串 |

### P1（中等价值）

| 字段 | 数据源 | 效果 |
|---|---|---|
| preview | terminal.preview | 终端末尾画面 |
| toolInput | payload.toolInput | 工具输入预览 |
| model | payload.model | 模型名 |
| writable | terminal.writable | 能否发消息 |
| orphaned | terminal.orphaned | 孤儿终端 |

---

**源码版本**: stablyai/orca v1.4.151+ (`~/tools/orca/`)
**分析日期**: 2026-08-10
