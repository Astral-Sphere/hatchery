# ADR-0010: 存储引擎用 turso 0.7.2（纯 Rust），递归 CTE 改为内存走树

状态：accepted（2026-09-28）

Supersedes: ADR-0002 的**引擎选型**部分。ADR-0002 的其余决策——daemon 内嵌、WAL、单写者 actor、`SessionStore` trait 隔离、JSONL 导出逃生门——全部继续有效。

## 背景

ADR-0002 定了「libSQL embedded + WAL + 单写者 actor」，但把具体 crate 留成开放问题（design/storage.md 开放问题 1），要求 M0 实测后再定（用户裁决：spike 按门槛实测后选型，允许改设计文档并新增 ADR）。

M0a 实测时的生态事实（crates.io 索引与上游 README，均为查阅所得）：

- `libsql` crate 最新稳定 0.9.30，embedded 走 `libsql-sys`（vendored C amalgamation），default features 还会拉入 replication/remote/sync/tls；上游已在发 0.10.0-pre。
- libSQL 项目已改名 Turso，上游 README 明说纯 Rust 重写 "**replaces libSQL as our intended direction**"，且已在生产使用但未到 1.0。
- `turso` crate 共 129 个版本、**25 个稳定版**，最新稳定 0.7.2；纯 Rust（`turso_core`），无 C 构建；`default-features = false` 可去掉 mimalloc 与 fts（tantivy）。
- 上游 COMPAT.md 自陈 tracking SQLite 3.50.4，其中两项与我们的设计冲突：`WITH RECURSIVE` 未支持、`PRAGMA synchronous` 只支持 OFF/FULL。

## 决策

1. **引擎 = `turso = { version = "0.7.2", default-features = false }`**，嵌入 daemon 进程。不再评估 libsql/rusqlite（门槛全过，无需降级）。
2. 因 `WITH RECURSIVE` 不受支持，`rebuild_history` 与 `DeleteBranch` 的子树收集改为**内存走树**：一次查出该 session 的 `(id, parent_id, kind, turn_id)` 骨架，在 Rust 内回溯/BFS，再按 id 批量取 payload。
3. `PRAGMA journal_mode = WAL` 必须走 `query()` 而非 `execute()`——它返回一行，turso 的 `execute()` 会以 `Misuse("unexpected row during execution")` 拒绝任何返回行的语句。
4. schema 修正两处：`checkpoints.commit` → `commit_id`（`commit` 是保留字，解析器报 `near "commit": syntax error`）；`sessions.active_head` 改为**可空**（`items.session_id → sessions` 与 `sessions.active_head → items` 互为外键，非空则两边都插不进去；NULL 语义 = 尚无 item）。
5. 门槛测试永久留在 `crates/hatchery-store/tests/spike_engine.rs`，作为引擎升级的回归网；其中两个 `SPIKE_*` 常量是**tripwire**：上游补上 `WITH RECURSIVE` 或 `synchronous=NORMAL` 行为变化时测试会失败，逼我们重新审视绕开方案，而不是让 workaround 悄悄留着。

## 实测结果（2026-09-28，Linux x86_64，turso 0.7.2，12 项全过）

| 门槛 | 结果 |
|---|---|
| schema v1 全量 DDL（6 表 + 2 索引 + 触发器 + FK）一批 `execute_batch` | ✅ 全部生效 |
| `items_no_update` 触发器 `RAISE(ABORT, …)` | ✅ 真的拦下 UPDATE，错误携带我们的消息，行内容不变；INSERT/DELETE 仍合法（不变量 3 成立） |
| `PRAGMA foreign_keys = ON` 是否真强制 | ✅ 引用不存在 session 的 item 被拒 |
| `ON DELETE CASCADE` 沿 `parent_id` 链 | ✅ 删子树根 → 子孙 item + 关联 checkpoint 一并删除 |
| active_head 指向待删子树时 | ✅ 被外键拒绝（`delete_branch_refuses_when_active_head_inside` 的数据库级兜底，白拿） |
| WAL：写事务开启时另一连接读 | ✅ 读到已提交、看不到未提交、commit 后立即可见 |
| `PRAGMA user_version` 跨重开 | ✅ 保留（迁移钩子可用） |
| 重开后已提交数据完整 | ✅（关闭后**留下 `-wal`，没有 `-shm`**） |
| `WITH RECURSIVE` | ❌ 不支持（与 COMPAT.md 一致） |
| `PRAGMA synchronous = NORMAL` | ✅ 接受且回读 1（**COMPAT.md 的「只支持 OFF/FULL」是过时的**） |
| 500 次 item 边界提交（每语句一提交） | NORMAL 1251 µs/commit、FULL 1222、OFF 1351 |
| 500 insert 放进单个事务 | 1212 µs/insert —— **比逐条自动提交更慢** |

