# 设计：存储与会话分支（hatchery-store）

> 状态：设计稿。依据 ADR-0002（libSQL 单写者）、ADR-0003（编辑即分叉）。

## 1. 职责

- 定义 `SessionStore` trait（daemon 依赖的唯一存储接口）。
- libSQL embedded 实现：schema、writer actor、只读连接池、分支树查询、迁移。
- JSONL 导出（审计/迁移）。

## 2. Schema（v1）

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE sessions (
  id            TEXT PRIMARY KEY,        -- ULID
  title         TEXT,
  mode          TEXT NOT NULL,           -- 'chat' | 'code' | 自定义
  workspace     TEXT,                    -- 绝对路径，可空（Chat）
  model_provider TEXT NOT NULL,
  model_id      TEXT NOT NULL,
  config_patch  TEXT,                    -- 会话级覆盖 JSON
  active_head   TEXT NOT NULL REFERENCES items(id),
  generation    INTEGER NOT NULL DEFAULT 0,
  status        TEXT NOT NULL,           -- idle|running|waiting_approval|error
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE TABLE items (                     -- append-only；无 UPDATE 路径（触发器强制）
  id         TEXT PRIMARY KEY,           -- ULID（时间有序）
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
  commit       TEXT NOT NULL,
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
    DeleteBranch { session: SessionId, head: ItemId, reply: Reply<u64> },  // 递归 CTE 找子树，级联删
    ExportJsonl { session: SessionId, path: PathBuf, reply: Reply<()> },
    Shutdown(Reply<()>),                 // flush + WAL checkpoint
}
```

- 单 tokio task 持有唯一写连接，串行消费；跨命令的事务（EditFork）在 actor 内完成。
- 流式 delta **不进** writer actor（内存聚合，item 完成才落库）；崩溃最多丢当前 item——可接受（ADR-0002）。
- 背压：channel 有界（如 1024），写满时 daemon 端 await（自然限流上游）。

## 4. 读路径

只读连接池（`libsql` 多连接 + WAL 并发读）：

- `rebuild_history(session, head) -> Vec<Message>`：从 head 沿 parent 链回溯（递归 CTE），反转，按 ADR-0007 能力过滤 reasoning，应用 compaction 覆盖区间。
- `list_sessions(filter, page)`、`branch_tree(session)`（GUI 分支可视化）、`item(session, id)`。
- 读接口是同步语义的 async fn，直接跑在 spawn_blocking/专用线程，不经 writer actor。

## 5. 分支语义细节

- **编辑**：`edit_item(X, C')` → 新 item N（parent = X.parent，kind 同 X），`active_head = N`；X 及其旧子树保留。若 X 是 turn 中间的 tool_result，编辑意味着「从这里重演」——新 turn 以 N 为起点。
- **切换**：`active_head` 指到任意 item；rebuild 自动生效。分支 = active_head 所在的根到节点链，无需显式 branch 实体（branch_tree 由 items 树推导）。
- **删除**：递归 CTE 收集以目标节点为根的子树（排除仍被其他分支引用的祖先），级联删 items/checkpoints；删除前校验 `active_head` 不在子树内（否则要求先切换）。
- **命名分支**（可选，M4）：`branch_note` item 给用户标注分支用途。

## 6. 迁移

`schema_meta.schema_version` + 顺序迁移脚本（嵌入二进制，启动时自动跑）。纪律：迁移只加不改语义；破坏性变更走「新表 + 回填 + 切换视图」。

## 7. JSONL 导出格式

每 session 一个文件，行 = `{"v":1,"item":{…}}`，按 active 分支顺序；导出含分支全量时加 `"branch"` 字段。作为逃生通道与将来导入功能的基础。

## 8. 测试

- 内存 libSQL（`:memory:`）跑全部 store 单测：分叉/切换/级联删除/并发读写（多 reader + writer actor 压测）。
- 属性测试：随机编辑序列后 `rebuild_history` 与参考实现（纯 Vec 模型）一致。
- 崩溃测试：kill -9 daemon 后重启，验证 WAL 恢复与「最多丢当前 item」。

## 开放问题

1. libSQL crate 选型确认：`libsql`（官方 Rust binding，embedded + remote 两态）版本与 API 稳定性——M0 做一个最小 spike 验证 WAL/多连接/触发器行为（实测，不靠文档推断）。
2. items.payload 是否需要抽列（如 tool_name）做二级索引以加速 GUI 过滤——先 JSON extract 查询，量大了再加生成列。
3. 全局 GC：孤儿 checkpoint 仓库（会话删了但影子仓库残留）的清扫策略——M2。
