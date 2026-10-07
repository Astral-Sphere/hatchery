# 工作记录：runtime daemon（hatchery-daemon）

- 范围：单实例发现、监听与传输、会话管理（租约/代际）、live hub、装配、可观测性
- 设计文档：[../design/daemon.md](../design/daemon.md)
- 相关 ADR：0001、0002、0009

## 当前状态

**M1 Phase 3 代码完成（2026-10-01，评审③）**：配置分层、prompt 管线（**只到装配与透明性，装配结果未进过模型请求**——见下 2026-10-07 对账）、单实例发现、UDS 传输、SessionManager、LiveHub、方法接线、审计与 disposer 全部落地；TestDaemon/ClientProbe 经真实 socket 的双前端扇出测试通过。`./scripts/ci.sh` 全绿（除**有意修改**的 protocol fixture 未提交导致的 determinism 步骤——提交后恢复）。

**M1 Phase 4 补齐（2026-09-30，评审④）**：生产入口 `entry`（audit → 锁 → store → serve → 信号驱动的逆序 teardown）、stdio 监听（连接循环泛化出传输）、tracing 落盘轮转（日轮转 + 14 天保留）、空闲 sweep 调度、`doctor` 探测模块。CLI 半边见 `worklog/cli.md`。

**M1 Phase 5 收口（2026-10-01，评审⑤）**：四处行为修正（见变更日志）+ e2e 场景与不变量组落地（hatchery-tests，测试面记录见 `worklog/testing.md`）。

**对账口径（2026-10-07，M2 重新规划）**：本文件此前把三处现状写得比实际宽，逐条更正——

1. ~~**prompt 管线不通到模型**~~ → **已修（2026-10-07，M2 Phase 0；实现见本日变更日志）**。下面这段是当时勘察到的证据，保留作为记录：`render_chat`（prompt.rs:57）的**唯一非测试调用方**曾是 `DaemonCore::render_prompt`（core.rs:227，服务 `prompt/render`），另外四处调用是 prompt.rs 自己的单测（:182/:190/:214/:234）；`ChatOptions`（kernel/src/message.rs:257-279）字段是 `model / reasoning_effort / temperature / max_output_tokens / tool_defs / extra`，**没有 system prompt 字段**；`StoreHistory::view()`（runtime.rs:64）只造 user/assistant/tool_result 三种消息，从不产 system；`checkpoints_are_not_provider_visible`（runtime.rs:611）还在 :618 主动断言 `view.messages` 里没有 `Role::System`。旁证：`render_prompt` 解析出会话的 model 之后直接丢掉（`let _ = model;`，core.rs:226）。**下游管道是就绪的**——`Message::system`（message.rs:56）在，`translate.rs:134` 已把 `Role::System` 翻成 wire 的 system 消息，缺的只是 daemon 这一侧的注入。已按 **D15** 注入（决策与理由的更正见 design/platform.md §2.1、design/kernel.md §6）。
2. **协议 20 个方法只服务 10 个**。`SERVED_METHODS`（core.rs:26-37）= daemon/hello、session/{new,load,list,prompt,cancel,set_config}、config/{get,set}、prompt/render；路由是 `DaemonCore::handle`（core.rs:106）里一个 `match request.method.as_str()`（:108），MethodNotFound 有真兜底（:189-192，全 crate 无 `todo!()`）。余下 10 个随 **M2 Phase 3**，其中 `session/rewind` 与 `approval/respond` 被 core.rs:594/:596 显式断言为「is M2」，届时翻转。daemon 也**不服务任何 notification**（server.rs:161："a `{}` notification arrived; M1 serves none"）。
3. **不存在 profile 类型**。design/daemon.md §3.1 的四个 profile（`local`/`headless`/`acp-stdio`/`acp-standalone`）无任何实现，`hatchery-daemon` 里 `profile` 只命中 src/lib.rs:10 的一句文档注释（"Assembly is profile-based and audited at startup"），Cargo.toml 的 `description` 同样写着 profile-based——两处都是愿景。M2 只交付「**按会话来源选后端的 `Backends` 装配点**」（今天 `Backends` 在 manager.rs:622 内联构造，无选择逻辑），四个命名 profile 顺延到出现第二个消费者（ACP 两行属 M3）。

## 待办

