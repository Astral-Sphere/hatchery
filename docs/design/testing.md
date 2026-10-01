# 设计：测试体系（横切）

> 状态：设计稿。目标：**所有代码都能如期运行**——每条不变量、每个协议方法、每种降级路径都有对应测试；测试是「实测优先于推测」纪律的执行载体。配套 crate：`hatchery-testkit`（测试基建，见 §2）。

## 0. 测试纪律（全项目强制）

1. **不变量必测**：architecture.md §5 的 6 条核心不变量，每条至少一个专属集成测试（§5 给出映射表）；CI 中归入 `invariants` 分组，永不 skip。
2. **实测优先**：涉及第三方行为的断言（SQL 引擎的 WAL/触发器/pragma、libgit2 的仓库与 checkout 语义、provider SSE 怪癖、ACP 宿主行为）必须来自真实执行或真实录制 fixture，禁止按文档/记忆推断后直接写死预期。**上游文档会过时**——M0a 实测：turso 的 COMPAT.md 称 `synchronous` 只支持 OFF/FULL，实际 NORMAL 可用。**自己写下的结论同样要复核**——M0a 曾记录「git2 需要 cmake」，实测 libgit2-sys 的 build.rs 只用 `cc`，该错误结论已在 ADR-0012 更正。spike 结论（worklog 各方向）沉淀为回归测试。
3. **Bug 修复必附回归测试**：先写复现测试（红），再修（绿）；worklog 条目引用测试名。
4. **文档示例可运行**：公共 API 的 rustdoc 示例必须是 doctest 且进 CI；不可运行的示例不写「ignore」了事——要么改成可运行，要么改成 `text` 代码块并说明原因。
5. **测试不碰真实用户环境**：一切文件/进程/数据库操作在 tempdir、内存或专用 fixture 工作区内；网络默认禁止，live 测试单独标记（§8）。

## 1. 测试分层

```
                 少 ▲            手动实测（Zed 真机、GUI 走查、doctor 探测）
                    │            live 测试（真实 provider 网络调用，标记 #[live]）
                    │            e2e 全栈（TestDaemon + 协议客户端 + mock wire）
                    │            集成（跨模块：store+分支、daemon+hub、acp+委派）
                    │            契约（trait 级共享测试套件，各实现必须通过）
                    │            属性（proptest：item 树操作、协议 serde）
                 多 ▼            单元（状态机、翻译层、纯函数）
```

组织与命名约定：

- 单元测试：crate 内 `#[cfg(test)]`，文件名 `测试对象::行为::预期`（如 `edit_fork_on_tool_result_starts_new_turn`）。
- 集成测试：各 crate `tests/`。跨 crate 的 e2e 与不变量套件放独立成员 crate `hatchery-tests`（依赖全家）——workspace 根是虚拟 manifest，不能有顶层 `tests/`。**已落地（M1 Phase 5）**：场景 1/2、invariants 组与 D7 的子进程对比都住在里面；共享 fixture（SSE 字节、provider 层、收集助手）经它的 lib target 提供。
- runner：**cargo-nextest**（分组、重试标记、JUnit 输出）；快照断言：**insta**（golden file，`INSTA_UPDATE=always` 审阅流）；属性测试：**proptest**。
- 分组：Rust 无法按属性过滤测试，所以用**命名前缀** + `.config/nextest.toml` 的 `default-filter` 实现（原设计的 `#[live]` 属性标记不可行）：
  - `live_*` 真实网络/需 secrets，另外再用 `live-tests` cargo feature 双保险（默认不编译）；
  - `slow_*` >5s；`gui_*` 需显示环境；
  - `invariant_*` 核心不变量套件（§5），**始终在默认组里跑**，永不 skip。
  - 默认组过滤掉 live/slow/gui；PR CI 跑默认组，其余进 nightly 或手动。
  - 实测（nextest 0.9.146）：profile 匹配不到任何测试时 `--no-tests` 默认 `auto → fail`（退出码 4），所以 nightly 跑空的 slow/gui 组必须显式传 `--no-tests=warn`。

