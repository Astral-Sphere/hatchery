# 设计：Runtime Daemon（hatchery-daemon）

> 状态：设计稿。依据 ADR-0001（全协议化）、ADR-0002（单写者）。

## 1. 职责

daemon 是系统唯一的 runtime 所有者与数据写者：

- 监听 UDS + stdio，说 `hatchery-protocol`。
- 会话管理：创建/加载 runtime、租约、代际号。
- Live hub：事件扇出到多前端，迟加入 replay。
- 装配：按模式组装工具注册表、按会话来源绑定能力后端（ADR-0004 的表）。
- 持有 store writer actor 与 CheckpointStore。

## 2. 单实例与发现

```
~/.local/state/hatchery/daemon/
├── daemon.lock        # advisory 文件锁（fs2），持锁者即活跃 daemon
├── daemon.json        # { pid, uds_path, protocol_version, started_at }
└── hatchery.sock      # UDS
```

- **attach-or-spawn**：客户端先读 daemon.json + 尝试连接；失败则抢锁 spawn（fork/daemonize 或 systemd socket activation，二选一 M1 定）再 attach。竞态由锁保证。
- UDS 权限 0700（用户级隔离）；`daemon.json` 里带随机 boot token，客户端连接时出示（防同机其他用户进程伪装前端——UDS 权限已挡，token 是纵深防御）。
- CLI `--embedded`：同进程内起 DaemonCore，走内存 transport（协议不变）。

## 3. 内部结构

```rust
pub struct DaemonCore {
    sessions: SessionManager,
    hub: LiveHub,
    store: StoreHandle,          // writer actor 的发送端
    checkpoints: CheckpointRegistry, // per-workspace CheckpointStore
    config: Arc<LayeredConfig>,
}

struct SessionSlot {
    runtime: Arc<SessionRuntime>, // kernel Agent 句柄 + 任务 + 在途标记
    generation: u64,              // 装配即 +1 并落库（不变量 1）
    last_activity: Instant,       // 空闲清扫的比较基准
}

// 订阅计数在 slots 之外单独记账：订阅先于首个 prompt 到达（先 session/new 后 prompt），
// 计数必须活过它挂靠的那个 runtime（否则空闲清扫会掐掉正被观看的会话）。
// 规划中的 per-session 文件锁（SessionLease）在 M1 不存在：单 daemon 内由
// turn 闸门 + 在途标记 + TurnInProgress 拒绝承担，跨 daemon 由单实例锁承担 —— 文件锁随
// 多 daemon 形态出现时再引入（2026-10-07 裁决：不挂 M2，理由见 worklog/daemon.md）。
```

- **runtime 生命周期**：runtime 在**首个 prompt 时惰性装配**（M1 实现与规划不同点：`session/new|load` 只建会话行），builder 注入 provider/tools/history/sink；turn 之间 runtime 常驻内存；空闲超时（默认 30min 无订阅者且无进行中 turn）落盘卸载；清扫在卸载前于锁内复核 busy/watched，清扫对象也包括 agent 已死的 slot。
- **system prompt 注入（M2 Phase 0）**：装配好的 prompt 经 daemon 的 `HistorySource` 实现进入请求——`StoreHistory::view()` 在分支历史之前**前置一条 system `Message`**。`ChatOptions` **有意不设** system prompt 字段：它是「一次请求的旋钮」（model/effort/temperature/max_output_tokens/tool_defs/extra），而 `tool_defs` 每轮被 kernel 用冻结的 tool snapshot 覆写，把 prompt 塞进去等于开一个每轮都可能与历史不一致的旁路。**渲染时机是「每次 runtime 装配渲染一次，冻结该 runtime 的整个生命周期」**（D15）：模式切换与 config 变更本来就 bump generation 并重组装，所以冻结不会让 prompt 变陈旧；反之每轮重渲染会让 environment 节的日期/cwd 破坏请求前缀的稳定性，而那正是 KV cache 友好性反复要求的东西。随之，**不变量 2 的边界只管分支历史**——system prompt 是可复现的派生态，由 `prompt/render` 的 golden 单独钉。
- **代际号**：runtime 每次（重）建 generation+1 并落库；所有事件带 generation，hub 与前端各自丢弃旧代事件；装配以 `GenerationBumped` 开场通知前端重置视图。
- **并发接受**：同一会话的 prompt 路径（装配→busy 检查→提交）走 per-session turn 闸门串行化，接受即在 runtime 上打在途标记（kernel TurnEnded 投影时清除）——第二条 prompt 无论落在装配期还是提交与开闸之间的窗口，都得到 `TurnInProgress` 拒绝，绝不静默丢弃或排队。
- **审批往返的 daemon 半边（M2 Phase 2）**：kernel 侧的机器已经齐了（`ToolHost::approval_for` 产请求、`KernelEvent::ApprovalNeeded` 发出后 turn 停在 `AwaitingApproval`、`AgentCommand::decide` 是回答它的唯一入口），daemon 要补三件：① **pending 请求注册表**，键是 `request_id`、值指回 session/runtime——没有它，`approval/respond` 收到的 id 无处投递；② **`approval/respond` 路由到 `AgentCommand::decide`**；③ **fail-closed 超时，住在 gate 而不是 kernel**——超时即拒绝（不是即允许），且 kernel 的状态机不需要知道「等多久」这种策略。规则求值语义与持久化是 **D8**，预览载荷形状是 **D14**。
- **崩溃恢复**：daemon 重启后先以 `open_turns` 找出所有未结 turn 行、以失败形态关闭（`ended_at` 置位、`stop_reason` 保持 NULL —— "仍在跑"与"失败"可区分，不造假原因），再把 status=running/waiting_approval 的会话标记回 idle。不自动续跑（v1）。

