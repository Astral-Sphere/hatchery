# 设计：Agent 循环（hatchery-kernel）

> 状态：**已实现（M0b）**，M1 增补两处（`TurnInput { turn, content }`、`AgentHandle::turn_running`）。文中标了 **M2 Phase N** 的形状是目标态、尚未实现。依赖 ADR-0004、0005、0007、0009。layer **L1**：kernel 依赖 protocol 的共享词汇表，但仍**不得**依赖 capabilities（否则成环，architecture.md §3）。

## 1. 职责边界

kernel 只做一件事：驱动「组装上下文 → LLM 流式请求 → 解析工具调用 → 经接缝执行 → 结果回填 → 循环」直到 turn 终结。它**不知道**：Chat/Code 模式、工作区、存储格式、前端、ACP。外界交互全部经注入的 trait：

- [`LlmProvider`]（hatchery-llm 实现）
- [`ToolHost`]（capabilities 的 `ToolRegistry` 实现；工具经接缝执行，见 §5）
- [`HistorySource`]（daemon 侧用 hatchery-store 实现：按 active 分支重建消息序列）
- [`EventSink`]（daemon 侧接 live hub + writer actor）
- `CancellationToken`（tokio-util）

四个 trait 打包在 `Ports` 里传入 builder：五个依赖里四个都是 `Arc<dyn …>`，按位置传参迟早传错。

## 2. 核心类型

```rust
pub struct Agent { /* 不可 Clone：一个 session 同一时刻只有一个 runtime（不变量 1） */ }
pub struct AgentHandle { /* Clone：多前端可驱动同一会话，daemon 决定谁的命令生效 */ }

impl AgentBuilder {
    pub fn new(session: SessionId, options: ChatOptions, ports: Ports) -> Self;
    pub fn limits(self, limits: TurnLimits) -> Self;
    pub fn build(self) -> (Agent, AgentHandle);
}

impl Agent {
    /// 跑到所有 AgentHandle 被丢弃为止。turn 失败不是返回值，而是 TurnEnded 事件：
    /// 一个失败的 turn 不该杀掉 session。
    pub async fn run(self);
}

pub enum AgentCommand {
    TurnInput { turn: TurnId, content: Content },          // 仅在 idle 时有效；turn id 由调用方铸币
    Interrupt,
    ApprovalDecision { request_id: ApprovalId, option: ApprovalOption },
}

pub struct TurnLimits { pub max_rounds: u32 }             // 默认 100
pub enum TurnCompletion {
    Completed { reason: StopReason, usage: Usage },
    Failed { error: KernelError, usage: Usage },  // 失败前的 rounds 也花了钱，usage 照报
}
```

`session` 只出现在 builder 里，因为 item 要带它；除此之外 kernel 对「会话」一无所知。`ChatOptions` 的 `tool_defs` 每轮由 kernel 用冻结快照覆盖，调用者无法故意或无意地让「模型看到的表」与「调用派发的表」不一致。

**`turn` 随命令进来、不由 kernel 自造**：`session/prompt` 的回复必须报出与后续每个事件、每个 item 同一个 TurnId，而回复是 daemon 写的——所以 daemon 铸币、kernel 沿用。（`AgentCommand::prompt` 构造器仍自己铸币，给不关心 id 的调用方；带 id 的是 `prompt_with_turn`。）

## 3. Turn 状态机

```
Idle ──TurnInput──▶ Assembling ──▶ Streaming{round} ──┬─(无 tool call)──▶ Completed
                      ▲                              │
                      │                              └─(有 tool call)──▶ AwaitingApproval?
                      │                                                      │ 批准 / 无需审批
                      │                                                      ▼
                      └────────────── Executing{round} ◀─────────────────────┘
                                          │ 工具结果回填
                                          └──▶ Streaming{round+1}
```

```rust
pub enum TurnState {
    Idle,
    Assembling,
    Streaming { round: u32 },
    AwaitingApproval { request_id: ApprovalId },
    Executing { round: u32 },
}
```

- **每次迁移都发 `KernelEvent::StateChanged`**（含回到 `Idle`），daemon 据此直接投影 `SessionStatus` 而不必从别的信号推断；测试断言的就是这条迁移序列。
- **round**：一次 LLM 请求 + 其触发的工具执行。`max_rounds` 熔断防失控循环，撞上则以 `StopReason::MaxRounds` 收尾。
- **`Interrupt` 在任意活动状态生效**：取消 turn 的 CancellationToken → LLM 流被 drop、正在跑的工具收到取消；已开始的 item 照常收尾并提交（见 §7），turn 以 `Completed { Interrupted }` 结束。
- **一次只跑一个 turn**：turn 进行中收到 `TurnInput` 是调用方 bug（daemon 用协议的 `TurnInProgress` 挡住），kernel 记一条 warning 后丢弃——排队会凭空造出一个 store 没有记录的 turn。

