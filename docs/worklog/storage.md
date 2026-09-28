# 工作记录：存储与分支（hatchery-store）

- 范围：SessionStore trait、引擎 schema、writer actor、分支查询、迁移、JSONL 导出
- 设计文档：[../design/storage.md](../design/storage.md)
- 相关 ADR：0002、0003、0006、**0010**

## 当前状态

**M0b 完成（2026-09-28）**：schema v1 迁移、writer actor、`SessionStore` 全量实现（含分支操作与 JSONL 导出）、属性测试与 kill -9 崩溃恢复测试全部落地；64 个测试全绿（12 项引擎门槛 + 22 项 store 集成 + 1 项属性测试 + 6 项崩溃恢复 + 单元测试）。设计文档 `docs/design/storage.md` 已按实现重写。

## 待办

- [x] (M0) **引擎 spike（实测，勿靠文档推断）**：turso 0.7.2 全门槛实测 → 选中；结论落 ADR-0010，测试沉淀为常驻回归（见下「实测记录」）
- [x] (M0b) schema v1 落成 `include_str!` 迁移脚本 + 迁移框架（`user_version` + `schema_meta` 双记录，每迁移一个事务，拒绝新版本库）
- [x] (M0b) writer actor + StoreCmd 全量实现 + 有界背压（1024）
- [x] (M0b) rebuild_chain（**内存走树**，引擎无递归 CTE）+ 属性测试（随机编辑序列 vs testkit 里独立写的 `ReferenceTree`）
- [x] (M0b) EditFork / SwitchBranch / DeleteBranch（内存 BFS 收子树 + active_head 校验 + 级联删 + **数量交叉校验**）
- [x] (M0b) kill -9 崩溃测试（**测试二进制自重入**，不新增 target；五种 StoreCmd 各一次 + 恢复后仍可用）
- [x] (M0b) ExportJsonl（从 M2 提前：逃生通道成本低、测试便宜）
- [ ] (M1) 只读连接池与 spawn_blocking 读路径接线
- [ ] (M2) checkpoints 表与 CheckpointStore 联动（级联删除时 GC；checkpoint 的级联已由引擎门槛测试锁定）
- [ ] (M5) 导入

## 实测记录（2026-09-28，turso 0.7.2，Linux x86_64）

除标注「推测」的两条解释外全部为**实测**；`cargo nextest run -p hatchery-store --nocapture` 可复现。

通过的门槛：

- schema v1 全量 DDL 一批 `execute_batch` 生效（6 表 + 2 索引 + 触发器 + FK）
- `items_no_update` 触发器 `RAISE(ABORT,'items are append-only')` 真拦 UPDATE，错误带我们的消息，行内容不变；INSERT/DELETE 仍合法 → **不变量 3 由数据库强制**
- `PRAGMA foreign_keys = ON` 真强制（引用不存在 session 的 item 被拒）
- `ON DELETE CASCADE` 沿 `parent_id` 链删整棵子树 + 关联 checkpoint
- active_head 指向待删子树时删除被外键拒绝 → testing.md §3.4 `delete_branch_refuses_when_active_head_inside` 白拿一层数据库兜底
- WAL：写事务开启时另一连接读已提交 ✅、读不到未提交 ✅、commit 后立即可见 ✅
- `PRAGMA user_version` 跨重开保留（迁移钩子可用）
- 重开后 20 条已提交 item 全在；**关闭后留下 `-wal`，没有生成 `-shm`**
- `PRAGMA synchronous = NORMAL` 被接受且回读 1 → **上游 COMPAT.md 的「只支持 OFF/FULL」是过时的**

不通过 / 需要绕开：

- `WITH RECURSIVE` 不支持（与 COMPAT.md 一致）→ `rebuild_history` 与 `DeleteBranch` 改内存走树；tripwire 常量 `SPIKE_RECURSIVE_CTE_SUPPORTED = false`，上游补上后测试会主动失败

性能（WAL，每 item 边界一次提交）：

- `synchronous` NORMAL / FULL / OFF：500 次提交各 1251 / 1222 / 1351 µs 每次 —— 差异在噪声内，OFF 反而最慢。**推测**：pragma 被解析并回读，但不改变 fsync 行为
- 单个事务批量 500 insert：1212 µs/insert，**比逐条自动提交（771 µs/commit）更慢** → ADR-0002 的「item 边界即提交」不必为性能妥协
- 1000 行骨架查询 `(id, parent_id)`：3.4–6.9 ms

API 怪癖（写 store 实现时一定会踩）：

- `PRAGMA journal_mode = WAL` 返回一行 → 必须走 `query()`；用 `execute()` 报 `Misuse("unexpected row during execution")`
- `commit` 是保留字，不能作列名（`near "commit": syntax error`）→ 改 `commit_id`
- `sessions.active_head NOT NULL` 与 `items.session_id NOT NULL` 互为外键 → 两边都插不进去；`active_head` 必须可空（NULL = 尚无 item）
- `Value` 只有 `Null/Integer(i64)/Real(f64)/Text(String)/Blob(Vec<u8>)`：UUID 走 TEXT，时间戳走 INTEGER
- `Connection::transaction()` 要 `&mut self`，`unchecked_transaction()` 只要 `&self`；`Transaction` Deref 到 `Connection`，drop 默认回滚
- `Builder::experimental_triggers(bool)` 是 no-op，源码注释写着 "Triggers are now always enabled"（`experimental_strict` 同理）
- `default-features = false` 去掉 mimalloc 与 fts（tantivy）后，整棵依赖树增量编译约 25 s

## 实测记录 · M0b（2026-09-28，turso 0.7.2）

写 store 时必然要碰的 API 事实。前两条是**读上游源码**得到的（`~/.cargo/registry/.../turso-0.7.2/src/params.rs` 与 `connection.rs`），其余由本 crate 的测试覆盖：

