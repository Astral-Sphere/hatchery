# 工作记录：agent 循环（hatchery-kernel）

- 范围：Turn 状态机、LlmProvider/ToolHost/HistorySource/EventSink trait、上下文组装、取消语义
- 设计文档：[../design/kernel.md](../design/kernel.md)
- 相关 ADR：0004、0005、0007、0009

## 当前状态

**M0b 完成（2026-09-28），评审后的两轮修复已入库（2026-09-29 / 2026-09-30）**：trait 全家、Turn 状态机、item 提交与审批往返全部落地；契约缺口（状态机的边、启动期取消、失败 turn 的 usage、审批答复校验）与两处「结构上不可能失败」的测试都已修完，工具进度与 delta 热路径的实现缺陷也已处理。设计文档 `docs/design/kernel.md` 已按实现重写。

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

### 2026-09-30 · RateLimited 透传通道（M1 Phase 1）

llm adapter 的重试退避需要一个能让前端倒计时的通知，但 `LlmProvider` 没有 sink 可发。定案：`StreamEvent` 与 `KernelEvent` 各加一个 `RateLimited { retry_after_ms }` 变体，kernel 收到后**只转发不解释**（round 状态不动）——它是一条信息，不是一个迁移。`is_control` 语义自动正确（不在 delta 白名单里），serde 形状 `rate_limited` 与 protocol 的 `ServerEvent::RateLimited` 对齐。这是 M1 计划中唯一预期的 kernel 改动。

### 2026-09-30 · 评审后的两轮修复（2026-09-29 与 2026-09-30）

**启动期的 await 必须在取消的 select 里面。** `history.view()` 与 `provider.chat_stream()` 原本是裸 await：连接期挂住的 provider 会让 agent 只剩 `abort()` 一条出路，而 abort 跳过终止事件——前端于是永远等不到 `TurnEnded`。两处现在都走 `guarded_startup`（取消令牌 + 命令通道 + future 同一个 biased select，future 被 pin 住、跨命令续跑）。**教训**：接缝上任何可能阻塞的 await 都要能被取消，包括「流还没开始」的那一段。

**设计文档里画的边必须真的发出来。** `AwaitingApproval → Executing{round}` 是 §3 画着的边，实现里从来没有：答复到达后，本轮后续每个工具都还在报「等审批」，而且带着已失效的 request id。按 `StateChanged` 投影 `SessionStatus` 的前端会在一个正在跑的工具上显示审批弹窗。

**失败的 turn 要保住已经花掉的 usage。** `TurnCompletion::Failed` 原本没有 usage 字段，与 `usage()` 自己的文档相矛盾；跑过的那几轮是真金白银，重试还要再付一次。

**审批答复必须是被提供过的选项之一。** 硬门（`once_only` 不带「记住」类选项）原本能被 `AllowAlways` 答复放行。「答复必然是被提供的选项之一」是我们把两个枚举合成一个的前提，检查它才让前提成立；未被提供的答复记 warning 并继续等。

**测试要能观测到它声称观测的东西。** 两处结构上不可能失败的测试：`an_interrupt_with_no_turn_running_is_ignored` 在当前线程 runtime 上是空转（`send` 不挂起 → agent 任务在断言之前根本没被 poll）；工具取消则因为 fake 在 gate 之后才记录调用、而 kernel 在取消胜出时会 drop 掉 invoke future，`cancelled` 永远不可能是 true。前者改成走同一条 FIFO 通道驱动一个真实 turn，后者改成进门即记录 + drop guard 写结论。

**工具返回那一刻还排在通道里的进度会丢（2026-09-30）。** biased select 先 poll 进度通道、再 poll invoke，所以「在同一次 poll 内发进度并返回」的工具，它最后那条进度留在通道里、跟着 invoke future 一起被丢掉。带 gate 的测试看不见这个窗口（放开 gate 会多给 select 一轮，进度分支就赢了）——补的不带 gate 的测试在改动前先跑过一次，确认是红的。中断路径不排空：那次调用正要被记成 `Cancelled`。

