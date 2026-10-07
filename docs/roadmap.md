# 路线图（M0–M5）

> 每个里程碑的完成定义（DoD）都包含：`cargo test`/`clippy` 全绿 + 列出的端到端验证 + worklog 更新。测试分层、CI 门禁与「不变量 → 测试」映射见 [design/testing.md](design/testing.md)；各里程碑的测试交付物已列入其范围与 [worklog/testing.md](worklog/testing.md)。当前进度：**M0 完成**；**M1 代码完成**（2026-10-01，Phase 1–5 全部落地：llm adapter + 能力表、capabilities/tools、daemon 全栈、cli TUI/exec、e2e 与不变量收口，`./scripts/ci.sh` 本地全绿），手动 live 验收进行中（已产出 2026-10-04/10-05 三轮 TUI 修正：widget 层与双主题、滚动条与滚轮、词级换行与点击折叠），[worklog/testing.md](worklog/testing.md) 的六项清单尚未勾选——**M2 Phase 0 负责收口 M1**。
>
> **M2 已重新规划（2026-10-07）**：原 M2 段基于 M0/M1 的自述写成，一次全仓库勘察发现若干与代码不符之处（详见 M2 节的「勘察更正」），阶段划分因此重写为 Phase 0–8。M2 的体量明显大于 M1（用户裁决保留全部 worklog 标注为 M2 的条目，含 Responses adapter 与多模态输入）；**Phase 4 结束时评估是否把余下部分拆成 M2b**。

## M0 — 地基（scaffold + 核心类型）

**范围**
- Cargo workspace 脚手架：12 个 workspace member（11 个 crate = 10 个产品 crate + dev-only 的 `hatchery-testkit`，外加 `xtask`；见 architecture.md §3）+ CI PR 门禁全套（testing.md §8）+ nextest 分组。**M1 又加了第 12 个 crate `hatchery-tests`（跨 crate e2e 与不变量套件的家，虚拟 manifest 不能有顶层 `tests/`），现为 13 个 member**——本行记的是 M0 交付时的形状。
- `hatchery-protocol`：Session/Turn/Item/事件/方法的完整类型定义 + JSON fixture 测试。**已完成**：9 种 ItemKind、13 种会话事件 + 1 种 daemon 事件、20 个方法与其参数/结果类型、JSON-RPC 帧（含增量解码器）、14 个错误码、版本协商；golden fixture 覆盖每个 ItemKind、每个事件、每个方法的参数与结果（覆盖率由测试机器检查）。
- `hatchery-kernel`：Turn 状态机 + trait 定义 + fake provider 单测。**已完成**：四个接缝、显式状态机（每次迁移发事件）、审批 id 往返、中断矩阵；每条路径由 `tests/turn_state_machine.rs` 的集成测试钉住（清单见 design/testing.md §3.2）。
- `hatchery-store`：schema v1 + writer actor + rebuild_chain + 分支操作（分叉/切换/级联删）+ 属性测试。**已完成**：迁移框架、单写者 actor、纯树遍历、JSONL 导出、属性测试对拍独立参考模型、kill -9 崩溃恢复。
- spike（各半天，结论写进对应 worklog）：**三个全部完成**——存储引擎 → turso 0.7.2（ADR-0010，门槛测试常驻 `crates/hatchery-store/tests/spike_engine.rs`，升级引擎必须重跑）；影子 Git 后端 → git2 vendored（ADR-0012，门槛测试常驻 `crates/hatchery-capabilities/tests/spike_shadow_git.rs`，升级 git2 必须重跑）；i18n → fluent（ADR-0011）。
- 本文档体系随代码入库。

**DoD**：`./scripts/ci.sh` 三平台全绿；store 的 kill -9 崩溃恢复测试通过；三个 spike 结论落档。 → **本地全绿**（`./scripts/ci.sh` 每一步：fmt / clippy / build / nextest `ci` 组 / doctest / fixture 确定性 / i18n 占位），kill -9 测试覆盖五种 StoreCmd + 恢复后仍可用，spike 结论在 ADR-0010/0011/0012。三平台 CI 待 M0b 代码 push 后确认。

**两处口径更正（2026-10-07 勘察）**：① 本行原写「nextest 默认组与 **invariants 组**」——实际 `scripts/ci.sh:134` 只调用 `--profile ci`，`--profile invariants` 在 ci.sh 与两个 workflow 里**从未被调用**，是死配置。不变量测试确实跑到了（`ci` 继承 `default`，过滤器只排除 live/slow/gui），但没有一个可单独报告或阻塞的门禁；且 testing.md §5 映射到不变量、却没有 `invariant_` 前缀的 5 条测试（含 `two_concurrent_prompts_yield_exactly_one_turn`）一旦真去跑那个 profile 就会被漏掉。M2 Phase 0 修。② `check_i18n`（ci.sh:121-124）是一条 `printf` 空操作（"i18n extraction check lands in M4 — nothing to verify yet"），不是检查，此前被算作通过的门禁步骤。

