# 工作记录：总体架构 / 跨方向协调

- 范围：crate 分层纪律、核心不变量、进程模型、跨方向决策协调、workspace 与 CI 基建
- 设计文档：[../architecture.md](../architecture.md)、[../references.md](../references.md)
- 相关 ADR：全部

## 当前状态

**M1 代码完成（2026-10-01）**：Phase 1–5 全部落地（llm adapter + 能力表、capabilities/tools、daemon 全栈、cli TUI/exec、e2e 与不变量收口），评审⑤自查轮的加固已入库；本地 `./scripts/ci.sh` 全绿、覆盖率门槛全过（数字见 worklog/testing.md 日期化条目）。手动 live 验收进行中——已产出 2026-10-04/10-05 三轮 TUI 修正，worklog/testing.md 的六项清单尚未勾选；**M1 的收口归入 M2 Phase 0**。三平台 CI 自 M0 收口后尚未见过新代码（push 由用户执行）。M0 历史结论：M0b 三 crate + testkit 于 2026-09-28 落地、评审后两轮修复（09-29/09-30）入库，M0 DoD 达成（门禁三平台绿 + kill -9 崩溃恢复 + 三 spike 落档），细节见下文变更日志。

**M2 已于 2026-10-07 重新规划**（见 [../roadmap.md](../roadmap.md) 的 M2 节）：一次三路并行的全仓库勘察发现原 M2 段是按 M0/M1 的自述写成的，与代码有 16 处不符，阶段划分重写为 Phase 0–8，跨方向的更正记在本文件下面的日期条目里。

仓库现状：`crates/`（12 个 crate = 10 个产品 crate + dev-only 的 `hatchery-testkit` 与 `hatchery-tests`）+ `xtask/` + `scripts/ci.sh` + `.github/workflows/{pr,nightly}.yml` + `docs/`。产品 crate 里 8 个已实现（protocol / kernel / store / llm / capabilities / tools / daemon / cli），`hatchery-acp` 与 `hatchery-gui` 仍是骨架（各只有文档注释，实现分别排 M3 与 M4）。（原文写「11 crate，其中 3 个已实现」是 M0b 时点的快照，2026-10-07 更正。）

## 待办

- [x] (M0) Cargo workspace 脚手架：12 member（11 crate + `xtask`）+ workspace Cargo.toml（共享依赖版本与 lints）+ rust-toolchain.toml
- [x] (M0) CI：PR 门禁全套（`scripts/ci.sh`：toolchain/fmt/clippy `-D warnings`/build/nextest ci 组/doctests/fixture 确定性/i18n 占位）+ nightly（slow/gui/audit/coverage）+ `disallowed_methods` lint 配置
- [x] (M0) xtask：`layering`（分层契约，含集成测试）、`coverage`；`i18n-extract` 与 `record-fixtures` 是 fail-loud 占位
- [x] (M0) 三个 spike：存储引擎（→ ADR-0010 turso）、影子 Git 后端（→ **ADR-0012 git2 vendored**；第一轮曾选 CLI，用户裁决后改 git2 并重测）、i18n gettext vs fluent（→ ADR-0011 fluent）
- [x] (M0) MSRV 实测：`rust-version = "1.90"`（1.85/1.88 均失败，1.90.0 编译通过）
- [x] (M0) CI 工具链改为**源码编译**（`cargo install cargo-nextest --version 0.9.146 --locked`，三平台一致；nightly 的 audit/llvm-cov 同）——用户裁决，避免预构建二进制在 MSYS2 下的不确定性
- [x] (M0) 编译 flags：`.cargo/config.toml` 加 `[build] rustflags = ["-C", "target-cpu=native"]`（用户偏好；已实测进入 rustc 调用）。**纪律**：发布产物与交叉编译必须覆盖（`RUSTFLAGS=""`），否则二进制不可跨 CPU 移植（2026-10-03 更正：该 flags 已于 10-01 停用、CI 与仓库配置归零——缓存污染事故，见同日变更日志）
- [x] (M0) 三平台 CI 跑通（第 1 次失败于 windows 的 `rustup-init --component` 参数 arity，已在 5b953fb 修复并补 windows-gnu 的 ABI 断言；细节见 worklog/testing.md「实测记录 · CI 首跑」）
- [x] (M0) 顶层 README 扩写：项目定位、快速开始、文档链接、MSYS2 ucrt64 环境清单、仓库布局；docs/README.md 加「代码布局」节
- [x] (M0b) 分层修正：protocol 沉为唯一最底层，kernel 升 L1（见下「M0b 修正」）
- [x] (M0b) protocol/kernel/store 三个 crate 实现 + testkit M0b 子集 + 各自的测试套件
- [x] (M1) 建立 docs/glossary.md 术语表（2026-10-01，M1 收口时建立）
- [x] (M1) MSRV CI job（`cargo +1.90.0 check`）加进 nightly，防止依赖升级悄悄抬高 MSRV（2026-10-01 随 Phase 5 门禁收口落地）
- [x] (M1) `hatchery-tests` 成员 crate（跨 crate e2e 与不变量套件的家；虚拟 manifest 不能有顶层 `tests/`，见 design/testing.md §1）（2026-10-01 落地，xtask layering LAYERS 表同步）

## 开放问题

1. ~~crate 命名前缀与是否发布到 crates.io~~ → **已定（2026-09-28）**：前缀 `hatchery-*`；**按可发布标准写全元数据**（`license = "GPL-3.0-only"`、repository、description、readme、keywords、categories、`rust-version`），但 **M0 不 publish**；`hatchery-testkit` 与 `xtask` 标 `publish = false`。protocol crate 从第一天走 semver 纪律 + `PROTOCOL_VERSION` 常量（M0b 落地）。注意 LICENSE 是 **GPL-3.0**，将来发布 library crate 会让下游必须接受 GPL，届时需重新确认。
2. ~~MSRV 策略~~ → **已定并实测（2026-09-28）**：`rust-toolchain.toml` 用 `channel = "stable"`（浮动），`rust-version = "1.90"`。实测：1.85.1 失败（turso 的 `icu_*`/`aristo`/`home` 要求 1.88）、1.88.0 失败（`roaring 0.11.5` 要求 1.90.0）、**1.90.0 通过** `cargo check --workspace --all-targets`。GTK 生态（M4 才引入）可能再抬高下限，届时以实测为准。
3. ~~references/ 目录的 license 与体积~~ → **已由用户自行解决**：`.gitignore` 里的 `/references` 使其不入库，只保留 `references.md` 的分析结论。

