# 设计：存储与会话分支（hatchery-store）

> 状态：**已实现（M0b）**，M1 增补两个 trait 方法（`bump_generation`、`open_turns`）。文中标了 **M2 Phase N** 的形状是目标态、尚未实现。依据 ADR-0002（单写者）、ADR-0003（编辑即分叉）、ADR-0010（turso 引擎）。引擎行为由常驻的 `tests/spike_engine.rs` 门槛套件锁定（M0a 实测；**升级引擎必须重跑**）。

## 1. 职责

- 定义 `SessionStore` trait（daemon 依赖的唯一存储接口）。
- **turso 0.7.2** embedded 实现（ADR-0010，纯 Rust，无 C 构建）：schema 迁移、writer actor、分支树查询、JSONL 导出。
- 模块划分：`store.rs`（trait + `TursoStore`）、`actor.rs`（`StoreCmd` + 单写者任务）、`sql.rs`（pragma 与行转换）、`tree.rs`（纯树遍历）、`migrations.rs`、`export.rs`。

trait 的形状按声明序列全，两个后加的方法各有其存在理由：`bump_generation` 因为 generation 有意**不在** `SessionPatch` 里（不变量 1：它只在新 runtime 接管会话时移动），`open_turns` 因为崩溃恢复要在任何 runtime 存在之前一次读全（重启后一行「还在跑」的 turn 描述的是没人在做的工作）。

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(&self, session: Session) -> Result<Session, StoreError>;
    async fn session(&self, session: SessionId) -> Result<Session, StoreError>;
    async fn update_session(&self, s: SessionId, patch: SessionPatch) -> Result<Session, StoreError>;
    async fn bump_generation(&self, s: SessionId) -> Result<Session, StoreError>;
    async fn list_sessions(&self, p: SessionListParams) -> Result<SessionListResult, StoreError>;
    async fn delete_session(&self, s: SessionId) -> Result<u64, StoreError>;
    async fn append_item(&self, item: Item) -> Result<ItemId, StoreError>;
    async fn append_items(&self, items: Vec<Item>) -> Result<(), StoreError>;
    async fn item(&self, s: SessionId, item: ItemId) -> Result<Item, StoreError>;
    async fn rebuild_chain(&self, s: SessionId, head: Option<ItemId>) -> Result<Vec<Item>, StoreError>;
    async fn branch_tree(&self, s: SessionId) -> Result<BranchTree, StoreError>;
    async fn edit_fork(&self, s: SessionId, item: ItemId, new: Content) -> Result<Item, StoreError>;
    async fn switch_branch(&self, s: SessionId, head: ItemId) -> Result<Session, StoreError>;
    async fn delete_branch(&self, s: SessionId, head: ItemId) -> Result<u64, StoreError>;
    async fn start_turn(&self, s: SessionId, turn: TurnId, at: Timestamp) -> Result<(), StoreError>;
    async fn finish_turn(&self, s: SessionId, turn: TurnId, c: Option<TurnCompletion>) -> Result<(), StoreError>;
    async fn open_turns(&self) -> Result<Vec<(SessionId, TurnId)>, StoreError>;
    async fn export_jsonl(&self, s: SessionId, path: PathBuf, all_branches: bool) -> Result<u64, StoreError>;
    async fn shutdown(&self) -> Result<(), StoreError>;
}
```

`SessionStoreError` 是**具体**的（`SessionNotFound`/`ItemNotFound`/`SessionMismatch`/`ActiveHeadInside`/`NotEditable`/`UnknownTurn`/`ExportExists`/`CorruptItem`/`Migration`/`Invalid`/`Database`/`WriterGone`），因为它要被翻译成 wire 错误码（`to_event_error()`）：「必须先切分支」是调用方问题（`InvalidRequest`），「磁盘满了」是存储问题（`StoreError`）。

## 2. Schema（v1）

`migrations/v1.sql` 就是 M0a spike 里实测通过的那份 DDL，`include_str!` 嵌入二进制。**它是迁移，发布后不得再改**——加列加表都要新文件。三处 spike 修正已内建：`sessions.active_head` 可空、列名 `commit_id`（`commit` 是保留字）、pragma 不在迁移里（per connection）。

六张表（`sessions`/`items`/`turns`/`checkpoints`/`approval_rules`/`schema_meta`）+ 两个索引 + `items_no_update` 触发器（`RAISE(ABORT, 'items are append-only')`）+ `ON DELETE CASCADE` 外键；细节见 v1.sql 的注释。

后两张表的 API 层排在 M2（`checkpoints` → Phase 1、`approval_rules` → Phase 2）；schema 版本仍是 1、`MIGRATIONS` 只有一项，所以**两者都不需要新迁移**。它们的列形状各自约束了一个设计决定：

```sql
checkpoints(id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            item_id    TEXT REFERENCES items(id) ON DELETE CASCADE,   -- 可空
            workspace TEXT NOT NULL, commit_id TEXT NOT NULL,
            kind TEXT NOT NULL, created_at INTEGER NOT NULL)

