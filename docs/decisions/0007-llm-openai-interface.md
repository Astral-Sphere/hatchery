# ADR-0007: LLM 层基于 openai-interface，reasoning 逐字节回放

状态：accepted（2026-09-28）

## 背景

用户自研的 `openai-interface`（crates.io，当前 0.14.0）已建模：Chat Completions（流式 `ChatCompletionChunk` + `ChatCompletionAccumulator`，`reasoning_content` 默认 feature）、Responses API（含 streaming events）、`reasoning_effort`、各厂商 feature（deepseek/qwen/vllm/zai）。hatchery 需要一个 provider 抽象层，并满足「reasoning_content 回传 + 用户可配置」。

参考项目的关键经验：
- qwen-code：canonical effort 阶梯（low|medium|high|xhigh|max）→ 每模型声明映射到哪个 wire 字段（`reasoning_effort` / `enable_thinking` / `thinking`）。
- dsh：reasoning 回放**逐字节精确**以命中 provider 端 KV cache（有专门的 bug-fix 决策记录）。
- atomcode：`ReasoningSignature` 统一承载各家不透明签名块（Anthropic signature / OpenAI encrypted_content / Gemini thoughtSignature），随历史回填。

## 决策

- `hatchery-llm` 实现 kernel 的 `LlmProvider` trait，内部**只用 openai-interface** 作为 wire 层，不自写 HTTP/SSE。v1 覆盖两种 wire：`ChatCompletions` 与 `Responses`；provider 配置声明用哪种。
- kernel 侧中立类型：

```rust
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),           // reasoning_content 增量
    ReasoningDone { signature: Option<SignatureBlock> }, // 不透明签名块，原样存库
    ToolCall(ToolCallDelta),
    Usage(Usage),
    Done { finish_reason: FinishReason },
    Error(LlmError),
}
```

- **回放规则**：重建请求历史时，assistant 消息的 `reasoning_content` 与签名块按 provider 要求逐字节原样回填（存库时不做任何 normalize/trim）；provider 不支持回传时按模型能力声明丢弃（能力表驱动，而非全局开关）。
- **effort 阶梯**：canonical `ReasoningEffort = Off|Low|Medium|High|Max`；每 provider/模型一条映射规则（config 可覆盖内置表），映射到 `reasoning_effort`、`enable_thinking`、thinking budget 等 wire 字段。
- **用户配置**：config.toml 每 provider 可设 `reasoning_effort` 与 UI 展示开关；会话内可临时切换（协议方法 + ACP config option 同时暴露）。
- 重试/退避与 rate-limit 事件在 llm 层处理并上报为协议事件（前端显示「rate limited, retrying in 12s」）。

## 理由

1. 复用用户自己的 crate：wire 细节（含各家怪癖）已有测试与维护者，hatchery 专注 agent 语义。
2. 「逐字节回放 + 能力表」是三个参考项目独立收敛出的最佳实践，直接采纳。
3. Chat Completions 必须保留（第三方 OpenAI 兼容生态是主战场），codex 押注 Responses-only 不适合 hatchery 定位。

## 替代方案（已否）

- 自写 wire 层：重复造轮子。
- 以某家 SDK 类型作内部 IR（qwen-code 用 @google/genai 类型）：被单一 provider 词汇表绑架；hatchery 用自有中立类型，openai-interface 类型只存在于 llm crate 内。

## 后果

- openai-interface 的 feature 组合（deepseek/qwen/vllm/zai）决定 hatchery 的 feature 表；上游缺字段时先修上游（用户是作者，成本可控）。
- 能力表需要随生态更新，设计成 config 可覆盖的内置默认表。
- Anthropic/Gemini 原生 wire 暂不支持（非目标），但 `LlmProvider` trait 不排斥将来加独立 adapter crate。