### 3.1 Profile 化装配与启动审计（ADR-0009）

> **状态（2026-10-07 勘察）：下表是目标形态，不是现状——`Profile` 类型不存在。** `hatchery-daemon` 里 `profile` 只命中 src/lib.rs 的一句文档注释（"Assembly is profile-based and audited at startup"），Cargo.toml 的 `description` 也写着 profile-based，两处都是愿景。**M2 只交付其中一个可机器验证的落点：按会话来源选择后端的 `Backends` 装配点**（今天 `Backends{fs, terminal}` 在 `manager.rs` 的 `chat_tools()` 里内联构造，没有任何选择逻辑，且不读 `session.mode`），外加 `FsBackend`/`TerminalBackend`/`ApprovalGate` 的契约测试套件（见 capabilities.md §1、testing.md §2）。**四个命名 profile 顺延到出现第二个消费者**——这正是 ADR-0009 反预拆分刹车的适用场景：只有一个消费者时，profile 表是一张没有读者的数据；其中 `acp-stdio`/`acp-standalone` 两行的消费者是 **M3** 的 ACP server，绑定表的 ACP 三行（`AcpClientFs`/`AcpClientTerminal`/`AcpPermission`）在 M2 只作为有文档的接缝留着。
>
> 启动审计（fail-loud）本身 M1 已落地，但它是**按必需组件清单**核对（provider 缺 key / 数据目录 / 状态目录，一次列全后拒绝服务），不是按 profile 声明核对——profile 出现后，「必需组件」的来源换成 profile 表，审计逻辑不变。

daemon 启动按命名 **profile** 装配组件捆绑，装配表是显式数据（不是散落的 if/else）：

| profile | 监听器 | 能力后端 | 工具集 | 场景 |
|---|---|---|---|---|
| `local` | UDS | Local 三件套 | 按模式 | 常驻 daemon（CLI/GTK attach） |
| `headless` | stdio | Local 三件套 | 按模式 | `hatchery exec` embedded |
| `acp-stdio` | stdio(ACP) | 按宿主能力协商（ADR-0004 矩阵） | Code 全量 | `hatchery acp` attach 常驻 daemon |
| `acp-standalone` | stdio(ACP) | 同上 | 同上 | 宿主 spawn 的自包含进程 |

- **启动审计（fail-loud）**：装配完成后核对 profile 声明的必需组件（provider 可用、密钥环境变量存在、监听器绑定成功、store 迁移完成）；任一未 resolve → 拒绝服务，输出缺失清单后退出（非零码）。dsh `auditStartupEntries` 语义，杜绝「运行时永久 PENDING」。
- 注册句柄：profile 内的组件注册（provider adapter、MCP 连接等）返回 `Handle`（disposer + 原子 `replace()`），运行中换 provider 走整表原子替换（见 capabilities.md §1）。

