# 工作记录：agent 循环（hatchery-kernel）

- 范围：Turn 状态机、LlmProvider/ToolHost/HistorySource/EventSink trait、上下文组装、取消语义
- 设计文档：[../design/kernel.md](../design/kernel.md)
- 相关 ADR：0004、0005、0007、0009

## 当前状态

**M0b 完成（2026-09-28）**：trait 全家、Turn 状态机、item 提交与审批往返全部落地；40 个测试（19 单元 + 20 集成 + 1 事件类型）+ 2 个可运行 doctest 全绿。设计文档 `docs/design/kernel.md` 已按实现重写。

## 待办

- [x] (M0) 解开 L0↔L1 依赖环：kernel 只暴露窄接口 **`ToolHost`**；`Tool`/`ToolCtx`/`FsBackend`/`TerminalBackend`/`ApprovalGate` 归 capabilities
- [x] (M0b) trait 全家定义（LlmProvider/ToolHost/HistorySource/EventSink）+ StreamEvent/ChatOptions/Message 类型
- [x] (M0b) Turn 状态机实现 + fake provider 脚本化单测（含 Interrupt 在各状态的行为矩阵）
- [x] (M0b) Turn Tool Snapshot 最小版：turn 开始时 `snapshot()` 冻结一次（完整语义 M2）
- [ ] (M1) 上下文组装 v1（token 预算最简版：估算 + 最旧 round 裁剪）——`HistorySource::view` 已给出接缝
- [ ] (M1) max_rounds 熔断与 TurnCompletion 语义在真实对话下验证（脚本化验证已做）
- [ ] (M2) ToolOutput::Spilled 路径（形状已就位，阈值与落盘未定）
- [ ] (M5) compaction 钩子

## 开放问题

见设计文档末尾 4 条（工具并行、SubAgent 原语归属、compaction 触发、工具级重试上限）。解决过程记录于此：

- 2026-09-28 `ToolCtx` 的归属问题（原设计让 kernel 引用 capabilities 的 trait，脚手架一建就撞出环）→ 用 `ToolHost` 窄接口解决。附带好处：kernel 的测试不需要任何 backend fake，只要一个 `ScriptedToolHost`（testkit 已交付）。
- 2026-09-28 M0b 落地时又定了几处（设计文档 §5/§7 有完整理由）：`ToolHost::summarize`（摘要需要工具语义，kernel 不该解析参数）、`ToolInvocation { output, is_error }`（「跑失败」与「没跑成」是两件事）、进度改走 **mpsc 通道**（同步回调和「await 工具的同时转发进度」不可兼得，同一 select 循环也让中断能取消工具）、审批改由 **kernel 发起 / daemon 应答**的 id 往返（capabilities.md 的 `ApprovalOutcome` 并入 `ApprovalOption`）。

## 变更日志

### 2026-09-28

**M0b 落地**（40 测试 + 2 doctest）。落地过程中被测试抓出的三个真实缺陷，都记下来因为它们是同一类错误：

- **计数信号量会「还」permit**：testkit 的 `Gate` 最初用 `Semaphore::acquire()` 后立刻 drop，permit 被还回信号量，于是 `release(1)` 放行了后续每一步——中断测试因此看到 turn 正常跑完（`ModelDone` 而不是 `Interrupted`）。改为 `permit.forget()` 才是「消耗」一个许可。
- **脚本化流在取消后仍吐出整个脚本**：`ScriptedProvider` 等待 gate 的 select 在取消时返回了事件本身。真实 provider 的请求被取消后不会继续供货，所以改成 `filter_map` 返回 `None` 结束流。
- **reasoning 的签名没落到 item 上**：签名存在 round 上、关闭 item 时却不读它。改成签名到达时写进**当时打开的那个** reasoning item（后到的属于后一个 item）。

其他实现决定与理由见设计文档 §7。集成测试用 testkit 的四个 fake（含 `answer_approvals` 替测试应答审批），确定性测试比对事件名、item 种类与迁移序列三重。

### 2026-09-28（M0a）
- 初稿。状态机吸收 atomcode kernel（AgentCommand/AgentEvent）与 qwen-code Turn.run() 生成器两种形态，选「命令进 + 事件出」而非 AsyncGenerator（Rust 生态 async generator 未稳定，channel 更直接）。
- M0a：crate 骨架建立；`ToolHost` 窄接口定案（design/kernel.md §5 与 design/capabilities.md §1 已同步，architecture.md §3 分层纪律加了对应一条）；`cargo xtask layering` 会把「kernel 不得依赖 capabilities」当契约检查。