## M1 — 最小对话闭环（Chat 模式端到端）

**范围**
- `hatchery-llm`：ChatCompletions adapter（deepseek/qwen 两家真实探测录制 fixture）、effort 映射表 v1、reasoning 采集/回放。
- `hatchery-daemon`：UDS/stdio 监听、attach-or-spawn、单会话 runtime、live hub（无 coalescing 优化）、generation。
- `hatchery-cli`：TUI 最小版（消息流 + 输入 + reasoning 折叠 + `/effort` `/model` `/prompt`）+ headless exec。
- Chat 模式：只读工具 `read_file`/`glob`/`grep`（LocalFs 直读）。
- 配置分层（platform.md §1）+ prompt 管线 v1（identity + mode_variant + environment + safety_gate）。**交付范围更正（2026-10-07 勘察）**：这一项实际只交付了**透明性**——四节装配、`{{var}}` 插值、per-section 来源标注与 `prompt/render`／CLI `/prompt`；`render_chat` 的唯一非测试调用方是 `core.rs:227` 的 `prompt/render`，**装配结果从未进过任何一次模型请求**（`ChatOptions` 无 system 字段、`StoreHistory::view()` 不产 `Role::System`，且 `runtime.rs:618` 有一条测试主动断言消息里没有 system 角色）。注入模型随 **M2 Phase 0**。
- `hatchery doctor`。

**DoD**：真实 provider 端到端对话（流式 + reasoning 展示与回放命中验证）；关终端重开会话 resume；两前端同时 attach 扇出一致。
→ **代码侧全绿**（2026-10-01）：mock wire 的 e2e 场景 1/2 与不变量组在 `hatchery-tests`；第二 turn 请求体对已存 reasoning 做了逐字节断言（不变量 2）。**真实 provider 的手动验收清单**见 worklog/testing.md（M1 条目），通过后 M1 收口。

## M2 — Code 模式（工具、审批、回滚、编辑分叉）

> 2026-10-07 重新规划。原 M2 段按 M0/M1 的自述写成，一次全仓库勘察（capabilities/tools、kernel/store、daemon/cli/protocol/tests 三路）发现若干与代码不符之处；下面的「勘察更正」是重写的依据，逐条带实证。各方向的细目已对账进对应 [worklog/](worklog/README.md)。

### 勘察更正（对原规划与既有文档）