## 2. 测试基建：hatchery-testkit（dev-only crate）

所有 fake/harness 集中在此，各 crate 以 dev-dependency 引用，避免 fake 散落重复：

```rust
// LLM
pub struct ScriptedProvider { rounds: Vec<Vec<StreamEvent>> }  // kernel 状态机测试用；带 gate 时逐条放行
pub struct MockWire;        // wiremock 装配：按 fixture 回放 SSE 字节流（llm crate HTTP 级测试，M1）

// kernel 的另外三个接缝
pub struct MemoryHistory;   // 固定对话 + 可控 head
pub struct RecordingSink;   // 记录每个事件、可按谓词等待（带超时，避免挂死）；wait_for_from 带游标，多 turn 测试才等得到第二次结束
pub struct ScriptedToolHost; // 目录 / 审批 / 结果全部脚本化；entries 可排队、可 gate
pub enum ScriptedApproval { AllowOnce, AllowAlways, Deny, DenyAlways, Script(Vec<ApprovalOption>) }  // 与协议的 ApprovalOption 同一套词汇
pub fn answer_approvals(handle, events, policy);   // 替测试应答审批请求
pub mod events;             // completion/reason/error/finished_items/kinds/states/tool_result_texts：从录下的事件里读出结论（daemon 的测试复用同一套）

// 能力后端（capabilities 接缝的内存实现，M1–M2）
pub struct MemoryFs;        // HashMap<PathBuf, Vec<u8>>，支持断言读写序列
pub struct MemoryTerminal;  // 脚本化进程行为：输出序列/退出码/挂起（测取消）

// 环境（M1–M2）
pub struct TempWorkspace;   // tempdir + 可选 git init + 文件树 DSL
pub struct TestDaemon;      // 进程内 DaemonCore + 内存 transport；暴露 ClientProbe
pub struct ClientProbe;     // 协议客户端：call/收集事件/断言事件序列（e2e 主驱动）

// store 的独立参考实现（不是 fake）
pub struct ReferenceTree;   // 纯 HashMap + head 的 item 树，**不复用 store 生产代码**，供 proptest 对拍

// 计时
pub struct Gate;            // 计数信号量：让脚本化 fake 在某一步停住，中断测试才有窗口

// fixture（M1，与 llm 的录制回放一起）
pub fn fixture(name: &str) -> Bytes;          // tests/fixtures 加载 + 元数据校验
pub fn assert_golden(value: impl Debug);      // insta 封装，统一快照命名
```

M0b 已实现的是 kernel 四接缝的 fake 与 `ReferenceTree`（`ScriptedProvider` / `MemoryHistory` / `RecordingSink` / `ScriptedToolHost` + `answer_approvals` + `Gate`）。`fixture`/`assert_golden` 推迟到 M1：protocol 的 golden 是纯 JSON（§3.1），不需要 insta 封装，而 SSE fixture 的加载器要等 llm crate（ADR-0009 反预拆分）。

**契约测试套件**：对 `LlmProvider`、`SessionStore`、`FsBackend`、`TerminalBackend`、`ApprovalGate` 各定义一组 trait 级共享测试（宏或泛型 fn），任何实现（本地/ACP 委派/内存/未来远程）必须整套通过——这是「换绑定不换工具」（ADR-0004）的机器保证。

## 3. 各方向测试设计

### 3.1 protocol（hatchery-protocol）

风险：wire 兼容破坏、serde 表示漂移。

