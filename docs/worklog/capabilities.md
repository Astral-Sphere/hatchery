# 工作记录：能力接缝 / 工具 / 审批 / 回滚（hatchery-capabilities, hatchery-tools）

- 范围：Fs/Terminal/Approval trait 与本地实现、影子 Git、内置工具、安全硬门、模式装配
- 设计文档：[../design/capabilities.md](../design/capabilities.md)
- 相关 ADR：0004、0005、0006、0009、**0012**（影子 Git 后端）；构建依赖取舍另见 0010/0011

## 当前状态

设计稿完成 + **影子 Git spike 实测两轮**（结论见 ADR-0012）+ **M1 只读层落地（2026-10-01）**：接缝 trait、LocalFs 只读路径、Chat 三工具与 ToolRegistry 全部可用，`./scripts/ci.sh` 全绿。

**M2 已于 2026-10-07 重新规划**：[roadmap](../roadmap.md) 的 M2 节是本方向 M2 范围的权威——Phase 0–8、决策点 D8–D18、16 条带实证的「勘察更正」。下面的待办已按它重挂 Phase。原 M2 段是按 M0/M1 的自述写成的，勘察推翻了其中对本方向最要紧的一条假设：**两个 crate 里零 stub**（无 `todo!()`、`unimplemented!()`、FIXME、占位返回），所以 M2 不是「填已搭好的骨架」而是**从零建**——写路径、PTY、审批后端、`CheckpointStore`、模式装配在代码里**完全不存在**，不是留了坑。勘察同时查出两处 trait 形状与设计草图不符（`FsBackend` 的路径是 `&str` 且无 `line_range`；`TerminalHandle` 无输出流、无 `release`），以及一处会破坏不变量 2 的设计错误（凭据「脱敏入库」），全部记在本日变更日志里。

## 待办

