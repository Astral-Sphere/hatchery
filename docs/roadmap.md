# 路线图（M0–M5）

> 每个里程碑的完成定义（DoD）都包含：`cargo test`/`clippy` 全绿 + 列出的端到端验证 + worklog 更新。测试分层、CI 门禁与「不变量 → 测试」映射见 [design/testing.md](design/testing.md)；各里程碑的测试交付物已列入其范围与 [worklog/testing.md](worklog/testing.md)。当前进度：**M0 完成**；**M1 代码完成**（2026-10-01，Phase 1–5 全部落地：llm adapter + 能力表、capabilities/tools、daemon 全栈、cli TUI/exec、e2e 与不变量收口，`./scripts/ci.sh` 本地全绿），手动 live 验收进行中（已产出 2026-10-04/10-05 三轮 TUI 修正：widget 层与双主题、滚动条与滚轮、词级换行与点击折叠），[worklog/testing.md](worklog/testing.md) 的六项清单尚未勾选——**M2 Phase 0 负责收口 M1**。**Phase 0 已于 2026-10-07 完成**，**M1 同日关闭**：live 验收（deepseek + qwen 真密钥、隔离的 state/data 目录）首跑六项里三项通过、三项部分通过，并跑出**五个清单外缺陷**——`/effort` 从不进入请求、TUI 不投影历史、TUI 不投影其他客户端的用户消息、`/prompt` 的响应被前端丢弃、detached daemon 的致命启动错误不进日志。用户裁决五项全修 + reasoning 默认折叠，当日修完并**重跑清单六项全过**；`/effort` 那条催生了 **D19**（哪些配置跟着 turn 走、哪些跟着 runtime 走）。逐条实测证据见 [worklog/testing.md](worklog/testing.md) 本日条目。**Phase 1 已于 2026-10-08 完成**（`CheckpointStore` 转正 + 协议 diff 载荷 + D13/D9 落定 + store 的 `checkpoints` API + 不变量 6 迁移；D10 只做了 Linux 实测、仍开放），M2 从 **Phase 2** 起。
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

**两处口径更正（2026-10-07 勘察，均已由 Phase 0 修掉）**：① 本行原写「nextest 默认组与 **invariants 组**」——实际 `scripts/ci.sh:134` 只调用 `--profile ci`，`--profile invariants` 在 ci.sh 与两个 workflow 里**从未被调用**，是死配置。不变量测试确实跑到了（`ci` 继承 `default`，过滤器只排除 live/slow/gui），但没有一个可单独报告或阻塞的门禁；且 testing.md §5 映射到不变量、却没有 `invariant_` 前缀的 5 条测试（含 `two_concurrent_prompts_yield_exactly_one_turn`）一旦真去跑那个 profile 就会被漏掉。**已修**：ci.sh 有独立的 `invariants` 步，五条测试已补前缀。② `check_i18n`（ci.sh:121-124）是一条 `printf` 空操作（"i18n extraction check lands in M4 — nothing to verify yet"），不是检查，此前被算作通过的门禁步骤。**已修**：函数与 `run_step i18n` 删除，改成脚本末尾一行明示未设门禁。

## M1 — 最小对话闭环（Chat 模式端到端）

**范围**
- `hatchery-llm`：ChatCompletions adapter（deepseek/qwen 两家真实探测录制 fixture）、effort 映射表 v1、reasoning 采集/回放。
- `hatchery-daemon`：UDS/stdio 监听、attach-or-spawn、单会话 runtime、live hub（无 coalescing 优化）、generation。
- `hatchery-cli`：TUI 最小版（消息流 + 输入 + reasoning 折叠 + `/effort` `/model` `/prompt`）+ headless exec。
- Chat 模式：只读工具 `read_file`/`glob`/`grep`（LocalFs 直读）。
- 配置分层（platform.md §1）+ prompt 管线 v1（identity + mode_variant + environment + safety_gate）。**交付范围更正（2026-10-07 勘察）**：这一项实际只交付了**透明性**——四节装配、`{{var}}` 插值、per-section 来源标注与 `prompt/render`／CLI `/prompt`；`render_chat` 的唯一非测试调用方是 `core.rs:227` 的 `prompt/render`，**装配结果从未进过任何一次模型请求**（`ChatOptions` 无 system 字段、`StoreHistory::view()` 不产 `Role::System`，且 `runtime.rs:618` 有一条测试主动断言消息里没有 system 角色）。注入模型随 **M2 Phase 0**——**已于 2026-10-07 落地**（`StoreHistory::view()` 前置 `Role::System`，kernel 与 llm 一行未改；D15 定稿，细节见 design/platform.md §2.1）。
- `hatchery doctor`。

**DoD**：真实 provider 端到端对话（流式 + reasoning 展示与回放命中验证）；关终端重开会话 resume；两前端同时 attach 扇出一致。
→ **代码侧全绿**（2026-10-01）：mock wire 的 e2e 场景 1/2 与不变量组在 `hatchery-tests`；第二 turn 请求体对已存 reasoning 做了逐字节断言（不变量 2）。
→ **DoD 达成、M1 关闭**（2026-10-07）：真实 provider 的手动验收清单六项全过（首跑三项部分通过、跑出五个清单外缺陷，当日修完重跑）。流式与 reasoning 展示/回放命中、resume、双前端扇出一致都有 live 证据；清单与逐条证据见 worklog/testing.md。