- 每个方法/事件的 **serde 往返测试** + golden JSON：字段增删一目了然。fixture 在 `tests/fixtures/protocol-v<N>/`（N = `PROTOCOL_MAJOR`），**纯 JSON**、由 `tests/support/mod.rs` 的注册表驱动，生成方式是 `UPDATE_FIXTURES=1 cargo nextest run -p hatchery-protocol`；`scripts/ci.sh` 导出的 `INSTA_UPDATE=no` 让它在门禁里拒绝重写自己的契约。
- **不用 insta** 的原因：insta 的快照名由断言表达式推导且必须是字面量，数据驱动的注册表无法驱动它（除非手写 62 条断言去重复注册表）。纯 JSON 另有好处——版本兼容 fixture 任何实现都能读。代价是 key 按字母序（`Value` 是 `BTreeMap`）；确定性不受影响，**声明序**由 `typed_serialization_keeps_declaration_order` 单独锁定。
- **版本兼容**：`version_compat.rs` 读**磁盘上的** fixture 反序列化，不与内存样本比对（那是 golden 测试的职责），所以「wire 形态变了」与「fixture 过期」是两种不同失败；目录名必须等于 `protocol-v{PROTOCOL_MAJOR}`，且该 major 必须在 `SUPPORTED_PROTOCOL_VERSIONS` 里。
- fixture 确定性：`scripts/ci.sh` 的 determinism 步用 `git status` 检查 `tests/fixtures/` 是否被测试改写。
- 另有 `no_golden_file_is_orphaned`（磁盘上有、注册表里没有的 fixture 报错）与 `no_event_field_collides_with_the_envelope`（信封字段与事件字段重名会让帧写得出、读不回——它抓出过两个真实冲突）。

### 3.2 kernel

风险：状态机死角、取消语义、上下文组装错误。

- 状态机全迁移矩阵：ScriptedProvider 脚本化每条路径（含错误注入）。M0b 已落地的集成测试（`tests/turn_state_machine.rs`）覆盖：
  - `interrupt_during_streaming_ends_the_turn_early_and_keeps_what_was_said`（中断点之后的 item 仍提交）
  - `interrupt_during_tool_execution_cancels_the_tool`（ScriptedToolHost 断言收到取消）
  - `interrupt_while_awaiting_approval_ends_the_turn`
  - `max_rounds_fuse_trips_at_limit`
  - `the_tool_snapshot_is_frozen_for_the_whole_turn`（turn 中途换注册表不影响本轮）
  - `a_mid_turn_prompt_is_dropped_not_queued`、`a_second_turn_chains_onto_the_first_turns_head`（一次一个 turn；第二个 turn 接在第一个的 head 上）
  - 审批：`an_approval_request_pauses_the_turn_until_it_is_answered`、`a_denied_call_becomes_an_error_result_the_model_can_read`（拒绝对话继续，item 记 `Denied`）、`two_calls_in_one_round_are_each_approved_separately`（一轮里的两次调用各有各的 request id 与等待，一个被拒不代替另一个作答）
  - 工具：`a_tool_result_is_appended_to_the_next_request`、`a_tool_the_model_reported_as_failed_is_recorded_but_the_turn_continues`、`tool_progress_is_forwarded_while_the_tool_runs`、`progress_sent_as_the_tool_finishes_still_reaches_the_sink`（工具在同一次 poll 内发出并返回的进度不能跟着 invoke future 一起丢）、`a_tool_that_cannot_be_invoked_fails_the_turn`
  - reasoning：`reasoning_is_streamed_and_stored_verbatim`、`a_signature_that_arrives_after_the_text_cannot_be_stored_but_still_replays`（adapter 违反事件顺序时的代价，见 kernel.md §4）
  - 失败：provider 起不来 / 流中途断 / 流没有 finish reason / 工具没跑成
  - 确定性：`the_same_script_produces_the_same_event_sequence_twice`（事件名、item 种类、迁移序列三重比对）、`the_documented_event_sequence_is_emitted_exactly`（逐字比对文档里那条序列）
- 组装（M1）：reasoning 块按能力表保留/丢弃；compaction 区间替换；token 裁剪顺序（先旧 round 的工具结果）。M0b 只到「把用户输入落成 item、把工具结果回填给下一轮」为止，过滤与裁剪住在 daemon 的装配器里（kernel.md §6）。

### 3.3 llm

风险：wire 翻译错误、reasoning 回放被污染、重试风暴。