## 4. LLM 接缝

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat_stream(
        &self,
        options: &ChatOptions,            // 借用：adapter 在发起请求时就地把两者序列化进 body，
        messages: &[Message],             // 返回的流仍是 'static。按值传会让长会话每一轮多拷一份全量上下文
        cancel: CancellationToken,        // 中断要能真正掐掉 HTTP 请求，而不只是 drop 流
    ) -> Result<BoxStream<'static, StreamEvent>, LlmError>;
}

pub struct ChatOptions {           // 「中立旋钮」：kernel 只转发不解释
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub tool_defs: Vec<ToolDef>,
    pub extra: serde_json::Value,  // provider 特有字段逃生门
}

#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {             // 变体用命名字段而非 newtype：
    TextDelta { text: String },    // serde 无法序列化「内部标签 + 持有原始值」的 newtype 变体
    ReasoningDelta { text: String },
    ReasoningDone { signature: Option<SignatureBlock> },
    ToolCall { delta: ToolCallDelta },
    RateLimited { retry_after_ms: u64 },  // adapter 退避期间的信息通告，kernel 原样转发
    Usage { usage: Usage },
    Done { finish_reason: FinishReason },
    Error { error: LlmError },
}
```

- `Message` 是 kernel 自有类型（`role` + `Content` + `reasoning: Option<ReasoningBlock>` + `tool_calls` + `tool_call_id` + `is_error`），与 openai-interface 类型的转换只存在于 hatchery-llm（ADR-0007）。
- `FinishReason`（provider 给的）与 `StopReason`（turn 级、落库的）是两个概念：`Length → MaxTokens`、`ContentFilter → ContentFilter`（被策略截断不是干净结束，用户必须能分辨），其余 → `ModelDone`。
- 流在没有 `Done` 的情况下结束（半途断开）→ `LlmError::retryable`，turn 失败但已收到的部分照常提交。provider 自己报的 `StreamEvent::Error` 同理。
- **`ReasoningDone` 必须早于紧随其后的首个 `TextDelta`**——这是 adapter 的义务，kernel 补不了：它一次只开一个 item，text delta 会关闭并提交 reasoning item，而 item 是 append-only，晚到的签名再也贴不上去。turn 内部照常带签名重放（下一轮请求用的是内存里这一轮的签名），但**从库里重建**出来的 reasoning 是无签名的，kernel 记一条 warning。代价由 `a_signature_that_arrives_after_the_text_cannot_be_stored_but_still_replays` 钉住；各家 provider 的真实事件顺序等 M1 的 adapter 落地后实测（若确实有晚到的，再决定值不值得缓冲一个事件）。

## 5. 工具调用接缝

kernel 只认一个**窄接口**。`Tool`、`ToolCtx` 与 `FsBackend`/`TerminalBackend`/`ApprovalGate` 全部住在 L2 的 capabilities（design/capabilities.md §1）。

```rust
#[async_trait]
pub trait ToolHost: Send + Sync {
    fn snapshot(&self) -> Vec<ToolDef>;                        // turn 开始时冻结一次
    fn summarize(&self, name: &str, args: &Value) -> ToolCallSummary;
    fn approval_for(&self, name: &str, args: &Value) -> Option<ApprovalRequest>;
    async fn invoke(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
        progress: UnboundedSender<ToolProgress>,     // 通道而非回调
    ) -> Result<ToolInvocation, KernelError>;
}

