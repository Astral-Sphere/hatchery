# 工作记录：agent 循环（hatchery-kernel）

- 范围：Turn 状态机、LlmProvider/ToolHost/HistorySource/EventSink trait、上下文组装、取消语义
- 设计文档：[../design/kernel.md](../design/kernel.md)
- 相关 ADR：0004、0005、0007、0009

## 当前状态

**M0b 完成（2026-09-28），评审后的两轮修复已入库（2026-09-29 / 2026-09-30）**：trait 全家、Turn 状态机、item 提交与审批往返全部落地；契约缺口（状态机的边、启动期取消、失败 turn 的 usage、审批答复校验）与两处「结构上不可能失败」的测试都已修完，工具进度与 delta 热路径的实现缺陷也已处理。设计文档 `docs/design/kernel.md` 已按实现重写。

**M1 代码完成（2026-10-01），kernel 侧只动了三处**：`RateLimited` 透传（Phase 1）、`TurnInput { turn, content }`（daemon 铸币、kernel 沿用）、`AgentHandle::turn_running()` 信号。**状态机、审批往返、取消语义、`ToolStatus` 全部是 M0b 的既有实现，M1 未改**：四个终态由 kernel 写入（`Failed`/`Completed`/`Denied`/`Cancelled`，agent.rs:610/622/629/635-637），`await_approval`（agent.rs:721，未被提供的答复在 :761 被拒并记 warning）校验答复必须是被提供过的选项之一，所以硬门的 `once_only()` 不可能被 `AllowAlways` 答复放行。**这条检查在生产代码里在，但我没找到钉住它的测试**（2026-10-07 勘察：全仓库 `once_only`/`is_hard_gate` 零生产消费者，`ScriptedToolHost::approval_for`（testkit/src/tools.rs:175）返回测试自己注册的请求，而没有一个 kernel 测试注册过收窄了 `options` 的请求）——M2 Phase 2 的硬门工作要一并补。

**M2 对 kernel 的增量只有一处**（Phase 1，**D13**）：`ToolInvocation` 带出工具收集到的检查点，kernel 在 ToolResult item **之前**追加 Checkpoint item。上下文组装的 token 预算 v1 与 `ToolOutput::Spilled` 都排 **Phase 4**（同一个问题的两半）。两个原标「M2 决定 / M2 定」的开放问题已在本里程碑内裁决完毕（1 = 不做，4 = 顺延 M5，见「开放问题」），**决定本身即交付**。

## 待办

- [x] (M0) 解开 L0↔L1 依赖环：kernel 只暴露窄接口 **`ToolHost`**；`Tool`/`ToolCtx`/`FsBackend`/`TerminalBackend`/`ApprovalGate` 归 capabilities
- [x] (M0b) trait 全家定义（LlmProvider/ToolHost/HistorySource/EventSink）+ StreamEvent/ChatOptions/Message 类型
- [x] (M0b) Turn 状态机实现 + fake provider 脚本化单测（含 Interrupt 在各状态的行为矩阵）
- [x] (M0b) Turn Tool Snapshot 最小版：turn 开始时 `snapshot()` 冻结一次（完整语义 M2）
- [ ] (M2 · Phase 4) 上下文组装 v1（token 预算最简版：估算 + 最旧 round 裁剪）——`HistorySource::view` 已给出接缝（2026-10-03 由 M1 改标 M2：M1 装配是纯机械映射、不做预算，见 daemon 的 `StoreHistory`；工具大输出进树后才是刚需）。**与 `ToolOutput::Spilled` 同排 Phase 4**：两者是「工具输出太大」这同一个问题的两半，来源是 shell 与 web_fetch
- [ ] (M2 · Phase 0，收口 M1) max_rounds 熔断与 TurnCompletion 语义在真实对话下验证（脚本化验证已做：`the_default_fuse_is_finite_and_generous` state.rs:178、`a_fuse_already_at_its_limit_ends_the_turn_without_a_provider_call` tests/turn_state_machine.rs:1451）
- [ ] (M2 · Phase 4) ToolOutput::Spilled 路径（形状已就位、**全仓库无人构造**；阈值与落盘位置是 **D12**）
- [ ] (M2 · Phase 1) **D13**：`ToolInvocation`（tools.rs:22-27）带出工具收集到的检查点，kernel 在 **ToolResult item 之前**追加 Checkpoint item，链变成 `… → ToolCall → Checkpoint → ToolResult`——M1 的 `TurnInput { turn, content }` 之后第一处 kernel 改动
- [x] (M2 · Phase 0，2026-10-07 完成) **D15**：system prompt 经 `HistorySource::view()` 以 system `Message` 进请求（`ChatOptions` 无 system 字段，message.rs:257-279，也不该有）。**kernel 侧无需改动**——`Message::system`（message.rs:56）与 llm 的 `Role::System => WireMessage::system(text)`（translate.rs:134）都已就位；要动的是 daemon 的 `StoreHistory` 与**不变量 2 的边界**（重划为只管分支历史，system prompt 是可复现的派生态、由 `prompt/render` 的 golden 单独钉）。**结果**：如预判，kernel 与 llm 一行未改（`Message::system` 从零消费者变成一个）；`prompt/render` 现在对活着的 runtime 返回冻结那一份，不变量测试断言「两次请求的 system 文本逐字节相同 + 等于 `prompt/render` 的 text」。D15 的理由①（config 变更会重组装）经实测不成立，更正写在 design/kernel.md §6 与 design/daemon.md 开放问题 5
- [ ] (M5) compaction 钩子