- [x] (M0) **git spike（实测）**：两轮——CLI git 2.55 与 git2 0.21（vendored libgit2 1.9.7）；最终选 git2，结论落 ADR-0012 + design/capabilities.md §2
- [x] (M1) trait 定型（M1 只读子集；见 2026-10-01 变更日志的三处有意收窄）+ `ToolRegistry` 实现 kernel 的 `ToolHost`（`snapshot` 排序快照 / `summarize` 委托工具 / `approval_for` 委托 / `invoke` 经 ToolCtx）
- [x] (M1) LocalFs 只读路径 + read_file/glob/grep 工具（Chat 模式用；`chat_tools()` 装配清单随工具走）
- [x] (M2 · Phase 0，2026-10-07 完成) **`clippy.toml` 的 tokio 洞**（不变量 4 的编译期门禁，故记在本方向）：现只禁 `std::fs::*` 与 `std::process::Command`（+`abort`、`env::set_var`），**没禁 `tokio::fs::*` / `tokio::process::*`**——而 hatchery-tools 依赖 tokio、`LocalFs` 自己就用 tokio::fs，工具里写一句 `tokio::fs::write` 就整条绕过。另补漏掉的 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`（roadmap 更正 8）。**结果**：`tokio::fs` 的全部孪生项与那四个 `std::fs` 项已加并逐条实测；`tokio::process` 那组**故意没加**（没有 crate 开那个 feature，clippy 会回 "does not refer to a reachable function"，实测不触发 `-D warnings` 失败但每次门禁留五条警告）。细节见本日变更日志与 clippy.toml 头部
- [x] (M2 · Phase 0) **两个 README 对账**（roadmap 更正 14）：原 `crates/hatchery-capabilities/README.md:3-5` 把影子 Git 检查点与「带注册句柄的工具注册表（ADR-0009）」写成既有，原 `crates/hatchery-tools/README.md:3-4` 列七个工具（实际三个）——都不成立。**2026-10-07 已随本轮文档对账改正**：capabilities 的 README 现在只声称 M1 只读切片（并写明 `ApprovalGate` 只有 trait、无实现，检查点/写路径/PTY/`DaemonApproval` 属 M2，注册句柄顺延 M5）；tools 的 README 只列 `read_file`/`glob`/`grep`，其余标 M2/M3/M5，与 src/lib.rs:45 的口径一致。Phase 0 剩下的文档对账（更正 16 那批）属其他方向
- [ ] (M2 · Phase 1) **CheckpointStore**：把测试里的 `Sandbox` 提炼成正式实现——open 配方（init_opts + 手写 `core.worktree`/`core.bare` + `set_workdir(.., false)`）、`harden()` 的配置钉扎、每次打开重放 ignore 规则、purge 走 `checkout_index(remove_untracked)`、restore 前自动 snapshot。**可提炼的材料至今只存在于 spike 的私有 `Sandbox` 里**（`crates/hatchery-capabilities/tests/spike_shadow_git.rs`，全绿）：`open_shadow()`（:96-146）、`harden()`（:73-94）、per-open ignore 重放（:130-138）、`snapshot()`（:255）、`restore(to, purge)`（:296-318）、`changed_paths()`（:320）、`shadow_dir_bytes()`（:346）；任何 `src/` 下都**没有** `CheckpointStore`。`git2` 已声明在 capabilities 的 `[dependencies]` 却无任何 `src/` 文件使用它（产品侧唯一的 git2 用法在 hatchery-daemon/src/prompt.rs:133-160）
- [ ] (M2 · Phase 1) **D13 检查点如何成为 item**：`ToolCtx` 加检查点收集器（`LocalFs` 写前 push）→ `ToolInvocation` 带出 `Vec<Checkpoint>` → kernel 在 ToolResult item **之前**追加 Checkpoint item（链成 `… → ToolCall → Checkpoint → ToolResult`；工具结果靠 `ToolResult.call` 配对而非父子关系，故此顺序安全）→ daemon 的 HubSink 在 item 落库后补写 `checkpoints` 行，行写失败只记日志（item 里已有 commit_id 可回退）
- [ ] (M2 · Phase 1) **`FsBackend` 加写原语**：`write_text_file` + **`create_dir` / `remove`**（write_file 要建目录、rewind 的 purge 要删）——三者今天**都不存在**；`LocalFs` 写路径写前打检查点，`MemoryFs` 同步（trait 一加方法它就编译不过：forcing function，也是工作量）
- [ ] (M2 · Phase 1) 预算熔断与 GC（**D9** 超预算行为：GC 最旧 vs 拒写）：`revwalk` 计数 + 影子 git-dir 体积求和（实测 5 次快照 = 3457 B，阈值逻辑与 GC 策略待定）
- [ ] (M2 · Phase 1 内并行做，实现落 Phase 4) **D10 PTY spike（提前，产 ADR）**：必须在三平台 CI 上实测**杀进程不留孤儿**（testing.md 要的 `kill -0` 断言）、取消、输出流。证据（读 references 得来，非本机实测）：`portable-pty` 目前**既不在** workspace `[workspace.dependencies]` **也不在** `Cargo.lock`；codex 用 `portable-pty = "0.9.0"`（references/codex/codex-rs/Cargo.toml:422），且 Windows 侧额外依赖带 `jobapi`/`jobapi2` feature 的 `winapi`（references/codex/codex-rs/utils/pty/Cargo.toml）——即 Job Object，杀 PTY 不留孤儿孙进程正需要它。这就是 windows-gnu CI job 上那条测试的风险点
- [ ] (M2 · Phase 2) `write_file` / `edit` 工具（`edit` = 精确 old/new 字符串替换，本方向开放问题 4 已裁决）；写前检查点由 Phase 1 的 `ToolCtx` 收集器带出
- [ ] (M2 · Phase 2) **`Backends` 扩字段**：现为 `Backends { fs, terminal }`（src/registry.rs:25），无 `approval`、无 `checkpoint`——`checkpoint` 随 Phase 1 的写前打点进来，`approval` 随 Phase 2 的审批管线进来
- [ ] (M2 · Phase 2) DaemonApproval + approval_rules 持久化 + 硬门测试（断言项目配置不可关闭）。起点比原以为的更空：`ApprovalGate`（src/approval.rs:18-21，`async fn request(&self, request: ApprovalRequest) -> ApprovalOption` 就是整个文件）在**全 workspace 零实现**，连 testkit 假件都没有（`hatchery_testkit::Gate` 是无关的信号量包装）。规则求值语义 = **D8**（scope/matcher/decision 文法、求值顺序、默认策略）；`approval_rules` 表已在 v1 schema 里但零 Rust 代码，所以**不需要新迁移**，缺的只是 API 层（roadmap 更正 3）；规则的 list/delete 协议方法同样缺——一条误存的 `DenyAlways` 会永久废掉一个工具且无法撤销。fail-closed 超时住在 gate 实现里，不在 kernel（M0b 已定）
- [ ] (M2 · Phase 2) **D14 审批预览载荷**：建议给 `ApprovalRequest` 加可选结构化 preview（`UnifiedDiff | Command{argv,cwd} | Excerpt`）而不是新开 `approval/details` 方法——M3 的 ACP `request_permission` 要同一份内容，放请求里一次到位。动因：`ApprovalRequest` 今天只有 `args_digest: String`（其文档明写不是原始 JSON），装不下 diff 也装不下完整命令，而 write/edit 的审批必须给人看 diff
- [ ] (M2 · Phase 2) **路径硬门**：今天只有工作区逃逸门（读侧）；`~/.ssh`、`~/.config/hatchery`、`.git/hooks`、`.env*` 无任何代码检查，无「规则不可 allow-always」的强制，无路径模式表。表达方式已定（roadmap 更正 7）：**不新增 `RiskLevel` 变体**（新增枚举值属协议 major bump）——`ApprovalRequest::once_only()` 已用「不提供 always 选项」表达不可记忆，路径门只需能为**工作区内**的敏感路径强制 `once_only`。必须这样绕的原因：`RiskLevel::is_hard_gate()`（crates/hatchery-protocol/src/approval.rs:34）只认 `WritesOutside`，而 `.env*` 与 `.git/hooks` 在工作区**内**，风险级本身表达不了它们
- [ ] (M2 · Phase 2) **契约测试套件**：`FsBackend` / `TerminalBackend` / `ApprovalGate` 各一套。这是「换绑定不换工具」在没有 ACP 协议时**唯一可机器验证**的形态；绑定矩阵的 ACP 行留作有文档的接缝，真验证在 M3
- [ ] (M2 · Phase 2) **模式装配**：`assemble(mode, backends)` + `builtin(spec, &backends)` + `ToolPolicy`——三者**全仓库零命中**。实际装配是 `SessionManager::chat_tools()`（crates/hatchery-daemon/src/manager.rs:612-628）内联造 `LocalFs` + `NoTerminal` 再循环 `hatchery_tools::chat_tools()`；而 `SessionManager::assemble`（manager.rs:523）**从不读 `session.mode`**，所以 code 会话今天拿到的是与 chat 完全相同的只读三件套 + `NoTerminal`
- [ ] (M2 · Phase 4) **`TerminalHandle` 加输出流 + `release`**：现仅 `async fn wait(&mut self) -> Result<TerminalOutcome, TermError>` + `fn kill(&mut self)`（src/terminal.rs:38-48），**没有输出流就没有 `ToolCallProgress`**，shell 工具的进度上报因此无从谈起。`TerminalBackend::create(spec, cancel) -> Result<Box<dyn TerminalHandle>, TermError>`（src/terminal.rs:82-92）唯一实现是 `NoTerminal`（src/terminal.rs:65-79），`TerminalHandle` 自身**无任何实现**、`LocalPty` 不存在——所以现在改 trait 最便宜。src/terminal.rs:34 那句「M1 形状是 M2 shell 工具所需的最窄物、流式只是 extend 不是 replace」对流式**已知不成立**，注释随实现一起改掉
- [ ] (M2 · Phase 4) LocalPty + shell 工具（输出环形缓冲、超时杀、危险命令模式表）——库选型按 D10 spike 结论
- [ ] (M2 · Phase 4) web_fetch + spill + 凭据脱敏。**脱敏位置更正**（design §5 同步改）：不是「脱敏入库」，而是**在接缝处、工具输出离开 backend 时**脱敏，让模型、存储、事件看到同一份字节；工具**参数**不在存储里脱敏（同样破坏不变量 2），改为「检出凭据 → 触发审批/告警」。现状：脱敏**完全不存在**——全仓库 `redact` 只有两处实质命中（docs/design/capabilities.md §5 的 M2 计划文本本身、xtask/src/record.rs:11 的无关注释），唯一的凭据检测代码是 `xtask/src/record.rs:450` 手写的 `sk-` 前缀 + 16 字符扫描，在 dev 工具里、不是可复用层；也没有任何熵值/脱敏 crate 在依赖里
- [ ] (M2 · Phase 4) **spill 的「谁来 spill」尚未裁决**（工具 vs 注册表）：protocol 侧值类型已在（`ToolOutput { text, artifacts, spilled }`、`SpilledOutput { path, bytes }`，crates/hatchery-protocol/src/tool.rs:52-127），但**全仓库无人构造 spilled 输出、无人写 `~/.local/state/hatchery/tool-results/`**；今天是工具在带内封顶（read_file 256 KiB 字节帽、glob 1000 条、grep 200 条）。阈值与落盘位置 = **D12**
- [ ] (M2 · Phase 4) **D17** web_fetch 的 HTTP client 与 HTML→Markdown 选型（= 本方向开放问题 3）：产品 crate 今天**没有任何通用 HTTP client**（`reqwest` 仅经 `openai-interface` 传递进 hatchery-llm；`wiremock` 是 testkit 专属且明写「never of a product crate」），也**没有任何 HTML→MD crate**（无 `htmd`/`html2md`）
- [ ] (M2 · Phase 7) disallowed_methods 的 compile-fail 测试（M0a 已人工实测 lint 生效，见 worklog/testing.md）；`trybuild` 尚未在任何依赖表里
- [ ] (M2 · Phase 7) 补 `walk.rs` 的测试洞：注释声称「不可读子目录跳过并计数」由真盘测试钉住，而 `crates/hatchery-tools/tests/assembly.rs` 里**没有** 0 权限目录的测试
- [ ] (M3) 与 AcpClientFs/AcpClientTerminal 的绑定矩阵联测
- [ ] (M5) WASM 工具插件评估（wasmtime + WASI；仅评估，实施须过新 ADR——ADR-0009 占位）

## 实测记录 · 第二轮：git2 0.21.0 + vendored libgit2 1.9.7（2026-09-28，Linux x86_64 / 24 核）

`cargo nextest run -p hatchery-capabilities --nocapture` 可复现；11 项全绿。所有仓库都在 tempdir 里，且显式钉住 `core.autocrlf`/`core.excludesFile`/`core.fsmonitor`/身份并关掉外部模板，因此不读也不写开发者自己的 git 配置。

通过的门槛：

- **不变量 6**：快照 + 硬恢复全程，用户仓库的 HEAD / 分支 / refs / `.git/index` mtime / `.git` 目录条目全部不变，用户的 staged/unstaged/untracked 内容原样保留。
- **不会在工作区里种 `.git`**：`no_gitlink_is_planted_in_the_user_workspace` 两种场景（工作区本来不是仓库 / 本来就是仓库）都验证过。
- **读用户仓库状态是安全的**：`statuses()` 实测**不重写**用户的 `.git/index`（CLI 的 `git status` 会）。
- 忽略规则生效（`big/`、`*.blob` 不进快照，文件本身不删）；`.git/` 恒被排除。
- `diff_tree_to_tree` + `stats()` 给出改动文件与增删行数（GUI diff 视图够用）。
- `revwalk` 数快照数、目录遍历求影子仓库体积（预算熔断的两个输入）。
- 非 git 工作区照常可用。
- purge 语义：`checkout_index(force().remove_untracked(true))` 确实删掉从未被快照跟踪的文件；默认路径不删。

性能（实测）：

| 操作 | git2 | 对照：CLI git（第一轮） |
|---|---|---|
| 冷快照 500 文件 | 48.9 ms | 24.8 ms |
| 热快照（10 处改动） | **6.0 ms** | 12.6 ms |
| 硬恢复 | **2.8 ms** | 5.9 ms |
| 5 次快照后影子仓库体积 | **3457 B** | 60 KiB |
| 构建代价 | 10.5 s（含 libz-sys + libgit2-sys + git2 + 本 crate） | 0 |

热路径（写前打点）快约 2 倍、体积小一个数量级（libgit2 不铺 hooks/模板）；冷快照慢一倍，但每个工作区只发生一次。

四个必须记住的 libgit2 坑（都已成为测试或写进 design/capabilities.md §2）：

1. `set_workdir(path, update_gitlink=false)` **只改内存句柄**；`core.worktree` 与 `core.bare=false` 必须自己写进 config，否则重新 `Repository::open` 后 workdir 丢失（第一轮跑出来 9/11 个测试在同一行断言挂掉才发现）。读 libgit2 `repository.c:3259` 确认：只有 `update_gitlink=true` 才写这两条 config，而那同一个 flag 会调用 `repo_write_gitlink`，在用户工作区里种 `.git` 文件——所以绝不能图省事用它。
2. `add_ignore_rule` 是 **per-handle** 的内存规则：在临时句柄上加的规则对下一次打开无效（实测：第一版测试因此失败）。CheckpointStore 每次打开都要重放规则。
3. `reset(Hard, remove_untracked)` **不会**删未跟踪文件（hard reset 的 checkout 只覆盖与目标有差异的路径）；purge 必须额外走 `checkout_index`。且不要加 `remove_ignored`，否则会删构建产物和用户的 `.env`。
4. libgit2 会读开发者的全局/系统配置与模板目录：`core.autocrlf` 和全局 `core.excludesFile` 会悄悄改变换行与快照范围。必须显式钉住（`harden()`）。

## 实测记录 · 第一轮：CLI git 2.55（已被取代，保留作对照与教训）

- 影子仓库（`--git-dir` + `--work-tree`）：不变量 6 成立；`info/exclude` 生效；`diff`/`rev-list`/`count-objects` 可用；非 git 工作区可用。
- **`git status --porcelain` 会重写用户的 `.git/index`**（tracked 文件 stat 过期时刷新缓存，实测连续两次都改写），而 `rev-parse`/`for-each-ref`/`log`/`ls-files`/`diff --stat` 都不改。当时据此定了「用户工作区只跑 plumbing」的纪律——**换到 git2 后这条纪律不再必要**（`statuses()` 实测不改 index），但结论本身对任何仍要调 CLI 的地方依然有效。
- `reset --hard` 不删未跟踪文件（与 libgit2 一致）→ 这条催生了 `purge_untracked` 选项（用户裁决：默认不 purge，显式 `--purge` 走审批 + 待删清单）。
- **更正一条错误记录**：第一轮我写下「git2 需要 cmake」。实测 libgit2-sys 0.18.8 的 `build.rs` 用 `cc::Build`（`add_c_files`/`add_pcre2_files`）自己编译 libgit2 与 pcre2，**不调用 cmake**（cmake 只出现在注释链接与 `#cmakedefine` 替换里）；当时探测机上 cmake 4.3.0 恰好在场，我把「在场」误当成「需要」。教训已写进 design/testing.md §0.2：自己写下的结论同样要复核。
- 否决 CLI 的真正原因不是性能（它冷快照更快），而是**可用性**：把 git 二进制变成运行时硬依赖，等于让 Code 模式的安全网在没装 git 的机器上静默失效。