- **HTTP 级**（MockWire + 录制 fixture）：SSE 逐 chunk 翻译、跨包分片（tool call arguments 被切断在两个 chunk）、`[DONE]`、半帧缓冲、非法 JSON 容错。
- **reasoning 逐字节纪律**（ADR-0007 的机器保证）：
  - `reasoning_passback_is_byte_exact`：fixture 含首尾空白/unicode/换行的 reasoning_content，入库→重建→断言 wire 请求体中逐字节相等（比较序列化后的 JSON 字段原文）。
  - 能力表驱动矩阵：每个 provider 族 × {echo, signature, 丢弃} 行为。
  - 填充剥离只发生在入库前（`(no reasoning detected)` 类），有正反用例。
- effort 映射表：每族 golden 请求体（deepseek Off→换模型、qwen→enable_thinking+budget…）。
- 重试：429 + `Retry-After` → RateLimited 事件与退避时序（tokio::time::pause 虚拟时钟）；401 不重试；流中断→TurnFailed。
- **live**（`#[live]`，手动跑）：`hatchery doctor --provider X` 即 live 测试的 CLI 形态；新 provider 接入时录制 fixture + 把实测行为写进 worklog/llm.md。

### 3.4 store

风险：分支树 SQL 错误、并发丢失、崩溃损坏——数据层错误最不可原谅，测试最重。

- **引擎门槛（M0a 已落地）**：`crates/hatchery-store/tests/spike_engine.rs` 12 项，锁定 turso 0.7.2 的真实行为——append-only 触发器、外键级联、WAL 下写事务与并发读、`user_version`、写入延迟、`WITH RECURSIVE` 缺失（tripwire 常量，上游补上就主动失败）。引擎升级必须重跑（ADR-0010）。
- **store 层（M0b 已落地）**：`crates/hatchery-store/tests/session_store.rs`——会话 CRUD 与分页游标、append 与 head 推进、批量原子性、跨会话父节点拒绝、**跨会话批次拒绝**、**写进不存在的会话报 `SessionNotFound` 而不是外键的消息**（两者在 wire 上是「调用方错误」与「存储故障」两种码）、300 条链的重建顺序（覆盖 payload 分块边界）、编辑分叉与旧分支保留、切换、**切换/删除拒绝别的会话的 item**、级联删除与拒绝、姐妹分支不受影响、`branch_tree` 的 active 标记、turn 起止、导出（active / 全树的逐行顺序 / **空会话** / 拒绝覆盖）、迁移与重开、损坏 payload 报告。两个测试用第二条连接绕过 store：`invariant_the_database_refuses_to_update_an_item`（直接 UPDATE 被触发器拒绝，错误里带我们的消息）与 `a_corrupt_payload_is_reported_with_its_item`。
- **属性测试（核心投入，已落地）**：`crates/hatchery-store/tests/tree_proptest.rs`，随机 append/edit_fork/switch/delete 脚本同时作用于 turso 实现与 `hatchery-testkit` 里独立写出的 `ReferenceTree`（纯 `HashMap` + head，不复用生产代码），每步之后断言链形态（id/parent/kind）、head、active 集合与总行数一致。64 cases；失败种子进 `*.proptest-regressions`（已入库）。
- 级联删除：`delete_branch_cascades_and_refuses_while_the_head_is_inside`、`deleting_a_branch_leaves_a_sibling_alone`；引擎级兜底已于 M0a 实测（`sessions.active_head` 的外键会拒绝该删除）。删除还会**交叉校验**走树数量与删除前后行数差（storage.md §5）。
- append-only：`invariant_items_are_never_rewritten`（编辑后原 item 逐字段不变）+ 引擎级触发器已实测 + store 层没有 update item 的 API 入口。
- **并发**：M0b 读也走 writer actor（串行但正确）；只读连接池压测（N reader + writer、1k items、无 busy 错误、背压 await 而非丢弃）排在 M1（storage.md 开放问题 6）。
- **崩溃测试（已落地）**：`crates/hatchery-store/tests/crash_recovery.rs` 6 项。子进程是**测试二进制自重入**（`current_exe()` + `HATCHERY_CRASH_PROBE` + `#[ignore]` 入口），不新增 target、不发布二进制；子进程提交后打印就绪行并挂起，父进程 `Child::kill()`（unix SIGKILL / Windows TerminateProcess）后重开断言。五种 StoreCmd 各一个 probe（append / edit_fork / switch_branch / delete_branch / finish_turn），另有一项断言恢复后数据库**可用**。**不覆盖断电**：spike 实测 `PRAGMA synchronous` 无可测影响，「掉电不丢已提交 item」目前没有证据（storage.md 开放问题 4）。
- 迁移：`migrations_run_once_and_both_records_agree`（`user_version` 与 `schema_meta` 双记录一致、重开不重复迁移）；拒绝启动的两条也各有测试——`a_database_from_a_newer_build_is_refused`（更新版本写出的库）与 `a_database_whose_version_record_was_deleted_is_refused`（两个版本记录只剩一个）。v(N-1) 库文件 fixture → 自动迁移仍待有 v2 时补。

