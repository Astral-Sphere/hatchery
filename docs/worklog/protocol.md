# 工作记录：wire 协议（hatchery-protocol）

- 范围：JSON-RPC 方法/事件/数据模型、client helper、版本化
- 设计文档：[../design/protocol.md](../design/protocol.md)
- 相关 ADR：0001、0003

## 当前状态

**M0b 完成（2026-09-28），评审后的两轮加固已入库（2026-09-29 / 2026-09-30）**：crate 的类型、方法表、事件、帧编解码与版本协商全部落地；golden fixture 在 `tests/fixtures/protocol-v1/`，覆盖率由测试自己机器检查（不靠文档里的数字）。设计文档 `docs/design/protocol.md` 已按实现重写，含一张「草图 vs 实际」的修正表。

**方法面已完整且双向机器强制**：20 个方法都有参数**与**结果类型、都有 golden fixture；样本数与方法表对账（`assert_eq!(samples.len(), m::ALL.len())`，tests/support/mod.rs:529 与 :720）、每个方法有会拒绝 `{}` 的 typed 校验器、注册集与磁盘集互相钉住（`no_golden_file_is_orphaned`，tests/golden_fixtures.rs:106）。9 种 `ItemKind`、13 种 `ServerEvent` + 1 种 `DaemonEvent`、14 个错误码，枚举覆盖由宏从同一份来源生成、编译器兜底。roadmap 里 M0 的协议声称因此**全部为真**。

**M2 要加的四样东西今天都还不在 wire 上**（别当成已有）：① diff 载荷类型——全仓库无 `UnifiedDiff`/`DiffHunk`/`DiffLine`/`FileDiff`（design/capabilities.md §2 草图的 `diff() -> UnifiedDiff` 没有返回类型）；② `ApprovalRequest` 的结构化 preview——今天只有 `args_digest: String`，其文档自己写着「Not the raw JSON … 可以是 megabytes」，**装不下 diff 或完整命令**；③ 审批规则的 list/delete 方法——今天**无法查看也无法撤销**一条已持久化的规则，一条误存的 `DenyAlways` 会永久废掉一个工具，除了改数据库没有办法；④ `SessionLoadResult.pending_approvals`——`PendingApproval`（method.rs:498）今天零 fixture、零消费者。四项都是**加性**的，因此在 major 1 内合法（design/protocol.md §6「方法/字段只增不改语义」）。

## 待办

