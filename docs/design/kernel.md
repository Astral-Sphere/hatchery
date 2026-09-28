# 设计：Agent 循环（hatchery-kernel）

> 状态：设计稿。kernel 是 L0，零业务语义（architecture.md 分层纪律）。

## 1. 职责边界

kernel 只做一件事：驱动「组装上下文 → LLM 流式请求 → 解析工具调用 → 经接缝执行 → 结果回填 → 循环」直到 turn 终结。它**不知道**：Chat/Code 模式、工作区、存储格式、前端、ACP。外界交互全部经注入的 trait：

- `LlmProvider`（hatchery-llm 实现）
- `ToolRegistry` / `Tool`（hatchery-tools 提供，经 capabilities 接缝执行）
- `HistorySource`（daemon 侧用 hatchery-store 实现：按 active 分支重建消息序列）
- `EventSink`（daemon 侧接 live hub + writer actor）
- `CancellationToken`（tokio-util）

## 2. 核心类型

```rust
pub struct Agent { /* builder 装配，不可 Clone */ }

pub struct AgentBuilder {
    pub fn provider(self, p: Arc<dyn LlmProvider>) -> Self;
    pub fn tools(self, r: ToolRegistry) -> Self;
    pub fn history(self, h: Arc<dyn HistorySource>) -> Self;
    pub fn sink(self, s: Arc<dyn EventSink>) -> Self;
    pub fn limits(self, l: TurnLimits) -> Self;   // max_rounds, max_tool_retries
    pub fn build(self) -> Agent;
}

pub enum AgentCommand {          // 前端命令经 daemon 翻译后进入
    TurnInput(Content),
    Interrupt,
}

pub struct TurnLimits { pub max_rounds: u32, /* … */ }

pub enum TurnCompletion {
    Completed { reason: StopReason },   // ModelDone | MaxRounds | Interrupted | MaxTokens
    Failed { error: KernelError },
}
```

## 3. Turn 状态机

```
Idle ──TurnInput──▶ Assembling ──▶ Streaming ──┬─(无 tool call)──▶ Finished
                     ▲                        │
                     │                        └─(有 tool call)──▶ AwaitingApproval?
                     │                                                │批准/无需审批
                     │                                                ▼
                     └────────────── Executing ◀──────────────────────┘
                                       │ 工具结果 append 进上下文
                                       └──▶ Streaming（下一 round）
Interrupt 在任意活动状态生效：取消 LLM 流 / 杀工具（经 TerminalHandle.kill）→ Finished{Interrupted}
```

- **round**：一次 LLM 请求 + 其触发的工具执行。`max_rounds` 熔断防失控循环（默认 100，可配）。
- 状态迁移全部发 `EventSink` 事件，daemon 投影为协议事件。
- 状态机本身可单测：注入 fake provider（脚本化 StreamEvent 序列）+ 内存工具 + 内存 history。

## 4. LLM 接缝

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat_stream(&self, opts: ChatOptions, msgs: Vec<Message>)
        -> Result<BoxStream<'static, StreamEvent>>;
}

pub struct ChatOptions {          // 「中立旋钮」：kernel 只转发不解释（atomcode slot-not-policy 哲学）
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub tool_defs: Vec<ToolDef>,
    pub extra: serde_json::Value,  // provider 特有字段逃生门
}
```

`Message` 为 kernel 自有类型（含 `reasoning_blocks: Vec<SignatureBlock>` 字段供回放），与 openai-interface 类型的转换只存在于 hatchery-llm（ADR-0007）。

## 5. 工具调用接缝

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;                       // JSON Schema
    fn needs_approval(&self, args: &Value) -> ApprovalRequest; // 或 AutoApproved
    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput>;
}

pub struct ToolCtx<'a> {          // 工具能拿到的全部外界能力 = 接缝
    pub fs: &'a dyn FsBackend,
    pub terminal: &'a dyn TerminalBackend,
    pub cancel: CancellationToken,
    pub emit: &'a dyn Fn(ToolProgress),   // 进度上报 → ToolCallProgress 事件
}
```

- **Turn Tool Snapshot**（借鉴 atomcode）：turn 开始时冻结工具表快照，turn 中模式/配置变化不影响进行中的 turn。
- 工具并行：同一 round 的多个 tool call 默认串行执行（输出顺序确定性利于回放）；显式标记 `parallel_safe` 的只读工具可并行（M2+ 优化）。
- 工具输出超限：`ToolOutput::Spilled { path, preview }` 落盘 + 引用（M2，借鉴 dsh spill）。

## 6. 上下文组装

`Assembler`（daemon 侧装配，kernel 调用）：

```
system = prompt sections（platform.md：默认 persona + mode 变体 + 用户覆盖 + AGENTS.md）
history = HistorySource::rebuild(active_branch_head)
        → 过滤/保留 reasoning 块（按 provider 能力表，ADR-0007）
        → compaction item 替换其覆盖区间
        → token 预算裁剪（最旧 round 优先，工具结果先于消息裁剪）
```

token 计数用模型对应的 tokenizer（tiktoken-rs / 近似估算 fallback），预算与裁剪策略在 M1 用最简版本，M5 compaction 里程碑再细化。

## 7. 取消与超时

- `Interrupt` → CancellationToken：LLM 流 drop、`TerminalHandle::kill`、`LocalFs` 写操作本身原子性短无需取消。
- 已执行一半的工具结果：写入类工具完成后照常 append（保持「model-visible = logged」），中断点标记在 turn 级。
- LLM 请求超时/重试在 llm 层，kernel 只见 `StreamEvent::Error`。

## 开放问题

1. 多 tool call 并行的启用条件与顺序保证——M2 决定。
2. kernel 是否需要 `SubAgent` 原语（spawn 子 Agent）还是由 daemon 层做多 runtime 编排——倾向后者（kernel 保持单纯），M3 ACP client 时验证。
3. compaction 触发策略（token 阈值 vs round 阈值）与 side-query 摘要实现——M5。