approval_rules(id TEXT PRIMARY KEY, scope TEXT NOT NULL, matcher TEXT NOT NULL,
               decision TEXT NOT NULL, created_at INTEGER NOT NULL)
```

- `checkpoints.item_id` **可空**是承重的：rewind 之前那次「安全快照」记进这张表时 `item_id = NULL` 且**不建 item**——它是 undo-of-undo，不属于对话历史（§5）。
- `checkpoints.session_id` 的 `ON DELETE CASCADE` 让「孤儿影子仓库」变成可判的：删会话后该 workspace 一行不剩，所以「`WHERE workspace = X` 还有行吗？」就是「这个影子仓库还有没有主」（开放问题 3，**Phase 1 已按此实现**）。这条判据要求 `workspace` 列的拼写与影子仓库自己记的那一份一致，两处都走 daemon 的 `recorded_workspace()`。
- `approval_rules` **没有排序列、没有 enabled 列、`scope` 是裸 TEXT、没有 session 外键**。所以规则的求值顺序与匹配语义必须由审批层的定义给出（M2 的 D8），不能指望从 schema 读出来；表本身只保证「一条规则存得下、查得回、删得掉」。

`items` 的 `kind` + `payload` 两列与 protocol 的 `ItemKind` 相邻标签表示一一对应：`ItemKind::to_payload()` 取内层 `payload`，`from_parts(tag, payload)` 反向重建；`items.kind` 存 `ItemKindTag::as_str()`。读到一个本版本不认识的 kind 或与 kind 不匹配的 payload → `StoreError::CorruptItem { id, message }`，**报错而不是猜**。

## 3. 写路径：writer actor

```rust
pub enum StoreCmd { /* CreateSession, Session, UpdateSession, BumpGeneration, ListSessions,
                       DeleteSession, AppendItem, AppendItems, Item, RebuildChain, BranchTree,
                       EditFork, SwitchBranch, DeleteBranch, StartTurn, FinishTurn, OpenTurns,
                       ExportBody, Shutdown */ }