1. **分支三原语 M0 就交付了**，M2 不是从零做。`edit_fork`/`switch_branch`/`delete_branch`/`branch_tree` 全部实现且有专测 + 属性测试对拍独立 `ReferenceTree` + kill -9 探针（`store/src/actor.rs:744/771/787`）。M2 的实际增量是：daemon 路由、rewind 组合逻辑、`checkpoints` 与 `approval_rules` 的 API 层、`CheckpointStore`、`DaemonApproval`、`confirm` 二次确认语义。
2. **daemon 只服务 20 个方法里的 10 个**（`core.rs:26-37`），`core.rs:594/597` 还显式断言 `session/rewind` 与 `approval/respond` 不在 `SERVED_METHODS`（"is M2"，届时翻转）。未路由的除会话级四项外还有 `session/set_mode`、`session/delete`、`session/rename`、`store/export_jsonl`——**M2 收尾时协议方法面应全部接通**。
3. **`checkpoints` 与 `approval_rules` 两张表零 Rust 代码**：无 `StoreCmd` 变体、无 trait 方法、无 `sql.rs` 行转换、无一行测试写入。表在 v1 schema 里，**M2 不需要新迁移**，缺的只是 API 层。`approval_rules` 只有 `id/scope/matcher/decision/created_at`——无排序列、无 enabled 列、scope 是裸 TEXT、无 session 外键。
4. **rewind 不依赖 `checkpoints` 表定位 commit**：`ItemKind::Checkpoint { commit_id, kind }` 自己带着 commit id，且 `is_conversation()` 不含 Checkpoint（不进模型请求）。Code scope 的实现是「`rebuild_chain` → 定位 target_item → 向后扫第一个 Checkpoint item → 读 commit_id → restore」；pre-write 快照恰好等于 target_item 时刻的工作区状态，扫不到即 no-op。`checkpoints` 表（`item_id` 可空）因此只服务**跨会话的预算核算与 GC**，不是 rewind 的主索引——文档此前把它写得像主索引。
5. **协议带不出 diff**：全仓库无 `UnifiedDiff`/`DiffHunk`/`FileDiff` 类型（capabilities.md §2 草图的 `diff() -> UnifiedDiff` 没有返回类型）；`ApprovalRequest` 只有 `args_digest: String`，其文档明写「Not the raw JSON … 可以是 megabytes」，**装不下 diff 或完整命令**，而 M2 要 TUI diff 预览、M3 的 ACP 要 `ToolCallContent{content=diff}`。也没有 list/delete 审批规则的方法（一条误存的 `DenyAlways` 会永久废掉一个工具且无法撤销）。`PendingApproval`（method.rs:498）零 fixture、零消费者。
6. **`TerminalHandle` 撑不起 shell 工具**：只有 `wait() -> TerminalOutcome`（一次性全量输出）+ `kill()`，**无输出流、无 `release()`**，与 capabilities.md §1 草图的 `{ output stream, wait_for_exit, kill, release }` 不符。没有输出流就没有 `ToolCallProgress`。当前唯一实现是 `NoTerminal`，所以现在改 trait 最便宜。
7. **不变量 5 的测试在任何形态下都不存在**，而它正是 M2 DoD 的「硬门测试全绿」。testing.md §5 点名的 `invariant_project_config_cannot_disable_hard_gates` 全仓库零命中；唯一沾边的是 `RiskLevel::is_hard_gate()` 这个**值类型谓词**的两条单测，生产代码无人调用。另：`is_hard_gate()` 只认 `WritesOutside`，而 design §5 的硬门含**工作区内**的 `.env*` 与 `.git/hooks`——不必动枚举（新增枚举值属 major bump），`once_only()` 已经用「不提供 always 选项」表达了不可记忆，路径门只要能为工作区内敏感路径强制它。
8. **不变量 4 的编译期门禁有洞**：`clippy.toml` 只禁 `std::fs::*` 与 `std::process::Command`，**没禁 `tokio::fs::*` 与 `tokio::process::*`**——而 tools crate 依赖 tokio、`LocalFs` 自己就用 tokio::fs。工具里写一句 `tokio::fs::write` 就绕过整条纪律。（另漏 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`。）
9. **模式装配不存在**：capabilities.md §4 的 `assemble(mode, backends)` 与 `builtin(spec, &backends)` 全仓库零命中，`ToolPolicy` 亦无；实际装配是 `manager.rs:612` 硬编码的 `chat_tools()` 循环，且 `SessionManager::assemble` **从不读 `session.mode`**——code 会话今天拿到的是与 chat 完全相同的三个只读工具 + `NoTerminal`。daemon.md §3.1 的四个 profile 也一个都不存在（无 `Profile` 类型）。
10. **headless exec 在 Code 模式下不可用**：`exec.rs:187` 硬编码 `SessionModeId::chat()`，`ExecArgs` 无 `--mode`、无任何审批策略标志。Code 会话的第一个审批会让它**永久挂住**（直到 120s `CALL_TIMEOUT`）。frontends.md §2.3 未规定。
11. **TUI 缺弹层与 diff 的地基**：`push_event` 末臂 `_ => {}` 静默丢弃 `ItemStarted`/`ModeSwitched`/`GenerationBumped`；`ApprovalRequested` 只改状态栏文字、把 `request_id` 与整个请求丢掉（tui/mod.rs:262-265）；`layout()` 返回硬编码 `[Rect; 6]`、`draw()` 无 z-order/`Clear` 通道；`key_input` 无模态（`1`-`4` 会被 `Char(c)` 吞进输入框）；`Esc` 硬接 `session/cancel`，而 `chat.rs:273` 明写「M2 的弹层拥有 Esc」，交接未实现。`actions()` 是 fire-and-forget（`submit_line` 丢弃回复），而 `branch/list` 要渲染节点表、`rewind` 要展示 `RewindReport`。可复用的是那套「借用 `&Model` 的纯函数 Widget + 自由函数报 height」惯例、`composer.rs` 的圆角框、以及三次重写才对的 `wrap_lines`（diff 正文正好要用）。
12. **testkit 缺四件 M2 前置**：`MemoryTerminal`（testing.md §2 声称 M1–M2 交付，不存在）；`MemoryFs` 写路径（trait 一加方法它就编译不过——是 forcing function，但也是工作量）；**协议级审批应答器**（现有 `answer_approvals` 拿 `AgentHandle`、绕过协议，测不到 `approval/respond`）；`hatchery-tests` 能读到的 toolcall SSE fixture（它**连 `tests/fixtures/` 目录都没有**，唯一的 SSE 是内联字符串且不含 `tool_calls`）。另 `TempWorkspace` 缺 testing.md §2 声称的 git init 与文件树 DSL，而 CheckpointStore 的不变量 6 测试需要「脏用户仓库」。
13. **`hatchery-tools` 没有覆盖率地板**（`xtask/src/coverage.rs:17-25` 只闸七个 crate），而 M2 的四个新工具全落在那里。
14. **两个 README 与代码不符**：capabilities 宣称有影子 Git 检查点与注册句柄；tools 宣称有七个工具（实际三个）。
15. **`hatchery-acp` 是死依赖边**：daemon 的 Cargo.toml 声明了它，源码零引用，crate 本体 13 行文档注释；`cargo xtask layering` 把它算作 build edge。
16. 文档小错：fixture 数「62」（protocol.md §6、testing.md §3.1、worklog/protocol.md）实际 **79**——但项目在 2026-09-30 已裁决「文档里的计数一律去掉」，所以是**删数字**不是改数字；ADR-0006 写 `RewindScope::ConversationAndCode`，协议实际是 `Both`（ADR 一经 accepted 不修改，以协议为准并在 worklog 留痕）；storage.md §1 的 trait 代码块列 17 个方法（实际 19，缺 `bump_generation`/`open_turns`）；kernel.md §2 的 `TurnInput(Content)` 草图过时（实际 `{turn, content}`）；storage.md 说只读连接池排 M1、worklog 已改 M3；testing.md §1/§3.8 仍写 insta（`d057a99` 已移除）。

### 范围（Phase 0–8；每阶段 `./scripts/ci.sh` 全绿 + worklog 更新 + 停下评审）

**Phase 0 — M1 收口 + 门禁诚实化 + prompt 注入**
把 system prompt 真正接进 turn（**D15**：建议在 runtime 装配时渲染一次并冻结整个 runtime 生命周期——模式切换与 config 变更本来就 bump generation 重组装；每轮重渲染会让 environment 节的日期/cwd 破坏前缀稳定性，而那正是项目为 KV cache 反复强调的东西）；明确**不变量 2 的边界**（建议：只管分支历史，system prompt 是可复现的派生态，由 `prompt/render` 的 golden 单独钉）并更新 `invariant_minimal_chat_replays_reasoning_byte_exact` 的期望请求体。**同一类缺陷的第二处**：`render_chat` 的 `override_dir` 参数在生产路径上恒为 `None`（`core.rs:227` 是全仓库唯一的生产调用点，没有任何代码构造 `~/.config/hatchery/prompts`），所以逐 section 的用户覆盖机制**实现了、单测覆盖了、但用户永远触发不到**——Phase 0 一并接线。`ci.sh` 加真正的 `invariants` 步骤 + testing.md 映射到不变量却无前缀的 5 条测试改名。**同类问题的第三处**：硬门唯一承重的机制——kernel 拒绝「未被提供的审批答复」（`agent.rs:761`）——**没有测试钉住**，而且 `ScriptedToolHost::requiring_approval` 恒用 `ApprovalRequest::new`（其 `options` 恒为 `ApprovalOption::ALL`），所以 fake 在 API 层面**造不出**收窄选项的请求：那条分支是结构性不可测。2026-09-30 的修复因此违反了项目纪律 #3（Bug 修复必附回归测试）。Phase 0 给 testkit 加一个能接收现成 `ApprovalRequest` 的构造器 + 补这条 kernel 用例；Phase 2 的路径硬门与 `invariant_project_config_cannot_disable_hard_gates` 建在它之上。`clippy.toml` 补 tokio 与漏掉的 `std::fs` 项（更正 8）；覆盖率表加 `hatchery-tools`（更正 13）。删掉 daemon 对 `hatchery-acp` 的死依赖边（更正 15）；`check_i18n` 改为诚实的 skip 而非门禁步骤。文档对账（更正 14/16）。勾 M1 live 验收清单、M1 关闭。

**Phase 1 — 检查点与写路径**（capabilities + store + protocol）→ 评审⑥
`CheckpointStore` 从 spike 的 `Sandbox` 转正：open 配方（`init_opts` + 手写 `core.worktree`/`core.bare` + `set_workdir(.., false)`）、`harden()` 钉扎、每次打开重放 ignore 规则、`snapshot`/`restore`/`diff`/`gc`、restore 前自动 snapshot、per-workspace 互斥、启动断言影子 git-dir ≠ 用户任何 `.git`。**协议加 diff 载荷类型**（更正 5，必须在此——`diff()` 需要返回类型，且 TUI 与 M4 GUI 共用）。**D13** 落定并按此实现：`ToolCtx` 加检查点收集器（`LocalFs` 写前 push）→ `ToolInvocation` 带出 `Vec<Checkpoint>` → **kernel 在 ToolResult item 之前追加 Checkpoint item**（它已在造 ToolCall/ToolResult item，用同一套机器，顺序天然正确；链变成 `… → ToolCall → Checkpoint → ToolResult`，而工具结果靠 `ToolResult.call` 配对而非父子关系，不受影响）→ daemon 的 HubSink 在 Checkpoint item 落库后补写 `checkpoints` 行（失败只记日志，item 里已有 commit_id 可回退）。`FsBackend` 加写原语（`write_text_file` + `create_dir`/`remove`，purge 与 write_file 都要）；`LocalFs` 写路径写前打检查点；`MemoryFs` 同步。store 的 `checkpoints` 表 API（记录 + 按 workspace 列举，供预算/GC）。**D9** 预算熔断行为（GC 最旧 vs 拒写）+ 孤儿影子仓库 GC（storage 开放问题 3）。`TempWorkspace` 补 git init + 树 DSL；不变量 6 的测试从 `Sandbox` 迁到真 `CheckpointStore`。**D10 的 PTY spike 建议在本阶段并行做**（半天，产 ADR），实现留 Phase 4。

**Phase 2 — write/edit 工具 + 审批管线 + 硬门**（tools + capabilities + daemon）→ 评审⑦
`write_file` / `edit`（old/new 精确替换，capabilities 开放问题 4 已裁决）。**D14** 审批预览载荷：建议给 `ApprovalRequest` 加可选结构化 preview（`UnifiedDiff | Command{argv,cwd} | Excerpt`）而非新开 `approval/details` 方法——ACP 的 `request_permission` 要同一份内容，放请求里一次到位。`DaemonApproval`：pending 注册表（request_id → runtime）+ `approval/respond` 路由到 `AgentCommand::decide` + fail-closed 超时（住在 gate，不在 kernel）。**D8** 规则求值语义（scope/matcher/decision 文法、求值顺序、默认策略）+ store 的规则读写 API + `AllowAlways`/`DenyAlways` 联动（`is_remembered()` 目前零消费者）+ 规则的 list/delete 协议方法（更正 5）。路径硬门（`~/.ssh`、`~/.config/hatchery`、`.git/hooks`、`.env*`）按更正 7 的方式表达。**`invariant_project_config_cannot_disable_hard_gates`**（DoD 项）+ `config.rs:154` 那个空的 `STRICT_KEYS` 接线。模式装配：`ToolPolicy` + `assemble(mode, backends)` + `builtin(spec, &backends)`（更正 9）。prompt：`mode-code.md` + `tool_discipline`（按 Turn Tool Snapshot 生成）+ `project_context`（AGENTS.md 向上发现，决策已定实现排 M2）。testkit：协议级审批应答器 + fake `ApprovalGate`；契约测试套件（`FsBackend`/`TerminalBackend`/`ApprovalGate`）——**这就是原范围那句「ACP fs/terminal 委派所需的后端绑定机制…接缝在真实工具下验证」的诚实交付形态**：没有 ACP 协议时唯一可机器验证的就是「换绑定不换工具」的契约套件 + daemon 侧一个按会话来源选后端的 `Backends` 装配点；绑定表的 ACP 行留作有文档的接缝，真验证在 M3。

**Phase 3 — daemon 服务会话级方法 + rewind 三 scope**（daemon + store）→ 评审⑧
接通 10 个未路由方法（更正 2），翻转 `core.rs:594/597` 那两条断言。rewind 三 scope；`Both` 的顺序是**先 restore 代码、成功再移 head**（restore 失败绝不能已经把历史移走）；restore 前的安全快照记进 `checkpoints` 表、`item_id = NULL`、**不建 item**（它是 undo-of-undo，不属于对话历史；该列可空正是为此留的）。模式切换语义（ADR-0005 的「进行中的写工具调用先完成或取消」）+ `hello` 广告 code 模式 + `ModeSwitched` 真正发出。`SessionLoadResult.pending_approvals`（给 `PendingApproval` 消费者，解决重连时审批弹层重画）。**历史移动后的前端重建约定：不加新事件**（新增事件 type 属 major bump）——`SessionUpdated.state.active_branch_head` 已在广播里，前端发现它不是自己已投影 head 的后继就 `session/load` 重建；发起方本来就能在自己的回复里拿到新 Session。

**Phase 4 — PTY / shell / web_fetch / 大输出**（capabilities + tools + kernel）→ 评审⑨
`TerminalBackend`/`TerminalHandle` 加输出流 + `release`（更正 6）；`LocalPty`（**D10** 按 spike 结论选型；证据：codex 用 `portable-pty = "0.9.0"`，且 Windows 侧额外挂 `winapi` 的 jobapi2/Job Object 才能保证杀进程不留孤儿——那正是 testing.md 要的「cancel 后无孤儿进程（`kill -0` 断言）」在 windows-gnu CI 上的坑）+ 环形缓冲 + 超时杀 + 会话注册表；`MemoryTerminal`。`shell`（含危险命令模式表）+ `web_fetch`（**D17** HTTP client 与 HTML→MD 选型；注意 `wiremock` 是 testkit 专属、明确「never of a product crate」，web_fetch 需要自己的 client 依赖）。**D12** spill 阈值与落盘位置 + **凭据脱敏**：脱敏必须发生在**接缝处（工具输出离开 backend 时）**，不是 design §5 写的「脱敏入库」——入库内容若与模型实际看到的不同，不变量 2 的「重建 == 实际请求体」就被破坏；参数侧不改存储（同样会破坏不变量 2），改为「检出凭据 → 触发审批/告警」，真正的防线是让密钥在读取环节就进不了模型视野。上下文 token 预算 v1（kernel 待办，2026-10-03 由 M1 改标 M2）——与 spill 是同一个问题的两半，工具大输出出现后才是刚需。**本阶段结束时评估是否把 Phase 5–8 拆成 M2b。**

**Phase 5 — llm：Responses adapter + 多模态 image 输入**（llm + cli）→ 评审⑩
Responses adapter（`wire = "responses"` 目前是 fatal 拒绝，诚实）；随之关闭 llm 开放问题 1（`encrypted_content`/签名块的统一存储表示——ChatCompletions 无线上字段，D3 把风险整体移交给了这里）。多模态 image 输入：协议侧 `ContentPart::Image` 已在，缺的是 llm 侧到 wire 的翻译 + CLI 侧的图片输入路径（frontends 开放问题 3）；两半要一起做，否则图片进得了库却发不出去。本阶段与 Phase 1–4 无依赖，可并行或任意插位。

**Phase 6 — TUI 与 headless**（cli）→ 评审⑪
**弹层架构裁决先行**（更正 11）：模态需要 `layout()` 的第七个约束或一次后置的居中 `Clear` pass、`Model` 上的 pending 请求字段与选项游标、`key_input` 的模态分支、以及 `Esc` 的所有权交接。注意这与「`Model` 是事件流的投影、无本地状态机」的纪律冲突——`/branch` `/rewind` 本质是交互式选择器，必须显式裁决而不是让它长出第二套状态机（那正是 ADR-0001 要规避的双接线）。审批弹层（preview + 快捷键 1-4）、diff 视图（**D11**：建议 `similar` 算 hunk + 主题语义色渲染，**不引 syntect**——D5 已经因为依赖耦合否掉过 termimad，而 diff 视图要的是 +/- 着色不是语法着色；syntect 留给代码块另议）。`/rewind` `/branch(list|switch|delete)` `/edit` `/approval` `/export` `/clear`；`actions()` 的 fire-and-forget 形状要改（更正 11）。**D16** exec 的 Code 模式：`--mode` + 审批策略标志（或明确「exec 拒绝 Code 会话」）。`hatchery sessions {list|resume|export|delete}` 子命令族。

**Phase 7 — e2e、契约套件与工具链**（tests + testkit + xtask）→ 评审⑫
e2e 场景 3–6（Chat→Code 切模式、编辑分叉重演 + 分支删除、写坏文件 → rewind 三 scope、审批全链路含 fail-closed 超时与 AllowAlways 生效）；前置是更正 12 的 toolcall fixture 可达性。契约套件补齐 `LlmProvider`/`SessionStore`。`disallowed_methods` 的 compile-fail 探针（trybuild 类）。fuzz targets 上 nightly（SSE 帧解析、JSON-RPC 帧解码、item payload serde、**LocalFs 逃逸面**——M2 让它变大了）。criterion 基线入库 + nightly 对比。cargo-mutants 试点（store/kernel）。`hatchery config schema` 导出 JSON Schema。补 `bump_generation` 在 store crate 内的直接测试（现仅 daemon 侧调用方覆盖）与 `walk.rs` 那个洞（注释声称不可读子目录由真盘测试钉住，`tests/assembly.rs` 里其实没有 0 权限目录的测试）。

**Phase 8 — 手动验收与收口**（M2 关闭）
testing.md §9 的 M2 清单：真工作区改坏 → rewind、审批规则持久化、非 git 目录检查点。各设计文档按实现重写（capabilities §1/§2/§3/§5、daemon §3.1、frontends §2.2/§2.3、kernel §5/§6、storage §1、testing §1/§2/§3.8）；两个 README 更正；worklog 全部勾框。

### 决策点（各 Phase 内定稿并写回文档）

| | 决策 | 落在 |
|---|---|---|
| D8 | 审批规则求值语义（scope/matcher/decision 文法、求值顺序、默认策略） | Phase 2 |
| D9 | 检查点超预算行为（GC 最旧 vs 拒写；spike 数据已备） | Phase 1 |
| D10 | PTY 库选型（**spike 提前到 Phase 1 并行**，三平台孤儿进程/取消/输出流实测） | Phase 1 spike → Phase 4 实现 |
| D11 | TUI diff 渲染方案（`similar` + 主题语义色 vs syntect） | Phase 6 |
| D12 | spill 阈值与落盘位置 | Phase 4 |
| D13 | 检查点如何成为 item、rewind 如何定位 commit | Phase 1 |
| D14 | 审批预览载荷形状（`ApprovalRequest` 加字段 vs 单独方法） | Phase 2 |
| D15 | system prompt 的渲染时机与不变量 2 的边界 | Phase 0 |
| D16 | headless exec 在 Code 模式下的审批策略 | Phase 6 |
| D17 | web_fetch 的 HTTP client 与 HTML→Markdown 选型 | Phase 4 |
| D18 | TUI 弹层/选择器与「Model 只是事件投影」纪律的和解方式 | Phase 6 |

kernel 的两个「M2 决定」开放问题在本里程碑内**裁决为不做**（决定本身即交付）：一轮多 tool call 并行——7 处结构阻碍（item 链单指针 `self.tail`、审批状态单槽 `AwaitingApproval{request_id}`、命令通道单消费者、进度通道独占，且 `two_calls_in_one_round_are_each_approved_separately` 与两条确定性测试会被打破），收益是延迟、代价是重做提交序与确定性纪律；`max_tool_retries`——全仓库只有设计文档一处提及，「可重试失败」还没有第一个消费者，正是 ADR-0009 反预拆分刹车的适用场景。

### 顺延（2026-10-07 用户裁决）

| 项 | 原挂 | 顺延到 | 理由 |
|---|---|---|---|
| hub coalescing + replay window | worklog/daemon (M2) | **M3** | `is_coalescable` 只含 text/reasoning delta，而 M2 新增的事件量主要来自 `ToolCallProgress`（**不可合并**）——coalescing 治不了 M2 的病；replay window 已被 `session/load` + `replay_from` 取代且有 e2e 覆盖。M2 只产出一次 Code 会话的事件量测量并记档（`hub.rs:4` 的原意） |
| `SessionLease` 跨进程文件锁 | worklog/daemon (M2)、glossary | **多 daemon 形态出现时** | `--embedded` 全仓库无实现，CLI 只有 attach-or-spawn，单实例 `daemon.lock` 已挡跨进程双 runtime；会话内由 turn 闸门 + 在途 CAS 标记承担且有不变量测试。跨会话共享影子仓库要的是 **daemon 内 per-workspace 互斥**（ADR-0006 已写明），不是文件锁 |
| store 只读连接池 | worklog/storage | M3（已于 2026-10-03 改标） | 读仍全部串行经 writer actor，正确优先 |
| `RegistrationHandle`（dispose/replace） | capabilities.md §1 | M5（已有记录） | 消费者是跨会话存活、需原地换 MCP 工具的注册表 |
| shell 会话复用（unified exec） | capabilities 开放问题 2 | M5（已有记录） | — |

### DoD 映射

| DoD | 落在 |
|---|---|
| Code 会话改坏文件后 rewind 恢复 | Phase 1（CheckpointStore + 写前打点 + D13）→ Phase 3（rewind 三 scope）→ Phase 7（e2e 场景 5）→ Phase 8（手动验收） |
| 编辑历史消息分叉重演 | store 原语已备 → Phase 3（`edit_item` + `rewind_scope` 联动）→ Phase 6（`/edit`）→ Phase 7（e2e 场景 4） |
| 审批规则持久化生效 | Phase 2（DaemonApproval + rules API + D8 + `approval/respond` 路由）→ Phase 7（e2e 场景 6 含 AllowAlways 与 fail-closed 超时） |
| 硬门测试全绿 | Phase 0（让 `invariants` profile 真被调用 + 前缀对账）→ Phase 2（路径门 + `STRICT_KEYS` + `invariant_project_config_cannot_disable_hard_gates`）→ Phase 7 |

### 主要风险

1. **Windows CI（MSYS2 ucrt64 + windows-gnu）上的 PTY**：三平台门禁里唯一有真实分歧的新依赖，孤儿进程语义在 Windows 要靠 Job Object。缓解 = D10 spike 提前到 Phase 1 并行做，实测三平台后再实现。
2. **体量**：M2 现在明显宽于 M1（用户裁决保留全部 worklog 标注 M2 的条目）。Phase 4 结束时评估拆分 M2b。
3. **协议加字段会翻动 fixture 确定性门禁**：项目已有先例（worklog 记过两次「有意修改导致 determinism 步骤保持红，提交即恢复」）。Phase 1/2/5 的协议改动要预留这个摩擦。
4. **Phase 2 体量**：两个工具 + 审批全链路 + 规则持久化 + 硬门 + 模式装配 + 三个 prompt 节 + 协议加字段。若评审⑦前发现太宽，最自然的切法是把「prompt 三节 + 模式装配」独立成 Phase 2b。

## M3 — ACP server + client

**范围**
- `hatchery-acp`：server 全量（design/acp.md §1，含 fs/terminal 委派、审批映射、session modes、load replay、config options）；client + `subagent` 工具（§2）。
- `hatchery acp` 子命令（attach 与 standalone 两态）。
- **从 M2 顺延（2026-10-07 裁决）**：hub coalescing（16ms 窗）+ replay window（M2 先产出一次 Code 会话的事件量测量作为策略依据）；store 只读连接池与 `spawn_blocking` 读路径（2026-10-03 已由 M1 改标 M3）。
- 兑现 M2 留下的接缝：绑定表的 ACP 行（`AcpClientFs`/`AcpClientTerminal`/`AcpPermission`）接上 M2 的契约测试套件——「换绑定不换工具」在这一里程碑才真正被验证。

**DoD**：Zed 真机全链路（编辑/终端/审批/thought chunk）；自举测试（hatchery 把 hatchery 当 subagent）；能力降级矩阵测试全绿。

## M4 — GTK 桌面端

**范围**
- `hatchery-gui`：frontends.md §3 全部（会话列表、消息流、审批弹层、设置窗、分支时间线、rewind 面板、prompt 查看器）。
- i18n 落地：po 工具链、zh/en 两语、RTL 冒烟。
- flatpak 打包。

**DoD**：GUI 完成一次完整 Code 会话（含审批与 rewind）；10k items 会话滚动流畅；`GTK_TEXT_DIR=rtl` 冒烟通过。

## M5 — 生态与打磨

**范围**：MCP client（rmcp，会话级配置透传兑现）、上下文压缩（compaction item + side-query 摘要）、沙箱（landlock/bwrap，接口已留）、自定义模式开放、JSONL 导入、otel（过 ADR 后）、**WASM 工具插件评估**（wasmtime + WASI，仅评估：能力边界/性能/生态调研，实施须过新 ADR——ADR-0009）、文档英文化评估、`ToolRegistry` 的注册句柄（`register() → Handle{dispose, replace}`，消费者是 MCP 工具）、shell 会话复用（unified exec 式，capabilities 开放问题 2）、工具级重试上限 `max_tool_retries`（M2 裁决顺延：先要有「可重试失败」的语义与第一个消费者）。

**DoD**：按范围逐项验收。

## 依赖关系

```
M0 ─▶ M1 ─▶ M2 ─▶ M3 ─▶ M4 ─▶ M5
            │            ▲
            └── 契约测试套件 + Backends 装配点为 M3 委派铺路
```

M4（GUI）只依赖协议稳定（M1）+ 分支/rewind 语义（M2），可与 M3 并行——若人力允许。

**M2 → M3 的接缝具体是什么**（2026-10-07 更正措辞）：原文写「后端绑定机制」，但没有 ACP 协议时它无法被独立验证。M2 交付的是两件可机器验证的东西——① `FsBackend`/`TerminalBackend`/`ApprovalGate` 的**契约测试套件**（任何实现都必须整套通过，这是「换绑定不换工具」的机器保证，testing.md §2）；② daemon 侧一个**按会话来源选后端的 `Backends` 装配点**（今天 `Backends` 是在 `manager.rs:612` 内联构造的，没有选择逻辑）。绑定表的 ACP 三行留作有文档的接缝，M3 用 fake 宿主与 Zed 真机兑现。
