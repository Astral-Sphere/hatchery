# 工作记录：LLM provider 层（hatchery-llm）

- 范围：openai-interface adapter、effort 能力表、reasoning 回放、重试/限流
- 设计文档：[../design/llm.md](../design/llm.md)
- 相关 ADR：0007
- 上游：`openai-interface`（用户自研，crates.io，当前 0.14.0；已确认建模 chat completions 流式、responses、reasoning_content/reasoning_effort——来源：docs.rs 页面，2026-09-28 查证）

## 当前状态

设计稿完成，未实现。

## 待办

- [ ] (M0) 确认 openai-interface feature 组合与 hatchery feature 表的映射
- [ ] (M1) ChatCompletions adapter：请求翻译 + SSE chunk → StreamEvent + tool call delta 累积
- [ ] (M1) effort 能力表 v1（deepseek/qwen/通用兼容三族）+ config 覆盖机制
- [ ] (M1) reasoning 逐字节回放（含签名块存储表示）
- [ ] (M1) 重试/退避/429 + RateLimited 事件
- [ ] (M1) fixture 录制工具（xtask）+ deepseek/qwen 真实探测录制
- [ ] (M1) `hatchery doctor --provider` 实测子命令
- [ ] (M2) Responses adapter
- [ ] (M2) 多模态 image 输入

## 开放问题

见设计文档末尾 3 条（encrypted_content 统一表示、多模态节奏、上游发版）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。三家参考项目（qwen-code 映射表、dsh 逐字节回放、atomcode 签名块）的经验合并成 ADR-0007；wire 层唯一依赖 openai-interface，类型不外泄。