- [x] (M1) 单实例：daemon.lock（fs2 独占）+ daemon.json（0600、write-then-rename）+ boot token；锁独占与死 pid 判定已测（attach-or-spawn 的 CLI 半边随 Phase 4）
- [x] (M1) **daemonize spike（D1 定案，见变更日志）**：CLI spawn 分离进程；不做双 fork、不依赖 sd_notify
- [x] (M1) UDS 监听 + JSON-RPC 分发（`DaemonCore::dispatch` 无传输纯函数面，server.rs 只做帧收发）；stdio 随 Phase 4
- [x] (M1) SessionManager：runtime 装配、generation **落库（store 新增 `bump_generation`）**、事件代际过滤（信封由 daemon 附带）
- [x] (M1) LiveHub v1：per-session broadcast(4096)、隐式订阅、无 replay window（tokio broadcast 无接收者不缓冲，与设计一致：重连走 `session/load`）
- [x] (M1) 崩溃恢复：running/waiting → idle（启动时 `recover_crashed_sessions`）
- [x] (M1) 启动 fail-loud 审计（provider 缺 key / 数据目录 / 状态目录，一次列全）
- [x] (M1) 配置分层加载 + prompt 管线 v1 落本 crate；坏 key 逐条忽略 + warning
- [x] (M1) disposer 逆序 teardown + **panic 容错**（一步炸了其余照跑）+ 逆序测试
- [x] (M1) stdio 监听 + tracing 落盘轮转 + 生产入口 `entry`（2026-09-30：`serve_connection` 对读写半泛型，duplex 测试证明管道与 socket 同核；日志日轮转 + 启动时按日期字符串修剪，`RUST_LOG` 过滤）
- [x] (M1) 空闲卸载的后台 sweep 任务（`entry` 内 60s 间隔 tick `sweep_idle`，D2 判定仍在 sweep 内）
- [x] (M1) `doctor` provider 实测探测模块（`doctor::probe_provider` 走真实 `LlmProvider` 轮次；离线 wiremock 验证；真实两家探测由 CLI 触发，留痕见 `worklog/cli.md`）
- [x] (M2 Phase 0，2026-10-07 完成) **把 system prompt 真正接进 turn**：装配结果经 daemon 的 `HistorySource` 实现进入请求——`StoreHistory::view()`（runtime.rs:64）在分支历史前**前置一条 system `Message`**；`ChatOptions` **不加** system 字段（它的 `tool_defs` 每轮被 kernel 用冻结的 tool snapshot 覆写，是 knobs 不是内容载体）。渲染时机与不变量 2 的边界由 **D15** 定：① 建议 **runtime 装配时渲染一次并冻结整个 runtime 生命周期**——模式切换与 config 变更本来就 bump generation 重组装，而每轮重渲染会让 environment 节的日期/cwd 破坏前缀稳定性，那正是本项目为 KV cache 反复强调的东西；② 建议**不变量 2 只管分支历史**，system prompt 是可复现的派生态，由 `prompt/render` 的 golden 单独钉。随之必须更新 `invariant_minimal_chat_replays_reasoning_byte_exact`（hatchery-tests/tests/scenario1.rs:14）——它把 turn 2 请求体的 `messages` 数组当作整个 `serde_json::Value` 与手写期望比对，system 消息一出现就对不上。**结果**：按 ①② 落地，但 **D15 的理由① 被实测推翻**（`session/set_config` 与 `config/set` 都不重组装活着的 runtime，唯一卸载路径是空闲清扫），冻结仍然对、依据换成前缀稳定性 + 与既有 model/effort 语义一致；由此暴露的缺陷记为 design/daemon.md **开放问题 5**。实现细节见本日变更日志。
- [x] (M2 Phase 0，2026-10-07 完成) 删掉 daemon → `hatchery-acp` 的**死依赖边**：Cargo.toml:21 声明、daemon 源码零引用、crate 本体只有文档注释（结尾自述 "Status: M0 skeleton; implementation lands in M3"），而 `cargo xtask layering` 把它算作一条 build edge。删边（或显式标注为 M3 接缝）。**结果**：选择删边（Cargo.toml 与 Cargo.lock 各一行）；`cargo xtask layering` 仍报 strictly downward、无环。M3 真要用时再加回来，那时它是有消费者的边
- [ ] (M2 Phase 1) HubSink 在 **Checkpoint item 落库后补写 `checkpoints` 行**：D13 已定「kernel 在 ToolResult item 之前追加 Checkpoint item」，链成 `… → ToolCall → Checkpoint → ToolResult`；daemon 侧沿用 `ItemFinished` 现有的「先 commit 再 publish、commit 失败扣发」顺序（runtime.rs:232-245），补写行失败**只记日志**（item 里已带 `commit_id`，可回退重建）。该表只服务跨会话的预算核算与 GC，不是 rewind 的主索引
- [ ] (M2 Phase 1/2) `Backends` 增审批与检查点字段（今天只有 `fs`/`terminal`，capabilities/src/registry.rs:25），并把 manager.rs:622 内联构造的 backends 提成**按会话来源选择的装配点**——这是 M2→M3 接缝里唯一可机器验证的 daemon 半边（另一半是 `FsBackend`/`TerminalBackend`/`ApprovalGate` 的契约测试套件，见 worklog/capabilities.md）；绑定表的 ACP 三行留作有文档的接缝，真验证在 M3
- [ ] (M2 Phase 2) **`DaemonApproval` 的 daemon 半边**：pending 请求注册表（request_id → session/runtime）+ `approval/respond` 路由到 `AgentCommand::decide`（kernel/src/command.rs:65）+ **fail-closed 超时（住在 gate，不在 kernel）**。今天上游齐、下游空：`ToolHost::approval_for`（kernel/src/tools.rs:69，registry 实现 capabilities/src/registry.rs:89）存在但**没有任何工具返回 `Some`**（read_file.rs:65 注释明写 Code 模式的越界审批随 M2 的 registry 接线）；`KernelEvent::ApprovalNeeded`（kernel/src/sink.rs:82，agent.rs:732 发出、:755 等待）→ `TurnState::AwaitingApproval` → `SessionStatus::WaitingApproval`（runtime.rs:194）→ `ServerEvent::ApprovalRequested`（runtime.rs:256-262）一路通到前端，而 daemon 里 `ApprovalDecision`/`decide(` **零命中**、server.rs 里 `approval` **零命中**、`impl ApprovalGate` **全仓库零命中**（`DaemonApproval` 不存在）。净结果：审批事件今天出得去、答不回来，且没有工具会触发它。同阶段还要 `approval_rules` 表的读写 API 与 **D8** 求值语义（该表现有列只有 `id/scope/matcher/decision/created_at`——无排序列、无 enabled 列、scope 是裸 TEXT、无 session 外键）
- [ ] (M2 Phase 2) **模式装配**：`ToolPolicy` + `assemble(mode, backends)` + `builtin(spec, &backends)` 取代 manager.rs:612 硬编码的 `chat_tools()` 循环——它**从不读 `session.mode`**，所以 code 会话今天拿到的是与 chat 完全相同的三个只读工具 + `NoTerminal`
- [ ] (M2 Phase 2) **`STRICT_KEYS` 接线**：config.rs:154 现在是空表，:148-153 的注释直说「接进 `filter_keys` 与 typed reader 是 M2 的任务」，唯一被钉住的行为就是「它是空的」。同时补配置 schema：`[modes.*]`、审批规则、工具策略、检查点预算四类 key **今天一个都没有**（已知 key 只有 `ui.{show_reasoning,theme,language,response_language}`、`daemon.idle_timeout_min` 与 `providers.<id>.*` 子树，见 `is_known_key`），capabilities.md §4 的 `ToolPolicy` 与 §2 的 `Budget` 在 schema 里无任何表示
- [ ] (M2 Phase 3) **接通余下 10 个未路由方法**（会话级四项之外还有 `session/set_mode`、`session/delete`、`session/rename`、`store/export_jsonl`；M2 收尾时协议方法面应全部接通），并翻转两条断言：`an_unknown_method_is_method_not_found`（core.rs:466，拿 `session/rewind` 当未知方法示例）与 `server_constants_name_what_m1_serves`（core.rs:592，:594 断言 rewind "is M2"、:596 断言 approvals "are M2"）
- [ ] (M2 Phase 3) **rewind 三 scope**：store 侧的 `branch_tree`/`edit_fork`/`switch_branch`/`delete_branch`/`delete_session` **M0 就实现且有专测**（store/src/actor.rs:710/744/771/787/534），`export_jsonl` 亦然（store.rs:365），而 daemon 侧 grep **零调用点**——本项主要是接线与组合逻辑，唯独 rewind 的 Code 半边要等 Phase 1 的 `CheckpointStore`（rewind 靠 `ItemKind::Checkpoint{commit_id,..}` 定位 commit，不查 `checkpoints` 表）。`RewindScope::Both` 的顺序是**先 restore 代码、成功再移 head**（restore 失败绝不能已经把历史移走）；restore 前的安全快照记进 `checkpoints` 表、`item_id = NULL`、**不建 item**——它是 undo-of-undo，不属于对话历史，该列可空正是为此留的
- [ ] (M2 Phase 3) **模式切换语义**：ADR-0005 的「切到 Chat 时进行中的写工具调用需先完成或取消」今天无代码；`daemon/hello` 的能力表（core.rs:68-71）只广告 `modes: vec![SessionModeId::chat()]`，`SessionModeId::code()` 在 hatchery-daemon 里从未被构造；`ModeSwitched` 需真正发出
- [ ] (M2 Phase 3) **`SessionLoadResult.pending_approvals`**：给尚无 fixture、尚无消费者的 `PendingApproval`（protocol/src/method.rs:498）一个落点，让重连的前端能重画审批弹层
- [ ] (M2 Phase 3) **历史移动后的前端重建约定：不加新事件**（新增事件 `type` 属协议 major bump）——`SessionUpdated.state.active_branch_head` 已在广播里，前端发现它不是自己已投影 head 的后继就 `session/load` 重建；发起方本来就能在自己的回复里拿到新 Session。daemon 侧要保证的只是：rewind/切分支/编辑分叉之后，落库的 Session 行与广播出去的 `active_branch_head` 一致
- [ ] (M2 只测量) **记档一次 Code 会话的事件量**到本文件——hub.rs:4-6 的原意就是「这些测量决定 M2 策略」；这是 M2 在 hub 上唯一的动作，实现见下条
- [ ] (M3) hub coalescing（16ms 窗）+ replay window —— **2026-10-07 由 M2 顺延（用户裁决）**：`ServerEvent::is_coalescable`（protocol/src/event.rs:195）只覆盖 `text_delta`/`reasoning_delta`，而 M2 新增的事件量主要来自 `ToolCallProgress`，**它不可合并**——coalescing 治不了 M2 的病；replay window 则已被 `session/load` + `replay_from` 取代且有 e2e 覆盖
- [ ] (多 daemon 形态出现时) SessionLease 跨进程文件锁 —— **2026-10-07 由 M2 顺延（用户裁决），不挂 M2**：`--embedded` 全仓库无实现（CLI 唯一路径是 attach-or-spawn），单实例 `daemon.lock`（fs2，discover.rs:123）已经挡住两个 daemon；单 daemon 内「一会话一 turn」由 per-session turn 闸门（`let _lease = gate.lock().await`，manager.rs:303）+ 在途 CAS 标记（runtime.rs:385/391/400 的 `begin_turn`/`end_turn`/`is_busy`，HubSink 在 `KernelEvent::TurnEnded` 时清除）承担，并由 `invariant_session_lease_blocks_second_runtime`（tests/invariants.rs:95）与 `two_concurrent_prompts_yield_exactly_one_turn`（:213）钉住。跨会话共享一个影子仓库要的是 **daemon 内 per-workspace 互斥**（ADR-0006 已写明），不是文件锁。（`docs/glossary.md` 的 "lease" 条带同一句旧口径「随 M2 检查点一起」，另行更正。）