## 变更日志

### 2026-10-24 · 评审⑨：安全、生命周期与测试可信度

**修复（6 个 Critical + 全部建议）**：

- **`web_fetch` 的重定向**：`Client` 不再用默认的 `Policy::limited(10)`，改为自定义策略——只跟随同 host + 同 port、且不把 https 降级为 http 的跳转；跨 host 的 3xx 原样返回，输出里带 `location:`，模型要抓就得再走一次审批。测试覆盖同 host 跟随与跨 host 停止。
- **`shell` 的审批永远是 `once_only`**：普通命令原来给四个选项，一次 `AllowAlways`（或 `[approval] allow=["shell"]`）就会让 `GatedTools` 对之后所有 shell 命令返回 `NotNeeded`，绕过 `.env`/`.git/hooks`/`~/.ssh` 的硬门。现在每次命令都问；危险模式表补了 `rm -rf //`、`"$HOME"`、`${HOME}`、`--no-preserve-root`。
- **脱敏**：键名允许 `[A-Za-z0-9_]*` 前缀，`DB_PASSWORD`/`AWS_SECRET_ACCESS_KEY`/`OPENAI_API_KEY`/`token=` 都能命中；值改为引号感知扫描（带空格的引号值、`SecretKey::new()` 这类表达式不误伤）；补了空用户 URL、密码含 `@`、PGP 私钥块、`Authorization: Basic/Digest`。
- **进度也脱敏**：`registry.invoke` 在把 `ToolProgress` 交给内核前套同一层 `redact`；最终输出与 spill 文件仍是整段脱敏。跨两个 reader chunk 拆开的密钥仍有残余，已在注释与文档写明。
- **`shell` 的 EOF 超时**：`next_chunk() == None` 不再 `break` 出带 timeout/cancel 的 `select!`，而是置 `eof` 并继续等 `wait`/timeout/cancel；`exec </dev/null …; sleep` 这类关闭 stdio 的命令仍会在 `timeout_secs` 被杀。
- **`LocalPty` 的生命周期**：`pgid` 直接取 `pid`（spawn 已 `setsid()`；`tcgetpgrp` 是前台组，会和 `set -m` 抢跑）；Linux 控制线程用 `waitid(WNOWAIT)` 观察退出但**不 reap**，在僵尸仍钉住 pgid 时排空 kill 请求并 `killpg`，收到 sender 断开后再 `waitpid`；leader 退出后的后台子进程终于能被 `kill`/`release`/`Drop` 杀到。新增 `TerminalOutcome.stream_incomplete` 区分「捕获满了」与「reader 没等到 EOF」。
- **其余**：`shave` 改 head+tail 2,000+2,000 并保留 `full output at` 指针；`[context]` 非正值夹到 4,096、错类型被 `type_accepts` 丢弃；spill 序号改为进程级静态；`MemoryTerminal` 缓存 outcome（幂等）且 `notify_one` 不再丢唤醒；`TermError` 的 cwd 越界文档改为 `Io`，`try_wait`/`waitid` 错误走 `Err`；`shell`/`web_fetch` 在 `needs_approval` 里先 `parse`，确定失败的调用不弹审批；`shell` 的 tool description 明写命令写入在 checkpoint/undo 之外。

**测试可信度**：进程组测试换成探针里 `Child::kill()` 也杀不死的同组 SIGHUP 免疫 payload（`trap '' HUP; sleep 60 & echo $!; wait`）；新增 leader 退出后 Drop 杀后台子进程、`stream_incomplete`/250ms EOF 宽限、Capture head cap、`StoreHistory::view` 小预算、trimming 工具配对、shave 尾部与 spill 指针、web_fetch 5 MiB/charset、脱敏正反例、MemoryTerminal 幂等/丢唤醒等测试。`/proc` 状态断言与 Linux-only 的 post-exit kill 测试统一 `#[cfg(target_os = "linux")]`；macOS 只共用代码，未测量。

**实测/机制**：pty master 关闭会把 SIGHUP 发给前台组，所以「reader 已 EOF 但进程仍活着」的测试必须让进程 `trap '' HUP`；`waitid(WNOWAIT)` 期间僵尸的 pid 就是 pgid，不能被复用，`killpg` 因此可证安全；`notify_waiters` 不存 permit，用 `notify_one` 才补上 kill 与 await 之间的窗口；`regex` 没有 look-ahead，键名/值形状必须在 Rust 里判断。

**验证**：`./scripts/ci.sh` 八道门全绿（26s）；`cargo llvm-cov clean --workspace && cargo xtask coverage` 全部门槛达标（capabilities 90.4/85、tools 93.1/85、daemon 90.9/80、kernel 95.6/85、protocol 93.3/80、store 93.8/85、llm 96.8/85、cli 85.0/60、testkit 91.8）。

**保留/未做**：macOS 的 post-exit group kill（没有 `waitid(WNOWAIT)` 的 nix 绑定，测试也只在 Linux 跑）；shell 写入仍在 checkpoint 之外（本轮按裁决只写文档）；跨 host 重定向需要新审批而不是自动跟随；进度脱敏的跨 chunk 残余；`setsid`/`set -m` 逃逸的孙进程仍杀不到（无 cgroup）。

### 2026-10-24 · Phase 4：PTY、shell、web_fetch、spill 与上下文预算

**交付**：`TerminalHandle` 换成流式形状（`next_chunk` / `wait` / `kill` / `release`，全部 `&self`，让消费者在一个 `select!` 里同时看输出与退出）；`TerminalOutcome` 合并成一路输出 + `dropped_bytes`（PTY 只有一个设备）。`LocalPty` 用 portable-pty 0.9 + nix：显式 cwd、CRLF 归一化、head 1 MiB + tail 7 MiB 捕获、进程组 SIGKILL、控制线程、退出后 250ms EOF 宽限、取消 watcher + Drop 双保险；非 unix 拒绝。`MemoryTerminal` 进 testkit。`shell`（`sh -c`、流式进度、120s/600s 超时、危险模式一次性批准）与 `web_fetch`（reqwest 0.13 + htmd、http(s)、5 MiB、30s/120s、二进制拒绝、Network 一次性审批）进 `code_tools()`；Code 绑 `LocalPty`，Chat 仍 `NoTerminal`。注册表在输出离开 backend 时统一脱敏 + spill（D12：64 KiB / `<data_dir>/tool-results/` / head-tail 16 KiB / 0600）。`[context] max_input_tokens`（默认 96000、下限 4096）+ `StoreHistory` 的裁剪（先削旧工具结果到 4000 字符、再整轮丢最旧 round）。