**delta 热路径：一轮只留一份文本。** `Round` 原本同时维护 `text`/`reasoning` 两个累加器**和**打开 item 里的 `text`，每个 delta 拷三次（累加器、item、事件里的 `to_owned`）。现在 round 记住「已关闭的 item」（`Vec<ItemKind>`），delta 按值传入、拷进 item 之后直接 move 进事件：**每 delta 一次拷贝**。行为由 `the_documented_event_sequence_is_emitted_exactly` 与 `reasoning_is_streamed_and_stored_verbatim` 兜住。

**LLM 接缝改借用**（`chat_stream(&ChatOptions, &[Message])`）：长会话原本每轮多拷一份全量上下文加一份工具表；`ChatOptions` 现在整个 turn 只构造一次（工具表本来就是 turn 级冻结）。

**晚到的 reasoning 签名是 adapter 违约，kernel 只能记账。** 承接 M0b 那条「签名要落到当时打开的 item 上」：如果 provider 先发 `TextDelta` 再发 `ReasoningDone`，那个 item 已经提交（append-only，改不了），签名就只剩内存里这一轮的份——turn 内重放照常带签名，**从库里重建**出来的 reasoning 无签名。kernel 记一条 warning，并把顺序要求写进 `LlmProvider`、`StreamEvent::ReasoningDone` 与设计文档 §4；代价由 `a_signature_that_arrives_after_the_text_cannot_be_stored_but_still_replays` 钉住。要不要为保住签名而缓冲一个事件，等 M1 有真 adapter、实测各家 provider 的事件顺序之后再定。

**`AgentHandle::try_submit` 删除。** 它把「队列满（agent 还活着）」和「通道关了（agent 已停）」都报成 `AgentGone`，而这两个答案对 daemon 意味着完全不同的处置；零调用方，M1 的 daemon 走 `submit().await` 的背压路径。真需要非阻塞入口时，再按真调用方的形状加回来。

**接缝枚举不加 `#[non_exhaustive]`**：理由与反方理由见 worklog/architecture.md 同日条目。

### 2026-09-28

**M0b 落地**（40 测试 + 2 doctest）。落地过程中被测试抓出的三个真实缺陷，都记下来因为它们是同一类错误：

- **计数信号量会「还」permit**：testkit 的 `Gate` 最初用 `Semaphore::acquire()` 后立刻 drop，permit 被还回信号量，于是 `release(1)` 放行了后续每一步——中断测试因此看到 turn 正常跑完（`ModelDone` 而不是 `Interrupted`）。改为 `permit.forget()` 才是「消耗」一个许可。
- **脚本化流在取消后仍吐出整个脚本**：`ScriptedProvider` 等待 gate 的 select 在取消时返回了事件本身。真实 provider 的请求被取消后不会继续供货，所以改成 `filter_map` 返回 `None` 结束流。
- **reasoning 的签名没落到 item 上**：签名存在 round 上、关闭 item 时却不读它。改成签名到达时写进**当时打开的那个** reasoning item（后到的属于后一个 item）。

其他实现决定与理由见设计文档 §7。集成测试用 testkit 的四个 fake（含 `answer_approvals` 替测试应答审批），确定性测试比对事件名、item 种类与迁移序列三重。

### 2026-09-28（M0a）
- 初稿。状态机吸收 atomcode kernel（AgentCommand/AgentEvent）与 qwen-code Turn.run() 生成器两种形态，选「命令进 + 事件出」而非 AsyncGenerator（Rust 生态 async generator 未稳定，channel 更直接）。
- M0a：crate 骨架建立；`ToolHost` 窄接口定案（design/kernel.md §5 与 design/capabilities.md §1 已同步，architecture.md §3 分层纪律加了对应一条）；`cargo xtask layering` 会把「kernel 不得依赖 capabilities」当契约检查。