## 开放问题

见设计文档末尾 4 条（daemonize 方式、空闲策略、检查点预算核算频率、token 轮换）。解决过程记录于此：

- **Phase 4 实测备注（2026-09-30）**：`pid_is_alive` 的「自述 pid = 陈旧文件」判定与 TestDaemon 的进程内 daemon 冲突（测试发布者就是测试进程）。生产语义保留不动，测试改走 `discover()`（不做存活过滤）+ `attach_to()`（跳过发现的直接握手接缝）。
- **Phase 4 实测备注（2026-09-30）**：单进程内的两次 `acquire_instance`：fs2 走 flock(LOCK_EX|LOCK_NB)，同进程异 fd 同样冲突（`a_second_start` 集成测试钉住）。
- **2026-10-07 对账**：第 3 条（检查点预算核算频率）仍属 M2，落 **Phase 1**、即决策点 **D9**（超预算时 GC 最旧 vs 拒写 + 核算频率；数据来自 §5 新增的 `checkpoints` 行）。同条的锁粒度半问已有答案——**daemon 内 per-workspace 互斥**（ADR-0006），不是跨进程文件锁（见待办的顺延项）。另：第 1/2/4 条在 M1 已定案（D1 = CLI spawn 分离进程、D2 = 无订阅者跑完为止、D4 = 每次启动换 boot token，见下 2026-10-01 Phase 3 条目），但 design/daemon.md 的对应三条仍写成「M1 定」的未决形态，尚未回填。

