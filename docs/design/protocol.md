# 设计：Wire 协议与数据模型（hatchery-protocol）

> 状态：**已实现（M0b）**。依赖 ADR-0001（全协议化）、ADR-0003（分叉模型）。本文档的代码块已与 `crates/hatchery-protocol` 的实际类型对齐——落地过程中有若干草图被修正，逐条记在下面的「M0b 修正」节。

## 1. 定位

`hatchery-protocol` 是所有前端（CLI/GTK/headless/未来的 Web UI）与 daemon 之间的唯一契约，也是 SDK 的基础。定义三样东西：**数据模型**（Session/Item）、**方法**（client→daemon）、**事件**（daemon→client 通知流）。

它同时是全 workspace 的**共享词汇表**：id 新类型、`Content`、`ToolOutput`、`ApprovalRequest`、`Usage` 这些既要进 wire、又被 kernel 与 capabilities 使用的值类型只定义一次，住在这里。因此它位于所有 crate 之下（M0b 分层裁决，见 architecture.md §3）。

选型：JSON-RPC 2.0。传输：UDS（daemon 常驻）与 stdio（embedded/ACP 桥接、测试）同一套消息帧（newline-delimited JSON）。不用 gRPC/protobuf 的理由：调试可用肉眼、与 ACP/MCP 生态一致、serde 即可。

帧的编解码在 `rpc.rs`：`encode_frame`（带换行符）与 `decode_frame` / `classify`（请求 / 通知 / 响应三类判别，`jsonrpc` 字段类型化，写错版本会被拒），外加 `FrameDecoder`——增量解码器，按**行**而不是按 chunk 解码，所以被两个 chunk 劈开的 UTF-8 字符不是错误；未结束的帧超过 4 MiB 报 `TooLong` 而不是无限缓冲。

## 2. 数据模型

```rust
pub struct Session {
    pub id: SessionId,                 // UUIDv7（时间有序，无需协调）
    pub title: Option<String>,
    pub mode: SessionModeId,           // "chat" | "code" | 自定义（ADR-0005）
    pub workspace: Option<PathBuf>,
    pub model: ModelRef,               // provider + model id
    pub config_patch: Option<Value>,   // 会话级覆盖文档，daemon 读配置时合并（platform.md §1）
    pub created_at: Timestamp,         // 整数毫秒
    pub updated_at: Timestamp,         // session/list 按它排序
    pub active_branch_head: Option<ItemId>,  // NULL = 尚无 item（ADR-0010）
    pub generation: u64,               // runtime 代际号（事件过滤）
    pub status: SessionStatus,         // Idle | Running | WaitingApproval | Error
}

pub struct Item {                      // 历史最小单元，append-only
    pub id: ItemId,
    pub session: SessionId,            // 冗余字段：store actor 收到的是裸 Item
    pub parent: Option<ItemId>,        // 树结构；None = 根
    pub turn: Option<TurnId>,
    pub kind: ItemKind,                // serde: flatten，见下
    pub created_at: Timestamp,
}

pub struct ItemStub {                  // ItemStarted 用：id + 位置，不含 payload
    pub id: ItemId,
    pub parent: Option<ItemId>,
    pub turn: Option<TurnId>,
    pub kind: ItemKindTag,             // 九个变体的类型化标签
}

#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
#[serde(flatten)]                      // 在 Item 上：kind/payload 落在 item 对象顶层
pub enum ItemKind {
    UserMessage(Content),
    AssistantMessage(Content),
    Reasoning(ReasoningBlock),         // 逐字节存储（ADR-0007）
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Checkpoint(Checkpoint),            // 影子 Git（ADR-0006）
    Compaction(Compaction),            // 上下文压缩摘要
    ModeSwitch(ModeSwitch),
    BranchNote(BranchNote),
}

pub struct ReasoningBlock { pub text: String, pub signature: Option<SignatureBlock> }
pub struct ToolCall { pub name: String, pub args: Value, pub status: ToolStatus }
pub struct ToolResult { pub call: ItemId, pub output: ToolOutput, pub is_error: bool }
pub struct Checkpoint { pub commit_id: String, pub kind: CheckpointKind }
pub struct Compaction { pub summary: String, pub covered: ItemIdRange }
pub struct ModeSwitch { pub from: SessionModeId, pub to: SessionModeId }
pub struct BranchNote { pub note: String }
```

**为什么 `kind`/`payload` 扁平成两个顶层键**：`items` 表就是两列（`kind` TEXT + `payload` TEXT）。相邻标签枚举 + `flatten` 让 wire 形态与数据库列一一对应，`ItemKind::to_payload()` / `from_parts(tag, payload)` 因此是「派生」而非「手写映射」——单元测试对九个变体逐一断言两者一致（实测：serde 1.0.229 的 flatten + 相邻标签可用）。