## 开放问题

见设计文档末尾 4 条（工具并行、SubAgent 原语归属、compaction 触发、工具级重试上限）。**1 与 4 已于 2026-10-07 在 M2 内裁决**（1 = 不做、4 = 顺延 M5，见下），2 与 3 仍开放。解决过程记录于此：

- 2026-10-07 **开放问题 1（一轮多 tool call 并行，原标「M2 决定」）→ 裁决为不做**。原话把「M2 决定」写成了待办，其实**决定本身就是交付物**。七处结构阻碍：① 执行是串行 `for request in requests { … }`（agent.rs:432-443）；② item 链只有单亲指针 `self.tail`、由 `commit()` 推进（agent.rs:901-910），而每个调用提交两个 item，并行提交要么需要确定性排序、要么需要同父多子——后者与「一次只开一个 item，树保持为链」的纪律（agent.rs:810-813 的注释、design/kernel.md §7）直接冲突；③ `AwaitingApproval { request_id }` 是单槽（state.rs:30-33）；④ `self.commands.recv()` 任一时刻只有一个消费者（stream / tool / approval 三个 select 互斥）；⑤ 每次 `invoke_tool` 独占自己的进度通道与 select，含「返回前排空」那个特例（agent.rs:659-707）；⑥ 三条测试会被打破——`two_calls_in_one_round_are_each_approved_separately`（tests/turn_state_machine.rs:641）钉住结果顺序与精确状态序列，`the_documented_event_sequence_is_emitted_exactly`（:1044）与 `the_same_script_produces_the_same_event_sequence_twice`（:1299）钉住精确事件序列；⑦ `ToolHost`（tools.rs:51-89）不带 `parallel_safe` 一类的元数据，**决策的输入本身也不存在**。收益是延迟，代价是重做提交序与确定性纪律——不划算。
- 2026-10-07 **开放问题 4（`max_tool_retries`）→ 顺延 M5**。全仓库 grep `max_tool_retries` 只命中 `docs/design/kernel.md` 的开放问题 4 与 roadmap 的顺延表（外加这两条记录本身）；「可重试的工具失败」既没有语义也没有第一个消费者，正是 ADR-0009 反预拆分刹车的适用场景。`TurnLimits` 今天只有 `max_rounds: u32`（默认 100，`with_max_rounds` 构造器，state.rs:63-80），由 `the_default_fuse_is_finite_and_generous`（state.rs:178）与 `a_fuse_already_at_its_limit_ends_the_turn_without_a_provider_call`（tests/turn_state_machine.rs:1451）钉住。
- 2026-09-28 `ToolCtx` 的归属问题（原设计让 kernel 引用 capabilities 的 trait，脚手架一建就撞出环）→ 用 `ToolHost` 窄接口解决。附带好处：kernel 的测试不需要任何 backend fake，只要一个 `ScriptedToolHost`（testkit 已交付）。
- 2026-09-28 M0b 落地时又定了几处（设计文档 §5/§7 有完整理由）：`ToolHost::summarize`（摘要需要工具语义，kernel 不该解析参数）、`ToolInvocation { output, is_error }`（「跑失败」与「没跑成」是两件事）、进度改走 **mpsc 通道**（同步回调和「await 工具的同时转发进度」不可兼得，同一 select 循环也让中断能取消工具）、审批改由 **kernel 发起 / daemon 应答**的 id 往返（capabilities.md 的 `ApprovalOutcome` 并入 `ApprovalOption`）。