**偏离草图与理由（机制级）**：

1. **`release` 在 PTY 上就是 `kill`**。探针实测关掉 master/slave 后 500ms 子进程仍活着；「交回一个没人看管的活进程」不是所有权转移而是泄漏，`unified_exec`（M5）才有资格改这个契约。
2. **不用库的 `clone_killer()` 直接杀，改成一个控制线程同时负责 reap 与 signal**。库的 `kill()` 只发一个 pid（探针 5 个场景 3 个留孤儿），所以必须 `killpg`；而「另一个线程 wait 完、这个线程再 killpg」存在 pid 被 reap 后复用、误杀无关进程组的窗口。控制线程每 20ms 轮询 `try_wait`，只在子进程仍活着时发信号，reap 与 signal 永不并发。
3. **捕获是 head+tail 而不是纯环形**。构建日志的尾部解释失败，头部解释命令；纯尾环会丢掉命令本身，纯头会丢掉最后的错误。上限 8 MiB（1+7），超出部分计数并在结果里说明。
4. **`wait` 在子进程退出时返回，只给 EOF 250ms 宽限**。孙进程可以一直占着 pty，等 EOF 会挂在一个已经结束的命令上；捕获里已有的字节照常返回。
5. **spill 放在注册表，不放在每个工具**。这是「模型上下文」的问题，不是工具语义的问题；每个新工具自己封顶会各自漂移（今天 read_file 256 KiB、glob 1000 条、grep 200 条）。工具仍保留带内上限，注册表只处理「离开 backend 之后」。
6. **spill 目录是 `<data_dir>/tool-results`，不是草图写的 `~/.local/state`**。manager 已经持有一个数据根，多穿第二个 XDG 根不换来任何行为差异；目录名与现有 store/影子仓库同级，测试也只需要一个临时根。
7. **`web_fetch` 每次都要批准，不提供 `AllowAlways`**。D8 的 matcher 只有工具名，`AllowAlways` 会变成「允许所有域名」，比用户看到的宽；域名规则需要更细的 matcher，尚未存在。
8. **`WebFetch::new()` 自己装 ferritls provider**。reqwest 的 `rustls-no-provider` 在**构造 Client 时**就断言进程已有 provider，即使只发 http；daemon 启动时已装过，所以这是 no-op，tools-only 测试则靠它自洽。不引第二套 TLS 后端；`ferritls-rustls` 必须 `default-features = false`，否则 `simd` 拉进 nightly 的 `portable_simd`。
9. **Chat 不含 `web_fetch`**。ADR-0005 说 Chat 无审批，而 Network 级工具在 Chat 下没有应答方；capabilities §3 的三个处置里选「移出 Chat」，并在文档留痕（幅度不足以另立 ADR）。
10. **危险模式只给 `AllowOnce/Deny`，不给 `DenyAlways`**。规则的 matcher 是工具名，记住一次「拒绝」会拒掉以后所有 shell 调用，比用户正在看的命令宽。
11. **上下文裁剪是静默的，不做摘要**。compaction/摘要仍是 M5；v1 只保证请求不超预算、system prompt 与最新 user 不被删。估算器不是 tokenizer，`[context]` 是操作员的校正旋钮。

**实测发现（测试跑红/构建失败才暴露的）**：

- `shell` 的 `select!` 里 `wait` 排在 `next_chunk` 之前（`biased`）会丢掉进程退出前刚写的进度；单测 `a_command_streams_its_output_and_reports_the_exit_code` 抓到了，改成进度优先、`wait` 兜底。
- `regex` crate **不支持 look-ahead**，赋值脱敏原计划用一个 `(?=…)` 同时表达「值至少 8 位且含数字/大写」；改成先匹配、再在 Rust 里判断值形状。
- `reqwest` 的 `rustls-no-provider` 在 `Client::new()` 就 panic（见偏离 8），不是等到 HTTPS 请求。
- `trim_to_budget` 在 `iter_mut()` 循环里调 `estimate_tokens(messages)` 借用了同一向量；改成索引循环，借用在下标访问后即结束。

**验证**：`./scripts/ci.sh` 八道门全绿（toolchain fmt clippy build tests invariants doctests determinism，21s）；`cargo llvm-cov clean --workspace && cargo xtask coverage` 821 条测试后全部门槛达标（capabilities 90.1/85、tools 92.4/85、daemon 90.8/80、kernel 95.6/85、protocol 93.3/80、store 93.8/85、llm 96.8/85、cli 84.9/60）。新增测试：PTY 9 条（含进程组孤儿断言读 `/proc` 状态）、shell 9 条、web_fetch 8 条、脱敏 7 条、spill 5 条、上下文预算 5 条、`[context]` 配置 3 条、接缝级 2 条（真 `shell` 走 `LocalPty`、超大输出在注册表处落盘）。

**本阶段未做（各有归属）**：Windows PTY 子进程（Job Object/ConPTY）待 TUI 之后；`web_fetch` 域名规则；JS/cookie/charset；shell 的 stdin 仍是 pty、unified exec M5；`setsid` 逃逸孙进程（无 cgroup）；shell/web_fetch 的活 runtime e2e → Phase 7 场景 3–6；compaction → M5。

### 2026-10-24 · 评审⑧：并发读、编辑顺序与一批契约修复

**两个 Critical**：① `prompt` 的 session 快照在 turn 闸门外读，而 `set_mode` 在同一闸门内写库——并发时按旧模式装配，库说 code、runtime 仍是 chat 的工具表。改为闸门内重读并复核 generation。② `edit_item` 先 `rewind_code` 再 `edit_fork`，对 ToolCall/Checkpoint 行发 `edit + both` 会先回滚工作区、再返回 `NotEditable`，用户被告知失败但文件已经变了。改为新增 `SessionStore::ensure_editable`（与 `edit_fork` 共用一个 `ensure_editable_item`），restore 前先拒绝。