## 4. Live Hub（事件扇出）

- per-session broadcast channel（tokio::sync::broadcast，容量如 4096）；订阅者 = 协议连接。
- **coalescing**（规划，**M3**；M1 未实现）：TextDelta/ReasoningDelta 在 hub 入口按时间窗（~16ms）合并成批量事件；ItemFinished 等控制事件立即发。`ServerEvent::is_coalescable` 已预留分类，但它只覆盖 `text_delta`/`reasoning_delta`，而 M2 新增的事件量主要来自 `ToolCallProgress`——**它不可合并**，所以 coalescing 治不了 M2 的病，2026-10-07 由 M2 顺延到 M3。**M2 只保留测量任务**：跑一次 Code 会话、把事件量记进 worklog/daemon.md（hub.rs 的模块文档当初写的就是「这些测量决定策略」）——测量先于策略，别在没有数据时先建缓冲。
- **replay window**（规划，**M3**；M1 未实现）：hub 保留最近 N 事件供瞬时断线重连补发。重连前端今天走 `session/load` 全量重建 + `replay_from` 补差（在 manager 层解析游标、只返回其后 item，游标不在活动分支上按 `InvalidRequest` 拒绝），且已有 e2e 覆盖——这条路径**已经取代**了 replay window 的用途，2026-10-07 同批顺延到 M3（届时是否还需要，取决于上面那次测量）。
- 慢消费者：broadcast lag 时断开该订阅者并让其走 load 重建（不阻塞其他前端）。

## 5. 与其他 crate 的接线

| 交互 | 机制 |
|---|---|
| kernel → daemon | `EventSink` 实现：事件 → hub 扇出 + store 落库（item 边界） |
| kernel → daemon → store（检查点，M2 Phase 1） | HubSink 的 `ItemFinished` 臂：kernel 追加的 `ItemKind::Checkpoint{commit_id, kind}` item 先 `append_item` 落库、成功再 publish（顺序不变）；**落库成功后补写 `checkpoints` 表一行**（见下） |
| kernel → 外界 | 装配好的 Backends（本地或 ACP 委派） |
| ACP server | daemon 内的适配层：ACP 连接 ↔ 一个 SessionSlot；`hatchery acp` 子命令 = 单连接精简 daemon（stdio） |
| ACP client | 作为 `subagent` 工具的实现被 runtime 调用，生命周期挂在该 turn 上 |
| CLI/GTK | 纯协议客户端；共用 `hatchery-protocol` 里的 client helper（连接、重连、订阅、generation 过滤） |

**检查点行怎么写（M2 Phase 1，D13 的 daemon 半边）**：写路径是 `LocalFs` 写前 push 到 `ToolCtx` 的收集器 → `ToolInvocation` 带出 `Vec<Checkpoint>` → **kernel 在 ToolResult item 之前追加 Checkpoint item**（它本来就在造 ToolCall/ToolResult item，用同一套机器，顺序天然正确；链成 `… → ToolCall → Checkpoint → ToolResult`，而工具结果靠 `ToolResult.call` 配对而非父子关系，不受影响）→ daemon 的 HubSink 在该 item 落库后补写 `checkpoints` 行。三条约束：

- **补写失败只记 error 日志，不影响 item 与事件**——`commit_id` 已经在 item 里，行属于可事后重建的核算数据；反过来，item 落库失败时沿用现有的「扣发 `ItemFinished`」，绝不发布一个 store 里没有的 item（不变量 2 的顺序）。
- **这张表不是 rewind 的主索引**。rewind 的 Code scope 走 `rebuild_chain` → 定位 target_item → 向后扫第一个 Checkpoint item → 读它自带的 `commit_id` → `restore`；扫不到即 no-op（pre-write 快照恰好等于 target_item 时刻的工作区状态）。`checkpoints` 表只服务**跨会话的磁盘预算核算与 GC**（开放问题 3 / D9）。
- **`item_id` 可空正是为 rewind 的安全快照留的**：`RewindScope::Both`/Code 在 restore 之前打的那一次快照写进 `checkpoints` 表、`item_id = NULL`、**不建 item**——它是 undo-of-undo，不属于对话历史，不该出现在任何前端的转录里。

## 6. 可观测性