**其余共享值类型**（`tool.rs` / `approval.rs` / `usage.rs` / `content.rs`）：

```rust
pub struct Content { pub text: String, pub parts: Vec<ContentPart> }
pub enum ContentPart { Image { mime_type, data }, Resource { uri, mime_type?, text? } }
pub struct SignatureBlock { pub scheme: String, pub data: String }   // 各家不透明签名块
pub struct ToolOutput { pub text: String, pub artifacts: Vec<ToolArtifact>, pub spilled: Option<SpilledOutput> }
pub enum ToolStatus { Pending, Running, Completed, Failed, Denied, Cancelled }
pub struct ApprovalRequest { pub tool: String, pub args_digest: String, pub risk: RiskLevel, pub options: Vec<ApprovalOption> }
pub enum RiskLevel { ReadOnly, WritesWorkspace, WritesOutside, Executes, Network }
pub enum ApprovalOption { AllowOnce, AllowAlways, Deny, DenyAlways }
pub struct Usage { pub prompt_tokens: Option<u64>, pub completion_tokens: Option<u64>,
                   pub reasoning_tokens: Option<u64>, pub requests: u32 }
pub enum StopReason { ModelDone, MaxRounds, MaxTokens, Interrupted }
```

- `ToolOutput` 是**结构体而不是枚举**：capabilities.md §3 的 `{ text, artifacts?, spilled? }` 与 kernel.md §5 的 `Spilled` 变体两说之间，真实形状是「溢出后仍有预览文本」，可选字段对演进也更友好。
- `ApprovalOption` **一个枚举兼作请求选项与答复**：答复必然是被提供的选项之一，两个镜像枚举只会制造漂移（capabilities.md 草图里的 `ApprovalOutcome` 即此）。
- `Usage` 的 token 字段是 `Option`：「未知」与「零」是两件事，前端展示不同。
- `ItemIdRange { first, last }`（**两端含**）替代草图中的 `std::ops::Range<ItemId>`——后者不实现 `Serialize`，且半开区间的端点语义在两端都是真实行时容易写错。

**`Content` 与 `Message` 的分工**：`Content` 是 wire 与存储的形态；kernel 另有自己的 `Message`（含 role/reasoning/tool_calls），由 daemon 侧的装配器从 item 链构造（kernel.md §6）。

## 3. 方法（client → daemon）

命名沿用 ACP 风格（`session/*`），便于 ACP 适配层直译。方法名的**单一真相**是 `method.rs` 的 20 个常量（`method::SESSION_PROMPT` 等），单元测试把它与本文档的列表逐条对账。

| 方法 | 参数 / 结果类型 | 说明 |
|---|---|---|
| `daemon/hello` | `HelloParams` → `HelloResult` | 握手：版本、daemon 版本、能力（方法清单 + 模式清单） |
| `session/new` | `SessionNewParams` → `SessionNewResult` | 创建会话 |
| `session/load` | `SessionLoadParams` → `SessionLoadResult` | 加载 + 按 active 分支重放（`next_cursor` 分页，`replay_from` 补差） |
| `session/list` | `SessionListParams` → `SessionListResult` | 分页（`next_cursor`）、按 updated_at 排序、mode/workspace/title 过滤 |
| `session/prompt` | `SessionPromptParams` → `SessionPromptResult` | 提交用户输入（`generation` 不匹配则拒绝） |
| `session/cancel` | `SessionCancelParams` → `SessionCancelResult` | 中断当前 turn |
| `session/set_mode` | `SetModeParams` → `SetModeResult` | turn 边界生效（ADR-0005） |
| `session/set_config` | `SetConfigParams` → `SetConfigResult` | 会话级覆盖（model / effort / overrides） |
| `session/edit_item` | `EditItemParams` → `EditItemResult` | 编辑即分叉（ADR-0003），返回新分支 head |
| `session/branch/list` | `BranchListParams` → `BranchListResult` | 树 + 哪些在 active 分支（GUI 分支视图） |
| `session/branch/switch` | `BranchSwitchParams` → `BranchSwitchResult` | 切换 active_branch_head |
| `session/branch/delete` | `BranchDeleteParams` → `BranchDeleteResult` | 级联删除，`confirm` 为二次确认 |
| `session/rewind` | `RewindParams` → `RewindResult` | 对话/代码/两者回滚（`purge_untracked` 默认 false） |
| `session/delete` \| `session/rename` | `…Params` → `…Result` | 会话管理 |
| `approval/respond` | `ApprovalRespondParams` → `ApprovalRespondResult` | `request_id` + 选项；重复答复幂等（`handled: false`） |
| `config/get` \| `config/set` | `ConfigGetParams`/`ConfigSetParams` | 分层配置读写，返回 per-key `origin` |
| `prompt/render` | `PromptRenderParams` → `PromptRenderResult` | 导出完整 system prompt + 每 section 来源 |
| `store/export_jsonl` | `ExportJsonlParams` → `ExportJsonlResult` | 审计导出（ADR-0002） |