pub struct ToolInvocation {
    pub output: ToolOutput,
    pub is_error: bool,
    pub checkpoints: Vec<Checkpoint>,   // M2 Phase 1（D13）：这次调用写前打的影子 git 检查点
}
```

相对 M0a 草图的四处加/改，都有实测或结构性理由：

1. **`summarize`**：`edit src/main.rs (+12 -3)` 需要知道工具语义，kernel 不该去解析工具参数。
2. **`ToolInvocation`**：工具「跑失败了」和「根本没跑成」是两件事——前者要以 `is_error` 告诉模型，后者才是 `KernelError`。
3. **进度走通道而不是 `Arc<dyn Fn>`**：kernel 必须在 await 工具的同时异步转发进度，同步回调做不到；同一个 select 循环也正是「中断能取消工具」的实现方式。工具**返回的那一刻**还排在通道里的进度要先排空再收尾：biased select 先 poll 进度通道、再 poll invoke，所以「一次 poll 内发进度并返回」的工具，它最后那条进度会留在通道里被丢掉（`progress_sent_as_the_tool_finishes_still_reaches_the_sink` 钉住；带 gate 的测试看不见这个窗口）。中断路径不排空——那次调用正要被记成 `Cancelled`，事后再冒出来的进度会描述一件记录上说没做完的事。
4. **Turn Tool Snapshot**：turn 开始时冻结 `snapshot()`，整轮（包括多轮往返）都用同一份 `tool_defs`；注册表的 `replace()` 是整表原子替换（ADR-0009 纪律 3），进行中的 turn 不受影响。

**检查点为什么由 kernel 提交成 item（M2 Phase 1 · D13）**：`ToolCtx` 带一个收集器，`LocalFs` 在写之前 push，`ToolInvocation` 把收集到的检查点带出来，然后 **kernel 在 ToolResult item 之前追加 Checkpoint item**，链变成 `… → ToolCall → Checkpoint → ToolResult`。kernel 是这件事的正确归属：它已经在造 ToolCall/ToolResult item、用的是同一套提交机器，顺序天然正确。而这个顺序是安全的，因为工具结果靠 `ToolResult.call: ItemId` 与其调用配对、**不靠父子关系**——§7「一次只开一个 item、树保持为链」的纪律不受影响。检查点因此对模型不可见（`ItemKind::is_conversation()` 不含 Checkpoint），却对 rewind 可见（`ItemKind::Checkpoint` 自己带着 `commit_id`，见 storage.md §5）。

**工具并行：同一 round 的多个 tool call 串行执行，这是设计而不是待优化项**（M2 裁决，原开放问题 1）。输出顺序确定性利于回放只是最表面的理由；真正的原因是把并行做对要重做提交序与确定性纪律，而收益只有延迟。七处结构阻碍记在 worklog/kernel.md 的 2026-10-07 条（item 链的单亲指针 `self.tail`、`AwaitingApproval` 的单槽、命令通道的单消费者、每次 `invoke_tool` 独占的进度通道与 select，加上 `ToolHost` 根本不带 `parallel_safe` 一类的元数据——**决策的输入本身也不存在**）。

## 6. 上下文组装

组装**不在 kernel**（M1 落地，`docs/design/kernel.md` 本节是接缝而非实现）：

```rust
pub struct HistoryView { pub head: Option<ItemId>, pub messages: Vec<Message> }
pub trait HistorySource { async fn view(&self) -> Result<HistoryView, KernelError>; }
```

一次调用同时给出「对话」与「分支终点」：分两次问会让分支在中间移动，而新 item 必须挂在真正被组装的那个 head 上。daemon 侧的实现从 store 的 active 分支重建 item 链，再做：

- 按 provider 能力表过滤/保留 reasoning 块（ADR-0007）；
- compaction item 替换其覆盖区间；
- token 预算裁剪（最旧 round 优先，工具结果先于消息裁剪）——**M2 Phase 4**，与 `ToolOutput::Spilled`（D12）同排：两者是「工具输出太大」这同一个问题的两半，来源是 shell 与 web_fetch；
- **system prompt 作为一条 `Role::System` 的 `Message` 排在最前**（**已落地**，2026-10-07 · Phase 0 · D15）。

store 只返回**有序 item 链**（storage.md §4），这些过滤都在 daemon——它们需要 provider 能力表，而 store 没有也不该有。

**per-turn 旋钮（D19，2026-10-07）**：`AgentCommand::TurnInput` 带一个 `options: Option<ChatOptions>`，kernel 用它覆盖装配时那份，`None` 就用装配时的。这让 daemon 能把「跟着 turn 走」的配置（model、reasoning effort）在 submit 时解析好交进来，而不必为了一个 effort 字段重组装 runtime。**唯一的例外是 `tool_defs`**：它始终取自冻结的 `ToolHost::snapshot`（显式字段写在 spread 之前，所以调用方给的工具表会被覆盖掉），一轮可以换模型换 effort，但绝不能让「模型被广告的工具表」与「调用被派发到的工具表」不是同一张。kernel 依旧不解释任何旋钮的含义。

**system prompt 不走 `ChatOptions`**：`ChatOptions` 是「中立旋钮」（§4），没有也不该有 system 字段——prompt 是一条**消息**，不是一个旋钮。它由 daemon 的 `HistorySource::view()` 产出，接缝已经够用（`Message::system` 与 llm 侧 `Role::System => WireMessage::system(text)` 的翻译都在），**kernel 一行未改**——落地时核对过：`Message::system` 此前零消费者，现在是这一个；llm 的 `translate.rs` 也未改。

渲染时机由 **D15 定稿**：在 runtime 装配时渲染一次、冻结整个 runtime 生命周期。理由**不是**规划时写的「模式切换与 config 变更本来就 bump generation 重组装」——实测不成立：全仓库唯一的卸载路径是空闲清扫，`session/set_config` 与 `config/set` 都不重组装活着的 runtime（daemon.md 开放问题 5）。真正的理由是每轮重渲染会让 environment 节的日期/cwd 破坏请求前缀的稳定性，那正是项目为 KV cache 反复强调的东西；而 model 与 effort 本来就同样是「下次装配才生效」，冻结让 prompt 与它们一致。

**不变量 2（重建 == 实际请求体）的边界随之重划为只管分支历史**：system prompt 是可复现的派生态，由 `prompt/render` 钉住——该方法对活着的 runtime 返回装配时冻结的那一份，而不是重新渲染一份可能已经不同的。e2e 的断言形状因此是「system 文本两次请求逐字节相同 + 等于 `prompt/render` 的 text + 它后面的 messages 数组仍整表比对」。

kernel 自己只做两件事：把本轮用户输入作为 item 提交（保证「模型可见 = 已记录」），并把工具结果作为 `Message` 回填给下一轮。

## 7. 取消、提交与不变量

- **一次只开一个 item**：provider 在同一条流里交错发 reasoning 与 text，若两个 item 同时开着，它们都只能挂在「最后一个**已完成**的 item」上——形成分叉，其中一个会从 active 分支消失并在重建时丢失。所以 text 到来会关闭已开的 reasoning item，reasoning 恢复则新开一个。
- **新 item 只挂在已提交的 item 上**：`ItemStarted` 提前公布 id（前端可以先建节点），但 store 只会收到 `ItemFinished`；挂在「已预留但未提交」的 id 上会让下一次插入撞外键。
- **中断也照常提交**：已经流出的文本、已经记录的工具调用都会收尾并提交，所以历史与用户看到的一致；被中断的工具记为 `ToolStatus::Cancelled`，被拒绝的调用记为 `Denied`。
- **审批是 id 往返**：kernel 进入 `AwaitingApproval` 后发 `ApprovalNeeded { request_id, request }`，等 `AgentCommand::ApprovalDecision { request_id, … }`。**kernel 不设超时**——超时是应答方（`ApprovalGate`）的策略，fail-closed 的 deny 由它决定；收到别的 request_id 的答复记 warning 后忽略，**收到不在所给选项里的答复同样忽略**（这条是硬门唯一承重的机制：`once_only()` 不提供 always 选项，所以「不可记忆」是真的不可绕；Phase 0 起由 `a_hard_gate_refuses_an_answer_it_never_offered` 钉住）。
- **拒绝不是失败**：被拒的调用以 `is_error` 的 tool result 回填，turn 继续，模型得到「用户拒绝了，别再重试」的明确信息。
- **事件序确定性**：同一脚本两次运行产生完全相同的事件序列（有测试）。

## 8. 事件与错误

```rust
pub enum KernelEvent {
    TurnStarted { turn },
    StateChanged { from, to },
    ItemStarted { item: ItemStub },
    TextDelta { item, text },
    ReasoningDelta { item, text },
    ItemFinished { item: Item },
    ToolCallStarted { item, summary },
    ToolCallProgress { item, chunk },
    RateLimited { retry_after_ms },          // StreamEvent::RateLimited 的直通投影
    ApprovalNeeded { request_id, request },
    TurnEnded { turn, completion },          // 唯一的终止事件（成功与失败都在这里）
}
```

`is_control()` 区分可合并的流式增量与控制事件；`ends_turn()` 标出终止事件。`KernelError::to_event_error()` 把 kernel 的失败翻译成 wire 的 `EventError`（provider → `LlmError` + retryable；history → `StoreError`；tool → `InternalError`）——kernel 最清楚自己失败的语义，映射放这里比放 daemon 少一处重复。

`EventSink::emit` 不返回 `Result`：daemon 的 sink 要接 live hub 与 writer actor，那里的失败是 daemon 的事；让 kernel 处理它，就等于要 kernel 决定「半个 turn 失败」长什么样。

## 开放问题

1. ~~多 tool call 并行的启用条件与顺序保证~~ → **已裁决（2026-10-07，M2）：不做**。串行是设计而不是待优化项；理由与七处结构阻碍见 §5 与 worklog/kernel.md 同日条目。
2. kernel 是否需要 `SubAgent` 原语（spawn 子 Agent）还是由 daemon 层做多 runtime 编排——倾向后者（kernel 保持单纯），M3 ACP client 时验证。
3. compaction 触发策略（token 阈值 vs round 阈值）与 side-query 摘要实现——M5。
4. 工具级重试上限（草图里的 `max_tool_retries`）→ **顺延 M5**（2026-10-07 裁决）：全仓库只有本节与 roadmap 的顺延表提及它，「可重试的工具失败」既没有语义也没有第一个消费者，正是 ADR-0009 反预拆分刹车的适用场景。`TurnLimits` 只有 `max_rounds`（默认 100）。