- **绑定参数的类型与方法**：`Connection::{execute,query}(sql, impl IntoParams)`；`IntoParams` 是 sealed trait，可用形态为 ≤16 项的**元组**（异构）、同类型数组、`Vec<T>`，以及动态个数的 `params_from_iter`。`IntoValue` 由 `TryInto<Value>` 统一实现，`Value` 本身也可直接绑。
- **`Option<T>` 绑成 NULL**：`impl<T: Into<Value>> From<Option<T>> for Value`，所以可空列直接绑 `Option<&str>`/`Option<i64>`，不必拼 SQL。
- **`Connection::unchecked_transaction()` 是 `async`**，要 `.await`；`Transaction` Deref 到 `Connection`，drop 默认回滚。
- **DDL 可以放在事务里**：`migrate()` 把 `execute_batch(DDL)` + 写 `user_version` + 写 `schema_meta` 放进同一个 `unchecked_transaction` 并提交，全部测试通过（`migrations_run_once_and_both_records_agree`）——半套 schema 的隐患因此不存在。
- **必须把 `Rows` 读到结束**：上游源码注释写着「Discard remaining rows ... Otherwise Drop of the statement will cause transaction rollback」。所有行读取辅助函数末尾都 `drain` 到空，否则一条半读的查询会让**后续**写入失败，错误现场与病因毫无关系。
- **`execute` 的返回值只算直接删除的行**（M0a 已测）：所以 `delete_branch` 的计数必须来自我们自己的走树，并与删除前后的行数差交叉校验。
- **`Builder::new_local(path: &str)` 收 `&str`**（不是 `AsRef<Path>`），`Database` 要与连接一起保活：`Writer` 持有 `_database` 字段。
- **payload 分块取 200 个 id 一批**：引擎的参数上限没有文档，300 条链的测试（两次分块）证明可行；上限不是实测出来的，因此选了保守值。
- **`cargo-llvm-cov` 不在本机**，`cargo xtask coverage` 按设计 fail-loud 报安装命令。覆盖率数字因此**未测**（阈值 enforcement 本来就排在 M1）。

## 开放问题

见设计文档末尾 5 条（payload 二级索引、孤儿仓库 GC、断电级 durability 无证据、10k items 加载策略）。选型问题已关闭。解决过程记录于此：

- 2026-09-28 选型关闭：候选优先级 turso > libsql(`features=["core"]`) > rusqlite(bundled)。turso 首轮门槛全过（唯一缺口 `WITH RECURSIVE` 有廉价绕法），故未评估后两者。**libsql 0.9.30 保留为第一顺位替代**——若 turso 出现阻塞性回归就切回，并把 ADR-0010 标 superseded。选 turso 的决定性理由之一是纯 Rust：CI 三平台（含 windows MSYS2 ucrt64 + `x86_64-pc-windows-gnu`）不必背 mingw。

## 变更日志

### 2026-09-28

**M0b 落地**（64 测试全绿）。要点：

- `migrations/v1.sql` = spike 的 DDL 原样搬入（三处修正已内建），此后只能新增文件；迁移**双记录**版本并拒绝更新版本的库。
- writer actor：有界通道 1024 + reply slot；单条 append 也走事务（插 item + 推进 head + 更新 updated_at 同事务，正确性而非速度）；批量 append 一个事务——测试 `a_batch_commits_atomically` 用「父节点属于别的会话」让整批失败，断言**一条都没落**且 head 未动。
- **`rebuild_history` 改名 `rebuild_chain` 并改语义**：返回有序 `Vec<Item>`，不做 reasoning 过滤/compaction/裁剪——那些需要 provider 能力表，属 daemon 的装配器（M0b 裁决，见 design/storage.md §4）。
- **交叉校验**：`delete_branch` 用 BFS 收子树计数（引擎只回报直接删除的行），删完后比对行数差，不一致即报错。
- **跨会话父节点**：schema 的外键证明不了父节点属于同一会话，`insert_item` 自己校验（`SessionMismatch`）。
- **崩溃测试形态**：测试二进制自重入（`current_exe()` + `HATCHERY_CRASH_PROBE` + `#[ignore]` 入口），不新增 target、不发布二进制、三平台同一份代码；父进程 `Child::kill()`（unix SIGKILL / Windows TerminateProcess）后重开断言，并额外断言 `-wal` 存在、恢复后数据库**可用**。
- **属性测试**：随机脚本对拍 testkit 的 `ReferenceTree`，每步比对链形态、head、active 集合、行数。它先抓出的是**参考模型**的错（哪些 kind 可编辑），而不是 store 的。
- `SessionStore` trait 17 个方法全部实现；`StoreError` 12 个变体并映射到 wire 错误码（「必须先切分支」是 `InvalidRequest`，「磁盘坏了」才是 `StoreError`）。

### 2026-09-28（M0a）
- 初稿。关键取舍：数据库做主存储（四家参考都用 JSONL，hatchery 因「历史可编辑」需求反向选择）；单写者 actor 规避多写者限制（ADR-0002）。
- **M0a 引擎 spike 完成**：12 项门槛测试落地全绿；选 turso 0.7.2 → 新增 **ADR-0010**（supersedes ADR-0002 的引擎部分），design/storage.md 同步（引擎与 async API 形态、内存走树、`active_head` 可空、`commit_id`、journal_mode 走 query、性能/durability 实测结论、开放问题 4/5）。
- spike 抓出三处 schema 缺陷，都是读设计文档看不出来、只有真跑引擎才会暴露的：`commit` 保留字、`active_head NOT NULL` 与 `items.session_id` 互锁、`journal_mode` 需要 `query()`。