**生命周期与契约**：`TurnEnded` 现在调用 `PendingApprovals::cancel_session`，`DaemonApproval::request` 在 `in_flight` 为假时直接 deny——取消的 turn 不会在 `session/load` 里留下「idle + 待审批」，也不会为一次没跑的调用写持久规则。`set_mode` 装配失败时回滚模式行并广播 `SessionUpdated`，重试不会再被 `mode ==` 早退吞掉事件。`rolled_back` 改为「目标树 vs 工作区」的 tracked diff（含 rename 两侧）：原来的「commit 差异 ∪ 工作区漂移」既漏「快照之后、尚未入快照的写」，也误报「内容已经等于目标」的路径；`worktree_drift` 随之删除。导出要求绝对路径并回显 canonicalize 后的路径；`branch_delete` 补发 `SessionUpdated`（子树删除不动 head）；`ItemNotFound` 从 `SessionNotFound` 改映射 `InvalidRequest`。`Checkpoints::restore` 不再接 `RestoreOptions`（purge 硬编码 false，守卫与删除同层）；同一安全 commit 不重复插行；安全提交在 reset 前先挂 `refs/hatchery/safety`，重置中途失败仍可寻址。`SessionDeleteParams` 增加 `#[serde(default)] confirm`。

**测试**：补 `edit_item` 成功/失败顺序、`Both` 成功路径、purge 拒绝的「文件确实没删」、Code 腿只在 target 之后找 checkpoint、`set_mode` 活 runtime 重组装、`load` 的重连审批、删除确认、相对导出路径、分支删除事件，以及 `cancelling_a_session_drops_every_waiter_it_had`。capabilities 的「工作区已等于目标 → rolled_back 为空」也补了一条。

**验证**：`./scripts/ci.sh` 全绿；`cargo xtask coverage` 全部 gated crate 达标。`purge_untracked` 的清单/审批流程仍未做，归属不变（Phase 7）。

### 2026-10-22 · M2 Phase 3：会话级方法、rewind 三 scope 与重连审批

**交付**：全部协议方法接通（`SERVED_METHODS == m::ALL`，测试钉住），`hello` 广告 chat + code。`session/set_mode` 是重组装路径：turn 闸门内写行、取走旧 runtime 并 `shutdown`、按新模式重新装配，hub 不删所以订阅者不断流，随后发 `ModeSwitched` + `SessionUpdated`；turn 进行中回 `TurnInProgress`；Code 无工作区在写库前拒绝。rewind：Code 腿沿 `rebuild_chain` 向后扫第一个 `ItemKind::Checkpoint` 取 commit（不查 `checkpoints` 表），没有则 no-op；`Both` 先 restore 成功再 `switch_branch`；restore 的安全快照由 capabilities 在破坏前拍，daemon 的 `Checkpoints::restore` 补写 `checkpoints` 行（`item_id = NULL`，写失败只记日志）。分支 list/switch/delete、`edit_item`、rename/delete、`store/export_jsonl` 全部接线；除 list/export 外都在 turn 闸门内并在 turn 进行中拒绝。`SessionLoadResult.pending_approvals` 落地：`PendingApprovals` 的值从 sender 变成 `{session, request, sender}`，`for_session` 按 id 排序返回，答复即移除。

**`edit_item` 的归属查询**：给 `EditItemParams` 加必填 `session_id` 会让 `version_compat` 的「新增字段必须可选」检查红掉（实测），所以协议零改动，改为 store 新增 `session_of_item`——`edit_fork` 本来就要校验归属，答案一直在库里，缺的只是出口。

**实测修正：`RestoreReport.rolled_back` 原来会漏报**。端到端测试里文件确实被恢复，报告却是空列表：`restore_from` 只 diff「最后一个快照 commit vs 目标 commit」，而快照总在写**之前**拍，所以「最后一次快照之后、尚未入快照的写」只存在于工作区。这正是 Code rewind 的常见路径，报告说「什么都没回滚」而文件已被改写。改成 commit 级差异 ∪ `diff_tree_to_workdir_with_index` 的 tracked 漂移（不含 untracked，purge 单列），排序去重。

**`purge_untracked: true` 拒绝**：capabilities §2 要求审批 + 先列出待删清单，协议没有取清单的方法；静默删除用户未跟踪文件是 rewind 唯一不可接受的失败形态，所以 daemon 返回 `InvalidRequest`，而不是把 `false` 以外的值当成「用户已同意」。

**验证**：`cargo nextest` 全绿（manager 侧的 rewind/分支/模式用例、`pending_requests_are_listed_per_session_and_disappear_once_answered`、`the_session_surgery_methods_round_trip_through_dispatch`）；clippy `-D warnings` 干净。未做：`purge_untracked` 的清单/审批流程、rewind 与 `set_mode` 的活 runtime e2e（Phase 7 场景 3/4）。

### 2026-10-21 · M2 Phase 2：写路径、审批管线与硬门

**交付**：protocol 的 `ApprovalPreview::{Diff, Command, Excerpt}`（D14，结构化 hunk，`preview` 可选且缺省不序列化）、`ApprovalScope`/`ApprovalRule`/`ApprovalRuleId`、`approval/rules/{list,delete}` 及其 fixture；store 的 `record_approval_rule`/`approval_rules`/`delete_approval_rule`；tools 的 `write_file`、`edit`（精确单次替换）、`hard_gate`（`.env*`、`.git/hooks/**`）与 `preview`（`similar 2.7`，超限退 `Excerpt`）；capabilities 的异步 `Tool::needs_approval`、`ToolPolicy`、`ApprovalGate::request(request_id, …)`；kernel 的 `ApprovalNeed::{NotNeeded, Ask, Refused}` 与 `ToolHost::approval_for` 异步化；daemon 的 `approvals.rs`（pending 注册表、fail-closed gate、`GatedTools`、pump）、`[approval]` 配置与 `STRICT_KEYS`、按模式装配、prompt 的 `mode_code`/`tool_discipline`/`project_context`；testkit 的 `ScriptedGate`、`refusing()`、协议级 `answer_approvals`；`invariant_project_config_cannot_disable_hard_gates`。