## 变更日志

### 2026-10-07 · M2 Phase 0：硬门唯一承重的那条腿终于可测

**kernel 本体一行未改**（D15 的注入住在 daemon 的 `HistorySource` 实现里）。本方向的交付是把 2026-09-30 那条「审批答复必须是提供过的选项之一」的修复真正钉住——它此前**结构性不可测**，因为 fake 造不出收窄选项的请求。

- testkit 加 `ScriptedToolHost::requiring_approval_with(ApprovalRequest)`，老的 `requiring_approval(name, risk)` 改为委托它（行为不变，仍是全四个选项）。
- 新增 `a_hard_gate_refuses_an_answer_it_never_offered`（`tests/turn_state_machine.rs`）：一个 `once_only()` 的请求，先答 `AllowAlways`（不在提供列表里）再答 `Deny`（在），断言 ① 工具**从未被调用**（`call_names()` 为空）② tool result 是「the user denied this call to `write_file`; do not repeat it」③ ToolCall item 状态是 `Denied` ④ turn 正常收尾。两条答复走同一条有序命令通道，所以顺序是确定的，没有 sleep、没有轮询。
- **变异验证**（项目纪律：新测试要证明它真的钉住了东西）：把 `await_approval` 里的 `!offers.contains(&option)` 分支去掉，该测试立刻红在「工具从未被调用」这条断言上；分支在时绿。
- 断言 ① 是这条测试的承重部分：如果那个不被提供的 `AllowAlways` 被接受，工具就会跑起来，`Denied` 也变成 `Completed`——这正是硬门被绕过的形状。Phase 2 的 `invariant_project_config_cannot_disable_hard_gates`（规则里存了 `AllowAlways` 也不生效）建在这条之上。

### 2026-10-07 · M2 重新规划对账

roadmap 的 M2 段按一次全仓库勘察重写为 Phase 0–8，本 worklog 随之对账。**kernel 在 M2 只改一处**（Phase 1，D13）：`ToolInvocation`（tools.rs:22-27）带出 `LocalFs` 写前收集的检查点，kernel 在 ToolResult item **之前**追加 Checkpoint item。理由是它已经在造 ToolCall/ToolResult item，用同一套机器顺序天然正确，而链变成 `… → ToolCall → Checkpoint → ToolResult` 是安全的——工具结果靠 `ToolResult.call: ItemId` 与其调用配对，不靠父子关系（design/kernel.md §7 的「一次只开一个 item」纪律不受影响，Checkpoint item 是在 ToolResult 之前**串行**提交的完整 item）。这是自 M1 的 `TurnInput { turn, content }` 以来第一次动 kernel 的公开形状。

**两个开放问题在 M2 内裁决完毕**（原话写「M2 决定」/「M2 与工具层一起定」，所以决定本身即交付）：一轮多 tool call 并行 → **不做**（七处结构阻碍见「开放问题」）；`max_tool_retries` → **顺延 M5**（「可重试失败」没有语义也没有第一个消费者，ADR-0009 反预拆分）。design/kernel.md 的开放问题 1/4 与 §5 的「`parallel_safe` 只读工具的并行是 M2+ 优化」一句已按此改写。

**待办重新挂到 Phase**：上下文组装 v1 与 `ToolOutput::Spilled` 同排 **Phase 4**（2026-10-03 的改标只说了「M2」；roadmap 把两者放在一起，因为它们是「工具大输出」这同一个问题的两半，来源是 shell 与 web_fetch）；max_rounds 熔断的真实对话验证归 **Phase 0**（M1 收口）。