- [x] (M0) 定 ItemId 方案 → **UUIDv7**（`uuid` crate 的 `v7` + `serde`；理由见设计文档开放问题 3）
- [x] (M0) 命名统一：wire 类型 `Session`（不再用 `Thread`），与 `session/*` 方法名、`sessions` 表名一致
- [x] (M0b) 全部类型定义（`Session`/`Item`/`ItemKind` 9 种/`Content`+`ContentPart`/`ToolStatus`/`ToolOutput`/`CheckpointKind`/`SignatureBlock`/`ServerEvent` 13 种 + `DaemonEvent`/JSON-RPC 帧）
- [x] (M0b) 方法名常量表（daemon 路由表的单一真相）+ `PROTOCOL_VERSION` 常量与支持区间
- [x] (M0b) 错误码枚举定型（14 个：5 个 JSON-RPC 标准 + 9 个应用码），数值 golden 锁定
- [x] (M0b) serde 往返测试 + golden JSON（`tests/fixtures/protocol-v1/`）+ 字段序确定性 + 公共 API 的可运行 doctest —— **golden 用纯 JSON 而非 insta**，理由见设计文档 §6
- [x] (M1) client helper（2026-10-01）：`DaemonClient`（call_raw/call/hello，id 路由 + 超时）+ `EventStream`（`session/event` notification 解包）；attach_or_spawn 的 spawn 半边随 CLI Phase 4，重连补差随 e2e Phase 5
- [x] (M1) `HelloParams.boot_token`（加性字段，fixture 已更新）
- [x] (M2) edit_item/branch/rewind 方法的参数细节随 store 实现定稿 → **早已定稿，条目本身过时**（2026-10-07 勘察）：`RewindParams` 已带 `purge_untracked`（method.rs:409-421，`#[serde(default)]` = false）、`RewindReport { rolled_back, purged }` 已在（:425）、`EditItemParams` 已带 `rewind_scope: Option<RewindScope>`（:314-322）、`BranchDeleteParams` 已带 `confirm: bool`（:389-395），四个都有 golden fixture（方法面双向机器强制，见「当前状态」）
- [ ] (**M3**，2026-10-07 由 M2 顺延) 事件 coalescing 策略实测调参（与 daemon hub 联动）；`ServerEvent::is_coalescable`（event.rs:195）已就位。**顺延理由**：它只覆盖 `TextDelta`/`ReasoningDelta`，而 M2 新增的事件量主要是 `ToolCallProgress`——**不可合并**，所以 coalescing 治不了 M2 的病。M2 只保留「一次 Code 会话的事件量测量」并记档（记在 worklog/daemon.md，`hub.rs` 顶部注释的原意）
- [ ] (M2 · Phase 1) **diff 载荷类型** + golden fixture（`UnifiedDiff` 一类）：全仓库今天没有这个类型，而 TUI 的 diff 预览、审批 preview 与 M4 的 GUI diff 视图共用它。**必须排在 Phase 1**——`CheckpointStore::diff(from, to)` 需要返回类型
- [ ] (M2 · Phase 2) **D14** 审批预览载荷：建议给 `ApprovalRequest` 加一个**可选的结构化 preview 字段**（`UnifiedDiff | Command{argv,cwd} | Excerpt`），而不是新开 `approval/details` 方法——M3 的 ACP `session/request_permission` 要同一份内容进 `ToolCallContent`，放请求里一次到位
- [ ] (M2 · Phase 2) 审批规则的 **list / delete** 方法（随 **D8** 定的 scope/matcher/decision 文法与求值顺序）：今天一条已持久化的规则既看不见也撤不掉
- [ ] (M2 · Phase 3) `SessionLoadResult.pending_approvals: Vec<PendingApproval>`——给 `PendingApproval` 第一个消费者，解决重连时审批弹层的重画

## 开放问题

见设计文档末尾 4 条（coalescing 策略、watch/takeover、大输出引用）。3 已关闭；1 与 4 在 2026-10-07 的 M2 重排里各有了归属，2 仍开放：

- 2026-10-07 **开放问题 1（coalescing 策略）→ 顺延 M3**，理由同待办那条：`is_coalescable`（event.rs:195）只覆盖 `TextDelta`/`ReasoningDelta`，而 M2 的新增事件量主要是**不可合并**的 `ToolCallProgress`。M2 只产出一次 Code 会话的事件量测量，作为 M3 定策略的依据。
- 2026-10-07 **开放问题 4（大工具输出走「存库 + 事件带引用」）→ 形状已就位，阈值与落盘路径是 M2 Phase 4 的 D12**。`ToolOutput.spilled`（`SpilledOutput`，tool.rs:52-60、:119）在协议里已存在，但**没有任何产品路径构造过它**——全仓库只有协议自己的 `ToolOutput::spilled` 构造器与 fixture 用到。它与 kernel 的上下文 token 预算 v1 同排 Phase 4，因为两者是「工具输出太大」这同一个问题的两半（来源是 shell 与 web_fetch）。Phase 4 还裁定**凭据脱敏发生在接缝处**（工具输出离开 backend 时）而不是入库时，理由是不变量 2 要求「重建 == 实际请求体」；对协议的含意是：`ToolOutput` 里存的必须就是模型看到的那份。
- 2026-09-28 ItemId → UUIDv7（用户裁决）。ULID 被否：26 字符可读性的收益不足以抵消「项目里出现第二种 id 格式」的成本；UUIDv7 同样时间有序，且 SQL/JSON/日志工具链天然认。

## 变更日志

### 2026-10-08 · M2 Phase 1：diff 载荷类型 + `CheckpointId`

新增 `src/diff.rs`：`Diff` / `DiffFile` / `DiffStatus` / `DiffHunk` / `DiffLine` / `DiffLineKind`，以及 `id.rs` 里的 `CheckpointId`（`checkpoints` 表的主键；GC 要按行删，而 item id 对安全快照不存在、commit id 属于影子仓库而不属于那条记录）。都是加类型，major 1 内合法（§6）。