## 变更日志

### 2026-10-07 · M2 Phase 0 落地：prompt 注入、覆盖目录接线、死依赖边删除

「机制建好了却没接线」的三处缺陷，本方向占两处（第三处是 kernel 的审批答复校验，见 worklog/kernel.md）。

**1. system prompt 现在真的进请求。** 链路是 `SessionManager::assemble` 渲染一次 → `RuntimeParts.prompt` → `SessionRuntime.prompt`（冻结）→ `StoreHistory::view()` 前置一条 `Role::System` 消息。`ChatOptions` 没加 system 字段，**kernel 与 llm 一行未改**（`Message::system` 从零消费者变成这一个，`translate.rs:134` 早就映射了 `Role::System`）。`SessionRuntime::spawn` 的八个位置参数换成 `RuntimeParts` 命名字段——多出来的那个参数撞上 clippy 的 `too_many_arguments`（`-D warnings` 下会红），而六个 `Arc`/bool 位置参数本来就容易调错顺序，换命名字段是两头都对的做法。

**2. `override_dir` 不再是 `None`。** 新增 `prompt::prompts_dir(config_home, home)`（纯函数，与 `config::LoadPaths::detect` 同一套 XDG/HOME 规则）与 `prompt::default_prompts_dir()`（读环境），`entry.rs` 传给 `SessionManager`（构造函数多一个参数，六处调用点；**测试一律传 `None`**，不继承跑测试那台机器的配置，与 `LayeredConfig` 用注入层同理）。`core.rs::render_prompt` 不再自己拼 `Environment`，改调 `SessionManager::render_prompt`——透明性 API 与装配因此走同一条路，两边不可能漂移；顺带删掉 `let _ = model;` 那个死绑定，以及随之失去唯一消费者的 `pub(crate) use crate::clock::humantime_date`。