### 3.5 capabilities + tools

风险：影子 Git 碰用户仓库（灾难级）、硬门被绕过、PTY 泄漏进程。

- **影子 Git 安全**（不变量 6 专属）：`invariant_shadow_git_never_touches_user_repo`——TempWorkspace 先造一个脏仓库（staged/unstaged/untracked + 一次 commit），跑 snapshot + restore，断言用户仓库的 HEAD、分支、refs、`.git/index` mtime、`.git` 目录条目全部不变；外加 `no_gitlink_is_planted_in_the_user_workspace`（libgit2 的 `set_workdir(.., update_gitlink=true)` 会在用户工作区里种一个 `.git` 文件，必须传 false 并自己写 `core.worktree`/`core.bare`，ADR-0012）。**后端已从 CLI git 换成 git2 / vendored libgit2，11 项门槛测试常驻 `crates/hatchery-capabilities/tests/spike_shadow_git.rs` 且全绿。**
- 检查点语义：写前必有 snapshot（MemoryFs 写序列 vs checkpoint 记录对齐）；restore 可回滚（restore 前自动 snapshot）；purge 才删未跟踪文件（`purge_restore_also_removes_never_tracked_files`，实现走 `checkout_index(remove_untracked)`）；忽略规则每次打开句柄都要重放（实测 `add_ignore_rule` 是 per-handle 的）；大文件跳过；预算熔断触发 GC（构造超预算 fixture）；非 git 工作区可用。
- 硬门（不变量 5 专属）：`project_config_cannot_disable_hard_gates`——加载恶意项目配置后断言危险路径写仍需审批、规则不可 allow-always。
- LocalFs：路径逃逸（`../`、symlink 出工作区、绝对路径）全部拦截；行范围读取边界。
- LocalPty：真实进程（`sh -c echo/sleep/kill -0`）——输出完整性、超时杀、cancel 后无孤儿进程（`kill -0` 断言）、环形缓冲截断。
- 工具单测：每个工具 × MemoryFs/MemoryTerminal 的行为矩阵；spill 阈值；凭据脱敏（高熵串 fixture 正反例）。
- 契约套件：LocalFs/AcpClientFs 过同一组 FsBackend 契约测试（§2）。

### 3.6 daemon

风险：竞态（双实例/双 runtime）、事件错序、代际污染。

- 单实例：两进程并发 attach-or-spawn → 恰一胜者（重复 20 次抓竞态）；陈旧 daemon.json（pid 已死）自愈。
- 租约与代际（不变量 1 专属）：`stale_runtime_events_are_dropped`——旧 generation 事件注入 hub，断言订阅者收不到；`session_lease_blocks_second_runtime`。
- hub：两订阅者收到同序事件；迟加入者 replay 完整；慢消费者被踢且不阻塞他人；coalescing 合并 delta 但控制事件不合并、不乱序。
- 崩溃恢复：status=running 的会话重启后标记 interrupted 且发过 TurnFailed 存档事件。
- **fail-loud 装配审计**（ADR-0009）：`startup_audit_missing_provider_refuses_service`——profile 必需组件缺失（如密钥环境变量不存在）时 daemon 拒绝服务、输出缺失清单、非零退出；逐个必需组件各一条。
- **teardown 逆序**（ADR-0009）：装配时注册带序号的 disposer，关闭后断言执行序严格为注册逆序（订阅者→runtime→检查点→store→监听器）。
- e2e 主干（§4）大部分在此层驱动。