## M2 — Code 模式（工具、审批、回滚、编辑分叉）

> 2026-10-07 重新规划。原 M2 段按 M0/M1 的自述写成，一次全仓库勘察（capabilities/tools、kernel/store、daemon/cli/protocol/tests 三路）发现若干与代码不符之处；下面的「勘察更正」是重写的依据，逐条带实证。各方向的细目已对账进对应 [worklog/](worklog/README.md)。

### 勘察更正（对原规划与既有文档）

1. **分支三原语 M0 就交付了**，M2 不是从零做。`edit_fork`/`switch_branch`/`delete_branch`/`branch_tree` 全部实现且有专测 + 属性测试对拍独立 `ReferenceTree` + kill -9 探针（`store/src/actor.rs:744/771/787`）。M2 的实际增量是：daemon 路由、rewind 组合逻辑、`checkpoints` 与 `approval_rules` 的 API 层、`CheckpointStore`、`DaemonApproval`、`confirm` 二次确认语义。
2. **daemon 只服务 20 个方法里的 10 个**（`core.rs:26-37`），`core.rs:594/597` 还显式断言 `session/rewind` 与 `approval/respond` 不在 `SERVED_METHODS`（"is M2"，届时翻转）。未路由的除会话级四项外还有 `session/set_mode`、`session/delete`、`session/rename`、`store/export_jsonl`——**M2 收尾时协议方法面应全部接通**。
3. **`checkpoints` 与 `approval_rules` 两张表零 Rust 代码**：无 `StoreCmd` 变体、无 trait 方法、无 `sql.rs` 行转换、无一行测试写入。表在 v1 schema 里，**M2 不需要新迁移**，缺的只是 API 层。`approval_rules` 只有 `id/scope/matcher/decision/created_at`——无排序列、无 enabled 列、scope 是裸 TEXT、无 session 外键。
4. **rewind 不依赖 `checkpoints` 表定位 commit**：`ItemKind::Checkpoint { commit_id, kind }` 自己带着 commit id，且 `is_conversation()` 不含 Checkpoint（不进模型请求）。Code scope 的实现是「`rebuild_chain` → 定位 target_item → 向后扫第一个 Checkpoint item → 读 commit_id → restore」；pre-write 快照恰好等于 target_item 时刻的工作区状态，扫不到即 no-op。`checkpoints` 表（`item_id` 可空）因此只服务**跨会话的预算核算与 GC**，不是 rewind 的主索引——文档此前把它写得像主索引。
5. **协议带不出 diff**：全仓库无 `UnifiedDiff`/`DiffHunk`/`FileDiff` 类型（capabilities.md §2 草图的 `diff() -> UnifiedDiff` 没有返回类型）；`ApprovalRequest` 只有 `args_digest: String`，其文档明写「Not the raw JSON … 可以是 megabytes」，**装不下 diff 或完整命令**，而 M2 要 TUI diff 预览、M3 的 ACP 要 `ToolCallContent{content=diff}`。也没有 list/delete 审批规则的方法（一条误存的 `DenyAlways` 会永久废掉一个工具且无法撤销）。`PendingApproval`（method.rs:498）零 fixture、零消费者。
6. **`TerminalHandle` 撑不起 shell 工具**：只有 `wait() -> TerminalOutcome`（一次性全量输出）+ `kill()`，**无输出流、无 `release()`**，与 capabilities.md §1 草图的 `{ output stream, wait_for_exit, kill, release }` 不符。没有输出流就没有 `ToolCallProgress`。当前唯一实现是 `NoTerminal`，所以现在改 trait 最便宜。
7. **不变量 5 的测试在任何形态下都不存在**（Phase 0 已补上 kernel 侧那条腿，端到端那条仍排 Phase 2），而它正是 M2 DoD 的「硬门测试全绿」。testing.md §5 点名的 `invariant_project_config_cannot_disable_hard_gates` 全仓库零命中；唯一沾边的是 `RiskLevel::is_hard_gate()` 这个**值类型谓词**的两条单测，生产代码无人调用。另：`is_hard_gate()` 只认 `WritesOutside`，而 design §5 的硬门含**工作区内**的 `.env*` 与 `.git/hooks`——不必动枚举（新增枚举值属 major bump），`once_only()` 已经用「不提供 always 选项」表达了不可记忆，路径门只要能为工作区内敏感路径强制它。
8. **不变量 4 的编译期门禁有洞**（Phase 0 已补 `tokio::fs` 与四个 `std::fs` 项；`tokio::process` 那组因无 crate 开该 feature 而暂缓，理由在 clippy.toml 头部）：`clippy.toml` 只禁 `std::fs::*` 与 `std::process::Command`，**没禁 `tokio::fs::*` 与 `tokio::process::*`**——而 tools crate 依赖 tokio、`LocalFs` 自己就用 tokio::fs。工具里写一句 `tokio::fs::write` 就绕过整条纪律。（另漏 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`。）
9. **模式装配不存在**：capabilities.md §4 的 `assemble(mode, backends)` 与 `builtin(spec, &backends)` 全仓库零命中，`ToolPolicy` 亦无；实际装配是 `manager.rs:612` 硬编码的 `chat_tools()` 循环，且 `SessionManager::assemble` **从不读 `session.mode`**——code 会话今天拿到的是与 chat 完全相同的三个只读工具 + `NoTerminal`。daemon.md §3.1 的四个 profile 也一个都不存在（无 `Profile` 类型）。
10. **headless exec 在 Code 模式下不可用**：`exec.rs:187` 硬编码 `SessionModeId::chat()`，`ExecArgs` 无 `--mode`、无任何审批策略标志。Code 会话的第一个审批会让它**永久挂住**（直到 120s `CALL_TIMEOUT`）。frontends.md §2.3 未规定。
11. **TUI 缺弹层与 diff 的地基**：`push_event` 末臂 `_ => {}` 静默丢弃 `ItemStarted`/`ModeSwitched`/`GenerationBumped`；`ApprovalRequested` 只改状态栏文字、把 `request_id` 与整个请求丢掉（tui/mod.rs:262-265）；`layout()` 返回硬编码 `[Rect; 6]`、`draw()` 无 z-order/`Clear` 通道；`key_input` 无模态（`1`-`4` 会被 `Char(c)` 吞进输入框）；`Esc` 硬接 `session/cancel`，而 `chat.rs:273` 明写「M2 的弹层拥有 Esc」，交接未实现。`actions()` 是 fire-and-forget（`submit_line` 丢弃回复），而 `branch/list` 要渲染节点表、`rewind` 要展示 `RewindReport`。可复用的是那套「借用 `&Model` 的纯函数 Widget + 自由函数报 height」惯例、`composer.rs` 的圆角框、以及三次重写才对的 `wrap_lines`（diff 正文正好要用）。
12. **testkit 缺四件 M2 前置**：`MemoryTerminal`（testing.md §2 声称 M1–M2 交付，不存在）；`MemoryFs` 写路径（trait 一加方法它就编译不过——是 forcing function，但也是工作量）；**协议级审批应答器**（现有 `answer_approvals` 拿 `AgentHandle`、绕过协议，测不到 `approval/respond`）；`hatchery-tests` 能读到的 toolcall SSE fixture（它**连 `tests/fixtures/` 目录都没有**，唯一的 SSE 是内联字符串且不含 `tool_calls`）。另 `TempWorkspace` 缺 testing.md §2 声称的 git init 与文件树 DSL，而 CheckpointStore 的不变量 6 测试需要「脏用户仓库」。
13. **`hatchery-tools` 没有覆盖率地板**（`xtask/src/coverage.rs:17-25` 只闸七个 crate），而 M2 的四个新工具全落在那里。（Phase 0 已加，地板与实测依据见下方清单。）
14. **两个 README 与代码不符**：capabilities 宣称有影子 Git 检查点与注册句柄；tools 宣称有七个工具（实际三个）。
15. **`hatchery-acp` 是死依赖边**（Phase 0 已删）：daemon 的 Cargo.toml 声明了它，源码零引用，crate 本体 13 行文档注释；`cargo xtask layering` 把它算作 build edge。
16. 文档小错：fixture 数「62」（protocol.md §6、testing.md §3.1、worklog/protocol.md）实际 **79**——但项目在 2026-09-30 已裁决「文档里的计数一律去掉」，所以是**删数字**不是改数字；ADR-0006 写 `RewindScope::ConversationAndCode`，协议实际是 `Both`（ADR 一经 accepted 不修改，以协议为准并在 worklog 留痕）；storage.md §1 的 trait 代码块列 17 个方法（实际 19，缺 `bump_generation`/`open_turns`）；kernel.md §2 的 `TurnInput(Content)` 草图过时（实际 `{turn, content}`）；storage.md 说只读连接池排 M1、worklog 已改 M3；testing.md §1/§3.8 仍写 insta（`d057a99` 已移除）。

### 范围（Phase 0–8；每阶段 `./scripts/ci.sh` 全绿 + worklog 更新 + 停下评审）

**Phase 0 — M1 收口 + 门禁诚实化 + prompt 注入** → **已完成（2026-10-07），只剩 M1 的手动 live 验收**

- [x] **system prompt 接进 turn，D15 定稿**：`SessionManager::assemble` 渲染一次 → `RuntimeParts.prompt` → `SessionRuntime.prompt`（冻结）→ `StoreHistory::view()` 前置一条 `Role::System`。`ChatOptions` 未加 system 字段，**kernel 与 llm 一行未改**（`Message::system` 此前零消费者，`translate.rs:134` 早就映射了 `Role::System`）。`invariant_minimal_chat_replays_reasoning_byte_exact` 改成三段断言：两次请求的 system 文本逐字节相同、它等于 `prompt/render` 的 `text`、其后的 messages 数组仍与手写期望整表比对。**不变量 2 的边界**写进 architecture.md §5 与 design/kernel.md §6：只管分支历史，system prompt 是可复现派生态、由 `prompt/render` 钉——该方法现在对活着的 runtime 返回装配时冻结的那一份，而不是重新渲染一份可能已经不同的。
  **D15 的理由①被实测推翻**：`session/set_config` 与 `config/set` 都不重组装活着的 runtime（唯一卸载路径是空闲清扫），所以 model、effort 与 prompt 三者都是「下次装配才生效」——用户敲完 `/model x`，下一轮仍发给旧模型且无提示。冻结依然成立（依据换成前缀稳定性 + 与既有语义一致），而这条缺陷记为 design/daemon.md **开放问题 5**；Phase 3 的 `session/set_mode` 必须先回答它，且解法不能是「`set_config` 里调 `unload`」（`unload` 对被 watch 的会话直接拒绝，而改配置的正是附着中的前端）。
- [x] **`override_dir` 接线**（同类缺陷第二处）：`prompt::prompts_dir(config_home, home)`（纯函数，XDG 与 HOME 两条分支都有单测）+ `default_prompts_dir()`，由 `entry.rs` 传进 `SessionManager`（构造函数多一个参数；**测试一律传 `None`**，不继承宿主机配置）。**生产路径**由 e2e 子进程测试 `a_prompt_override_in_the_standard_location_reaches_the_request` 钉住：子进程自己读 `XDG_CONFIG_HOME`（`set_var` 在 edition 2024 是 unsafe 且被 clippy 禁，「测试拥有子进程环境」是唯一能测真路径的形状），断言覆盖文本进了请求体的 system 消息、`{{platform}}` 插值了、`safety_gate.md` 的覆盖尝试经真路径依然被拒、`prompt/render` 回报的 source 与 text 与之相符。变异验证过（把 `default_prompts_dir()` 换成 `None`，该测试立刻红）。
- [x] **硬门唯一承重的那条腿可测了**（同类缺陷第三处）：testkit 加 `ScriptedToolHost::requiring_approval_with(ApprovalRequest)`——老的 `requiring_approval` 恒用 `ApprovalRequest::new`（options 恒为全四个），造不出收窄的请求，所以 `agent.rs` 的 `!offers.contains(&option)` 是结构性不可测，这也解释了 2026-09-30 那轮为什么没兑现「修复必附回归测试」。新增 kernel 用例 `a_hard_gate_refuses_an_answer_it_never_offered`（`once_only()` 请求先答 `AllowAlways` 再答 `Deny`：工具从未被调用、结果是拒绝文本、ToolCall 状态 `Denied`），变异验证过。Phase 2 的 `invariant_project_config_cannot_disable_hard_gates` 建在它之上。
- [x] **ci.sh 加真正的 `invariants` 步 + 五条测试补前缀**：选择改名而不是把 profile 换成显式清单（清单会在 testing.md §5 与 `nextest.toml` 两处重复同一份知识并各自腐烂）。改完逐条核对该 profile 的选中集合并全绿；重复跑是有意的，命名步骤才可单独报告，且过滤器选空时 nextest 默认失败。**代价**：ADR-0012 引的是 `no_gitlink_...` 旧名，ADR 一经 accepted 不改，映射记在 worklog/capabilities.md。
- [x] **`clippy.toml` 补 tokio 洞**（更正 8）：`tokio::fs` 的全部孪生项 + `std::fs::{remove_dir, read_link, hard_link, set_permissions}`，并用一个临时 scratch 模块**逐条实测**（clippy 对解析不到的路径静默忽略，打错字等于留洞；实测结果是表里每条都开火、无多余项）。`tokio::process` 那组**故意没加**：没有 crate 开该 feature，clippy 回 "does not refer to a reachable function"，实测不触发 `-D warnings` 失败，但会在每次门禁输出里留下警告。
- [x] **覆盖率表加 `hatchery-tools`**（更正 13）：85%，与其他产品核心 crate 同档，**先实测再定**（加入前 `--report-only` 的读数远高于此）。钉阈值表的测试同步改名，把「seven」从测试名里拿掉。
- [x] **删掉 daemon → `hatchery-acp` 的死依赖边**（更正 15）：Cargo.toml 与 Cargo.lock 各一行，`cargo xtask layering` 仍报 strictly downward、无环。
- [x] **`check_i18n` 改成诚实的非门禁**：函数与 `run_step i18n` 删除，脚本末尾打印 `--- i18n: not gated yet (extraction lands in M4)`，`--help` 的步骤表照实写。
- [x] **文档对账**（更正 14/16）：两个 crate README 已在前一批提交改对；本批把 platform.md §2.1、kernel.md §6/§7、testing.md §1/§3.5/§3.6/§5/§8、architecture.md §5 里已被实现推翻的口径改成实现后的真相。顺手修了 core.rs 里 `audit` 的文档注释错挂在 `env_key_unusable` 上（rustdoc 因此在错的函数上显示审计说明，而 `audit` 自己没有文档；`missing_docs` 未开所以一直没被发现）。
- [x] **M1 关闭**（2026-10-07）：live 验收首跑六项里 doctor、exec（含 `--json`）、回放命中三项通过，TUI、resume、双前端扇出三项**部分通过**，并跑出五个清单外缺陷；用户裁决五项全修 + reasoning 默认折叠，当日修完**重跑六项全过**，`./scripts/ci.sh` 全绿。五项与修法：① `/effort` 从不进请求 → **D19**（effort 与 model 跟着 turn 走，跨 provider 的 `/model` 才重组装）；② TUI 不投影历史 + ③ 不投影他人的用户消息 → 同一个缺失机制，补 `push_history`/`push_live_item`（自己那条按 turn id 去重）；④ `/prompt` 无输出 → `project_reply` + `CellKind::Prompt`，`/effort` `/model` 吐司回报**实际生效**的值；⑤ detached daemon 的致命错误不进日志 → 两个 spawn 点都把子进程 stdout/stderr 追加进 `logs/hatchery-stdio.log`，失败消息点名它。另加 ⑥ reasoning 默认折叠（`ui.show_reasoning` builtin 默认与 CLI 兜底都改成 `false`；清单那句措辞本来就是对的，是 shipped 默认与它相反）。Phase 0 自身的代码、门禁、文档三部分已完成，`./scripts/ci.sh` 全绿。**意外收获**：prompt 注入在真实 provider 上被直接证实——模型自述「我是 Hatchery…这里是只读的聊天模式…不能替你写文件或执行命令」，只调 `grep`/`glob`/`read_file`（带行号区间），被问「你现在处于什么模式」时答「Chat」，并跑通了多轮工具循环（round 1 grep+glob → round 2 read_file ×2 → 终答）。kernel 待办里「max_rounds 熔断与 `TurnCompletion` 语义在真实对话下验证」仍未做——live 那几轮都没触到熔断。

**Phase 1 — 检查点与写路径**（capabilities + store + protocol）→ **已完成（2026-10-08），D9/D13 落定，D10 仅 Linux 实测、仍开放**

- [x] **`CheckpointStore` 从 spike 的 `Sandbox` 转正**（`capabilities/src/checkpoint.rs`）：open 配方逐条照 spike 转录（`init_opts` 的 `no_dotgit_dir`+`bare`+`external_template(false)` → **手写** `core.worktree`/`core.bare=false` → `set_workdir(.., false)`），`harden()` 每次打开都跑（模板、`core.excludesFile` 指向不存在的路径、`autocrlf=false`、`fsmonitor=false`、固定身份），ignore 规则每次打开重放。**启动断言**落在 `open()`：`assert_disjoint` 拒四种重叠（影子 git-dir 就是用户 `.git`／在其内部／在工作区内部——那会把自己快照进去并每次变大／工作区在其内部），另外每次打开都校验仓库自己记的 `hatchery.workspace` 标记，所以拿别人的 git dir 会被拒而不是被写。`CheckpointPool` 提供 per-workspace 互斥与共享（同一工作区两个会话必须共用一个影子仓库，否则各自的快照看不见对方的写）；`recorded_workspace()` 从仓库内部反查归属，是孤儿判定唯一可行的入口（目录名是派生 uuid）。阻塞的 git2 调用一律经 `spawn_blocking`：500 文件冷快照实测 48.9ms，停在 runtime worker 上对别的会话是可见的。
- [x] **协议加 diff 载荷**（更正 5）：**结构化 hunk**（用户裁决）——`Diff/DiffFile/DiffStatus/DiffHunk/DiffLine/DiffLineKind` + `CheckpointId` 新 id 型。理由是 D11 已定 TUI 用 `similar` 算 hunk、git2 侧也原生产出 hunk，两个生产者喂同一个类型、两个前端都不用写 unified-diff 解析器；codex 那条「线上传文本、前端解析」的路为此付了 2745 行 `diff_render.rs`。**没有 golden fixture，这是对的而不是漏的**：`Diff` 既不是 `ItemKind` 载荷、也不是方法结果或事件，而 fixture 注册表只覆盖这三类；等第一个返回它的方法（Phase 3 的 rewind 或 `checkpoint_diff` 工具）带上。serde 拼写由 7 个单测钉住。**风险 3「协议加字段翻动 determinism 门禁」本次没有兑现**。
- [x] **D13 落定，但机制改了一处**：原写法是 `ToolInvocation` 带出 `Vec<Checkpoint>`，**取消路径会把它丢掉**——kernel 的工具 select 是 cancel-first，被中断的 invocation 直接 drop 且不再被 poll（这条事实早就写在 testkit 假 host 的注释里，它为此专门写了 drop guard）。改成 kernel 造 `CheckpointCollector` 借给 `ToolHost::invoke`，三条出口（完成/失败/中断）都 drain 并把 Checkpoint item 追加进链。不这么做的后果是具体的：取消的 `write_file` 留下半截文件而没有任何 item 指向它的 undo 点，Code rewind 向后扫会跳过它、恢复出**包含损坏**的状态。item 顺序仍是 `… → ToolCall → Checkpoint → ToolResult`（结果靠 `ToolResult.call` 配对，插入不影响配对）；daemon 的 HubSink 在 item 落库后补写 `checkpoints` 行，失败只记日志。新测试 `an_interrupted_call_keeps_the_checkpoints_it_already_took`（**变异验证过**：摘掉中断分支的 drain 立刻红）+ `a_call_that_wrote_records_its_checkpoints_before_its_result` + 一条只读对照。
- [x] **检查点做成 `FsBackend` 的装饰器**，不是 `LocalFs` 的字段（与 design 草图「`LocalFs` 写前打检查点」的偏差）：`CheckpointedFs` + `Checkpointer` trait。三条理由：local backend 保持「只是个文件系统」；收集器天然按**调用**划分而不是按会话（两个调用永不混检查点）；任何 backend 都能被包住，包括 `MemoryFs`——testing.md 要的「写序列 vs 检查点记录对齐」因此有得测。`Checkpointer` 是 trait 而非直接用 `CheckpointStore`，因为「要不要打点」是需要 `checkpoints` 表的策略，而那张表在同层的另一个 crate 里（L2 之间不能互相依赖，策略只能住在 daemon）。接缝外看，行为与草图要求的一模一样。
- [x] **写原语只加了 `write_text_file`，`create_dir`/`remove` 没加**——各自被指名的消费者不存在：`write_text_file` 自己建父目录（Phase 2 的 `write_file` 只要这一个），而 purge 是 `CheckpointStore::restore` 里的 `checkout_index(remove_untracked)`，**从不经过接缝**（spike 实测的形状，也正是 `RestoreReport.purged` 的数据来源）。3 个 backend × 2 个无消费者原语 = 没人跑过的死代码（ADR-0009 反预拆分刹车）。design/capabilities.md §1 已改，本行是那份改动的记录。
- [x] **写路径另写了一条解析**：`resolve` 靠 `canonicalize`，对「还不存在的文件」必然失败。`resolve_write` 锚定**最近的可解析祖先**再往下拼，因此 `create_dir_all` 不会在符号链接祖先的另一侧建目录；目标本身若已存在则整体 canonicalize，悬空符号链接被拒（穿过去写会在链接目标处创建文件，落在所有检查点之外）。两条都有实测钉住：`link/` 指向外部目录时写 `link/deeper/file.txt` 被拒**且外部一个目录都没建**。
- [x] **D9 落定（用户裁决「GC 最旧，仍超则跳过打点」），但「GC 最旧」只能按仓库粒度做**：libgit2 没有对象级 GC（`Repository` 只有 `odb()` 读写与 `cleanup_state()`，没有删对象的东西），而丢弃链上的提交必须重提交幸存者、**重提交的 commit id 会变**——`ItemKind::Checkpoint.commit_id` 已经在 append-only 的 items 表里（`items_no_update` 触发器拒绝那次修正）。所以阶梯是：① 扫孤儿（无行 → 无 item 引用 → 删整个仓库是安全的，也确实回收字节）② 本工作区超预算且快照数 >1 → **删库重来** + 删行（保住*将来*的可回滚；代价是旧 rewind 目标报 `UnknownCommit`，测试钉住了这句话）③ 单个快照就超预算 → 跳过打点 + 告警，写照常进行。只有真 git/IO 故障才拒写，而且是 `FsError::Checkpoint` 不是 `Io`——让模型听见「缺的是 undo 点」而不是「磁盘或权限出事」。ADR-0006 要的「熔断可配置」落成 `[checkpoints]` 四个键（两个预算 + 大文件阈值 + ignore 规则，`_mb` 一律按 MiB）。
- [x] **孤儿影子仓库 GC（storage 开放问题 3 关闭）**：启动扫一次 + 每次预算检查时扫；判定 = `recorded_workspace()` 反查 + `WHERE workspace = X` 无行。**「认不出属于谁」一律保留不删**。顺带发现并修掉一个会删用户数据的坑：影子仓库记录的是**规范化后**的工作区路径，而 HubSink 写行用的是会话里的原始拼写，两者不一致时孤儿清扫会把**活着的**仓库当孤儿删掉。统一成 `recorded_workspace()` 一个函数、两处写入方都用它，测试用符号链接进来的同一工作区钉住「仍算有主」。
- [x] **两条实测纠正了实现**：① 影子仓库**尊重工作区自己的 `.gitignore`**——即便它是 bare + 外部 work tree + `core.excludesFile` 指向不存在的路径。所以 git 工作区的 `target/` 天然不入快照，不需要我们自造排除表；非 git 工作区才需要 `checkpoints.ignore_rules`。② `delta.flags()` 在 `Patch::from_diff` **之前**是空的、之后才有 `BINARY`（libgit2 要加载内容才判定）；先读 flags 的 diff 会把每个二进制文件报成「文本文件、零 hunk」。
- [x] **restore 的安全快照有两个反直觉的约束**（都是测试跑红才发现的）：它**不能移动 HEAD**，也**不能留下已 stage 的 index**——`reset(Hard)` 按 index 决定删什么，安全快照把整个工作区 stage 进去之后，「从未被任何快照跟踪」的用户文件看起来就是已跟踪的，于是每次 rewind 都变成一次 purge，正是 `purge_untracked` 默认 false 要防的事。修法：`commit_snapshot(.., update_head: false)`（提交对象不挂 ref，靠对象库 + `checkpoints` 行仍可寻址）+ 快照后把 index 读回 HEAD 的树。**变异验证过**：摘掉 `unstage_to` 两条测试立刻红。
- [x] **store 的 `checkpoints` API**：`CheckpointRecord` + `record_checkpoint`/`checkpoints_for_workspace`（最旧优先，GC 从头 drain）/`delete_checkpoints`（回报真实行数，与引擎对账）；写入前认会话、认 item 归属、拒空 `commit_id`，`created_at` 同毫秒时按 id 定序（引擎的行序不保证）。9 个新测试，含**会话删除**与**分支删除**两条级联——前者正是「影子仓库还有没有主」变成一句查询的原因。schema 停在 v1，未加迁移。
- [x] **testkit**：`TempWorkspace::git()`（脏用户仓库：一个提交 + 已 stage + 未 stage + 未跟踪，与 spike 的 fixture 逐项对齐）+ `file`/`dir`/`stage`/`commit` DSL + `UserRepoState`/`user_repo_state()`（spike 与 invariant 6 共用同一份度量，避免两边各写一套然后各自腐烂）。`MemoryFs` 补 `write_text_file`（含建父目录、目录占位报 `WrongKind`），并让**根目录恒存在**——原先「空 `MemoryFs` 没有根条目」是假的特性，与 `LocalFs`（根是别人递给它的真目录）不一致；`walk.rs` 那条测试的前提因此失效，改成用一个**会拒绝的假 backend** 测同一条性质（「walk 的根被拒是 `ToolError::Backend` 不是 panic」），另加一条钉住「空工作区 walk 出空结果」。**大文件阈值 0 的语义定为「不按大小过滤」**，另一种读法（凡有字节就排除）会让每个快照都静默变空。
- [x] **不变量 6 迁移**：三条 `invariant_` 测试从 spike 的私有 `Sandbox` 移到 `tests/checkpoint.rs`，跑真的 `CheckpointStore`；**名字未变**（ADR-0012 引的旧名映射仍记在 worklog/capabilities.md）。spike 只留**libgit2 后端事实**：配方、per-handle ignore、`statuses()` 不重写用户 index、性能、预算可查询、reset 语义、diff 列举。新增 22 条 `CheckpointStore` 测试，capabilities 共 51 条全绿。
- [x] **D10 未关闭（用户裁决「Linux 实测 + 三平台探针」）**：`spikes/pty/` 是**独立 workspace**（自带 `[workspace]` 表，根 `Cargo.toml`/`Cargo.lock` 一行未动，`cargo metadata` 看不到它，portable-pty 不进项目锁文件——决策没定之前不该进）。Linux 实测七组，macOS/Windows 只有「读 codex 源码」级证据并在报告里逐条标注。测出四条会改 Phase 4 设计的事实：① PTY 的 ONLCR 把 LF 变 CRLF，**输出与子进程 stdout 不逐字节相同**，golden 与 `TerminalOutcome` 必须归一化，而 portable-pty 传 `termios = NULL` 且没有改它的 API；② `Child::kill()` 只发**一个 pid**（源码：SIGHUP → 5×50ms 轮询 → SIGKILL，无 killpg），5 个场景里 **3 个留孤儿**（setsid、`trap '' HUP`、`set -m` 作业控制）；③ **`kill -0` 无法区分僵尸与活孤儿**（实测：killed-but-unreaped 的 pid 仍答 ALIVE，`/proc` 状态是 `Z`）——testing.md §3.5 写的那条断言手法本身不成立，必须配 `wait()`/读进程状态，否则一张全是僵尸的进程表也能让它通过；④ `CommandBuilder` 的 cwd 默认是 **`$HOME`** 不是进程 cwd，`TerminalSpec` 必须显式设。另有：父进程不立刻 drop slave fd 则 reader 永远等不到 EOF（实测挂到 2522ms 才在 drop 后结束）；内核缓冲只有 **4095 字节**，消费者停读会**冻住**子进程（所以超时杀可能杀在一个只是在等我们的进程上）；行纪律让换行密集输出体积涨 **50%**、吞吐从 171.7 MiB/s 掉到 **12.7 MiB/s**（D12 的 spill 阈值要按后者定）；4 MiB 无换行载荷逐字节无损。本工作区 `unsafe_code = "deny"`，而自带 killpg 需要 libc/`nix` 或一处 `#[allow]`——这是 Phase 4 要先答的。
- **本阶段没做的**：端到端（写工具 → 检查点 item → `checkpoints` 行 → rewind）还串不起来，因为**写工具是 Phase 2 的交付物**；链条分三段各有测试（capabilities 的接缝写路径、kernel 的 item 追加、daemon 的行索引），Phase 2 的 `write_file` 落地时才有第一条真 e2e。`CheckpointStore::diff()` 目前也只有测试消费者，它的第一个产品消费者是 Phase 3 的 rewind 面板或 `checkpoint_diff` 工具。

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
| D9 | ~~检查点超预算行为（GC 最旧 vs 拒写；spike 数据已备）~~ → **已定并实现（2026-10-08）**：用户裁决「GC 最旧，仍超则跳过打点」，但**「GC 最旧」只能按整个影子仓库的粒度做**——libgit2 没有对象级 GC，而重提交幸存者会改 commit id，那些 id 已经在 append-only 的 items 表里。落成的阶梯：扫孤儿 → 本工作区删库重来（快照数 >1 时）→ 单个快照就超预算则跳过打点 + 告警、写照常；只有真 git/IO 故障才拒写。可配置：`[checkpoints]` 四个键 | Phase 1 ✅ |
| D10 | PTY 库选型（**spike 提前到 Phase 1 并行**，三平台孤儿进程/取消/输出流实测）→ **仅 Linux 实测（2026-10-08），决策仍开放**：探针常驻 `spikes/pty/`（独立 workspace，不进项目锁文件；跑法与平台 payload 的成熟度见其 README），Linux 七组测量在 `spikes/pty/measured-linux.txt`。**探针的 macOS/Windows payload 成熟度不同**：macOS 复用 POSIX 脚本（只把 `setsid` 用 `command -v` 挡住），Windows 那几条是照文档写的 PowerShell 字符串、**从未执行过**——所以 CI 上跑出来的 Windows 数字在有人对着 payload 读过之前不算实测。已测出四条会改 Phase 4 设计（ONLCR 改写输出、`Child::kill()` 只发一个 pid、**`kill -0` 分不清僵尸与活孤儿**、`CommandBuilder` 的 cwd 默认 `$HOME`），另有一条读源码得来的：portable-pty 0.9.0 **完全不含** Job Object 代码，codex 在 Windows 侧是**换掉**库的 child 实现（自带 ConPTY）而不是包一层 | Phase 1 spike（部分）→ Phase 4 实现 |
| D11 | TUI diff 渲染方案（`similar` + 主题语义色 vs syntect） | Phase 6 |
| D12 | spill 阈值与落盘位置 | Phase 4 |
| D13 | ~~检查点如何成为 item、rewind 如何定位 commit~~ → **已定并实现（2026-10-08），机制比原写法多一处改动**：链是 `… → ToolCall → Checkpoint → ToolResult`、由 kernel 追加、daemon 的 HubSink 补写 `checkpoints` 行（失败只记日志）。原写法「`ToolInvocation` 带出 `Vec<Checkpoint>`」在**取消路径上会丢掉检查点**（被中断的 invocation 直接 drop 且不再被 poll），改成 kernel 造 `CheckpointCollector` 借给 `ToolHost::invoke`、三条出口都 drain。rewind 的定位规则（向后扫第一个 Checkpoint item）留 Phase 3 | Phase 1 ✅ |
| D14 | 审批预览载荷形状（`ApprovalRequest` 加字段 vs 单独方法） | Phase 2 |
| D15 | ~~system prompt 的渲染时机与不变量 2 的边界~~ → **已定稿并实现（2026-10-07）**：装配时渲染一次并冻结；不变量 2 只管分支历史，system prompt 由 `prompt/render` 钉（它返回活 runtime 冻结的那份）。**理由①「config 变更会重组装」经实测推翻**，见 design/daemon.md 开放问题 5 | Phase 0 ✅ |
| D16 | headless exec 在 Code 模式下的审批策略 | Phase 6 |
| D17 | web_fetch 的 HTTP client 与 HTML→Markdown 选型 | Phase 4 |
| D18 | TUI 弹层/选择器与「Model 只是事件投影」纪律的和解方式 | Phase 6 |
| D19 | ~~哪些配置跟着 turn 走、哪些跟着 runtime 走~~ → **已定并实现（2026-10-07）**：model 与 effort 跟着 turn（`turn_options` 在 submit 时解析，随 `TurnInput { options }` 进 kernel；`tool_defs` 例外，始终取冻结的 snapshot），provider adapter/能力表/echo 与 prompt 跟着 runtime，所以跨 provider 的 `/model` 触发重组装并广播 `GenerationBumped`，turn 进行中则拒绝而不是杀 turn。由 live 验收量出的「`/effort` 从不进请求」催生 | Phase 0 ✅ |

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
| Code 会话改坏文件后 rewind 恢复 | **Phase 1 ✅**（CheckpointStore + 写前打点 + D13 的 item 链 + `checkpoints` 索引行）→ Phase 2（第一个写工具，链条才有真 e2e）→ Phase 3（rewind 三 scope）→ Phase 7（e2e 场景 5）→ Phase 8（手动验收） |
| 编辑历史消息分叉重演 | store 原语已备 → Phase 3（`edit_item` + `rewind_scope` 联动）→ Phase 6（`/edit`）→ Phase 7（e2e 场景 4） |
| 审批规则持久化生效 | Phase 2（DaemonApproval + rules API + D8 + `approval/respond` 路由）→ Phase 7（e2e 场景 6 含 AllowAlways 与 fail-closed 超时） |
| 硬门测试全绿 | Phase 0（让 `invariants` profile 真被调用 + 前缀对账）→ Phase 2（路径门 + `STRICT_KEYS` + `invariant_project_config_cannot_disable_hard_gates`）→ Phase 7 |