## M0b 记录（2026-09-28）

capabilities 的**代码**在 M0b 没有动（trait 与工具实现是 M1/M2 的活），但它的设计落定了两件必须现在决定的事——kernel 落地的同时撞上了这两处接缝：

1. **值类型的归属**：初稿把 `ApprovalRequest`/`ApprovalOutcome`/`ToolOutput`/`ToolProgress` 写成 kernel 类型（M0a 修正）。M0b 发现它们既要进 wire 又要被 kernel 与 capabilities 共用，而同层横向依赖被 layering 禁止，于是统一搬到最底层的 **protocol**。`ApprovalOutcome` 并入 `ApprovalOption`（答复必然是被提供的选项之一）。kernel 只留 `ToolInvocation`（「跑失败」与「没跑成」的分野）。
2. **审批往返的分工**：**kernel 发起、daemon 应答**。kernel 发 `ApprovalNeeded { request_id, request }` 并等 `ApprovalDecision { request_id, option }`；daemon 收到事件后调用会话绑定的 `ApprovalGate`（`DaemonApproval` 弹前端 / `AcpPermission` 转发 `session/request_permission`），把结果作为命令回灌。`request_id` 与协议 `approval/respond` 一一对应；`ToolCtx` 里**没有** approval 字段（工具不请求审批，审批发生在工具被调用之前）；**超时 fail-closed = deny 住在 gate 实现里**，不在 kernel。