**3. `prompt/render` 对活着的 runtime 返回冻结的那一份**（`SessionManager::rendered_prompt`；agent 已停的 runtime 视为不存在，因为下一个 prompt 会重组装并重渲染）。这不是锦上添花：D15 把不变量 2 里 system prompt 的那一半交给这个方法钉，它若返回重新渲染的结果，「模型看到了什么」在日期翻页或覆盖文件被改之后就会说谎。

**实测推翻了 D15 的理由①，并暴露一个真缺陷。** 规划时写的「模式切换与 config 变更本来就 bump generation 并重组装」不成立：`session/set_config`（CLI 的 `/model` `/effort` 走的就是它）只改库里的行并发 `SessionUpdated`，`config/set` 只改 `LayeredConfig` 本身，而全仓库唯一的卸载路径是空闲清扫（`sweep_after` → `unload`）。provider adapter、`ChatOptions` 与 prompt 都在 `assemble` 里绑死，所以三者一律**下次装配才生效**——用户敲完 `/model x`，下一轮请求仍然发给旧模型，且没有任何提示。prompt 冻结与之一致、不是它引入的问题，但这条记为 design/daemon.md **开放问题 5**：Phase 3 的 `session/set_mode` 必须先回答它（换模式要换工具表、审批策略与 prompt 变体，非重组装不可），而解法不能是「`set_config` 里调 `unload`」——`unload` 对被 watch 的会话直接拒绝，改配置的恰恰是附着中的前端。

**死依赖边删除**：daemon → `hatchery-acp`（源码零引用，crate 本体只有文档注释）从 Cargo.toml 去掉，Cargo.lock 同步少一行，`cargo xtask layering` 仍全绿。

