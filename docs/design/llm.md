# 设计：LLM Provider 层（hatchery-llm）

> 状态：设计稿。依据 ADR-0007。wire 层唯一依赖：`openai-interface`（≥0.14，features: `reasoning` 默认 + 按需 `deepseek`/`qwen`/`vllm`/`zai`）。

## 1. 职责

- 实现 kernel 的 `LlmProvider` trait，把中立的 `ChatOptions` + `Message` 翻译为 openai-interface 的 `RequestBody`（Chat Completions）或 Responses API 输入；把 `ChatCompletionChunk` 流翻译为 `StreamEvent`。
- reasoning 的采集、存储表示与逐字节回放。
- effort 阶梯到各 provider wire 字段的映射（能力表驱动）。
- 重试/退避、rate-limit 上报、usage 统计。

openai-interface 的类型**不得**泄漏出本 crate（公共 API 只暴露 kernel 中立类型）。

## 2. Provider 配置模型

```toml
# config.toml 片段
[providers.deepseek]
base_url = "https://api.deepseek.com"
env_key = "DEEPSEEK_API_KEY"          # 密钥只从环境读取，不落盘
wire = "chat-completions"              # 或 "responses"
reasoning_effort = "high"              # Off|Low|Medium|High|Max
show_reasoning = true                  # UI 展示开关（不影响回传）
models = ["deepseek-flash"]

[providers.my-gateway]
base_url = "https://gw.example.com/v1"
env_key = "GW_KEY"
wire = "chat-completions"
http_headers = { "X-Custom" = "1" }
retry = { max = 4, backoff_ms = 500 }
```

```rust
pub struct ProviderConfig {
    pub base_url: Url,
    pub env_key: String,
    pub wire: WireApi,                       // ChatCompletions | Responses
    pub headers: HeaderMap,
    pub retry: RetryPolicy,
    pub reasoning: ReasoningConfig,          // 默认 effort + 展示开关
}
```

adapter 注册表采用与 ToolRegistry 相同的注册句柄模式（`register() → Handle{dispose, replace}`，见 capabilities.md §1 与 ADR-0009 纪律 3）：运行中更换/升级 provider adapter 走整表原子替换，进行中的 turn 使用其开始时的冻结快照。

## 3. effort 映射（能力表）

canonical：`Off | Low | Medium | High | Max`。内置默认表（借鉴 qwen-code 的 per-model `disableField` 思路），config 可整条覆盖：

| provider 族 | wire 表达 |
|---|---|
| OpenAI / Responses | `reasoning.effort: minimal\|low\|medium\|high`（Max→high） |
| DeepSeek | `thinking: { type: "enabled"/"disabled" }`；当前世代（2026-09-30 校准）为混合推理模型，**默认开**，该开关关掉推理，模型不再切换 |
| Qwen (DashScope 兼容) | `enable_thinking: bool` + `thinking_budget`（Low..Max 映射预算档）；当前世代混合模型**默认开**，显式传参保证确定性 |
| 智谱 GLM | `thinking: { type: "enabled"/"disabled" }` |
| 通用 OpenAI 兼容 | `reasoning_effort` 透传；不支持则忽略并记录一次 warning |

能力表条目：

```rust
pub struct ModelCapabilities {
    pub reasoning_field: ReasoningField,   // ReasoningEffort | EnableThinking | ThinkingBudget | ModelSwitch | None
    pub echo_reasoning: bool,              // 是否支持/要求历史回传 reasoning_content
    pub signature_blocks: bool,            // 是否有不透明签名块（Responses encrypted_content 等）
    pub max_context_tokens: Option<u64>,
}
```

## 4. reasoning 数据通路

```
流式:  chunk.delta.reasoning_content ─▶ StreamEvent::ReasoningDelta
       (ChatCompletionAccumulator 同步累积用于非流式路径与校验)
完成:  provider 附带签名/加密块 ─▶ StreamEvent::ReasoningDone{signature}
存储:  ItemKind::Reasoning{text, signature} 原文入库，不 trim 不 normalize（ADR-0003/0007）
回放:  HistorySource 重建时 ─▶ Message.reasoning_blocks ─▶ llm 层按能力表:
         echo_reasoning=true  → 逐字节写回 assistant message 的 reasoning_content
         signature_blocks=true → 写回对应 wire 字段（encrypted_content 等）
         均为 false           → 丢弃（不回传）
```

**逐字节纪律**（dsh 的 KV-cache 教训）：入库到回传之间禁止任何 transform（trim/normalize/占位符剥离）。例外：provider 返回的显式填充（如 atomcode 观察到的 "(no reasoning detected)"）在**入库前**按 provider 规则剥离，规则表与能力表放一起。

## 5. 两种 wire 的适配

- **ChatCompletions**：`RequestBody::get_stream_response` → `PostStream` → chunk 翻译。工具调用用 OpenAI function calling delta 累积（openai-interface accumulator 已处理 id/arguments 分片）。
- **Responses**：streaming events（`response.output_text.delta`、`response.reasoning_summary_text.delta`、function call 事件）翻译到同一 `StreamEvent` 词汇表。
- 差异（如 Responses 的 `previous_response_id`、reasoning summary vs content）封在各自 adapter 模块，kernel 无感知。

## 6. 错误、重试与限流

- 分类：网络/5xx/429 → retryable（指数退避 + jitter，尊重 `Retry-After`）；4xx（除 429）→ fatal，透传 provider error body 摘要。
- 401/403：提示检查 `env_key` 对应环境变量；不重试。
- 重试期间向 sink 发 `RateLimited{retry_after}` 事件，前端展示倒计时。
- 流中断（SSE 半途断开）：若已产生 partial item，标记 `TurnCompletion::Failed`，不做自动续写（v1）。

## 7. 测试策略

- **录制回放**：对真实 provider 的一次性探测录制 SSE 字节流为 fixture（含 reasoning_content、tool call 分片、限流响应），adapter 单测全部离线跑。录制脚本放 `xtask/`。
- 契约测试：每个 adapter 过同一组 `LlmProvider` 行为测试（trait 级 test suite 宏）。
- 真实探测（用户偏好的实证方式）：`hatchery doctor --provider <id>` 子命令发一次最小真实请求，报告 wire/effort/reasoning 实测行为，输出到 worklog 供能力表校正。

## 开放问题

1. Responses wire 的 reasoning 回放字段（`encrypted_content`）与 store 的 signature 表示统一格式——M1 实测 OpenAI/DeepSeek-harness 后定。
2. 多模态（image 输入）v1 是否进 ChatOptions——协议层已留 Content 类型，llm 层 M2 再实现。
3. 上游 openai-interface 缺能力时的发版节奏（用户是作者，直接上游修）。