### 3.7 acp

风险：能力协商矩阵漏分支、委派语义与本地语义漂移。

- 协商矩阵测试：client caps {fs,terminal} × {有,无} 四组合 → 断言绑定的后端类型与降级行为（用 testkit 的 fake ACP 连接）。
- **fake 宿主集成**：用官方 crate 写 minimal test client，进程内驱动完整 turn：prompt → `fs/write_text_file` 委派往返 → `terminal/create..release` 委派往返 → `session/request_permission`（断言 option 映射与 allow_always 落规则表）→ 完成；`agent_thought_chunk` 携带 reasoning。
- `session/load` replay：长会话重放的 update 序列与 active 分支一致（golden）。
- client 侧：fake 子 agent 进程（testkit 提供 `dummy-acp-agent` 二进制 target）→ subagent 工具全流程；权限上浮策略三档；深度限制与环检测（自举：hatchery spawn hatchery，深度 2 熔断）。
- **手动实测清单**（M3 验收，结果记 worklog/acp.md）：Zed 真机——编辑/终端/审批/thought 展示/取消/多会话；宿主行为与规范不符处记录并加防御测试。

### 3.8 cli

- **TUI golden 帧**：ratatui `TestBackend` + insta——喂脚本化事件流，断言关键帧（消息渲染、reasoning 折叠展开、审批弹层、分支列表）。布局改动的 diff 审阅即视觉回归。
- headless：`exec --json` 输出 golden（JSONL 逐行）；退出码矩阵（completed/failed/cancelled/审批拒绝）。
- 斜杠命令：每个命令一个「输入命令 → 断言发出的协议方法」单测（命令层与渲染层分离，命令层纯函数可测）。

### 3.9 gui

GUI 是测试最薄弱层，策略 = 「逻辑出 GTK，GTK 只做投影」+ 分层兜底：

- **view-model 单测**（主力）：所有状态变换（事件→列表模型、分支树→可视化结构、审批弹层状态机）为不依赖 GTK 的纯 Rust 类型，全覆盖单测。
- GTK 冒烟（`#[gui]`，CI 用 GNOME SDK 容器 + xvfb-run）：窗口构建、导航、空会话渲染不 panic；每个 milestone 扩一条路径。
- 性能 fixture：10k items 会话的 ListStore 构建耗时基准（criterion）+ 滚动冒烟。
- RTL/i18n：`GTK_TEXT_DIR=rtl` + `LANGUAGE=ar` 截图冒烟（人工比对存档）；catalog 完整性（fluent，ADR-0011）——FTL 解析无错、每个 message id 在所有语言都存在、`xtask i18n-extract` 断言源码无未包裹的用户可见字符串（启发式 lint）；golden 里的 bidi 隔离符 U+2068/U+2069 要显式保留。
- 手动走查清单（M4 验收）：完整 Code 会话、审批、rewind 面板、分支时间线、设置窗全项。

### 3.10 platform（配置/提示词/i18n）

- 配置：分层合并 golden（五级 fixture 叠加）、per-key origins 正确、数组整体替换语义、项目级越权忽略+warning、坏 TOML 逐 key 降级。
- 提示词：`prompt/render` golden（含 section 来源标注）；user_override 生效但 safety_gate/tool_discipline 不可覆盖（正反用例）；AGENTS.md 层级发现（嵌套工作区 fixture）。
- i18n：见 §3.9 的提取完整性检查。

## 4. e2e 全栈主干

每个里程碑的 DoD 落到一组 e2e 场景测试（TestDaemon + ClientProbe + MockWire，无真实网络）：