- 结构化日志（tracing），级别/输出可配；默认 `~/.local/state/hatchery/daemon.log` 轮转。
- `hatchery daemon status` 子命令：版本、uptime、会话数、DB 大小、检查点磁盘占用。
- otel 遥测：接口预留（feature gate），v1 默认关。

## 开放问题

1. ~~daemonize 方式：双 fork vs `sd_notify` socket activation vs 前台进程 + CLI spawn 等待握手~~ → **已定（2026-10-01，D1）**：**CLI spawn 分离进程**（unix `setsid` / Windows `DETACHED_PROCESS`，`process_group(0)` 脱离前台进程组），握手靠 CLI 轮询 `daemon.json` 与 socket 出现；**不做双 fork、不依赖 `sd_notify`**——双 fork 的复杂度与 sd_notify 的依赖都不值得，而握手代码本来就在 CLI 里。systemd 用户直接跑前台的 `hatchery daemon run`。实现在 `entry.rs` 与 CLI 的 `attach_or_spawn`；证据与取舍见 worklog/daemon.md。
2. ~~daemon 空闲退出策略与「进行中 turn 但无订阅者」的取舍（跑完 vs 暂停）~~ → **已定（2026-10-01，D2）**：**跑完为止**，结果无论如何落库。杀掉一个没人看着的 turn 等于烧掉已经付费的推理。空闲卸载因此有双守卫（busy 不扫、被看的不扫，且卸载前在槽位锁内复核）；`invariant_an_unwatched_turn_runs_to_completion_and_persists` 钉住这条（Phase 0 补前缀，见 design/testing.md §5）。
3. 多工作区并发会话共享 CheckpointStore 的锁粒度（per-workspace mutex 已定，跨 workspace 的全局磁盘预算核算频率）——**M2（Phase 1），决策点 D9**。锁粒度那半已经有答案：**daemon 内 per-workspace 互斥**（ADR-0006），不是跨进程文件锁——`SessionLease` 一条 2026-10-07 已顺延到「多 daemon 形态出现时」（`--embedded` 无实现、单实例 `daemon.lock` 已挡双 daemon，理由见 worklog/daemon.md）。留给 D9 的是预算本身：超预算时 **GC 最旧** vs **拒绝写入**，以及核算频率（每次快照后同步核 vs 后台定期核）；`checkpoints` 表就是这项核算的数据来源（§5）。spike 数据已备，D9 在 Phase 1 内定稿并写回本条。
4. ~~boot token 的轮换时机（daemon 重启即换 vs 定期）~~ → **已定（2026-10-01，D4）**：**每次 daemon 启动换新 token**，写进 0600 的 `daemon.json`（write-then-rename），客户端 attach 时现读；不做定期轮换——token 是 UDS 权限之外的纵深防御，而 UDS 权限已经挡住同机其他用户，定期轮换只增加「轮换窗口内前端连不上」这一种失败形态。死 pid 判定 best-effort 且无 unsafe（Linux 走 `/proc`，其余平台保守视为活、由 hello 握手兜底）。
5. ~~配置变更要不要重组装活着的 runtime？~~ → **已定（2026-10-07，D19）** 今天不重组装：全仓库唯一的卸载路径是空闲清扫（`sweep_after` → `unload`），`session/set_config`（CLI 的 `/model` `/effort` 走的就是它）只改库里的行并发 `SessionUpdated`，`config/set` 只改 `LayeredConfig` 本身。而 provider adapter、`ChatOptions` 与 prompt 都是在 `assemble` 里绑死的，所以 `/model` 的变更**下次装配才生效**——用户敲完 `/model x`，下一轮请求仍然发给旧模型，且没有任何提示。prompt 冻结（D15）与之一致，不是它引入的问题。