## 理由

1. **纯 Rust = 无 C 工具链**。CI 要在 ubuntu/macos/windows 三平台跑，且平台策略是 windows 走 MSYS2 ucrt64 + `x86_64-pc-windows-gnu`；`libsql-sys`/`rusqlite bundled` 都需要 mingw 交叉环境，turso 不需要。
2. **押注上游的主线方向**。libSQL 是 fork 路线，上游自己声明 turso 取代它；选 fork 的长期维护风险更高。
3. **关键门槛实测全过**，包括最担心的 append-only 触发器——不变量 3 能继续由数据库强制，而不是退化成 Rust 侧的自觉。
4. **逃生门还在**：`SessionStore` trait（ADR-0002）隔离了引擎，若 turso 出现阻塞性回归，切换成本局限在 store crate 内。

## 代价与已知缺口

- **无 `WITH RECURSIVE`** → 树遍历在 Rust 内做。副作用是好的：纯函数遍历更容易 proptest 对拍参考模型（testing.md §3.4）。
- **`PRAGMA synchronous` 未见可测效果**：实测三种设置耗时在噪声范围内相同（OFF 甚至最慢），据此**推测**该 pragma 被解析并回读，但不改变 fsync 行为。后果：不把 durability 调优寄托在这个 pragma 上；durability 以「重开后已提交数据完整」的实测为准。**断电级 durability 未测**；进程级崩溃（kill -9）测试在 M0b 随 store 实现补上（testing.md §3.4）。
- **上游文档可能过时**（本例即 COMPAT.md 的 synchronous 条目）。纪律：第三方行为一律以本地实测为准（testing.md §0.2），文档结论只作为待验证假设。
- **0.x 版本**：minor 之间可能有破坏性变更。`Cargo.lock` 入库；引擎升级必须是独立提交并跑完整门槛测试。
- **批量事务不划算**（实测反直觉）：因此 ADR-0002 的「item 边界即 commit」不必为性能让步；若将来实测翻转，再考虑 writer actor 内合并提交。
- **绝对写入延迟偏高**（~0.8–1.3 ms/语句，SQLite WAL 通常快一个量级）：对 agent 会话（每秒数十 item 封顶）无影响，但 GUI 的 10k items 会话加载要靠骨架查询 + 分页，不能逐条查。

## 替代方案（已否）

- **`libsql` 0.9.30（`default-features = false, features = ["core"]`）**：SQLite 语义最完整（递归 CTE、synchronous=NORMAL 都可用），设计文档不用改；但 vendored C 构建让三平台 CI（尤其 windows-gnu）多背 mingw，且上游已声明转向 turso。**保留为第一顺位替代**：若 turso 出现阻塞性回归，切回它并把本 ADR 标 superseded。
- **`rusqlite`（bundled SQLite）**：语义最保守、生态最熟；同样需要 C 构建，且与「将来接 sqld/turso server」的路线不一致。
- **`turso` 0.8.0-pre**：预发布版，无稳定承诺，不进主干。

## 后果

- design/storage.md 更新：引擎、async API 形态、内存走树、`active_head` 可空、`commit_id`、`journal_mode` 走 `query`、synchronous 实测结论。
- store 的 SQL 全走 turso 的 async API：`Builder::new_local(path).build().await` → `Database::connect()` → `Connection::{execute, query, execute_batch, prepare, unchecked_transaction}`；`Value` 只有 `Null/Integer(i64)/Real(f64)/Text(String)/Blob(Vec<u8>)`，故 UUID 以 TEXT 存取、时间戳以 INTEGER。
- 迁移用 `PRAGMA user_version`（实测跨重开保留）+ `schema_meta` 表双记录。
- 只读连接不需要 `-shm`（实测未生成）；仍按 ADR-0002 的「单写者 + 多只读连接」组织，实测已验证写事务开启时并发读可用。
- worklog/storage.md 记录实测过程与数字；开放问题 1（crate 选型）关闭。
