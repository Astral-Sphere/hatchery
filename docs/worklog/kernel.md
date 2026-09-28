# 工作记录：agent 循环（hatchery-kernel）

- 范围：Turn 状态机、LlmProvider/Tool/HistorySource/EventSink trait、上下文组装、取消语义
- 设计文档：[../design/kernel.md](../design/kernel.md)
- 相关 ADR：0004、0005、0007

## 当前状态

设计稿完成，未实现。kernel 保持零业务语义（不知道模式/工作区/存储）。

## 待办

- [ ] (M0) trait 全家定义（LlmProvider/Tool/ToolCtx/HistorySource/EventSink）+ StreamEvent/ChatOptions/Message 类型
- [ ] (M0) Turn 状态机实现 + fake provider 脚本化单测（含 Interrupt 在各状态的行为矩阵）
- [ ] (M1) 上下文组装 v1（token 预算最简版：估算 + 最旧 round 裁剪）
- [ ] (M1) max_rounds 熔断与 TurnCompletion 语义在真实对话下验证
- [ ] (M2) Turn Tool Snapshot（模式切换在 turn 边界生效）
- [ ] (M2) ToolOutput::Spilled 路径
- [ ] (M5) compaction 钩子

## 开放问题

见设计文档末尾 3 条（工具并行、SubAgent 原语归属、compaction 触发）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。状态机吸收 atomcode kernel（AgentCommand/AgentEvent）与 qwen-code Turn.run() 生成器两种形态，选「命令进 + 事件出」而非 AsyncGenerator（Rust 生态 async generator 未稳定，channel 更直接）。