两条都写进了 design/capabilities.md §1 与 design/kernel.md §7。M2 实现 `DaemonApproval` 时按这份分工接。

## 开放问题

见设计文档末尾 4 条（glob/grep 在 ACP 会话的降级、unified exec、HTML→MD 选型、edit 格式）。解决过程记录于此：

- 2026-10-07 **开放问题 3（web_fetch 的 HTML→Markdown 选型）不再是开放问题的粒度**：升级为 roadmap 决策点 **D17**，与 HTTP client 选型合并成一个决策，落在 **M2 Phase 4**。理由是这两半不能分开选——产品 crate 今天既没有通用 HTTP client 也没有任何 HTML→MD crate，选型同时要决定新依赖面的大小。
- 2026-10-07 **开放问题 4（编辑工具格式）已裁决，不再开放**：v1 用精确 old/new 字符串替换（各家共识），M2 Phase 2 按此实现 `edit`；误配率观察后再议 diff/patch。设计文档的开放问题列表同步标注为已裁决。
- 2026-10-07 开放问题 1（glob/grep 在 ACP 会话的降级）仍挂 **M3**（要实测 Zed 行为）；开放问题 2（shell 会话复用 / unified exec）仍挂 **M5**，roadmap 的顺延表已收进去。
- 2026-09-28 rewind 的 purge 语义：默认不删未跟踪文件，`--purge` 显式开启且需审批 + 待删清单（用户裁决）。git2 下用 `checkout_index(remove_untracked)` 实现，已实测。
- 2026-09-28 影子 Git 后端：git2 vendored（用户裁决 + 第二轮实测），ADR-0012。若将来要摆脱 C 依赖，替代候选是 `gix`（纯 Rust，**未实测**），前提是把这 11 项门槛在 gix 上重跑全绿。

