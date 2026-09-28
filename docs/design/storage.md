# 设计：存储与会话分支（hatchery-store）

> 状态：设计稿，引擎已实测定型。依据 ADR-0002（单写者）、ADR-0003（编辑即分叉）、ADR-0010（turso 引擎）。

## 1. 职责

- 定义 `SessionStore` trait（daemon 依赖的唯一存储接口）。
- **turso 0.7.2** embedded 实现（ADR-0010，纯 Rust，无 C 构建）：schema、writer actor、只读连接池、分支树查询、迁移。
- JSONL 导出（审计/迁移）。
- 引擎行为由 `tests/spike_engine.rs` 的门槛测试锁定（append-only 触发器、级联删除、WAL 并发读、`user_version`、写入延迟、`WITH RECURSIVE` 缺失）；升级引擎必须重跑全套。

## 2. Schema（v1）

```sql
-- journal_mode 必须走 query()：它返回一行，turso 的 execute() 会以
-- Misuse("unexpected row during execution") 拒绝任何返回行的语句（ADR-0010）
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;  -- 实测：接受且回读 1，但对 fsync 行为无可测影响（ADR-0010）
PRAGMA foreign_keys = ON;     -- 每个连接都要设；实测确实强制（孤立 item 被拒）

CREATE TABLE sessions (
  id            TEXT PRIMARY KEY,        -- UUIDv7（文本形式，时间有序）
  title         TEXT,
  mode          TEXT NOT NULL,           -- 'chat' | 'code' | 自定义
  workspace     TEXT,                    -- 绝对路径，可空（Chat）
  model_provider TEXT NOT NULL,
  model_id      TEXT NOT NULL,
  config_patch  TEXT,                    -- 会话级覆盖 JSON
  active_head   TEXT REFERENCES items(id),  -- 可空：NULL = 尚无 item。与 items.session_id 互为
                                            -- 外键，若 NOT NULL 则两边都插不进去（ADR-0010 实测）
  generation    INTEGER NOT NULL DEFAULT 0,
  status        TEXT NOT NULL,           -- idle|running|waiting_approval|error
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE TABLE items (                     -- append-only；无 UPDATE 路径（触发器强制）
  id         TEXT PRIMARY KEY,           -- UUIDv7（时间有序）
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  parent_id  TEXT REFERENCES items(id) ON DELETE CASCADE,  -- NULL = 根
  turn_id    TEXT,
  kind       TEXT NOT NULL,              -- user_message|assistant_message|reasoning|tool_call|tool_result|checkpoint|compaction|mode_switch|branch_note
  payload    TEXT NOT NULL,              -- JSON，逐 kind 的 serde 表示；reasoning 原文不 normalize
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_items_session_parent ON items(session_id, parent_id);
CREATE INDEX idx_items_turn ON items(turn_id);

CREATE TABLE turns (                     -- turn 级统计（非历史实体）
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  started_at INTEGER NOT NULL,
  ended_at   INTEGER,
  stop_reason TEXT,
  usage      TEXT                        -- JSON: prompt/completion/reasoning tokens, 请求数
);

CREATE TABLE checkpoints (               -- 影子 Git 元数据（ADR-0006）
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  item_id      TEXT REFERENCES items(id) ON DELETE CASCADE,
  workspace    TEXT NOT NULL,
  commit_id    TEXT NOT NULL,            -- 不能叫 commit：保留字，解析器直接报错（ADR-0010）
  kind         TEXT NOT NULL,            -- pre_write|pre_shell|manual
  created_at   INTEGER NOT NULL
);

CREATE TABLE approval_rules (            -- 持久化审批规则（ProceedAlways 类）
  id         TEXT PRIMARY KEY,
  scope      TEXT NOT NULL,              -- user|workspace
  matcher    TEXT NOT NULL,              -- JSON: 工具名 + 参数模式
  decision   TEXT NOT NULL,              -- allow|deny
  created_at INTEGER NOT NULL
);

CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); -- 含 schema_version
```

防误改：`CREATE TRIGGER items_no_update BEFORE UPDATE ON items BEGIN SELECT RAISE(ABORT, 'items are append-only'); END;`

## 3. 写路径：writer actor

```rust
pub enum StoreCmd {                      // daemon 各处 → mpsc → writer task
    AppendItem(Item, Reply<ItemId>),     // item 完成即发；actor 内单事务提交
    AppendItems(Vec<Item>, Reply<()>),   // 同 round 多 item 原子提交
    UpdateSession(SessionPatch, Reply<()>),
    FinishTurn(TurnId, StopReason, Usage, Reply<()>),
    EditFork { item: ItemId, new_content: Content, reply: Reply<ItemId> }, // 事务：插新 item + 切 active_head
    SwitchBranch { session: SessionId, head: ItemId, reply: Reply<()> },
    DeleteBranch { session: SessionId, head: ItemId, reply: Reply<u64> },  // 内存 BFS 找子树（ADR-0010：引擎无 WITH RECURSIVE），级联删
    ExportJsonl { session: SessionId, path: PathBuf, reply: Reply<()> },
    Shutdown(Reply<()>),                 // flush + WAL checkpoint
}
```