**e2e 子进程的世界现在真的由测试拥有**：`hatchery-tests/tests/subprocess.rs` 的 `spawn` helper 给子进程设 `XDG_CONFIG_HOME=<tempdir>/config`。注入 prompt 之后这一步是必须的——`e2e_daemon` 的文档注释声称「Nothing is read from the user's config」，而 daemon 现在会从环境推导出覆盖目录，不设就等于让 e2e 读宿主机上开发者的 `~/.config/hatchery/prompts`。新增的 `a_prompt_override_in_the_standard_location_reaches_the_request` 就建在这个 helper 上：它写一个 `identity.md` 覆盖与一个 `safety_gate.md` 覆盖尝试，断言前者经**真生产路径**进了请求体的 system 消息（且 `{{platform}}` 插值了）、后者被拒，并且 `prompt/render` 回报的 source 分别是 `user:prompts/identity.md` 与 `builtin`、`text` 与请求体里的 system 文本逐字节相同。做过变异验证：把 `entry.rs` 的 `default_prompts_dir()` 临时换成 `None`，该测试立刻红，另一条子进程测试仍绿。

`./scripts/ci.sh` 全绿（含本日新增的 `invariants` 步）。

### 2026-10-07 · M2 重新规划对账

一次全仓库勘察重写了 roadmap 的 M2 段（Phase 0–8 + 决策点 D8–D18 + 顺延表），本文件按它重新对账：待办重挂阶段、补上遗漏项、更正「当前状态」里三处比实际宽的口径（见该节）。本方向最重要的发现与两条顺延裁决记在这里。

**发现：prompt 管线从未到达模型（载荷性缺陷，归 M2 Phase 0，不作为 M1 阻塞项）。** M1 自述「prompt 管线落地」，实际落地的只有**装配 + 透明性**——四条实证：① `render_chat`（prompt.rs:57）的**唯一非测试调用方**是 `DaemonCore::render_prompt`（core.rs:227），它服务 `prompt/render` 协议方法，另外四处调用是 prompt.rs 自己的单测（:182/:190/:214/:234）；② `ChatOptions`（kernel/src/message.rs:257-279）**没有 system prompt 字段**，只有 `model / reasoning_effort / temperature / max_output_tokens / tool_defs / extra`；③ `StoreHistory::view()`（runtime.rs:64）只造 user/assistant/tool_result，从不产 system 消息，而 `checkpoints_are_not_provider_visible` 在 runtime.rs:618 **主动断言** `view.messages` 里没有 `Role::System`——也就是说这不是漏接，是被测试钉住的现状；④ 旁证：`render_prompt` 取出会话的 model 之后 `let _ = model;` 丢掉（core.rs:226），因为它渲染出来的东西不与任何一次请求相关。下游其实已就绪（`Message::system` 在 message.rs:56，`translate.rs:134` 已映射 `Role::System => WireMessage::system(text)`），缺的只是 daemon 这一侧把 system 消息前置进 `view()`。注入方式与时机由 **D15** 定（两条建议见待办里的 Phase 0 条目），e2e 的 `invariant_minimal_chat_replays_reasoning_byte_exact`（tests/scenario1.rs:14）期望请求体必须同步更新。

**顺延裁决一：hub coalescing + replay window 由 M2 改挂 M3。** 理由是 coalescing 治不了 M2 的病：`ServerEvent::is_coalescable`（protocol/src/event.rs:195）只覆盖 `text_delta`/`reasoning_delta`，而 M2 新增的事件量主要来自 `ToolCallProgress`，**它不可合并**；replay window 则已被 `session/load` + `replay_from` 取代且有 e2e 覆盖，再建一套缓冲是重复机制。M2 只保留一件测量任务：跑一次 Code 会话、把事件量记进本文件——这正是 hub.rs:4-6 当初写下的意思（"the measurements this produces decide the M2 strategy"），测量先于策略。