**`/effort` 比这更糟：它永远不生效（2026-10-07 live 实测）。** `session/set_config` 把 effort 写进 `session.config_patch`（manager.rs:391），而 `config_patch` 在 daemon 侧**没有任何读者**——读它的只有 CLI，用来画状态栏（chat.rs:91、tui/mod.rs:506）。provider 配置里的 `reasoning.reasoning_effort`（builtin 给 deepseek 与 qwen 都是 High）同样不进 turn：`runtime.rs:415` 用 `ChatOptions::new(model)`，`reasoning_effort` 恒为 `None`（message.rs:287），而 `translate.rs:75` 的 `apply_effort` 遇 `None` 直接 return。全仓库唯一喂 effort 的生产代码是 `doctor.rs:272`（探测的两轮）。实测：TUI 里 `/effort off` 之后状态栏确实变成 `effort off`，下一轮推理照旧流式出现。后果是 llm 侧 `ReasoningWire` 的三种拼法（`Effort` / `QwenThinking` / `ThinkingSwitch`，都已实现且有单测）在正常 turn 上从未被触发过——这是与 system prompt 同一类的第四处「机制建好了没接线」，Phase 0 的勘察没扫到它，因为勘察沿着 prompt 走而不是沿着 effort 走。三个约束决定了解法不能是「`set_config` 里调 `unload`」：① `unload` 对被 watch 的会话直接拒绝，而改配置的恰恰是附着中的前端；② 重组装会 bump generation，前端因此收到 `GenerationBumped` 并要重建视图——为一个 model 字段付这个代价太大；③ 正在跑的 turn 绝不能被抽掉 runtime（D2）。倾向的方向是**把 per-turn 可变的部分从装配里拿出来**（provider 解析与 `ChatOptions` 在 turn 开始时读一次），而 prompt 仍按 D15 冻结、只在重组装时换。**D19 的分界（已实现）**：

- **跟着 turn 走**：model 与 reasoning effort。`SessionManager::turn_options` 在 `prompt()` 里、拿闸门之前解析一次，随 `AgentCommand::TurnInput { options }` 交给 kernel；kernel 用它覆盖装配时的 `ChatOptions`，但 `tool_defs` 例外——它始终取自冻结的 `ToolHost::snapshot`，所以一轮可以换模型与 effort，永远换不到「模型被广告的工具表」与「调用被派发到的工具表」不一致。effort 的优先级是 session patch（`/effort` 写的那份）> provider 配置的默认；patch 里的值解析不出来时记 warning 并落回默认，不静默当成「无偏好」。
- **跟着 runtime 走**：provider adapter、能力表与 reasoning echo 标志（三者同属一个 provider，不能分开换），以及 prompt（D15）。所以 `/model` 换到**另一个 provider** 时必须重组装：`ensure_runtime` 比较 session 当前的 provider 与 runtime 记录的那个，不同就 `shutdown()` 旧的再装配，`GenerationBumped` 照常广播——前端本来就要为它重置视图。**turn 正在跑时不重组装**：那会为了一个 model 字段杀掉一个没人看着也必须跑完的 turn（D2），此时 prompt 按 `TurnInProgress` 被拒，切换落在下一轮。
- **仍无消费者**：`ConfigPatch.overrides`（自由格式的 per-session 覆盖）今天既没有生产者（CLI 恒发 `None`）也没有消费者（daemon 侧无人读 `config_patch`），所以它和 effort 曾经一样只是存着。按 ADR-0009 的反预拆分刹车，等有第一个生产者再定它作用于装配还是 turn。

`session/set_mode`（Phase 3）因此是**重组装**路径：换模式要换工具表、审批策略与 prompt 变体，三者都跟着 runtime 走。
6. **订阅登记与 load 快照之间有一个窗口，落在里面的 item 会显示两次（2026-10-07 由 CLI 的 item 投影发现）**。`EventStream::subscribe` 先登记订阅者、再取 `session/load` 的快照，所以在这两步之间提交的 item 既在回复的 `items` 里、又会作为一个 live `ItemFinished` 送达。投影路径补上之前这个重复看不见（前端根本不投影 item），补上之后它就是「同一句话出现两次」。前端侧没有干净的解法：按 item id 去重要求无界状态，而 id 集合本来就该由游标表达。真正的修法在 daemon 侧——要么快照与登记原子化（先取快照、以快照的 head 作为 replay 起点登记），要么让 `session/load` 的回复自带「从哪条之后开始算 live」的游标，前端据此丢弃重复。**与 `next_cursor` 是同一个问题的两面**：`SessionManager::load` 今天恒回 `None`（一页给全），所以既没有分页也没有「快照到哪为止」的边界；M4 的长会话分页落地时必须一起定。CLI 的 `drain_history` 已按契约跟随游标（并对「游标不前进」报错而不是挂死），所以 daemon 一旦开始分页，客户端不会静默截断历史。