**四处偏离，各有机制级理由**：① `DaemonApproval` 住 daemon 而不是 capabilities——它要 hub/session/generation，capabilities 看不到这些；capabilities 只留 trait。② kernel 的 `approval_for` 从同步 `Option` 变成异步三态：预览要读被覆盖的文件（走异步 fs 接缝），而 `DenyAlways` 要能在**不弹窗**的情况下拒绝。③ `matcher` 只有工具名，没有 path glob：UI 的「总是允许」只表达工具级作用域，路径规则的消费者还没出现（atomcode 的 `Edit(src/**)` 是手写权限块的形态，不是审批面板的产物）。④ 硬门只列工作区内可达的 `.env*` 与 `.git/hooks/**`；`~/.ssh`、`~/.config/hatchery` 在工作区外，`LocalFs` 在工具运行前就拒绝，写成审批门是**不可达的声称**。`builtin(spec, &backends)` 也没有按草图加 `backends` 参数：没有 per-tool 配置的读者，按 ADR-0009 不预造。

**请求形状的连带**：`ApprovalRequest` 新增可选 `preview`，旧 fixture 不变（缺省省略）；`ApprovalGate` 多了 `request_id`，因为 pending 必须登记在 kernel 将用来回答的那个 id 下。pending 在 publish **之前**登记，否则脚本化前端的微秒级回答会撞上「waiter 还没注册」并白等到超时。

**验证**：`./scripts/ci.sh` 八步全绿（toolchain fmt clippy build tests invariants doctests determinism，i18n 仍明示未设门禁）；全量 nextest 全绿（含新 invariant；fixture 更新随协议提交）。未做：daemon 全链路审批 e2e（需脚本化 provider 走工具调用，Phase 7 场景 6）、`TerminalBackend` 契约腿（M3）、`session/set_mode` 与 `hello` 广告 code（Phase 3）。

### 2026-10-21 · D10 的 Windows 栏顺延到 TUI 之后（用户裁决）

用户裁决：Windows CI 确实有问题，但可以等 TUI 做完之后再到 Windows 开发机上修理，现在先推进 Phase 2。

落档口径（roadmap 的 D10 行、Phase 4 行与风险 1 已同步）：**D10 仍未关闭**；顺延的是 Windows 那一栏的两件事——修 `spikes/pty` 里从未执行过的 PowerShell payload，以及在 Windows 开发机上取数。后果写明：Phase 4 的 `LocalPty` 只能按 Linux（+ 可跑的 macOS）结论实现，Windows 的 child 替换（codex 那条自带 ConPTY + Job Object 的路）**不在 Phase 4 范围内**；M2 的三平台 CI 里 windows-gnu 仍跑普通测试，只是不跑 PTY 探针。这不是砍范围而是排期，TUI 收口时这条要重新排期——顺延表刻意没有收它。

### 2026-10-08 · M2 Phase 1 顺带对账：七个 crate README 的层号还是 M0b 之前的

§3 的层号在 M0b 那次「protocol 沉淀为唯一最底层、其余各层顺次 +1」之后变过，但 crate README 结尾那行 `Workspace layer **LN**` 没跟着改，于是 10 个里有 7 个是错的（kernel L0→**L1**、llm/store/capabilities L1→**L2**、tools/acp L2→**L3**、daemon L3→**L4**；protocol 的 L0 与 cli/gui 的 D 本来就对）。Phase 0 那轮 README 对账改的是**内容声称**（工具数量、检查点是否已存在），没核这一行——它看起来像元数据，其实同样是会被引用的事实：读者据此判断「这个 crate 能不能依赖那个 crate」。已全部改正，与 §3 的图逐项核对。

同一轮里另修两处 README 的过期声称：`hatchery-llm` 写着「Chat Completions and Responses wires」，而 `wire = "responses"` 在 `provider.rs:92` 是 fatal 拒绝（Responses adapter 是 M2 后面的阶段）；`hatchery-tools` 写着 ban list「still has a `tokio::fs` / `tokio::process` hole, which M2 Phase 0 closes」，而 Phase 0 的结论是 `tokio::fs` 已补、`tokio::process` **故意不补**（没有 crate 开那个 feature，clippy 会回 "does not refer to a reachable function"，理由记在 `clippy.toml` 头部）。

### 2026-10-07 · M2 Phase 0：两处门禁口径不实已修，两条不变量的口径已更正

本日勘察记下的两处「门禁说自己跑了其实没跑」都修掉了：① `--profile invariants` 现在真有步骤（`scripts/ci.sh` 的 `invariants` 步），五条映射到不变量却缺前缀的测试已改名，改完逐条核对该 profile 的选中集合并全绿；② `check_i18n` 从「返回成功的空操作」变成脚本末尾一行明示未设门禁，`--help` 的步骤表照实写。细节见 worklog/testing.md。

**不变量 2 的边界写进 architecture.md §5**：本条管辖分支历史；请求最前面那条 system prompt 是可复现的派生态（嵌入模板 + 装配时冻结的运行时事实），不是 item，由 `prompt/render` 钉——它对活着的 runtime 返回装配时冻结的那一份（D15，落地见 worklog/daemon.md）。这不是给不变量开口子：分支历史的逐字节断言强度没变，而 system prompt 现在有了一个比「混进请求体比对」更强的钉子（透明性 API 必须与模型实际收到的字节相同）。

**不变量 4 的 §5 描述更正为真实机制**：原文写「CI 中用 lint（如 `#![deny(clippy::disallowed_methods)]`）强制」，而 crate 属性压不过 Cargo 的 lint 表（2026-10-01 实测），真正生效的是 `hatchery-tools` 自带的完整本地 `[lints]` 表 + 根目录 `clippy.toml` 的禁令表；禁令表本日补齐 `tokio::fs` 孪生项（见 worklog/capabilities.md）。

**分层未变**：Phase 0 只动 daemon 内部装配与门禁配置，没有跨层新边；删掉 daemon → `hatchery-acp` 的死依赖边后 `cargo xtask layering` 仍报 strictly downward、无环。`./scripts/ci.sh` 全绿。

### 2026-10-07 · M2 重新规划：跨方向的勘察更正