### 主要风险

1. **Windows CI（MSYS2 ucrt64 + windows-gnu）上的 PTY**：三平台门禁里唯一有真实分歧的新依赖，孤儿进程语义在 Windows 要靠 Job Object。缓解 = D10 spike 提前到 Phase 1 并行做，实测三平台后再实现。**Phase 1 只完成了 Linux 那一栏**（`spikes/pty/measured-linux.txt`）：探针写成了三平台可跑的独立 crate，但 macOS/Windows 的读数要等一次 CI 跑，所以 D10 仍开放、Phase 4 开工前必须先补。**风险比原来估计的更集中**：读 codex 源码得知 portable-pty 0.9.0 **完全不含** Job Object 代码（`grep -rn "Job\|JOB_OBJECT\|jobapi"` 全 crate 零命中），它的 `WinChild::kill()` 就是单个 `TerminateProcess`——所以 Windows 侧不是「包一层」而是**换掉 child 实现**（codex 自带 ConPTY，用 `PROC_THREAD_ATTRIBUTE_JOB_LIST` 让子进程**生来就在 job 里**，因为事后 `AssignProcessToJobObject` 对已存在的后代不保证生效）。而探针的 Windows payload 是照文档写的 PowerShell 字符串、从未执行过，这一栏的仪器本身也还需要先修。Linux 已经证明「库给的 kill 不够用」——`Child::kill()` 只发一个 pid，5 个场景里 3 个留孤儿。构建成本不是障碍（实测冷构建 1.74s、Linux 侧新增 6 个 crate、MSRV 1.90 干净，见 `spikes/pty/README.md`）。
2. **体量**：M2 现在明显宽于 M1（用户裁决保留全部 worklog 标注 M2 的条目）。Phase 4 结束时评估拆分 M2b。
3. **协议加字段会翻动 fixture 确定性门禁**：项目已有先例（worklog 记过两次「有意修改导致 determinism 步骤保持红，提交即恢复」）。Phase 1/2/5 的协议改动要预留这个摩擦。**Phase 1 没有兑现**：新增的 `Diff` 与 `CheckpointId` 都不在 fixture 注册表覆盖的三类里（`ItemKind` 载荷、方法结果、事件），determinism 步骤保持绿；Phase 2 给 `ApprovalRequest` 加 preview 字段时会真正撞上。
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
