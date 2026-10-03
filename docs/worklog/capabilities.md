# 工作记录：能力接缝 / 工具 / 审批 / 回滚（hatchery-capabilities, hatchery-tools）

- 范围：Fs/Terminal/Approval trait 与本地实现、影子 Git、内置工具、安全硬门、模式装配
- 设计文档：[../design/capabilities.md](../design/capabilities.md)
- 相关 ADR：0004、0005、0006、0009、**0012**（影子 Git 后端）；构建依赖取舍另见 0010/0011

## 当前状态

设计稿完成 + **影子 Git spike 实测两轮**（结论见 ADR-0012）+ **M1 只读层落地（2026-10-01）**：接缝 trait、LocalFs 只读路径、Chat 三工具与 ToolRegistry 全部可用，`./scripts/ci.sh` 全绿。写路径 / PTY / 审批后端 / CheckpointStore 是 M2。

## 待办

- [x] (M0) **git spike（实测）**：两轮——CLI git 2.55 与 git2 0.21（vendored libgit2 1.9.7）；最终选 git2，结论落 ADR-0012 + design/capabilities.md §2
- [x] (M1) trait 定型（M1 只读子集；见 2026-10-01 变更日志的三处有意收窄）+ `ToolRegistry` 实现 kernel 的 `ToolHost`（`snapshot` 排序快照 / `summarize` 委托工具 / `approval_for` 委托 / `invoke` 经 ToolCtx）
- [x] (M1) LocalFs 只读路径 + read_file/glob/grep 工具（Chat 模式用；`chat_tools()` 装配清单随工具走）
- [ ] (M2) **CheckpointStore**：把测试里的 `Sandbox` 提炼成正式实现——open 配方（init_opts + 手写 `core.worktree`/`core.bare` + `set_workdir(.., false)`）、`harden()` 的配置钉扎、每次打开重放 ignore 规则、purge 走 `checkout_index(remove_untracked)`、restore 前自动 snapshot
- [ ] (M2) LocalFs 写路径 + write/edit 工具（写前打检查点）
- [ ] (M2) 预算熔断与 GC：`revwalk` 计数 + 影子 git-dir 体积求和（实测 5 次快照 = 3457 B，阈值逻辑与 GC 策略待定）
- [ ] (M2) LocalPty + shell 工具（输出环形缓冲、超时杀、危险命令模式表）
- [ ] (M2) DaemonApproval + approval_rules 持久化 + 硬门测试（断言项目配置不可关闭）
- [ ] (M2) web_fetch + spill + 凭据脱敏
- [ ] (M2) disallowed_methods 的 compile-fail 测试（M0a 已人工实测 lint 生效，见 worklog/testing.md）
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

- 2026-09-28 rewind 的 purge 语义：默认不删未跟踪文件，`--purge` 显式开启且需审批 + 待删清单（用户裁决）。git2 下用 `checkout_index(remove_untracked)` 实现，已实测。
- 2026-09-28 影子 Git 后端：git2 vendored（用户裁决 + 第二轮实测），ADR-0012。若将来要摆脱 C 依赖，替代候选是 `gix`（纯 Rust，**未实测**），前提是把这 11 项门槛在 gix 上重跑全绿。

## 变更日志

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