形状由用户裁决为**结构化 hunk**，不是 unified 文本：D11 已经定了 TUI 用 `similar` 算 hunk，git2 侧也原生产出 hunk，所以两个生产者喂同一个类型、两个前端都不必写解析器。codex 走的是文本那条路，代价是每个渲染方一个解析器（`references/codex/codex-rs/tui/src/diff_render.rs` 2745 行）。三条细节：`DiffLine.text` **不带** `+`/`-`/空格标记（`kind` 已经说了，两个都带就会互相矛盾）、不带换行（渲染方决定怎么接行，折行不该继承折点）；`DiffStatus` 是 `git2::Delta` 去掉三个描述工作树状态的变体（`Ignored`/`Untracked`/`Conflicted`）——检查点 diff 永远是树对树，预览 diff 里也没有这三个概念；`binary: true` 时 `hunks` 为空，渲染方因此能说「二进制」而不是显示一片空白。

**roadmap 风险 3 这次没有兑现**：新增的 `Diff` 与 `CheckpointId` 都不在 fixture 注册表覆盖的三类里（`ItemKind` 载荷、方法结果、事件），所以 determinism 步骤全程绿、没有「有意修改 → 提交前保持红」那一轮摩擦。**也没有为 `Diff` 加 golden，这是对的而不是漏的**——第一个返回它的方法（Phase 3 的 rewind，或 `checkpoint_diff` 工具）落地时补；在那之前 serde 拼写由 `diff.rs` 内 7 条单测钉住，含「省略字段取默认、多出字段忽略、未知枚举值拒绝」三向。Phase 2 给 `ApprovalRequest` 加 preview 字段时才会真撞上那条摩擦。

protocol 覆盖率 93.1%（地板 80%）。

### 2026-10-07 · M2 重新规划对账

roadmap 的 M2 段按一次全仓库勘察重写为 Phase 0–8。**协议侧的结论是「M0 的声称全部为真、M2 全是加性改动」**：20 个方法都有参数与结果类型与 golden fixture（双向机器强制）、9 种 `ItemKind`、13 种 `ServerEvent` + 1 种 `DaemonEvent`、14 个错误码——枚举覆盖由宏从同一份来源生成。四项 M2 新增（diff 载荷类型 → Phase 1；`ApprovalRequest` 的结构化 preview 与规则 list/delete → Phase 2；`SessionLoadResult.pending_approvals` → Phase 3）都是加字段或加方法，因此落在 §6「方法/字段只增不改语义」里，major 1 内合法。**代价是已知的摩擦**：加字段会翻动 fixture 确定性门禁，而门禁把「工作树里有未提交的 fixture 改动」一律当成测试改写了 golden——M1 已经撞过（worklog/testing.md、worklog/cli.md、worklog/daemon.md 各记了一批「有意修改 → 提交前保持红、提交即恢复」），Phase 1/2 要预留。

**硬门不靠新增 `RiskLevel` 变体表达。** `RiskLevel::is_hard_gate()`（approval.rs:34）只认 `WritesOutside`，而 design/capabilities.md §5 把**工作区内**的 `.env*` 与 `.git/hooks` 也列为硬门——两者对不上。裁决是**不动枚举**：新增枚举值属 major bump（§6），而且未知枚举值一律硬失败是刻意设计，加一个值就要所有前端同步升级。已有的机制够用：`ApprovalRequest::once_only()`（approval.rs:112）用「不提供 `AllowAlways`/`DenyAlways`」表达「不可记忆」，而 kernel 的 `await_approval` 会拒绝一个没被提供过的答复（kernel/src/agent.rs:761）。所以路径门要做的只是**为工作区内的敏感路径强制 `once_only`**，不需要协议改动。

**历史移动后不加新事件。** rewind / branch switch / edit_item 都会让前端的投影失效，但新增事件 `type` 属 major bump。约定是：`SessionUpdated.state.active_branch_head` **已经在广播里**，前端发现它不是自己已投影 head 的后继就发 `session/load` 重建；发起方本来就能在自己的回复里拿到新 Session。协议侧因此零改动，这条约定写进 design/protocol.md §4。

