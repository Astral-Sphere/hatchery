# 工作记录：能力接缝 / 工具 / 审批 / 回滚（hatchery-capabilities, hatchery-tools）

- 范围：Fs/Terminal/Approval trait 与本地实现、影子 Git、内置工具、安全硬门、模式装配
- 设计文档：[../design/capabilities.md](../design/capabilities.md)
- 相关 ADR：0004、0005、0006、0009、**0010/0011**（同为「拒绝 C 构建」的理由链）

## 当前状态

设计稿完成 + **影子 Git spike 已实测**（2026-09-28，9 项测试常驻 `crates/hatchery-capabilities/tests/spike_shadow_git.rs`，全绿）。git 后端定为 **CLI `git`**，`git2` 已否。trait 与工具实现未开工（M1/M2）。

## 待办

- [x] (M0) **git spike（实测）**：CLI `git --git-dir --work-tree` 全操作集 + git2 构建代价对比；结论见下「实测记录」，已写回 design/capabilities.md §2
- [ ] (M0b) trait 定型（FsBackend/TerminalBackend/ApprovalGate/TerminalHandle 取消与流语义）+ `ToolHost`（kernel 侧窄接口，解 L0↔L1 环，见 worklog/kernel.md）
- [ ] (M1) LocalFs 只读路径 + read_file/glob/grep 工具（Chat 模式用）
- [ ] (M1) 启动审计加一条：`git --version` 不可用即 fail-loud（影子 Git 是 Code 模式硬依赖，ADR-0009）
- [ ] (M2) LocalFs 写路径 + CheckpointStore（含 `RestoreOptions.purge_untracked` 与待删清单）+ write/edit 工具
- [ ] (M2) LocalPty + shell 工具（输出环形缓冲、超时杀、危险命令模式表）
- [ ] (M2) DaemonApproval + approval_rules 持久化 + 硬门测试（断言项目配置不可关闭）
- [ ] (M2) web_fetch + spill + 凭据脱敏
- [ ] (M2) disallowed_methods 的 compile-fail 测试（M0a 已实测 lint 生效，见 worklog/testing.md）
- [ ] (M3) 与 AcpClientFs/AcpClientTerminal 的绑定矩阵联测
- [ ] (M5) WASM 工具插件评估（wasmtime + WASI；仅评估，实施须过新 ADR——ADR-0009 占位）

## 实测记录（2026-09-28，git 2.55.0，Linux x86_64）

`cargo nextest run -p hatchery-capabilities --nocapture` 可复现。所有 git 调用都在 tempdir 里跑，环境隔离（假 HOME、`GIT_CONFIG_NOSYSTEM=1`、空 global config），不碰开发者自己的 git 配置。

影子仓库（独立 `--git-dir` + `--work-tree` 指向用户工作区）：

- **不变量 6 成立**：init + 两次快照 + `reset --hard` 全程，用户仓库的 HEAD / 分支 / refs / `.git/index` mtime / `.git` 目录条目全部不变；用户的 staged/unstaged/untracked 内容原样保留。
- 影子仓库不会跟踪用户的 `.git`（我们额外把 `.git/` 写进 `<git-dir>/info/exclude`）。
- `info/exclude` 的排除规则生效（`big/`、`*.blob` 不进快照，且文件本身不被删）。
- `diff --name-only <c1> <c2>` 与 unified diff 正常；`rev-list --count`、`count-objects -vH` 可用于预算核算。
- 非 git 工作区照常可用。
- 性能：500 文件冷快照 **24.8 ms**、10 处改动的热快照 **12.6 ms**、`reset --hard` 恢复 **5.9 ms**；5 次快照后影子仓库 **60 KiB**。
- restore 语义：`reset --hard <c1>` 会回滚已跟踪文件、**并删除被 c2 跟踪过的新文件**，但不删「从未被任何快照跟踪」的文件 → 由此产生 `purge_untracked` 选项（默认 false；用户裁决：默认不 purge + 显式 `--purge` 走审批）。

用户仓库内的命令副作用（对 prompt environment 节与工具设计是硬约束）：

- `git status --porcelain` **会重写用户的 `.git/index`**（tracked 文件 stat 信息过期时刷新缓存）；实测连续两次 status 都改写。
- `rev-parse HEAD` / `rev-parse --abbrev-ref HEAD` / `rev-parse --is-inside-work-tree` / `for-each-ref` / `log --oneline` / `ls-files` / `diff --stat` **都不改 index**（实测）。
- 结论：hatchery 在用户工作区里只跑 plumbing 命令，永不跑 `git status`；测试 `status_rewrites_the_user_index_but_plumbing_commands_do_not` 常驻锁定。

git2 对比（一次性探测，在 /tmp 里做，未进仓库）：

- `git2 0.21.0`（default-features=false）→ `libgit2-sys 0.18.8+1.9.7`：本机无系统 libgit2（pkg-config 查不到），实测**vendored 编译了 libgit2**（`out/build`、`out/include` 存在，probe 二进制 `Repository::init` 可跑），构建 **4.58 s / 24 核**，前提是 **cmake 4.3.0 在场**。
- 否决理由不是性能而是**工具链**：cmake + C 编译器要在 ubuntu/macos/windows-msys2-ucrt64 三平台都就位（windows-gnu 还要 mingw），与 ADR-0010（turso）、ADR-0011（fluent）同一条「不引入 C 构建」的理由链。
- 若将来「要求用户装 git」成为问题（如 flatpak 运行时），可重新评估 git2——届时需先在三平台实测 cmake + mingw 组合。

## 开放问题

见设计文档末尾 4 条（glob/grep 在 ACP 会话的降级、unified exec、HTML→MD 选型、edit 格式）。解决过程记录于此：

- 2026-09-28 rewind 的 purge 语义：默认不删未跟踪文件，`--purge` 显式开启且需审批 + 待删清单（用户裁决）。依据是上面的 restore 实测行为。

## 变更日志

### 2026-09-28
- 初稿。核心设计输入：dsh capability seam 三角色模型 + atomcode 影子 Git 实现细节（预算熔断、RAII 补偿）+ atomcode ACP 缺 fs/terminal 的反面教材（ADR-0004）。
- ADR-0009 落地：ToolRegistry 正式采用注册句柄模式（`register() → Handle{dispose, replace}`，整表原子替换，与 kernel Turn Tool Snapshot 衔接）；工具表加 WASM 插件 M5 评估占位行；反预拆分刹车对 trait/包演进生效（第二个实现出现前不拆）。
- **M0a 影子 Git spike 完成**：9 项测试落地全绿，git 后端定为 CLI；design/capabilities.md §2 重写（`RestoreOptions`/`RestoreReport`、实测纪律若干条、git2 否决理由）。三个意外收获：① `git status` 会改用户 index → 全线只用 plumbing；② `reset --hard` 不删未跟踪文件 → 新增 purge 选项；③ 不变量 6 的准确含义是「影子操作不改用户仓库状态」，agent 自己写的文件出现在用户 status 里属正常（测试断言据此收紧，第一版断言写宽了才暴露出来）。