- 单 tokio task 持有唯一写连接，串行消费；跨命令的事务（EditFork）在 actor 内完成。
- 流式 delta **不进** writer actor（内存聚合，item 完成才落库）；崩溃最多丢当前 item——可接受（ADR-0002）。
- 背压：channel 有界（如 1024），写满时 daemon 端 await（自然限流上游）。
- 事务 API：`Connection::unchecked_transaction()`（`&self`）或 `transaction()`（`&mut self`），`Transaction` 会 `Deref` 到 `Connection`，drop 默认回滚。
- **不为性能合并提交**：实测单事务 500 insert（1212 µs/insert）比逐条自动提交（771 µs/commit）更慢，所以「item 边界即提交」保留；`AppendItems`/`EditFork` 用单事务是为了原子性（正确性），不是为了速度。

## 4. 读路径

只读连接池（turso `Database::connect()` 多连接 + WAL；实测：写事务开启时其他连接能读已提交、读不到未提交、commit 后立即可见）：

- `rebuild_history(session, head) -> Vec<Message>`：**内存走树**（ADR-0010，引擎无 `WITH RECURSIVE`）——一次查出该 session 的骨架 `(id, parent_id, kind, turn_id)`，在 Rust 内从 head 回溯到根并反转，再按 id 批量取 payload，最后按 ADR-0007 能力过滤 reasoning、应用 compaction 覆盖区间。回溯/反转/区间替换是纯函数，单独可测并与 proptest 参考模型对拍。
- `list_sessions(filter, page)`、`branch_tree(session)`（GUI 分支可视化）、`item(session, id)`。
- 读接口是同步语义的 async fn，直接跑在 spawn_blocking/专用线程，不经 writer actor。
- 大 session 注意：骨架一次全取是可行的（实测 1000 行 3.4 ms），但 payload 必须按需/分页取，不能全量加载。

## 5. 分支语义细节

- **编辑**：`edit_item(X, C')` → 新 item N（parent = X.parent，kind 同 X），`active_head = N`；X 及其旧子树保留。若 X 是 turn 中间的 tool_result，编辑意味着「从这里重演」——新 turn 以 N 为起点。
- **切换**：`active_head` 指到任意 item；rebuild 自动生效。分支 = active_head 所在的根到节点链，无需显式 branch 实体（branch_tree 由 items 树推导）。
- **删除**：内存 BFS 收集以目标节点为根的子树（ADR-0010；排除仍被其他分支引用的祖先），级联删 items/checkpoints；删除前校验 `active_head` 不在子树内（否则要求先切换）。**实测兜底**：即使应用层漏了这道校验，`sessions.active_head` 的外键也会拒绝删除（见 `delete_branch_cascades_and_refuses_while_head_is_inside`），会话不会留下悬空 head。
- **命名分支**（可选，M4）：`branch_note` item 给用户标注分支用途。

## 6. 迁移

`PRAGMA user_version`（实测跨重开保留）+ `schema_meta.schema_version` 双记录 + 顺序迁移脚本（`include_str!` 嵌入二进制，启动时自动跑，DDL 用 `execute_batch`）。纪律：迁移只加不改语义；破坏性变更走「新表 + 回填 + 切换视图」。

## 7. JSONL 导出格式

每 session 一个文件，行 = `{"v":1,"item":{…}}`，按 active 分支顺序；导出含分支全量时加 `"branch"` 字段。作为逃生通道与将来导入功能的基础。

## 8. 测试

- **引擎门槛测试已落地**：`tests/spike_engine.rs`（12 项，ADR-0010 的实测证据）。任何引擎升级或 pragma 改动都必须重跑；其中两个 `SPIKE_*` 常量是 tripwire——上游补上 `WITH RECURSIVE` 或 `synchronous` 行为变化时测试会主动失败，逼我们重新评估绕开方案。
- store 单测以文件库（tempdir）为主：turso 支持 `:memory:`，但多连接语义（写事务 + 并发读）只在文件库上成立。
- 属性测试：随机编辑序列后 `rebuild_history` 与参考实现（纯 Vec 模型，写在 testkit 里，不复用生产代码）一致。
- 崩溃测试：kill -9 真实子进程后重启，验证 WAL 恢复与「最多丢当前 item」——需要专用 writer 子进程，M0b 落地。M0a 已验证的是较弱形式：「关闭后留下 `-wal`（无 `-shm`），重开后已提交 item 全在」。

## 开放问题

1. ~~libSQL crate 选型确认~~ → **已关闭（2026-09-28，ADR-0010）**：选 turso 0.7.2；实测过程与数字见 [worklog/storage.md](../worklog/storage.md) 与 `tests/spike_engine.rs`。
2. items.payload 是否需要抽列（如 tool_name）做二级索引以加速 GUI 过滤——先 JSON extract 查询，量大了再加生成列。注意 turso 的 `GENERATED` 列是 partial 支持且需 `--experimental-generated-columns`（上游 COMPAT.md），真要走这条路得先实测。
3. 全局 GC：孤儿 checkpoint 仓库（会话删了但影子仓库残留）的清扫策略——M2。
4. 断电级 durability：实测 `PRAGMA synchronous` 三种取值耗时相同（推测 pragma 无实际 fsync 效果），因此「掉电不丢已提交 item」目前**没有证据**。M0b 的 kill -9 测试只覆盖进程级崩溃；若需要更强保证，得实测 turso 的 checkpoint/fsync 时机（M1 结合 daemon 崩溃恢复评估）。
5. 10k items 会话的加载策略（骨架全取 + payload 分页的具体阈值）——M4 GUI 性能 fixture 时定。