**顺延裁决二：`SessionLease` 跨进程文件锁改挂「多 daemon 形态出现时」，不挂 M2。** 三层理由：① `--embedded` 全仓库无实现，CLI 唯一路径是 attach-or-spawn，所以单实例 `daemon.lock`（fs2，discover.rs:123）已经挡住了两个 daemon 同时存在；② 单 daemon 内「一会话一 turn」由 per-session turn 闸门（manager.rs:303）+ 在途 CAS 标记（runtime.rs:385/391/400，HubSink 在 `TurnEnded` 时清除）承担，且已有 `invariant_session_lease_blocks_second_runtime` 与 `two_concurrent_prompts_yield_exactly_one_turn` 钉住——文件锁在这里没有新增任何保证；③ M2 真正会碰到的共享是**跨会话共用一个影子仓库**，那要的是 daemon 内的 per-workspace 互斥（ADR-0006 已写明），进程内互斥用文件锁表达是错的工具。

**顺带更正的两处本方向文档错**：design/daemon.md §3.1 的四个 profile 无任何实现（`hatchery-daemon` 里 `profile` 只命中 lib.rs:10 的文档注释），已改标为目标形态；`hatchery-acp` 是死依赖边（Cargo.toml:21 声明、源码零引用、crate 本体只有文档注释），M2 Phase 0 删边。

### 2026-10-01 · 评审⑤自查轮（并发正确性与传输层）

本轮自查在 daemon 里挖出的全是「单连接顺序测试看不见」的问题：并发 prompt 竞窗（turn 闸门 + runtime 在途 CAS 标记）、订阅计数活不过重组装（watchers 表移出 slots）、清扫的 check-then-act（卸载前锁内复核）、provider 缓存永不替换（注册表每次装配整表替换）、`store_error` 压平 SessionNotFound（改直通 `to_event_error()`）、恢复不关 turn 行（store 新增 `open_turns` 只读命令，恢复以失败形态关闭）、hub 不滤旧代且从不发 `GenerationBumped`（都补上）、commit 失败仍发布 ItemFinished（改为扣发）。传输层：读循环整块喂 decoder、decoder 错误路径携带完好帧（`Scan`）、每条回复后的双换行修掉、bind 失败也走逆序拆锁、spawn 轮询容忍 connect-before-bind、daemon.json 以 0600 直接创建。设计文档 daemon.md 已同步实际形态（惰性装配、无 SessionLease、coalescing/replay window 标注未实现）。计数见 worklog/testing.md 本日条目（daemon 75 项）。

### 2026-10-01 · M1 Phase 5 收口（评审⑤）

四处修正，全部先有测试再改（e2e 的失败暴露了前三处）：

1. **turn 在跑的判定换成状态机自己的话**（kernel 加 `AgentHandle::turn_running()`，watch 镜像 `TurnState::is_active`）：`SessionRuntime::is_busy` 原来的 `!handle.is_closed()` 测的是「agent 活着」——manager 的忙拒因此缺失（kernel 静默丢弃 mid-turn prompt，调用方却拿到 Ok），`sweep_idle` 的 `!is_busy` 谓词永远为假，**空闲卸载从未触发过**（D2 形同虚设）。
2. **`session/prompt` 忙拒**：turn 在途时返回 `TurnInProgress`（`core::turn_in_progress()` 终于有了调用方）；e2e 用 MockWire 延迟 5s 制造确定性在途 turn，第二 prompt 拒绝 + cancel 后原 turn 以 `Interrupted`（`TurnFinished`，不是 `TurnFailed`——中断是一种正常收束）收束。
3. **`session/load` 的 `replay_from` 语义修正**：协议文档说「该 item 之后」，实现却把它当 `rebuild_chain` 的 head（「到 head 为止」）直透——重连补差会拿到整段旧历史装作成功。现在在 manager 解析：活动分支上定位游标、只返回其后 item；游标不在活动分支上按 `InvalidRequest` 拒绝（宁可报错也不整段重放）。
4. **echo 判定与 adapter 同源**：manager 原来用 `CapabilityTable::builtin()` 决定推理是否回填历史，provider 却用 config 覆盖后的表决定请求——config 的 `echo_reasoning` 覆盖只对一半生效。`ProviderConfig::capability_table()` 成为唯一折算点，`provider_for` 一并返回 echo。

### 2026-10-01 · M1 Phase 4 补齐（评审④）

