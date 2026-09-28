# 工作记录：agent 循环（hatchery-kernel）

- 范围：Turn 状态机、LlmProvider/ToolHost/HistorySource/EventSink trait、上下文组装、取消语义
- 设计文档：[../design/kernel.md](../design/kernel.md)
- 相关 ADR：0004、0005、0007、0009

## 当前状态

crate 骨架已在（M0a），**状态机与 trait 尚未实现**（M0b）。M0a 修正了一处结构问题：kernel 不再直接引用 capabilities 的 backend trait（见下）。

## 待办

- [x] (M0) 解开 L0↔L1 依赖环：kernel 只暴露窄接口 **`ToolHost`**（`snapshot()` / `approval_for()` / `invoke()`）；`Tool`/`ToolCtx`/`FsBackend`/`TerminalBackend`/`ApprovalGate` 归 capabilities，`ToolDef`/`ToolOutput`/`ToolProgress`/`ApprovalRequest` 留 kernel
- [ ] (M0b) trait 全家定义（LlmProvider/ToolHost/HistorySource/EventSink）+ StreamEvent/ChatOptions/Message 类型
- [ ] (M0b) Turn 状态机实现 + fake provider 脚本化单测（含 Interrupt 在各状态的行为矩阵）
- [ ] (M0b) Turn Tool Snapshot 最小版：turn 开始时 `snapshot()` 冻结一次（完整语义 M2）
- [ ] (M1) 上下文组装 v1（token 预算最简版：估算 + 最旧 round 裁剪）
- [ ] (M1) max_rounds 熔断与 TurnCompletion 语义在真实对话下验证
- [ ] (M2) ToolOutput::Spilled 路径
- [ ] (M5) compaction 钩子

## 开放问题

见设计文档末尾 3 条（工具并行、SubAgent 原语归属、compaction 触发）。解决过程记录于此：

- 2026-09-28 `ToolCtx` 的归属问题（原设计让 kernel 引用 capabilities 的 trait，脚手架一建就撞出环）→ 用 `ToolHost` 窄接口解决。附带好处：kernel 的测试不需要任何 backend fake，只要一个 `ScriptedToolHost`（testkit M0b 交付）。

## 变更日志

### 2026-09-28
- 初稿。状态机吸收 atomcode kernel（AgentCommand/AgentEvent）与 qwen-code Turn.run() 生成器两种形态，选「命令进 + 事件出」而非 AsyncGenerator（Rust 生态 async generator 未稳定，channel 更直接）。
- M0a：crate 骨架建立；`ToolHost` 窄接口定案（design/kernel.md §5 与 design/capabilities.md §1 已同步，architecture.md §3 分层纪律加了对应一条）；`cargo xtask layering` 会把「kernel 不得依赖 capabilities」当契约检查。