**ADR-0006 与协议的拼写不一致 → 以协议为准。** ADR-0006（docs/decisions/0006-shadow-git-rewind.md:14）写 `RewindScope::{Conversation, Code, ConversationAndCode}`，协议实际的三个变体是 `{Conversation, Code, Both}`（session.rs:291-299）。ADR 一经 accepted 不修改，所以**不改 ADR**，在此留痕：wire 上与代码里的拼写都是 `Both`，读到 ADR 那个名字按 `Both` 理解。

**关掉的与顺延的。** 待办里「edit_item/branch/rewind 方法的参数细节随 store 实现定稿」是**过时条目**——四个参数早就定稿并有 fixture（见待办里的逐条实证），本次勾掉。事件 coalescing 由 M2 **顺延 M3**（用户 2026-10-07 裁决）：`is_coalescable`（event.rs:195）只覆盖 text/reasoning delta，而 M2 新增的事件量主要是不可合并的 `ToolCallProgress`；M2 只留一次事件量测量。

**删掉一处过时计数。** 本文件 2026-09-28 条目里那句「insta 不适合数据驱动 fixture」原本带着一个 fixture 数与一个等量的断言数，两者都已与磁盘上的实际数量不符。按 2026-09-30 的用户裁决（文档里的计数一律去掉，覆盖率交给测试自己机器检查），**删数字而不是改数字**——所以这里不写新数；design/protocol.md §6 当时已改为「不靠文档里的数字」，本次把 worklog 这处也对齐。

### 2026-09-30 · 评审后的两轮加固（2026-09-29 与 2026-09-30）

**读回来的形态要和写出去的形态分开验证。** `SessionPatch { title: Some(None) }`（「清空标题」的记号）写出去是 `"title":null`，读回来却成了外层 `None`（「别动」）——serde 不处理嵌套 Option，双 Option 存在的意义正好被吃掉。自定义 visitor 之后三态分明：缺失 = `None`、`null` = `Some(None)`、有值 = `Some(Some(_))`；写出去的形态没变，所以没有一个 golden 动过。同一套写法现在也用在帧的 `id` 上。

**显式 `"id": null` 一律拒绝（`FrameError::NullId`）。** `Probe.id` 原本是 `Option<Id>` + `#[serde(default)]`，于是 `null` 与「字段缺失」塌成同一个值：`{"id":null,"method":"session/prompt"}` 被分类成通知——一个本该有回复的请求被静默降级，调用方永远等下去。漏写 `skip_serializing_if` 的 `Option<Id>` 序列化出来正好是这种帧，所以它是要报告的客户端 bug，不是要吸收的噪声。`classify` 的文档早就这么写了，这轮是让代码追上文档；规范允许的那种 null-id 错误响应由 M3 的 ACP bridge 归一化。

**`FrameDecoder` 的三处修正（2026-09-29）**：坏行必须在报错**之前**排空，否则此后每次 push 都在同一批字节上失败；`MAX_FRAME_BYTES` 要在线循环**内部**检查，否则带终止符的超长帧能整帧通过；扫描要记住水位（`scanned`）——原本每次 push 都从字节 0 重扫，实测 2 MiB 帧按 32 字节分块喂要 157 s，加水位后 0.02 s。

**方法结果与枚举覆盖改成机器强制（2026-09-29）。** 20 个方法里只有 5 个的结果有 fixture，而所谓「typed 校验」接受任意 JSON 对象；现在每个方法都有 fixture + 会拒绝 `{}` 的校验器。枚举 → 注册表的覆盖由宏从同一份来源生成名字表与变体表，编译器兜底。未知枚举值是硬失败、未知字段必须忽略，两条都写进设计文档 §6 并各有测试。

**fixture 生成要能并发跑。** nextest 并行跑多个测试二进制，而 `version_compat` 从磁盘读 golden、`golden_fixtures` 往磁盘写：非原子的重写会让读者看到半截文件，报一个从没发生过的兼容性破坏。改成 tempfile + rename，读者在 `UPDATE_FIXTURES=1` 期间跳过。