M2 开工前做了一次全仓库勘察（三路并行：capabilities/tools、kernel/store、daemon/cli/protocol/tests），逐条对着代码复核后重写了 roadmap 的 M2 段（Phase 0–8 + 决策点 D8–D18 + 顺延表）。方向内的细节在各自的 worklog，这里只记跨方向的部分。

**门禁有两处口径不实（Phase 0 修）。** ① roadmap 的 M0 DoD 写「nextest 默认组与 **invariants 组**」，而 `--profile invariants` 在 `scripts/ci.sh`、`pr.yml`、`nightly.yml` 里**从未被调用过**——ci.sh:134 只跑 `--profile ci`，nightly 只跑 slow 与 gui。不变量测试确实进了 PR 门禁（`ci` 继承 `default`，过滤器只排除 live/slow/gui），但那是巧合而不是设计：没有一个可单独报告或阻塞的门禁，而 testing.md §5 映射到不变量、却没有 `invariant_` 前缀的 5 条测试（含 `two_concurrent_prompts_yield_exactly_one_turn`）一旦真去跑那个 profile 就会被静默丢掉。② `check_i18n`（ci.sh:121-124）是一条 `printf` 空操作（"i18n extraction check lands in M4 — nothing to verify yet"），一直被算作通过的门禁步骤。

**两条不变量的实际状态与文档不符。** 不变量 4 的编译期门禁有洞：`clippy.toml` 禁了 `std::fs::*` 与 `std::process::Command`，**没禁 `tokio::fs::*` 与 `tokio::process::*`**——而 hatchery-tools 依赖 tokio、`LocalFs` 自己就用 tokio::fs，所以工具里写一句 `tokio::fs::write` 就绕过了整条纪律（另漏 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`）。不变量 5 则**在任何形态下都没有测试**：testing.md 点名的 `invariant_project_config_cannot_disable_hard_gates` 全仓库零命中，唯一沾边的是 `RiskLevel::is_hard_gate()` 这个值类型谓词的两条单测，生产代码无人调用它——而「硬门测试全绿」正是 M2 的 DoD 之一。

**`hatchery-acp` 是死依赖边。** `crates/hatchery-daemon/Cargo.toml:21` 声明了它，daemon 源码零引用（`hatchery_acp` 在 `src/` 下零命中），crate 本体是 13 行文档注释 + 「implementation lands in M3」。`cargo xtask layering` 把它算作一条 build edge，于是分层契约在替一个不存在的接线背书。Phase 0 摘掉这条边（M3 真接线时再加回来）；这与 ADR-0009 的反预拆分刹车一致——依赖边也是预拆分。

**同类问题：三处文档在宣称不存在的东西。** daemon 的 `src/lib.rs:10` 与 Cargo `description` 都写「profile-based assembly」，而 `Profile` 类型不存在（grep 只命中那一条文档注释），design/daemon.md §3.1 的四个 profile 一个都没实现；`crates/hatchery-capabilities/README.md` 宣称有影子 Git 检查点与注册句柄；`crates/hatchery-tools/README.md` 列了七个工具（实际三个）。Phase 0 一并更正——「README 比代码乐观」是接手者最贵的那种误导。

**ADR-0006 的枚举拼写与协议不一致，按约定不改 ADR。** ADR-0006 写 `RewindScope::{Conversation, Code, ConversationAndCode}`，协议的实际变体是 `{Conversation, Code, Both}`（`hatchery-protocol/src/session.rs:289-300`，ADR-0003 与 design/protocol.md 的修正表用的也是 `Both`）。ADR 一经 accepted 不修改，**以协议为准**，此处留痕。

**文档计数纪律照 2026-09-30 的裁决执行：删数字，不改数字。** design/protocol.md §6、design/testing.md §3.1 与 worklog/protocol.md 里写的 fixture 数「62」已经过期（实数是 79），但按裁决这类数字本就不该出现在散文里——所以是把计数删掉，而不是更新成一个新的、下次照样过期的数。

**分层图在 M2 不变。** M2 不新增 crate（反预拆分刹车：`CheckpointStore` 住 capabilities，`DaemonApproval` 住 capabilities，工具住 tools，模式装配住 daemon，都是既有归属）。新增的外部依赖待各自决策点定：`portable-pty`（D10，spike 提前到 Phase 1 并行做；证据是 codex 用 `portable-pty = "0.9.0"` 且在 Windows 侧额外挂 `winapi` 的 jobapi2/Job Object 才能保证杀进程不留孤儿——那正是 windows-gnu CI 上「cancel 后无孤儿进程」的坑）、diff 库（D11，atomcode 用 `similar = "2"`）、web_fetch 的 HTTP client 与 HTML→Markdown（D17，注意 `wiremock` 是 testkit 专属、明确「never of a product crate」）。`git2` 已在 workspace 与 capabilities 的依赖里，只是 `src/` 下零使用——spike 的 11 项门槛是唯一消费者，Phase 1 转正。

**kernel 在 M2 只改一处。** `ToolInvocation { output, is_error }` 增加携带检查点的能力，kernel 在 ToolResult item **之前**追加 Checkpoint item（决策点 D13，理由与链形状见 worklog/kernel.md 与 design/storage.md）。这是自 M1 的 `TurnInput { turn, content }` 以来第一次动 kernel 的公开形状。另两个「M2 决定」的开放问题在本里程碑内**裁决为不做**：一轮多 tool call 并行（7 处结构阻碍 + 会打破三条确定性/审批测试）与 `max_tool_retries`（「可重试失败」还没有第一个消费者）。

**两处顺延（用户裁决）。** hub coalescing + replay window → M3：`is_coalescable` 只含 text/reasoning delta，而 M2 新增的事件量主要来自 `ToolCallProgress`（不可合并），coalescing 治不了 M2 的病；replay window 已被 `session/load` + `replay_from` 取代且有 e2e 覆盖。M2 只保留一次「Code 会话事件量测量」，那正是 `hub.rs:4` 原本要的东西。`SessionLease` 跨进程文件锁 → 多 daemon 形态出现时：`--embedded` 全仓库无实现，CLI 只有 attach-or-spawn，单实例 `daemon.lock` 已挡跨进程双 runtime，会话内由 turn 闸门 + 在途 CAS 标记承担且有不变量测试；跨会话共享影子仓库要的是 daemon 内 per-workspace 互斥（ADR-0006 已写明），不是文件锁。glossary 的 lease 条目随此更正。

### 2026-10-03 · CI 编译 flags 裁决收口 + worklog 对账

**CI 缓存污染的完整定案（用户裁决补齐根因）**。时间线：2026-09-30 在 pr.yml 设 `RUSTFLAGS=x86-64-v3`（提速实验，88f2561）→ 同日移除（6c4e005）；2026-10-01 注释掉 `.cargo/config.toml` 的 `-C target-cpu=native`（1c75e69）并给 Gate 加 `cargo clean`（4ae1042）。机制：仓库配置对 CI 同样生效，native/baseline+ 的缓存产物只对构建它的那颗 CPU 合法，rust-cache 把它们恢复到不同世代的 runner 上就出非法指令。`cargo clean` 是当时有效的应急，但让 rust-cache 整套缓存白干。**最终形态**：仓库与 CI 一律零 flags（可移植为默认）；本地想提速把 flag 写进**用户级** `~/.cargo/config.toml`（CI 永远读不到它，而 runner 缓存只由 CI 自己的构建写入，故不可能再污染）；加固轮删掉的 `cargo clean` 维持删除，缓存价值恢复。无需手工失效缓存：`add-rust-environment-hash-key: "true"` 把 `.cargo/config.toml` 内容与 RUST*/CARGO*/CC_* 环境都算进 key，任何 flags 变更即换 key。`.cargo/config.toml` 与 pr.yml 的注释已按此改写；上面 M0 那条「已实测进入 rustc 调用」的记录保留为历史。

**worklog 对账**（M1 代码完成后的全表核对）：勾掉已完成但仍开着的框——本文件（glossary/MSRV job/hatchery-tests）、platform 的（配置分层/prompt 管线/prompt-render/git plumbing/glossary）、llm 的 doctor 子命令；改里程碑标注（附日期与理由）——AGENTS.md 注入、上下文组装 v1、hub coalescing 调参 → (M2)，store 只读连接池 → (M3)；修正 platform.md 过期的「未实现」现状句、cli.md 重复的两组待办框、nightly.yml 的 TODO(M1) fuzz 注释（worklog 中 fuzz 本就标 M2）。kernel 的「max_rounds 在真实对话下验证」框留待 live 验收后勾。

### 2026-09-30 · M0b 评审后的两轮修复

M0b 的三个 crate 按范围分块评审了一遍（protocol 源码 / protocol 测试 / kernel 源码 / kernel 测试与 testkit / store 源码 / store 测试 / 跨 crate 一致性），约 50 条发现逐条对着代码复核后分成四组：① 协议健壮性，② 行为与契约缺陷，③ 测试可信度，④ 打磨与前瞻。用户裁决「②③ 全修，④ 留到 M1」（2026-09-29 落地），随后又裁决「④ 里能修的都修掉」（2026-09-30 落地，本条）。crate 内的细节在各自的 worklog，这里只记跨方向的部分。

**四个跨 crate 的决策**

1. **接缝枚举不加 `#[non_exhaustive]`**（`KernelEvent`/`AgentCommand`/`StreamEvent`/`TurnCompletion`/`TurnState`）。消费方全部在本 workspace 内，穷尽匹配正是「加一个变体时编译器替你找齐所有消费方」的机制；加上它只会逼出 `_ =>`，把新变体藏起来。wire 层的开放性由 protocol 承担，而且规则更强：枚举值在 major 内冻结、未知值一律硬失败（design/protocol.md §6）。反方理由是「第三方前端会 match 我们的类型」，但按同一条冻结规则，加变体本身就是 major bump，`#[non_exhaustive]` 换不来在 minor 里加变体的自由。
2. **LLM 接缝改借用**：`chat_stream(&ChatOptions, &[Message])`（原为按值）。adapter 在发起请求时就地把两者序列化进 body，返回的流仍是 `'static`，所以按值传只是让长会话每轮多拷一份全量上下文。**在 M1 写 adapter 之前定案比之后改便宜**——这是本轮唯一一处主动改动公开签名。
3. **显式 `"id": null` 一律拒绝**（`FrameError::NullId`），与「字段缺失 = 通知」严格区分（worklog/protocol.md）。
4. **`ItemIdRange` 不提供成员判定**：span 是位置语义，只有持有链的一方能解析（worklog/protocol.md）。解析函数留到 M5 与它的第一个消费者一起落地，避免又造一个没有调用方的 API。