## 变更日志

### 2026-10-07 · M2 Phase 0：不变量 4 的编译期门禁补洞

`clippy.toml` 加了 `tokio::fs` 的全部孪生项（read/read_to_string/read_dir/write/copy/rename/remove_file/remove_dir/remove_dir_all/create_dir/create_dir_all/metadata/symlink_metadata/read_link/hard_link/set_permissions/File::open/File::create/OpenOptions::open）与漏掉的 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`。此前只禁 `std`：hatchery-tools 依赖 tokio，一句 `tokio::fs::write` 就绕过整条纪律。

**逐条实测过**，因为 clippy 对解析不到的路径是**静默忽略**而不是报错——打错一个路径就等于留一个洞，而看不出来。做法：临时给 hatchery-tools 开 tokio 的 `fs`/`process` feature，写一个 scratch 测试模块调用表里每一条路径，跑 `cargo clippy -p hatchery-tools --all-targets`，把诊断里点名的路径与 clippy.toml 的表做集合差；两边完全一致（表里每条都开火，没有多余项）。scratch 模块与 feature 改动随后撤掉，`git status` 里 hatchery-tools 干净。

**`tokio::process::Command` 的孪生项故意没加。** 没有任何 crate 开 tokio 的 `process` feature，clippy 对这五条路径回 `does not refer to a reachable function`。实测（`cargo clippy --workspace --all-targets -- -D warnings`）：退出码仍是 0——它是配置诊断不是 lint，`-D warnings` 管不到——但会在每次门禁输出里留五条警告，而「输出里有可以忽略的警告」正是让真警告被跳过的原因。等有 crate 开那个 feature（Phase 4 的 shell/PTY 若走 tokio::process）再加，届时它们会真正生效。理由写在 clippy.toml 头部，不是只写在这里。

**ADR-0012 引的测试名已过时。** `no_gitlink_is_planted_in_the_user_workspace` 于本日改名为 `invariant_no_gitlink_is_planted_in_the_user_workspace`（补前缀进 `invariants` 门禁组，同批还有 `purge_restore_also_removes_never_tracked_files`）。ADR 一经 accepted 不修改，所以 ADR-0012 正文里那个名字不再能 grep 到——映射记在这里，处理方式与 ADR-0006 的 `RewindScope::ConversationAndCode` vs 协议 `Both` 一致（以代码为准，ADR 不动，worklog 留痕）。

### 2026-10-07 · M2 重新规划对账

### 2026-10-07 · M2 重新规划对账

M2 按 [roadmap](../roadmap.md) 的新规划（Phase 0–8）重挂，本方向待办逐条对账。**代码未动**，以下是勘察结论，每条都可复核：

- **零 stub，所以 M2 是从零建不是填空**：两个 crate 里没有任何 `todo!()`、`unimplemented!()`、FIXME 或占位返回。凡标 M2 的东西**整体缺席**。这与 10-01 记下那条收窄理由一致——放一个 NotImplemented 的 `write_text_file`「只会撒谎」，当时选择不放；代价是今天的勘察不能靠 grep stub 找工作量，只能靠 grep 缺失。
- **`FsBackend` 与 ADR-0004 草图有偏差，且偏差已成既定事实**（src/fs.rs:58-80）：只有 `read_text_file` / `read_dir` / `metadata` 三个方法；路径是 **`&str` 且工作区相对**，不是 `PathBuf`；**没有 `line_range` 参数**（分页住在 `read_file` 工具里，不在接缝上）。design §1 的草图已按此更正。实现侧：`LocalFs`（src/local_fs.rs:118-186，读路径完整——两段门：先词法拒绝绝对路径与 `..`，再 `canonicalize` + 根前缀检查抓符号链接逃逸；首 KB 含 NUL 判二进制拒读）与 `MemoryFs`（hatchery-testkit src/fs.rs:117-190，刻意镜像 LocalFs 的拒绝行为）；`NoFs` 是 src/registry.rs:139-152 里的私有测试替身；`AcpClientFs` 属 M3。
- **`TerminalHandle` 撑不起 shell 工具**（src/terminal.rs:38-48）：只有 `wait() -> TerminalOutcome`（一次性全量输出）+ `kill()`，**无输出流、无 `release()`**——没有输出流就没有 `ToolCallProgress`。唯一实现是 `NoTerminal`（src/terminal.rs:65-79），`TerminalHandle` 自身全仓库无实现，`LocalPty` 不存在。**因为只有一个实现，现在改 trait 最便宜**（这条决定了它排 Phase 4 而不是更晚）。src/terminal.rs:34 现仍声称 M1 形状「是 M2 shell 工具所需的最窄物」、流式「extend 而非 replace」——**对流式已知不成立**，实现时连注释一起改。
- **`CheckpointStore` 的可提炼材料仍只活在 spike 的私有 `Sandbox` 里**：`tests/spike_shadow_git.rs`（744 行，全绿）已有 `open_shadow()`（:96-146）、`harden()`（:73-94）、per-open ignore 重放（:130-138）、`snapshot()`（:255）、`restore(to, purge)`（:296-318）、`changed_paths()`（:320）、`shadow_dir_bytes()`（:346）；`src/` 下无任何 `CheckpointStore`。`git2` 已在 capabilities 的 `[dependencies]` 里但**无任何 `src/` 文件用它**（产品侧唯一 git2 用法在 hatchery-daemon/src/prompt.rs:133-160）。
- **`ApprovalGate` 零实现**：src/approval.rs:18-21 就是整个文件（`async fn request(&self, request: ApprovalRequest) -> ApprovalOption`），全 workspace 找不到一个实现，连 testkit 假件都没有（`hatchery_testkit::Gate` 是无关的信号量包装）。
- **模式装配不存在，装配硬编码在 daemon 且不认模式**：`assemble(mode, backends)`、`builtin(spec, &backends)` 全仓库零命中，`ToolPolicy` 零命中。真实装配是 `SessionManager::chat_tools()`（crates/hatchery-daemon/src/manager.rs:612-628）内联造 `LocalFs` + `NoTerminal` 再循环 `hatchery_tools::chat_tools()`；`SessionManager::assemble`（manager.rs:523）**从不读 `session.mode`**，故 code 会话今天拿到的是 chat 的只读三件套 + `NoTerminal`。
- **注册句柄仍按记录不做**：`ToolRegistry`（src/registry.rs:33-133）对 M1 完整且已实现 `kernel::ToolHost`；`register()` 返回 `()`，`RegistrationHandle`/`dispose()`/`replace()` 不存在，理由照 src/registry.rs:4-6（有消费者再长，MCP 是 M5）。roadmap 顺延表已把这一条收进 M5。但 `Backends { fs, terminal }`（src/registry.rs:25）**必须扩**：加 `approval` 与 `checkpoint`。
- **凭据脱敏的设计写错了，会破坏不变量 2**：design §5 原写「脱敏入库」。若入库内容与模型实际看到的字节不同，e2e 的 `invariant_minimal_chat_replays_reasoning_byte_exact`（拿重建上下文与实际发出的字节对比）就会失败。**更正为：脱敏发生在接缝处、工具输出离开 backend 时**，模型/存储/事件三方看到同一份字节；工具**参数**不在存储里脱敏（同一个不变量 2 问题），改为「检出凭据 → 触发审批/告警」。真正的防线是让密钥在读取环节就进不了模型视野。现状：脱敏**完全缺席**——`redact` 全仓库两处实质命中（design/capabilities.md §5 的 M2 计划文本本身、xtask/src/record.rs:11 的无关注释），唯一的凭据检测代码是 xtask/src/record.rs:450 手写的 `sk-` 前缀 + 16 字符扫描，在 dev 工具里、不是可复用层。
- **不变量 4 的编译期门禁有洞**：`clippy.toml` 禁了 `std::fs::*` 与 `std::process::Command`（+`abort`、`env::set_var`），**没禁 `tokio::fs::*` / `tokio::process::*`**——而 hatchery-tools 依赖 tokio、`LocalFs` 自己就用 tokio::fs，工具里一句 `tokio::fs::write` 就绕过整条纪律。另漏 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`。修在 M2 Phase 0，但门禁本身属本方向的不变量，故记在这里。
- **两个 README 都在说谎**（roadmap 更正 14）：capabilities 的 README 把影子 Git 检查点与「带注册句柄的工具注册表（ADR-0009）」写成既有；tools 的 README 列七个工具，实际只有 `read_file`/`glob`/`grep`（装配清单 `chat_tools()`，src/lib.rs:81）+ `walk.rs`。两份都已改为与代码一致。
- **spill 无人裁决「谁来 spill」**：protocol 的值类型已在（`ToolOutput { text, artifacts, spilled }`、`SpilledOutput { path, bytes }`，crates/hatchery-protocol/src/tool.rs:52-127），但全仓库无人构造 spilled 输出、无人写 `~/.local/state/hatchery/tool-results/`；今天是工具带内封顶（read_file 256 KiB、glob 1000 条、grep 200 条）。阈值与落盘位置 = D12（Phase 4）。
- **M2 缺的依赖**（均不在 workspace deps 也不在 `Cargo.lock`）：`portable-pty`；任何通用 HTTP client（`reqwest` 仅经 `openai-interface` 传递进 hatchery-llm，`wiremock` 是 testkit 专属且明写「never of a product crate」）；任何 HTML→Markdown crate（无 `htmd`/`html2md`）；任何熵值/脱敏 crate；`trybuild`；`criterion`。另缺 diff 库：`git2` 能出检查点之间的 diff，但 `write_file`/`edit` 的**预览** diff（旧缓冲 vs 新内容，还没提交）无来源——D11 的证据是 atomcode 用 `similar = "2"`（references/atomcode/Cargo.toml:40）。
- **一个测试洞**：`crates/hatchery-tools/src/walk.rs` 的注释声称「不可读子目录跳过并计数」由真盘测试钉住，而 `crates/hatchery-tools/tests/assembly.rs` 里没有 0 权限目录的测试。挂 Phase 7。
- **依赖卫生（只记录，本轮不删任何东西）**：hatchery-capabilities 声明的 `serde` 与 `tracing` 在 `src/` 下无使用；hatchery-tools 声明的 `serde`、`thiserror`、`tracing` 同样在 `src/` 下无使用。