pub type Reply<T> = oneshot::Sender<Result<T, StoreError>>;
```

- 单 tokio task 持有唯一写连接，串行消费；channel 有界（`COMMAND_CAPACITY = 1024`），写满时提交方 await——**背压就是通道填满**，不靠无限排队。
- 调用方等在 reply slot 上：`ask(|reply| StoreCmd::X { .., reply })`；actor 任务消失 → `WriterGone`。`shutdown` 既等这个回复、也等 writer 任务真正结束：任务在回复之后 panic 或被 abort 会报 `Database`，不静默吞掉。
- **写入前先认会话**：`append_items`/`start_turn`/`finish_turn` 都先确认会话存在，报 `SessionNotFound`，而不是让外键拒绝后把引擎的消息裹成 `Database`——按 §1 的映射，前者是调用方错误、后者是存储故障，到前端是两个不同的错误码。一批 item 还必须**同属一个会话**：head 推进只认最后一个 item 的会话，混批会让另一个会话的 item 永远够不着（`sessions.active_head` 的外键只证明 item 存在，证不了同会话——与 `insert_item` 已有的跨会话父节点校验是同一类洞）。
- 流式 delta **不进** writer actor（内存聚合，item 完成才落库）；崩溃最多丢当前 item。
- **事务边界**：单条 append 也让「插 item + 推进 active_head + 更新 updated_at」同事务（不是为速度，是为正确性——插入失败时 head 绝不能动）；`append_items` 整批一个事务。实测反直觉的结论是「单事务批量 500 insert 比逐条自动提交更慢」，所以不做无谓的合并。
- 读命令也走同一个 actor（**只读连接池排 M3**，见 worklog 待办：读一直串行经 writer actor，正确优先，等读吞吐成为实测瓶颈再做）；`export_jsonl` 的渲染走 actor、写文件在 store 侧（`std::fs`：tokio 的 feature 集没有 `fs`，而导出是偶发的小文件写，不值得为此加 feature）。

## 4. 读路径

```rust
pub struct SkeletonRow { pub id: ItemId, pub parent: Option<ItemId>, pub kind: ItemKindTag, pub turn: Option<TurnId> }
pub fn chain(skeleton, head) -> Result<Vec<ItemId>, TreeError>;     // 回溯到根 + 反转
pub fn subtree(skeleton, root) -> Vec<ItemId>;                      // BFS 收子树
pub fn tips_map(skeleton) -> Result<HashMap<ItemId, Vec<ItemId>>, TreeError>;  // 每条分支的 tip
```

- **内存走树**（ADR-0010：引擎无 `WITH RECURSIVE`）：一次查出该 session 的骨架，在 Rust 内走，再按 id 分块（`PAYLOAD_CHUNK = 200`）取 payload，最后按链序拼回（引擎返回的行序不保证是链序）。300 条链的测试覆盖分块边界。
- 纯函数 + 环检测：`chain` 遇到父指针成环返回 `TreeError::Cycle`，而不是永远走；`tips_map` 同样。
- `rebuild_chain(session, head)` 返回**有序的 `Vec<Item>`**，不做任何 payload 过滤。草图中的 `rebuild_history(session, head) -> Vec<Message>`（含 reasoning 能力过滤与 compaction 区间替换）被否决：那需要 provider 能力表，属于 daemon 的装配器（kernel.md §6）。store 只回答「记录了什么、什么顺序」。
- 大 session：骨架一次全取可行（实测 1000 行 3.4 ms），payload 必须分块。
- `branch_tree` 返回 `BranchTree { head, nodes: Vec<BranchNode { row, created_at, active }> }`，`active` 由同一次 `chain` 的结果标记——GUI 的分支视图直接可用。

## 5. 分支语义细节

- **编辑**：`edit_fork(session, X, C')` → 新 item N（`parent = X.parent`、`turn` 同 X、**kind 同 X**、内容换成 C'），`active_head = N`；X 及其旧子树保留。只能编辑带内容的 kind（`UserMessage`/`AssistantMessage`），其余返回 `NotEditable`——编辑的载体是 `Content`。
- **切换**：`active_head` 指向任意 item；rebuild 自动生效。分支 = active_head 所在的根到节点链，没有显式 branch 实体。
- **删除**：内存 BFS 收子树（只删后代，共享祖先不动）→ 校验 `active_head` 不在子树内（否则 `ActiveHeadInside`）→ 删子树根，靠 `ON DELETE CASCADE` 完成 → **交叉校验**：删除前后的 item 行数差必须等于 BFS 收到的数量，不等就报错（引擎的 `execute` 只回报直接删除的行数，计数必须来自我们自己的走树；两者不一致说明有一边错了，不能把错的数字报给调用方）。实测兜底：即使应用层漏了校验，`sessions.active_head` 的外键也会拒绝删除。
- **跨会话父节点**：schema 的 `parent_id` 外键只能证明父节点**存在**，不能证明它属于同一会话；`insert_item` 额外校验，否则会造出一棵谁都不走的树。同一类校验覆盖 `switch_branch`/`delete_branch` 的 head 参数（`SessionMismatch`）与整批 append（§3）。
- **检查点如何进链（M2 Phase 1 · D13，已落地）**：kernel 造一个 `CheckpointCollector` **借给** `ToolHost::invoke`；capabilities 的注册表把 backend 包进 `CheckpointedFs`，它在每次写之前 push 进去；kernel 在 **ToolResult item 之前**追加 Checkpoint item，链因此是 `… → ToolCall → Checkpoint → ToolResult`。这个顺序是安全的，因为工具结果靠 `ToolResult.call: ItemId` 与其调用配对、**不靠父子关系**（kernel.md §7「一次只开一个 item」的纪律不受影响：Checkpoint 是在 ToolResult 之前串行提交的完整 item）。daemon 在 Checkpoint item 落库后补写 `checkpoints` 行；**行写失败只记日志**——item 里已经带着 commit_id，rewind 可以回退到走链。
  早先写法是「`ToolCtx` 带收集器、`ToolInvocation` 把 `Vec<Checkpoint>` 带出来」，**在取消路径上会丢掉它们**：kernel 的工具 select 是 cancel-first，被中断的 invocation 直接 drop 且不再被 poll，所以它要返回的东西永远到不了 kernel。收集器改成借进去的，完成/失败/中断三条出口都能 drain（kernel.md §5）。
- **rewind 靠 item 链定位 commit，不靠 `checkpoints` 表**：`ItemKind::Checkpoint { commit_id, kind }` 自己带着 commit id，而 `ItemKind::is_conversation()` **不含** Checkpoint，所以检查点 item 永远不会进模型请求。Code scope 因此是「`rebuild_chain(session, old_head)` → 定位 `target_item` → **向后**扫第一个 Checkpoint item → 读它的 `commit_id` → restore」。这条规则的正确性只依赖一件事：**pre-write 快照恰好等于 target_item 时刻的工作区状态**——所以向后扫不到 Checkpoint 意味着 target_item 之后根本没写过东西，Code rewind 是 **no-op**（不是错误）。`checkpoints` 表因此只服务**跨会话的预算核算与 GC**，不是 rewind 的主索引。
- **`Both` 的顺序**：先 restore 代码、成功之后再移 head——restore 失败时历史绝不能已经被移走。restore 之前自动打一次安全快照，记进 `checkpoints` 表且 `item_id = NULL`、**不建 item**（它是 undo-of-undo，不属于对话历史；该列可空正是为此留的，见 §2）。
- **级联删除不驱动 git 侧的 commit GC，那件事做不到**：`delete_branch`/`delete_session` 让 `checkpoints` 行随外键级联消失（引擎门槛测试已锁定），影子仓库里的 **commit 对象也确实不会自己消失**——但 libgit2 没有对象级 GC（`Repository` 只有 `odb()` 读写与 `cleanup_state()`），而丢弃链上的提交必须重提交幸存者、**重提交的 commit id 会变**，那些 id 已经在 append-only 的 `items` 表里（`items_no_update` 触发器拒绝修正）。所以回收只有「整个影子仓库」一种粒度：行全部级联消失 ⇒ 没有 item 还指着它 ⇒ `sweep_orphans` 删目录是安全的（开放问题 3）。仍被 item 指着的工作区超预算时走 D9 的阶梯（capabilities.md §2），代价是旧 rewind 目标报 `UnknownCommit`。
- **命名分支**（可选，M4）：`branch_note` item 给用户标注分支用途。

## 6. 迁移

`PRAGMA user_version`（实测跨重开保留）+ `schema_meta.schema_version` **双记录**，两者不一致即报 `Migration` 错——说明有东西绕过了迁移。`schema_meta` 少了那一行同样拒绝启动，**不为 `user_version = 0` 开口子**：迁移表从版本 1 开始（`the_migration_list_is_ordered_and_starts_at_one` 钉死），能走到这一步的库必然已经写过两个记录，缺一个就说明有人动过它，而「没有 schema 也算开库成功」是最糟的那种成功。每个迁移一个事务（半套 schema 比没有更糟：重试会撞上第一次已建的表）；`PRAGMA user_version = N` 的值是本进程算出的 u32，不是用户输入，所以格式化进语句是安全的（pragma 不接受绑定参数）。数据库来自更新版本的 hatchery（`user_version > latest`）时**拒绝启动**，而不是按旧 schema 去读。

## 7. JSONL 导出格式

每个 session 一个文件，每行 `{"v":1,"item":{…}}`：

- 默认按 active 分支顺序，只有 `v` 与 `item`。
- `all_branches` 时导出全部 item（按 created_at 排序），每行多一个 `"branches"` 字段，列出**该 item 是其祖先的每条分支的 tip**（叶子是自己的 tip；共享祖先列出所有下游 tip）。草图的 `"branch"`（单数）改为复数：一个共享祖先属于多条分支。
- 目标文件已存在 → `ExportExists`：审计导出绝不能悄悄覆盖上一份。
- `FORMAT_VERSION = 1` 写在每一行：读不认识的版本应当拒绝文件，而不是猜字段含义。

## 8. 测试

- **引擎门槛测试**（M0a）：`tests/spike_engine.rs` 是常驻套件，锁定 turso 的真实行为（触发器、外键级联、WAL 并发读、`user_version`、`WITH RECURSIVE` 缺失、写入延迟）。两个 `SPIKE_*` 常量是 tripwire：上游补上递归 CTE 或 `synchronous` 行为变化时测试主动失败。
- **`tests/session_store.rs`**：会话 CRUD 与分页、append 与 head 推进、批量原子性、跨会话父节点与跨会话批次拒绝、写进不存在的会话（单条 / 批量 / turn 起止）报 `SessionNotFound`、300 条链的重建顺序、编辑分叉与旧分支保留、切换、切换与删除拒绝别的会话的 item、级联删除与拒绝、姐妹分支不受影响、branch_tree 的 active 标记、turn 起止、导出（active / 全树及其逐行顺序 / 空会话 / 拒绝覆盖）、迁移与重开（含更新的 build 与 `schema_meta` 缺行两种拒绝）、损坏 payload 报告。其中下面第 2、3 条走「store 背后」用第二条连接（第 1 条只经 store 重读）：
  - `invariant_items_are_never_rewritten`：编辑后原 item 逐字段不变；
  - `invariant_the_database_refuses_to_update_an_item`：直接 `UPDATE items` 被触发器拒绝，错误里带着我们的消息；
  - `a_corrupt_payload_is_reported_with_its_item`：手写一条不匹配的行，读回时报 `CorruptItem` 且带 item id。
- **`tests/tree_proptest.rs`**：随机脚本（append / edit_fork / switch / delete）同时作用于 `TursoStore` 与 testkit 里**独立写出**的 `ReferenceTree`，每步之后比较链形态（id/parent/kind）、head、active 集合与总行数。64 cases；失败种子入库（`*.proptest-regressions`）。
- **`tests/crash_recovery.rs`**：kill -9 崩溃恢复。子进程是**这个测试二进制自己**（`current_exe()` + 环境变量 `HATCHERY_CRASH_PROBE` + `#[ignore]` 的入口测试），不新增 target、不发布任何二进制；子进程提交后打印 `ready {json}` 并挂起，父进程读到就绪行后 `Child::kill()`（unix = SIGKILL，Windows = TerminateProcess），重开断言。五个 probe 分别覆盖 append / edit_fork / switch_branch / delete_branch / finish_turn，另有一项断言恢复后的数据库**可用**（不只是可读）。
- 崩溃测试**不覆盖断电**：spike 实测 `PRAGMA synchronous` 无可测影响，所以进程边界以下没有证据（开放问题 4）。

## 开放问题

1. ~~libSQL crate 选型确认~~ → **已关闭（2026-09-28，ADR-0010）**。
2. items.payload 是否需要抽列（如 tool_name）做二级索引以加速 GUI 过滤——先 JSON extract 查询，量大了再加生成列（turso 的 `GENERATED` 列是 partial 支持，真要走得先实测）。
3. ~~全局 GC：孤儿 checkpoint 仓库的清扫策略~~ → **已关闭（2026-10-08，M2 Phase 1）**。判据用的就是 schema 里那条：`checkpoints.session_id` 带 `ON DELETE CASCADE`，所以「`WHERE workspace = X` 还有行吗？」等价于「这个影子仓库还有没有主」。实现是 daemon 的 `sweep_orphans`：启动扫一次 + 每次预算检查时扫，`CheckpointStore::recorded_workspace(dir)` 从仓库自己的 config 反查归属，无行则删整个目录；**认不出属于谁的一律保留**（「cannot tell」不等于「delete」）。
   同一条里那半句「级联删除驱动的 git 侧 commit GC」**做不到，也不该做**：libgit2 没有对象级 GC（`Repository` 只有 `odb()` 读写与 `cleanup_state()`），而丢弃链上的提交必须重提交幸存者——**重提交的 commit id 会变**，那些 id 已经在 append-only 的 `items` 表里（`items_no_update` 触发器拒绝修正）。所以回收只有「整个仓库」这一种粒度，预算超限时走 D9 的阶梯（capabilities.md §2）。
   另有一处实现时必须对齐的细节：影子仓库记录的是**规范化后**的工作区路径，写行的地方也必须用同一拼写（daemon 的 `recorded_workspace()`），否则清扫会把活着的仓库当孤儿删掉。
4. 断电级 durability：实测 `PRAGMA synchronous` 三种取值耗时相同（推测 pragma 无实际 fsync 效果），因此「掉电不丢已提交 item」目前**没有证据**。M0b 的 kill -9 测试覆盖进程级崩溃；若需要更强保证，得实测 turso 的 checkpoint/fsync 时机（M1 结合 daemon 崩溃恢复评估）。
5. 10k items 会话的加载策略（骨架全取 + payload 分页的具体阈值）——M4 GUI 性能 fixture 时定。
6. 只读连接池与 `spawn_blocking` 读路径（**M3**，2026-10-03 由 M1 改标）——现在读也走 writer actor，串行但正确；等读吞吐成为实测瓶颈再做。