不建路由 trait：daemon 是 M1 的事，现在只需要常量与类型（ADR-0009 反预拆分）。

## 4. 事件（daemon → client 通知）

会话订阅制：`session/load`/`session/new` 即隐式订阅该会话事件；多前端各自收到全量扇出（live hub）。

```rust
pub struct SessionEvent {              // 会话级事件的信封
    pub session: SessionId,
    pub generation: u64,               // 不变量 1：客户端丢弃低于已知代际的事件
    #[serde(flatten)]
    pub event: ServerEvent,
}

#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {                 // 13 种
    ItemStarted { item: ItemStub },
    TextDelta { item: ItemId, text: String },
    ReasoningDelta { item: ItemId, text: String },
    ItemFinished { item: Item },
    ToolCallStarted { item: ItemId, summary: ToolCallSummary },
    ToolCallProgress { item: ItemId, chunk: String },
    ApprovalRequested { request_id: ApprovalId, request: ApprovalRequest },
    TurnFinished { turn: TurnId, completion: TurnCompletion },
    TurnFailed { turn: TurnId, error: EventError },
    SessionUpdated { state: Session },   // 字段名 `state`，不是 `session`
    ModeSwitched { from: SessionModeId, to: SessionModeId },
    RateLimited { retry_after_ms: u64 },
    GenerationBumped,                    // 无载荷：新代际就是信封的 generation
}

#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonEvent { DaemonShuttingDown { reason: String } }   // 非会话级
```

**为什么把 generation 放信封而不是每个变体里**：14 个变体各自带一个 `generation` 字段既重复又容易漏；信封统一后，客户端的代际过滤只需读一个字段。代价是信封与变体的字段不能重名——测试 `no_event_field_collides_with_the_envelope` 机器检查这一点（它抓出了两个真实冲突：`SessionUpdated` 原字段名 `session`、`GenerationBumped` 原字段 `generation`，后者改为无载荷变体）。

规则：

- 事件顺序保证：同一 session 内严格有序（UDS 单连接 FIFO + daemon 内 per-session 广播队列）。
- 只有 `TextDelta`/`ReasoningDelta` 允许 daemon 侧合并（`ServerEvent::is_coalescable`）；控制事件不合并、不乱序。
- 迟加入的前端：`session/load` 返回 active 分支 items（或 `replay_from` 之后的增量），随后接实时流。
- 终止事件唯一：`TurnFinished` 或 `TurnFailed`（`ServerEvent::ends_turn`）。

## 5. 错误模型

```rust
pub enum ErrorCode {                   // 数值一旦发布不可变（golden 测试锁定）
    ParseError = -32700, InvalidRequest = -32600, MethodNotFound = -32601,
    InvalidParams = -32602, InternalError = -32603,
    SessionNotFound = -32000, GenerationMismatch = -32001, TurnInProgress = -32002,
    ApprovalDenied = -32003, ApprovalTimedOut = -32004, StoreError = -32005,
    LlmError = -32006, ConfigError = -32007, UnsupportedProtocolVersion = -32008,
}
```

两种形态，各有其理：JSON-RPC **响应**里是数字（`ErrorObject { code: i64, message, data? }`），因为规范如此、且「新版 daemon 的错误码」必须能原样传递而不是被解析失败；**事件内**的错误用可读名（`EventError { code: ErrorCode, message, retryable: Option<bool> }`），因为它们要进 golden fixture 与日志。`retryable` 只在它是真问题时有值（provider 失败），其余情况缺省——`EventError::llm(msg, retryable)` 是唯一的构造入口。

## 6. 版本化