### 2026-10-01 · 评审⑤自查轮（capabilities/tools）

不变量 4 的真相与修复：09-28 记录的「crate 属性优先于命令行 lint level」实测不成立——workspace `[lints]` 的 allow 一直在压着它，禁令自 M0a 起实际未生效（用探针在 tools 里放 `std::fs::read_to_string` 复测，零告警）。修复：hatchery-tools 自带完整本地 `[lints]` 表（`workspace = true` 与本地表混用会被 cargo 拒载，报错还会指向无关 crate——这是本轮最贵的发现），两项 `disallowed_* = "deny"`；探针复测报错、干净树通过。tools 行为：read_file 的 `limit:0` 判 InvalidArgs、offset 越界与首行超帽各自具名（不再都读成 `[empty file]`/`lines 1–0`）；grep 超限文件跳过计数入摘要；walk 对不可读子目录跳过并计数（根目录失败仍致命）。capabilities：LocalFs 读目录判 WrongKind（与 MemoryFs 一致，此前是裸 EISDIR 字符串）、空路径拒读。新增 TempWorkspace 真盘集成三条（符号链接逃逸经工具层拒绝、自环不致命、glob/grep 走真磁盘）。计数见 worklog/testing.md 本日条目（capabilities 25、tools 38 项）。

### 2026-10-01 · M1 只读层（评审②）