| 场景 | 里程碑 | 断言要点 |
|---|---|---|
| 最小对话：prompt → 流式响应 → 落库 → resume | M1 | 事件序、DB items、reasoning 回放（第二 turn 请求体逐字节）。**已落地**：`invariant_minimal_chat_replays_reasoning_byte_exact` + `a_reconnecting_frontend_gap_fills_from_the_store`（hatchery-tests） |
| 双前端扇出一致性 | M1 | 两 probe 事件序列相同；断线重连 replay 补齐。**已落地**：`two_frontends_see_identical_sequences_and_a_reconnect_gap_fills`（hatchery-tests） |
| Chat→Code 切模式 | M2 | 工具表变化在 turn 边界生效 |
| 编辑分叉重演 + 分支删除 | M2 | 新旧分支 rebuild、级联删除、active_head 校验 |
| 写坏文件 → rewind 三 scope | M2 | 工作区文件恢复、对话回退、检查点联动 |
| 审批全链路（本地 + 规则持久化） | M2 | fail-closed 超时、ProceedAlways 生效 |
| ACP 委派 turn（fake 宿主） | M3 | §3.7 场景在真实 daemon 进程形态下复跑 |
| subagent 自举 | M3 | 深度限制、权限上浮 |
| GUI view-model 场景回放 | M4 | 同一 e2e 事件流喂 view-model 断言 UI 状态 |

## 5. 不变量 → 测试映射（CI `invariants` 分组）

测试名一律带 `invariant_` 前缀，nextest 的 `invariants` profile 就是靠这个前缀选出来的（§1）。

| 不变量（architecture.md §5） | 专属测试 |
|---|---|
| 1 单一 runtime 所有者 | `invariant_stale_runtime_events_are_dropped`（客户端代际过滤，hatchery-tests）、`invariant_session_lease_blocks_second_runtime`（第二 prompt 拒绝，hatchery-tests）、`invariant_single_instance_race_admits_exactly_one_winner`（20 线程竞态，hatchery-tests）。**三条均已落地** |
| 2 模型可见=已记录 | e2e 每场景收尾断言「重建上下文 == MockWire 实际收到的请求体」（逐 turn）。**已落地**：场景 1 对第二 turn 请求体的 messages 数组做整表比对（serde 字符串相等即字节相等，含首尾空白/unicode/换行） |
| 3 items append-only | `invariant_items_are_never_rewritten`、`invariant_the_database_refuses_to_update_an_item`（**均已落地**：前者断言编辑后原 item 逐字段不变，后者直接用第二条连接 `UPDATE items` 被触发器拒绝，错误带我们的消息；引擎级 `invariant_items_update_trigger_aborts` 于 M0a 实测）、store 属性测试 |
| 4 工具只经接缝 | clippy `disallowed_methods`（编译期；**实测**：workspace 级 allow + `hatchery-tools` crate 属性 deny，违规确实报错）+ 工具单测只注入 Memory 后端（运行期证明） |
| 5 安全门不可覆盖 | `invariant_project_config_cannot_disable_hard_gates` + prompts 覆盖正反用例 |
| 6 影子 Git 不碰用户仓库 | `invariant_shadow_git_never_touches_user_repo` |

不变量 2 的实现方式值得单列：TestDaemon 里 MockWire 记录**实际发出的请求体字节**，测试收尾从 store rebuild 上下文并序列化，两者必须逐字节一致——这一条测试同时锁死了组装、存储、回放三层。

## 6. fuzz 与边界

- **cargo-fuzz**（M2+，nightly 短跑）：SSE 帧解析、JSON-RPC 帧解码（半帧/超长/嵌套）、item payload serde、路径校验（LocalFs 逃逸面）。
- proptest 除 store 外还覆盖：effort 映射全定义域、token 裁剪单调性、coalescing 时间窗（虚拟时钟随机事件流不丢不乱）。

## 7. 性能基准（criterion）

`rebuild_chain`(1k/10k items)、hub 扇出吞吐、prompt 组装、SSE 解析吞吐、GUI ListStore 构建。基线存仓库（criterion baseline），nightly 对比，劣化 >20% 标红（人工裁决，不自动 block）。

