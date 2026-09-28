# 设计：Wire 协议与数据模型（hatchery-protocol）

> 状态：设计稿。依赖 ADR-0001（全协议化）、ADR-0003（分叉模型）。

## 1. 定位

`hatchery-protocol` 是所有前端（CLI/GTK/headless/未来的 Web UI）与 daemon 之间的唯一契约，也是 SDK 的基础。定义三样东西：**数据模型**（Thread/Turn/Item）、**方法**（client→daemon）、**事件**（daemon→client 通知流）。

选型：JSON-RPC 2.0。传输：UDS（daemon 常驻）与 stdio（embedded/ACP 桥接、测试）同一套消息帧（newline-delimited JSON）。不用 gRPC/protobuf 的理由：调试可用肉眼、与 ACP/MCP 生态一致、serde 即可。

## 2. 数据模型

```rust
pub struct Session {             // 会话（wire 类型名统一用 Session：方法名 session/*、表名 sessions；
                                 //  早期草稿里的 Thread 已废弃，避免一物两名）
    pub id: SessionId,           // UUIDv7（时间有序，无需协调；开放问题 3 的结论）
    pub title: Option<String>,
    pub mode: SessionModeId,     // "chat" | "code" | 自定义
    pub workspace: Option<PathBuf>,
    pub model: ModelRef,         // provider + model id
    pub created_at: Timestamp,
    pub active_branch_head: ItemId,
    pub generation: u64,         // runtime 代际号（事件过滤）
    pub status: ThreadStatus,    // Idle | Running | WaitingApproval | Error
}

pub struct Item {                // 历史最小单元，append-only
    pub id: ItemId,
    pub parent: Option<ItemId>,  // 树结构；None = 根
    pub turn: Option<TurnId>,
    pub kind: ItemKind,
    pub created_at: Timestamp,
}

pub enum ItemKind {
    UserMessage(Content),                  // 文本 + 附件（image/embedded context）
    AssistantMessage(Content),
    Reasoning { text: String, signature: Option<SignatureBlock> }, // 逐字节存储（ADR-0007）
    ToolCall { name: String, args: serde_json::Value, status: ToolStatus },
    ToolResult { call: ItemId, output: ToolOutput, is_error: bool },
    Checkpoint { commit: String, scope: CheckpointScope },       // 影子 Git（ADR-0006）
    Compaction { summary: String, covered: Range<ItemId> },      // 上下文压缩摘要
    ModeSwitch { from: SessionModeId, to: SessionModeId },
    BranchNote(String),                                            // 用户对分支的标注
}
```

Turn 不作为实体表，而是 items 上的分组标签（`turn: Option<TurnId>`）+ `turn/finished` 事件里的统计（usage、stop reason）。

## 3. 方法（client → daemon）

命名沿用 ACP 风格（`session/*`），便于 ACP 适配层直译。

| 方法 | 参数要点 | 说明 |
|---|---|---|
| `daemon/hello` | protocol_version | 握手，返回 daemon 版本、能力、支持的协议版本区间 |
| `session/new` | mode, workspace?, model?, config_overrides? | 创建会话，返回 Thread |
| `session/load` | session_id, replay_from? | 加载 + 按 active 分支重放 items（replay window） |
| `session/list` | 分页游标 | 历史列表（按 updated_at 排序） |
| `session/prompt` | session_id, content, generation? | 提交用户输入，启动 turn；generation 不匹配则拒绝 |
| `session/cancel` | session_id | 中断当前 turn（取消 LLM 流 + 工具） |
| `session/set_mode` | session_id, mode | turn 边界生效（ADR-0005） |
| `session/set_config` | session_id, patch | 会话级覆盖（model/effort 等） |
| `session/edit_item` | item_id, new_content, rewind_scope? | 编辑即分叉（ADR-0003），返回新分支 head |
| `session/branch/list` \| `switch` \| `delete` | session_id / head / branch | 分支管理；delete 级联 + 二次确认标记 |
| `session/rewind` | session_id, target_item, scope | 对话/代码/两者回滚 |
| `session/delete` \| `session/rename` | session_id | 会话管理 |
| `approval/respond` | request_id, outcome, persist_rule? | 回应审批请求 |
| `config/get` \| `config/set` | key path | 分层配置读写，返回 per-key origins |
| `prompt/render` | session_id?, mode? | 导出当前生效的完整 system prompt（透明性需求） |
| `store/export_jsonl` | session_id | 审计导出（ADR-0002） |

## 4. 事件（daemon → client 通知）

会话订阅制：`session/load`/`session/new` 即隐式订阅该会话事件；多前端各自收到全量扇出（live hub）。

```rust
pub enum ServerEvent {
    // 流式增量（高频，daemon 侧做 coalescing：同 item 的 delta 合并批量发）
    ItemStarted { session, item_stub },
    TextDelta { session, item, text },
    ReasoningDelta { session, item, text },
    ItemFinished { session, item },           // item 已落库，携带完整 item
    // 工具与审批
    ToolCallStarted { session, item, display }, // 人类可读的调用摘要（借鉴 ACP tool call updates）
    ToolCallProgress { session, item, chunk },
    ApprovalRequested { session, request },     // 前端需弹审批 UI，回应 approval/respond
    // turn 生命周期
    TurnFinished { session, turn, completion }, // stop reason + usage
    TurnFailed { session, turn, error },
    // 会话与 daemon 级
    ThreadUpdated { thread },                   // 标题/模式/状态变化
    ModeSwitched { session, from, to },
    RateLimited { session, retry_after },
    GenerationBumped { session, generation },   // runtime 换代，客户端丢弃旧代事件
    DaemonShuttingDown { reason },
}
```

规则：
- 每个事件携带 `generation`；客户端丢弃小于当前已知代际的事件（防旧 runtime 污染，architecture.md 不变量 1）。
- 迟加入的前端：`session/load` 返回 active 分支全量 items（或 replay_from 之后的增量），随后接实时流。
- 事件顺序保证：同一 session 内严格有序（UDS 单连接 FIFO + daemon 内 per-session 广播队列）。

## 5. 错误模型

JSON-RPC error + 结构化 `code`：`SessionNotFound` / `GenerationMismatch` / `TurnInProgress` / `ApprovalDenied` / `StoreError` / `LlmError{retryable}` 等。`LlmError.retryable=true` 时 daemon 自动重试并发 `RateLimited` 事件，前端只展示。

## 6. 版本化

`daemon/hello` 协商 protocol_version（semver）；daemon 支持一个区间，方法/字段只增不改语义；废弃字段标 `#[deprecated]` 注释并在 release note 列出。protocol crate 提供 `PROTOCOL_VERSION` 常量与兼容性测试（旧版本 fixture 反序列化）。

## 开放问题

1. 事件 coalescing 的具体策略（按帧时间窗还是按 delta 数）——M1 实测后定。
2. 是否需要 `session/watch`（观察他人会话而不注入）与 `session/takeover`（多前端抢占输入权）——倾向 M4 GTK 多窗口时再设计。
3. ~~ItemId 用 ULID 还是自增 + session 前缀~~ → **已定（2026-09-28）：UUIDv7**（`uuid` crate 的 `v7` + `serde` feature，wire 上是小写带连字符的 36 字符字符串）。理由：时间有序（字典序 = 时间序）、无需协调、生态工具（SQL/JSON/日志）都认 UUID；ULID 的 26 字符可读性优势不足以抵消「引入第二种 id 格式」的成本。
4. 大工具输出（如 shell 日志 >1MB）是否走「存库 + 事件带引用」而非内联——倾向带引用（借鉴 dsh spill），M2 定。