**trait 定型（M1 子集）**：`FsBackend`（read_text_file / read_dir / metadata）、`TerminalBackend` + `TerminalHandle` + `TerminalOutcome`、`ApprovalGate`、`Tool`（def / needs_approval / summarize / execute）、`ToolCtx { fs, terminal, cancel, emit }`。三处**有意收窄**，都记录在案：

1. `FsBackend` 没有 `write_text_file`：写路径的真实形状包含「写前打影子 Git 检查点」，M2 与 CheckpointStore 一起定型，先放一个 NotImplemented 占位只会撒谎。
2. `read_dir` 提前进 M1（设计稿写「M2 加 list/glob 支持」）：glob/grep 必须有目录原语才能在接缝内行走，这是它的只读子集；设计稿说的模式过滤下推（把 glob 语义交给 backend）仍留 M2。
3. `Tool` 比设计稿多一个 `summarize`：摘要需要工具语义（`read_file a.txt` vs `grep todo`），注册表的通用回退只兜底未知工具。

**D6（glob/grep 选型）定案**：`globset` + `regex`（ripgrep 家族的纯匹配引擎）；**行走本身过 `FsBackend::read_dir` 接缝**（`hatchery-tools/src/walk.rs`，`.git` 不深入，MAX_VISITED 上限 10 万）。考虑过 `ignore`+`grep-searcher` 全家桶：它们自带 fs 行走，会绕开接缝（MemoryFs 测不了、ACP 委派换不了），否决。工具保持「纯逻辑 + 接缝调用」，deny lint 全绿。