**一处此前没记进本 worklog 的事实**：审批答复校验（agent.rs:761「答复必须是被提供过的选项之一」，2026-09-30 那条修复加的）**没有测试钉住它**——`once_only`/`is_hard_gate` 全仓库零生产消费者，kernel 的测试也没有一个注册过收窄 `options` 的 `ApprovalRequest`。roadmap 的更正 7 把这件事挂在 capabilities/daemon 侧（路径硬门 + `invariant_project_config_cannot_disable_hard_gates`），但缺的那条用例形状上是 kernel 的（答复校验住在 `await_approval` 里）。

**根因比「没人写这条测试」更硬一层（2026-10-07 复核）**：`ScriptedToolHost::requiring_approval(name, risk)`（testkit/src/tools.rs:88-94）内部用 `ApprovalRequest::new(name, …, risk)` 造请求，而 `new` 恒取 `ApprovalOption::ALL`（protocol/src/approval.rs:101，其中 :106 是 `options: ApprovalOption::ALL.to_vec()`）——**这个 fake 在 API 层面就表达不出一个收窄了 `options` 的请求**。所以那条分支不是「暂时没测」，是**结构性不可测**：想测它必须先给 `ScriptedToolHost` 加一个能接收现成 `ApprovalRequest`（或选项表）的构造器。这也解释了为什么 2026-09-30 那轮「先写复现测试再修」的纪律在这条上没兑现——fake 造不出复现形状，而当时没有人为它扩 API。

排在 **Phase 0**（不是 Phase 2）：项目纪律 #3 是「Bug 修复必附回归测试」，这条修复违反了它；而硬门整个压在「答复必须是提供过的选项之一」上（`once_only()` 之所以不可绕过，全靠这条检查），一个承重且不可测的分支与「从未被调用的 `invariants` profile」是同一类问题——门禁在宣称它没有保证的东西。改动很小：testkit 加一个构造器 + kernel 加一条用例（注册 `once_only()` 请求 → 以 `AllowAlways` 答复 → 断言被忽略且 turn 仍在等）。Phase 2 的路径硬门与 `invariant_project_config_cannot_disable_hard_gates` 建在这条之上。

### 2026-10-01 · TurnInput 携带调用方 TurnId（评审⑤自查轮）

`session/prompt` 的回复此前返回一个 manager 自造、永不复现的 TurnId——kernel 在 `run_turn` 里另造一个，回复与任何事件都对不上。改为 `AgentCommand::TurnInput { turn, content }`：daemon 铸币、kernel 沿用，回复、每个 item、终止事件同名（e2e `the_wire_orders_housekeeping_around_item_events_and_names_the_turn` 钉住）。`AgentCommand::prompt` 构造器保留（自己铸币），新增 `prompt_with_turn`。同轮补测：RateLimited 直通到 sink、`max_rounds = 0` 引信（不开 provider、正常收尾）、全部句柄中途 drop 仍以 Interrupted 收尾。计数见 worklog/testing.md 本日条目。

### 2026-10-01 · turn_running 句柄信号（M1 Phase 5）

`AgentHandle` 增加只读的 turn 在跑信号：agent 持 `watch::Sender<bool>`，`transition()` 里以 `send_if_modified` 镜像 `TurnState::is_active`，handle 侧 `turn_running()` 直接 `borrow()` 读。选 watch 而非事件回推，是因为读者（manager 的忙拒、空闲 sweep）是**轮询语义**——在两个 prompt 之间问一嘴，不跟着 turn 走。这打破了「Phase 1 的 RateLimited 是唯一 kernel 改动」的记录：is_busy 原实现（`!is_closed()`）测的是「agent 活着」，daemon 拿它当「turn 在跑」用，两处调用方都被误导（忙拒缺失、空闲卸载永不触发）——信号必须来自状态机本身，而不是从通道状态反推。回归测试 `turn_running_tracks_the_state_machine` 用 gated provider 钉住开/关两个时刻。

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