## 8. CI 门禁

**PR（必须全绿，<10min 目标）**：fmt → clippy `-D warnings`（含 disallowed_methods）→ nextest 默认组（单元+契约+属性+集成+e2e+invariants）→ doctests → fixture 确定性（git diff 为空）→ i18n 提取检查。

**nightly**：slow 组、gui 组（GNOME SDK 容器 + xvfb）、fuzz 短跑（10min/target）、criterion 基线对比、cargo-audit、`cargo mutants`（可选，先只跑 store/kernel 两个高风险 crate）。

**live 组（不进 CI，手动/自托管）**：需要真实 provider 密钥；`cargo nextest run -E 'test(live_)'`；触发时机 = 新 provider 接入、上游 openai-interface 升级、能力表改动。结果记 worklog/llm.md。

**覆盖率**：cargo-llvm-cov，PR 报告不 block；阈值（line）：kernel/store/llm/capabilities ≥ 85%，protocol/daemon ≥ 80%，cli ≥ 60%，gui 豁免（view-model 部分 ≥ 80%）。**阈值已 enforce（M1 Phase 5）**：`cargo xtask coverage` 把 llvm-cov 的逐文件报告按 `crates/<name>/src` 前缀折算成 crate 线覆盖（tests 目录不计），低于下限即失败；`--report-only` 只出表。执行位在 nightly（不是 PR 门禁）——挡百分比易诱发凑数，nightly 失败则点名漂移的 crate。**MSRV**：nightly 另有 `msrv` job，`cargo +1.90.0 check --workspace --all-targets --locked`。覆盖率是指标不是目标——不变量套件与属性测试的通过优先于数字。

## 9. 手动实测清单（模板）

每次里程碑验收与真机测试按清单执行，结果（含失败与宿主怪癖）记入对应 worklog：

- M1：真实 provider（deepseek/qwen 各一）对话、reasoning 展示与回放命中、resume、双终端扇出。
- M2：真工作区改坏→rewind、审批规则持久化、非 git 目录检查点。
- M3：Zed 真机全链路（§3.7）。
- M4：GUI 走查（§3.9）+ flatpak 安装冒烟。

## 开放问题

1. ~~testkit 的 `dummy-acp-agent`、`dummy-provider` 等辅助二进制以 workspace member（`[[bin]]` + `required-features = ["testkit"]`）还是独立小 crate 存在~~ → **已定（2026-09-28，M0b）**：需要「真实子进程」的测试用**测试二进制自重入**——`std::env::current_exe()` + 环境变量 + 一个 `#[ignore]` 的入口测试。崩溃恢复测试（`crash_recovery.rs`）就是这么做的：不新增 target、不发布任何二进制、三平台同一份代码，子进程拿到的是真正的 `TursoStore` 而不是副本。M3 的 `dummy-acp-agent` 仍是另一回事（它需要被 ACP client 当作**外部程序**拉起），继续倾向 workspace member + `required-features`。
2. cargo-mutants 的投入产出（跑一次全 workspace 很慢）——先 nightly 只对 store/kernel，M2 评估。
3. GUI 快照测试（截图 diff）是否引入（GTK 渲染跨环境像素不稳定，倾向只做 RTL/i18n 人工存档）——M4 评估。
4. **已定（2026-10-01，M1 D7）**：e2e 默认形态维持**进程内过真 socket**（TestDaemon），另有一条真子进程对比测试常驻 `hatchery-tests`（`a_real_subprocess_daemon_serves_the_same_scenario` + `e2e_daemon` bin）。对比实测：同一最小场景，进程内 ~0.15s，子进程 ~0.2s（多 fork/exec + daemon.json 轮询），两者断言集合相同；子进程额外覆盖的只有 entry 壳（fork/exec、published pid、跨进程 UDS）——而 entry 生命周期已由 daemon 的 entry 集成测试专测。结论：e2e 场景不默认起子进程；「进程形态」的覆盖由 entry 测试 + 这一条对比测试供给，M3 的 ACP 委派场景复跑时再评估是否扩子进程形态。