**LocalFs 的安全设计**（读路径）：路径两段校验——先词法（拒绝绝对路径与 `..`，`./` 归一化），后 `canonicalize` 对根前缀（**符号链接逃逸在这里被抓住**，词法检查看不见它）；二进制文件（首 KB 含 NUL）拒绝而非转码。工作区外的读取：设计稿说「Executes 级审批」，那是 Code 模式（M2，配合 DaemonApproval）；**M1 Chat 按 ADR-0005 无审批**，越界读取直接以错误结果返回（模型能看到原因），否则会挂在无人应答的审批等待上。测试钉住：绝对路径/`..`/符号链接逃逸三类拒绝 + 二进制拒绝。

**ToolRegistry 的装配模型**：每会话一个、装配后不可变（kernel 每 turn 冻结快照，「换工具」= 下一 turn 换注册表）。设计稿的 `register() → Handle{dispose, replace}` 句柄模式**暂不实现**——它是给跨会话存活、需要原地换 MCP 工具的注册表用的（M5）；现在没有消费者，反预拆分刹车。`snapshot()` 按 BTreeMap 名字序输出：模型看到的目录表在两次装配间稳定，provider 侧 prompt 缓存不失效。

**MemoryFs 与 LocalFs 对齐**：`..`/绝对路径拒绝、NUL 嗅探拒绝二进制，行为一致——工具不能长出只在某个 backend 上成立的习惯（grep 的二进制跳过路径就是靠 MemoryFs 的 Binary 错误测出来的）。

### 2026-09-28 · M0b

- 代码未动，设计落定两处（详见上面「M0b 记录」）：值类型归 protocol（`ApprovalOutcome` 并入 `ApprovalOption`）、审批往返由 kernel 发起 / daemon 应答。design/capabilities.md §1 相应重写（新增两张表：值类型归属、审批分工）。

### 2026-09-28
- 初稿。核心设计输入：dsh capability seam 三角色模型 + atomcode 影子 Git 实现细节（预算熔断、RAII 补偿）+ atomcode ACP 缺 fs/terminal 的反面教材（ADR-0004）。
- ADR-0009 落地：ToolRegistry 采用注册句柄模式（`register() → Handle{dispose, replace}`，整表原子替换，与 kernel Turn Tool Snapshot 衔接）；工具表加 WASM 插件 M5 评估占位行；反预拆分刹车对 trait/包演进生效。
- **M0a 第一轮 spike**：CLI git 门槛全过，当时定为 CLI；design/capabilities.md §2 重写（`RestoreOptions`/`RestoreReport` + 实测纪律）。收获三条：`git status` 会改用户 index、`reset --hard` 不删未跟踪文件、不变量 6 的准确含义是「影子操作不改用户仓库状态」（agent 自己写的文件出现在用户 status 里属正常，第一版断言写宽了才暴露）。
- **M0a 第二轮 spike（用户裁决改 git2 vendored）**：新增 **ADR-0012**；11 项门槛在 git2 上重测全绿；design/capabilities.md §2 再重写（open 配方、`harden()` 配置钉扎、ignore 规则 per-handle、purge 走 checkout_index、性能数字）；worklog/daemon.md 删掉 `git --version` 审计项；README 的运行时依赖改为「构建期需要 C 编译器」。同时更正第一轮的 cmake 误记。