**分层契约**：`cargo xtask layering` 现在检查全部三种依赖（normal / build / dev）。构建图 = normal + build，要求无环且严格向下；dev 边也必须向下，只有指向 Dev 层 crate 的例外（kernel 的 dev-dependency 指向 testkit，而 testkit 正常依赖 kernel，这条边不参与环检测）。

**文档计数纪律（用户裁决）**：README、roadmap 与设计文档不再写测试数与 fixture 数——这轮修复本身就是证据：同一个 M0b 在四处写着 219 / 198 / 197 三个互不相同的数。数字只留在 worklog 的日期化条目里；覆盖率改由测试自己机器检查（protocol 的 fixture 覆盖率有三条测试兜着，见 design/protocol.md §6）。

**门禁（本轮实测）**：`cargo nextest list --workspace` 默认组共 258 项、全绿——protocol 109、kernel 46、store 73、testkit 5、capabilities 11、xtask 13、cli 1；另有 1 项 `#[ignore]`（崩溃测试重入的子进程入口），invariants 组 4 项，doctest 7 个。`./scripts/ci.sh` 全绿，`cargo xtask layering` 报 12 members、22 build edges + 8 dev edges，严格向下无环。三平台 CI 仍未看到 M0b 之后的代码（push 由用户执行）。

### 2026-09-28 · M0b（分层修正 + 三个 crate 落地）

**M0b 修正：把 protocol 沉为唯一最底层。** M0a 把 `hatchery-protocol` 与 `hatchery-kernel` 并列在 L0，layering 契约禁止同层横向依赖（`from_layer <= to_layer` 即违规）。但 protocol 的数据模型必须引用 `ToolOutput`/`ApprovalRequest`/`Content`/`Usage`——这些值既要进 wire、又被 kernel 与 capabilities 共用。两个选择：镜像约 10 个类型 + 在 daemon 里加一层翻译，或者让 protocol 成为共享词汇表。选后者：

