# 术语表

> M1 收口时建立（2026-10-01）。只收「读代码/文档时需要一句话解释」的词；每条尽量给出权威定义所在的文档或类型。按主题分组、组内按字母序。

## 会话模型

- **active branch head（活动分支头）**：会话当前指向的 item；`rebuild_chain` 从它向根回溯。编辑分叉、切换分支改的都是它（ADR-0003，design/storage.md §4）。
- **branch（分支）**：共享祖先的 item 链。编辑历史消息产生分叉，旧分支保留；同一时刻只有活动分支对模型可见。
- **generation（代际号）**：runtime 装配序号，落在会话行上。新 runtime 组装前先 `bump_generation`，其事件都带此号；客户端丢弃低于已知代际的事件（不变量 1，protocol.md）。
- **item**：会话树上的一条记录（用户消息、推理块、助手消息、工具调用/结果、检查点等 9 种，`ItemKind`）。append-only，永不改写（不变量 3）。
- **lease（会话租约）**：「一个会话同一时刻至多一个在跑 turn」的守卫。M1 的实现是 manager 的 runtime 槽位 + `turn_running` 信号，在途第二 prompt 拒绝 `TurnInProgress`；跨进程文件租约随 M2 检查点（worklog/daemon.md）。
- **replay_from**：`session/load` 的补差游标——返回活动分支上该 item **之后**的部分；游标不在活动分支上则拒绝。重连前端用它补上断线期间错过的事。
- **session / turn / runtime**：会话是持久实体（store 里的一行 + item 树）；turn 是一次 prompt 驱动的多轮模型-工具循环（kernel 状态机）；runtime 是某一代际下装配起来的运行物（provider+tools+history+sink），可空闲卸载、再次 prompt 时重组。
- **resume**：关掉前端再回来：`session/load` 重建视图，下一个 prompt 从 store 重建上下文继续——模型可见的历史与断线前逐字节一致（不变量 2）。

## 状态机与事件

- **`AgentHandle` / `AgentCommand`**：kernel 的命令半边；`turn_running()` 是状态机对外的「在跑」真值（watch 镜像）。
- **`HubSink`**：daemon 侧的 `EventSink` 实现——kernel 事件投影成 wire 事件 + store 提交，item **先落库再发布**（「模型可见=已记录」的顺序保证）。
- **`KernelEvent` / `ServerEvent` / `SessionEvent`**：三层事件词汇：kernel 的中立事件 → wire 的带类型事件（`type` tag）→ 加了 session/generation 信封的投递形式（`event` 字段 flatten 在信封上，wire 上 `type` 是顶层键）。
- **turn 状态机**：`Idle → Assembling → Streaming ⇄ Executing → Idle`（可入 `AwaitingApproval`），每次迁移发事件；全路径由 `turn_state_machine.rs` 钉住。

## daemon 与传输

- **attach-or-spawn**：CLI 连不上去就拉起 daemon 的启动模式（D1：`process_group(0)` 分离进程，不双 fork）。
- **boot token**：daemon 启动时生成、写进 daemon.json 的握手令牌；`daemon/hello` 校验，防止连错实例（D4：每次启动轮换）。
- **daemon.json / daemon.lock**：单实例发现的两件套——发布的自述文件（0600，write-then-rename）与 fs2 独占文件锁。锁是真相，json 是名片；陈旧 json 由 pid 存活判定自愈。
- **DaemonClient / EventStream**：前端唯一的数据通道（ADR-0001 瘦客户端）：调用连接按 id 路由回复；事件连接单独一条，订阅调用（`session/new`/`session/load`）在它上面发出。
- **disposer**：逆序执行的 teardown 步骤栈（ADR-0009）；一步 panic 不阻断其余步骤。
- **fail-loud 审计**：启动时把所有缺件（provider 缺 key、目录不可写……）一次列全再退出，拒绝半可用的 daemon。
- **LiveHub**：per-session broadcast 扇出（容量 4096）。M1 无 coalescing、无 replay window——重连靠 `session/load` 重建，这是文档化的形状而非缺口。
- **stdio 监听**：与 UDS 同一条 `serve_connection` 循环的 stdin/stdout 实例，嵌入方用。

## llm 与能力

- **capability table（能力表）**：模型族 → wire 行为（reasoning 旋钮、echo、签名）的数据行；`ProviderConfig::capability_table()` 是 built-in + config 覆盖的唯一折算点，adapter 与 daemon 的历史回填读同一份。
- **echo_reasoning**：是否把已存的推理块回填进后续请求。DeepSeek/Qwen 当前行是 false（provider 自生推理）；打开时**逐字节**回传（ADR-0007，不 trim 不改写——那点空白就是 provider 的缓存键）。
- **effort / ReasoningEffort**：推理力度档位（off–max），按能力表映射成各家的 wire 字段（`reasoning_effort`、`thinking:{type}`、`enable_thinking`+budget）。
- **hybrid model（混合推理模型）**：默认带推理、可用参数关闭的单模型世代（2026-09-30 校准：`deepseek-flash`、`qwen3.8-flash`），区别于旧「off 换模型」的 ModelSwitch 行。
- **MockWire**：testkit 的 wiremock 装配——按 fixture 回放 SSE 字节并记录收到的请求体；不变量 2 的「实际请求」一端由它供给。
- **thinking switch**：DeepSeek 家族的推理开关（`thinking:{enabled|disabled}`），模型不换。

## store

- **writer actor**：store 的单写者任务，所有命令经一个 mpsc 串行进库；读也随之串行（正确优先，并发读池是 M1 之后的开放问题）。
- **`rebuild_chain`**：按活动分支重建 item 序列（根在前）；payload 原样返回，过滤是 daemon 的事。
- **shadow git（影子 Git）**：检查点后端——libgit2 vendored、独立 git-dir、绝不碰用户仓库（不变量 6，ADR-0012）。
- **TursoStore**：存储引擎（ADR-0010）；崩溃恢复由 kill -9 测试矩阵钉住。

## 测试体系

- **e2e 形态（D7 定案）**：默认**进程内过真 socket**（TestDaemon），子进程形态保留一条常驻对比；entry 生命周期由 daemon 的 entry 测试专供。
- **fixture**：`tests/fixtures/<name>.sse` + `.meta.json` 成对出现——字节级录制 + 来源元数据；脱敏扫描命中即拒写。
- **invariants 分组**：`invariant_` 前缀的测试集合，nextest profile 按前缀选取，默认组也跑、永不 skip（testing.md §5 有不变量 → 测试映射表）。
- **`TestDaemon` / `ClientProbe`**：testkit 的 e2e 主驱动——真 turso + 真配置 + 真 UDS 的进程内 daemon，与协议客户端探针。

## 配置与 prompt

- **LayeredConfig**：五层 TOML（builtin → system → user → project → runtime）深合并，per-key origin；数组整替；项目层安全硬门不可覆盖。
- **prompt sections**：identity / mode_variant / environment / safety_gate 四段装配，`{{var}}` 插值；safety_gate 不可覆盖（不变量 5）。