- `PROTOCOL_VERSION = "1.0.0"`，`PROTOCOL_MAJOR = 1`；**兼容判定只看 major**（`is_compatible`），无法解析的版本一律拒绝而不是猜。
- `daemon/hello` 协商版本；`SUPPORTED_PROTOCOL_VERSIONS` 是区间下界与拒绝路径的凭据。
- 方法/字段只增不改语义；wire 类型**不用** `deny_unknown_fields`（旧客户端必须忽略未知字段）；可选字段一律 `skip_serializing_if`，不写 `null`（`SessionPatch::title` 的 `Some(None)` 是唯一例外：它意味着「清空标题」）。**枚举值在 major 内冻结**：新增 `ItemKind`、事件 `type` 或状态拼写属于 major bump——未知枚举值一律硬失败（`an_unknown_enum_value_is_refused_rather_than_defaulted` 钉死），不静默降级；未知字段则必须被忽略。数字错误码是唯一的开放集（`ErrorObject.code: i64`），因为规范要求原样传递。
- golden fixture 在 `tests/fixtures/protocol-v<N>/`（N = major），共 79 个：每个 `ItemKind`（含无父项的根形态）、每个事件、每个方法的参数**与结果**、空会话形态、四个帧形态、以及方法表与错误码表。生成方式 `UPDATE_FIXTURES=1 cargo nextest run -p hatchery-protocol`；`scripts/ci.sh` 导出的 `INSTA_UPDATE=no` 会让它在门禁里拒绝重写自己的契约。
- **不用 insta**：insta 的快照名由断言表达式推导且要求字面量，数据驱动的 fixture 注册表无法驱动它（除非手写 79 条断言去重复注册表）。纯 JSON 另有好处：版本兼容 fixture 任何实现都能读，不只 Rust。代价是 key 按字母序（Value 是 BTreeMap）——确定性不受影响，声明序由 `typed_serialization_keeps_declaration_order` 单独锁定。
- 版本兼容测试读**磁盘上的** fixture 反序列化，不与内存样本比对（后者是 golden 测试的职责），所以「改了 wire 形态」与「fixture 过期」是两种不同的失败。

## M0b 修正（相对初稿草图）

| 草图 | 实际 | 原因 |
|---|---|---|
| `ThreadStatus` / `ThreadUpdated` | `SessionStatus` / `SessionUpdated { state }` | `Thread` 更名 `Session` 的遗留；且 `session` 字段与信封冲突 |
| `status: ThreadStatus` | `SessionStatus` | 同上 |
| `active_branch_head: ItemId` | `Option<ItemId>` | 外键互锁：非空则第一条 item 插不进去（ADR-0010） |
| `Item` 无 `session` | 有 | store actor 收到的是裸 `Item`，且跨会话挂错父节点值得早发现 |
| `ItemKind::Compaction { covered: Range<ItemId> }` | `ItemIdRange { first, last }` | `Range` 不实现 `Serialize` |
| `ItemKind::Checkpoint { commit, scope }` | `{ commit_id, kind: CheckpointKind }` | `commit` 是保留字；schema 的列是 `kind`（pre_write/pre_shell/manual） |
| `ItemKind::BranchNote(String)` | `{ note: String }` | payload 统一为对象，加字段不动别人 |
| `ItemStarted { item_stub }` | `ItemStub` 具名类型 | 别处也要用（重放游标、骨架） |
| 每个事件带 `generation` 字段 | 信封统一携带 | 见 §4 |
| `Session` 无 `updated_at`/`config_patch` | 有 | `session/list` 排序需要前者；`sessions.config_patch` 列需要后者 |
| `ToolOutput::Spilled { path, preview }` | `ToolOutput { text, artifacts?, spilled? }` | 溢出后仍有预览文本，两者不是互斥的 |
| `ApprovalRequest` 回复用 `ApprovalOutcome` | 复用 `ApprovalOption` | 答复必然是被提供的选项之一 |
| 「insta golden JSON」 | 纯 JSON golden + 显式生成器 | 见 §6 |
| `CheckpointScope` | `CheckpointKind`（条目内）+ `RewindScope`（rewind 方法） | 草图把两个概念混进了一个名字 |

## 开放问题

1. 事件 coalescing 的具体策略（按帧时间窗还是按 delta 数）——M1 实测后定。`is_coalescable` 已就位。
2. 是否需要 `session/watch`（观察他人会话而不注入）与 `session/takeover`（多前端抢占输入权）——倾向 M4 GTK 多窗口时再设计。
3. ~~ItemId 用 ULID 还是自增 + session 前缀~~ → **已定（2026-09-28）：UUIDv7**（`uuid` crate 的 `v7` + `serde` feature，wire 上是小写带连字符的 36 字符字符串）。理由：时间有序（字典序 = 时间序）、无需协调、生态工具（SQL/JSON/日志）都认 UUID；ULID 的 26 字符可读性优势不足以抵消「引入第二种 id 格式」的成本。M0b 实测 `Uuid::now_v7()` 产出 v7 且文本序与时间序一致。
4. 大工具输出（如 shell 日志 >1MB）是否走「存库 + 事件带引用」而非内联——倾向带引用（借鉴 dsh spill），**形状已就位**（`ToolOutput::spilled`），M2 定阈值与落盘路径。