1. **生产入口 `entry::run_until`**：TLS 安装 → 日志初始化 → 配置 → **审计一次列全** → 实例锁 → store → 崩溃恢复 → 发布 → serve → 信号取消 → disposer 逆序（daemon.json 先清、socket 后删）→ 锁释放。测试走 `run_until(options, token)`：把信号换成 cancellation token，生产与测试同一条路径。
2. **传输泛化**：`serve_connection` 从 `UnixStream` 改为对读/写半的泛型，UDS 与 stdio 管道同一条连接循环；`serve_stdio` 是它的 stdin/stdout 实例。duplex 双通道测试钉住「管道上同一批帧、同一个核」。
3. **日志**：tracing-appender 日轮转（`hatchery.log.YYYY-MM-DD`）+ 启动时保留 14 天修剪（`YYYY-MM-DD` 字典序即时间序，形状不对的文件不动）；`RUST_LOG` 过滤，默认 info。
4. **sweep 调度**：60s 间隔 tick `sweep_idle`；D2（忙则不卸）判定仍在 sweep 的谓词里。
5. **doctor**：环境检查（provider/env key/目录可写，报告而非拒绝）+ `probe_provider`——真实 `LlmProvider` 轮次（含重试、能力表、翻译），流上实测 reasoning 字符数、finish reason、usage、RateLimited 次数；30s 上限。
6. **测试留痕**：daemon crate 58 项（entry 集成 3 项：UDS 全生命周期 + 单实例拒绝 + 审计点名缺失 env key；stdio 1 项）。

### 2026-10-01 · M1 Phase 3 落地（评审③）

**四个 M1 决策点在此定案（D1/D2/D4 + 传输帧形）：**

1. **D1 daemonize**：CLI spawn 分离进程（unix `setsid` / Windows `DETACHED_PROCESS`），systemd 用户直接跑前台 `hatchery daemon run`。双 fork 的复杂度与 sd_notify 的依赖都不值得——握手代码就在 CLI 里，轮询 socket 出现即完成 attach。实现随 Phase 4 的 `hatchery daemon start`。
2. **D2 无订阅者在途 turn**：跑完为止。结果无论如何落库，杀掉一个没人看着的 turn 是烧掉已付费的推理。
3. **D4 boot token**：每次 daemon 启动换新 token，`daemon.json` 0600 write-then-rename；客户端 attach 时现读。死 pid 判定 best-effort 无 unsafe（Linux 走 `/proc`，其余平台保守视为活、由 hello 握手兜底——workspace `unsafe_code = deny`，为 kill(0) 开洞不值得）。
4. **事件帧形（实测发现的协议面修正）**：裸 `SessionEvent` 不是 JSON-RPC 帧（无 method/id，`classify` 判 `Unclassifiable`），client 端永远收不到。**事件现以 `session/event` notification 包装传输**（params = 信封），对分类器无特例。这条已写进 client.rs 与 server.rs 两端。

**订阅语义按协议文档实现**：`session/new`/`session/load` 成功即隐式订阅本连接（§4），server 从回复的 `session.id` 叶子提取会话——第一版把整个 Session 对象当 SessionId 反序列化，静默失败，靠「hub 直订 vs socket 断点」的二分测试定位。

**store 补了 `bump_generation`**：M0 的 `SessionPatch` 有意不含 generation（runtime 私有），但 store 也没有任何落库路径——manager 组装 runtime 前调用它，行与事件信封才一致（不变量 1 的「generation 落库」）。加性 trait 方法，无迁移。

**其他落点**：配置分层（per-key origin、坏 key 逐条忽略、Runtime 层恒存）；prompt 管线四 section + `{{var}}` 未知占位保留可见 + safety_gate 不可覆盖（override 被拒并 warning，测试断言不泄漏）；`prompt/render` 带 per-section 来源；history 重建（reasoning 按能力表 echo/drop、工具配对用 `provider_call_id`、legacy 行合成 `call-synth-N`）；audit 一次列全；`Disposers::run_reverse` 逆序且 panic 容错。

### （此前无条目）

### 2026-09-28
- 初稿。模型 = codex app-server（传输与协议纪律）+ atomcode Live Hub/租约/代际（会话生命周期纪律）的合成。
- ADR-0009 落地：daemon.md 新增 §3.1 profile 化装配（local/headless/acp-stdio/acp-standalone 四捆绑）+ 启动 fail-loud 审计（dsh `auditStartupEntries` 语义）；runtime 卸载/关闭明确 disposer 逆序规则；注册句柄模式（Handle{dispose, replace}）用于运行中换 provider/MCP 连接。