- protocol = L0（共享词汇表 + wire 契约）；kernel = L1；llm/store/capabilities = L2；tools/acp = L3；daemon = L4；cli/gui = frontend。
- 连带改动：`xtask::layering::LAYERS`、`Layer` 枚举 + `L4`、8 个 crate 的 lib.rs 层号注释、architecture.md §3 的分层图与纪律条目。
- **ADR-0004 的「kernel 不得依赖 capabilities」不受影响**：那条边向上，仍然禁止。这条纪律的措辞也从「L0 反过来依赖 L1」改为「kernel 反过来依赖上层」。
- 没有新增 ADR：层号是 architecture.md 内部的表述，推翻记录留在本文件（依约定，ADR 才需要 supersede 链）。
- 用户四项裁决同时落定：① protocol 沉底（本条）；② store 只出有序 `Vec<Item>`（storage.md §4）；③ 审批由 kernel 发起、daemon 应答（kernel.md §7 / capabilities.md §1）；④ 崩溃测试用测试二进制自重入（testing.md 开放问题 1）。

**三个 crate 的实现**（各自的 worklog 有细节）：protocol 93 测试 + 62 golden fixture；kernel 40 测试；store 64 测试（含属性测试与 kill -9）。`cargo xtask layering` 报 12 members / 22 edges，严格向下。

### 2026-09-28 · M0a
- 项目启动设计：深读四款参考项目（分析结论存 ../references.md），与用户对齐 8 项关键决策（ADR-0001~0008），产出 architecture/roadmap + 9 份方向设计文档 + 本 worklog 体系。
- 用户明确的核心差异化诉求：完整 ACP（含 fs/terminal 委派，atomcode 的反面教材）、reasoning_content 可配置回传、历史可编辑（分叉+删除）、提示词透明、CLI+GTK 双前端。
- 补充测试体系设计（用户要求「详尽的测试，确保所有代码都能如期运行」）：新增 design/testing.md + worklog/testing.md；crate 清单加入 dev-only 的 `hatchery-testkit`；architecture.md 不变量节与 roadmap DoD 挂接测试文档。
- dsh/Cordis 模块化二次深读 → **ADR-0009**（吸收五条语言无关纪律，拒绝运行时机制）；第三方扩展面 = MCP + ACP，WASM 工具插件列 M5 评估占位。
- **M0 细化规划**（用户四轮裁决）：M0 拆 M0a/M0b；CI 用 bash 编排（`scripts/ci.sh` 本地=CI 同一条命令）+ xtask 只做专属检查；PR 与 main 都跑三平台，Windows 走 **MSYS2 ucrt64 + windows-gnu**；ItemId = **UUIDv7**；三平台跑全量 default 组 + 强缓存；dev 分支分阶段 commit 不 push。
- **M0a 执行**（6 个 commit）：脚手架 + 门禁 + CI；存储引擎 spike → **ADR-0010**；影子 Git spike → 定 CLI；i18n spike → **ADR-0011**；MSRV 实测 1.90；文档同步。
- M0a 修正的两处设计矛盾（都是脚手架一落地就暴露的）：
  - **「12 个 crate」与 architecture.md §3 只列 11 个不符** → 定为 **11 crate（10 产品 + testkit）+ xtask = 12 member**；配置/提示词代码落 daemon（前端经协议访问，无第二个消费者 → 按 ADR-0009 反预拆分不新建 `hatchery-platform`/`hatchery-prompts` crate）。
  - **kernel(L0) 与 capabilities(L1) 依赖环**：原设计里 kernel 的 `ToolCtx` 直接引用 capabilities 的 `FsBackend`/`TerminalBackend` → 改为 kernel 只暴露窄接口 **`ToolHost`**（snapshot/approval_for/invoke），`Tool`/`ToolCtx`/三个 backend trait 全部归 capabilities；`ToolDef`/`ToolOutput`/`ToolProgress`/`ApprovalRequest` 留 kernel（组装 LLM 请求与投影事件要用）。`cargo xtask layering` 的 LAYERS 表把这条规则变成机器检查。
- 术语统一：wire 类型 `Thread` → **`Session`**（与方法名 `session/*`、表名 `sessions` 一致），避免一物两名。
- **用户三项裁决后的第二轮**（M0a 收尾）：
  - 影子 Git 后端从 CLI git 改为 **git2（vendored libgit2）** → 新增 **ADR-0012**，理由是可用性（很多用户机器没有 git）；11 项门槛在 git2 上重测全绿，热路径反而快约 2 倍。同时更正第一轮的错误记录「git2 需要 cmake」（实测 libgit2-sys 用 `cc`，不用 cmake）——教训写进 design/testing.md §0.2：**自己写下的结论也要复核**。
  - CI 工具链一律 `cargo install --locked` 源码编译（pr.yml 三平台 + nightly 的 audit/llvm-cov），不再下载预构建二进制。
  - `.cargo/config.toml` 加 `-C target-cpu=native`（实测已进入 rustc 调用）；连带纪律：发布产物与交叉编译必须用 `RUSTFLAGS=""` 覆盖，README 与 docs/README.md 都写明了。
  - 运行时依赖变化：不再需要用户装 git；构建期改为需要一个 C 编译器。README、worklog/daemon.md（删掉 `git --version` 审计项）、worklog/platform.md（构建代价对照行）已同步。
- **CI 第 1 次实跑（用户 push 后）失败并修正**：windows job 的 `rustup-init --component rustfmt clippy` 不是合法参数（实测 1.29.1：该选项只接受单个逗号分隔值，且 `--default-toolchain none` 时组件被静默忽略）。改成 `rustup set default-host` + `rustup toolchain install "$channel-$host" --component rustfmt,clippy` + 「active toolchain 必须是 windows-gnu」的断言——顺带堵掉两个同源隐患：热缓存下 `command -v rustup` 命中镜像自带的 msvc rustup 会跳过安装、并让 job 悄悄按 MSVC 编译（与 ADR-0012 的平台决策相反）。README 里给开发者的同一条命令也改了。验证与教训：worklog/testing.md「实测记录 · CI 首跑」。