**UUIDv7 的 id 在同一进程内严格递增（实测，uuid 1.26.1）。** `Uuid::now_v7()` 走进程级共享的 `ContextV7`：同毫秒内计数器递增而不是重新随机，计数器溢出就把时间戳往前推一毫秒。`ids_minted_back_to_back_increase` 连铸 1000 个 id 把这条钉住（上游改换上下文会主动失败，性质同 store 的 `SPIKE_*` tripwire）。依赖它的地方：`list_sessions` 的 `id ASC` tiebreak、`all_items`/`branch_tree` 的 `ORDER BY created_at ASC, id ASC`。**跨进程不成立**：时钟回拨后重启的进程会铸出比库里已有 id 更小的 id。（此前多处注释把同毫秒的 id 说成「随机尾巴、不保证顺序」，与实现相反，已一并改正。）

**`ItemIdRange` 不提供成员判定。** 原来的 `contains` 比 id 大小，前提是「id 序 == 链序」，而按上一条，这个前提只在同进程、时钟未回拨时成立。compaction（M5）用它判「这条 item 是否已被摘要覆盖」，错一次就是静默的历史损坏（该重放的不再重放，或反之）。span 因此是**位置**语义：成员是沿链落在两端点之间的那些 item，只有持有链的一方能解析——M5 与它的第一个消费者一起在 store 的树遍历里实现，现在不加没有调用方的 API。类型上只剩 `first`/`last` 与「两端含」的约定。

### 2026-09-28

**M0b 落地**（93 测试 + 4 doctest）。以下结论全部来自实测或落地时的结构性矛盾，不来自推断：

- **`#[serde(flatten)]` + 相邻标签枚举可用（serde 1.0.229）**：`ItemKind` 用 `tag="kind"`/`content="payload"` 并在 `Item` 上 flatten，得到 `{"id":…,"kind":"user_message","payload":{…}}`——与 `items` 表的 `kind`/`payload` 两列一一对应。九个变体的 `to_payload`/`from_parts` 往返有测试逐条断言。
- **内部标签枚举写不出「持有原始值的 newtype 变体」**：`StreamEvent::TextDelta(String)` 序列化直接失败（serde 的限制），改成命名字段变体 `TextDelta { text }`。
- **信封与事件字段重名会让帧写得出、读不回**：`SessionUpdated { session }` 与信封的 `session`、`GenerationBumped { generation }` 与信封的 `generation` 各撞一次。前者改成 `state`，后者改成无载荷变体（新代际就是信封的 `generation`）。新增测试 `no_event_field_collides_with_the_envelope` 防复发——它抓出的正是这两个。
- **内部标签枚举的 tag 写在载荷字段之前**：`{"session":…,"generation":…,"type":"text_delta","item":…,"text":…}`。`typed_serialization_keeps_declaration_order` 锁定这一顺序。
- **flatten 会把可变长字段放到最后**：`Item` 的 `created_at` 在 `kind`/`payload` 之后输出，与声明序一致（`value` 是 BTreeMap，golden 里是字母序）。
- **insta 不适合数据驱动 fixture**：快照名由断言表达式推导且必须字面量，于是要么给每个 fixture 手写一条断言、要么放弃注册表。（计数按 2026-09-30 的裁决不写进文档：见 2026-10-07 条。）最终用纯 JSON + `UPDATE_FIXTURES=1` 生成器；`scripts/ci.sh` 的 `INSTA_UPDATE=no` 让门禁里的重写被拒绝。详见设计文档 §6。
- **协议层不需要 `deny_unknown_fields`**：forward-compat 要求旧客户端忽略未知字段，测试 `a_missing_optional_field_deserializes_to_none` 断言未知字段不致命。
- `PROTOCOL_VERSION = "1.0.0"`，兼容判定只看 major（`is_compatible`）；无法解析的版本一律拒绝。

### 2026-09-28（M0a）

- 初稿。方法命名对齐 ACP 风格（session/*）以降低 acp crate 的翻译成本；事件模型吸收 codex EventMsg 与 atomcode AgentEvent 的教训（generation 字段是一等公民）。
- M0a：crate 骨架建立（GPL-3.0-only 元数据按可发布标准写全，依赖 serde/serde_json/thiserror/uuid）；ItemId 定为 UUIDv7；`Thread` 更名 `Session`（architecture.md 术语表、design/protocol.md §2 已同步）。存储侧的连带影响：`sessions.id`/`items.id` 以 TEXT 存 UUID（turso 的 `Value` 没有 UUID 类型，ADR-0010）。
