# ADR-0012: 影子 Git 后端用 git2（vendored libgit2），不依赖用户的 git 二进制

状态：accepted（2026-09-28）

Supersedes: M0a 记在 design/capabilities.md §2 的「后端 = CLI `git`」结论（当时未立 ADR），并关闭 ADR-0006 后果节留下的 git2 / CLI-git 选型开放问题。ADR-0006 的其余决策（独立 git-dir、检查点时机、预算熔断、`RewindScope`）不变。

## 背景

M0a 的影子 Git spike 选了 CLI `git`，理由是「无 C 构建、行为可预期」，并把 git2 否掉，当时记录为「需要 cmake + C 工具链」。用户随后指出决定性的一点：**很多用户的电脑上没有 git**——把 git 二进制变成运行时硬依赖，等于把可用性风险转嫁给用户；并裁决改用 git2（vendored）。

重新实测（2026-09-28）也**修正了 M0a 的一条错误记录**：libgit2-sys 0.18.8 的 `build.rs` 用 `cc::Build`（`add_c_files` / `add_pcre2_files`）自己编译 libgit2 与 pcre2，**不需要 cmake**——cmake 只出现在注释链接与 `#cmakedefine` 替换里。M0a 那次探测机上 cmake 恰好在场（4.3.0），于是把「cmake 在场」误记成「需要 cmake」。这条错误结论已经写进过 worklog，此处显式更正。

## 决策

- 影子 Git 后端 = **`git2 = { version = "0.21.0", features = ["vendored-libgit2"] }`**（libgit2-sys 0.18.8+1.9.7，vendored 源码编译）。git2 的 default features 为空 → 不引入 https/ssh/openssl。
- **运行时不再依赖用户的 git 二进制**：daemon 启动审计删掉 `git --version` 检查；flatpak 不必 bundle git。
- 构建期新增要求：一个 C 编译器。三平台：ubuntu / macos runner 自带；Windows 走 MSYS2 ucrt64 的 `mingw-w64-ucrt-x86_64-gcc`（pr.yml 已装）。**不需要 cmake。**
- CI 工具链改为**源码编译**（用户裁决）：`cargo install cargo-nextest --version 0.9.146 --locked`，三平台一致，不再下载预构建二进制；nightly 的 cargo-audit / cargo-llvm-cov 同样源码安装。目的是消除「MSVC 预构建 exe 在 MSYS2 下运行」这类不确定性，并减少供应链面。
- 编译 flags：`.cargo/config.toml` 的 `[build] rustflags = ["-C", "target-cpu=native"]`（用户偏好；已实测进入 rustc 调用）。**纪律**：产物不可跨 CPU 移植，因此发布二进制 / flatpak 的打包流水线必须覆盖它（不得继承本仓库的 `.cargo/config.toml`，或显式 `RUSTFLAGS=""`）；跨目标编译同理。

## 实测（`crates/hatchery-capabilities/tests/spike_shadow_git.rs`，11 项全绿，Linux x86_64 / 24 核）

| 项 | git2（vendored libgit2 1.9.7） | M0a 的 CLI git 2.55（对照） |
|---|---|---|
| 独立 git-dir + 用户工作区作 work tree | ✅（需手写 config，见坑 1） | ✅（`--git-dir/--work-tree`） |
| 不变量 6：HEAD / 分支 / refs / index mtime / `.git` 条目全不变 | ✅ | ✅ |
| 读用户仓库状态是否改用户 index | ✅ **`statuses()` 不改** | ❌ `git status` 会重写 |
| 冷快照 500 文件 | 48.9 ms | 24.8 ms |
| 热快照（10 处改动，写前打点的常态） | **6.0 ms** | 12.6 ms |
| 硬恢复到检查点 | **2.8 ms** | 5.9 ms |
| 5 次快照后影子仓库体积 | **3457 B** | 60 KiB |
| 忽略规则 | ✅ `add_ignore_rule`（按 handle 生效） | ✅ `<git-dir>/info/exclude` |
| 精确恢复（删未跟踪文件） | ✅ `checkout_index(remove_untracked)` | 需另跑 `git clean -fd` |
| 构建代价 | 10.5 s（含 libz-sys + libgit2-sys + git2），需 C 编译器，不需 cmake | 0 |

## libgit2 的四个坑（都已成为测试或纪律）

1. **`set_workdir(path, update_gitlink=false)` 不落盘。** 读 libgit2 `repository.c:3259` 确认：只有 `update_gitlink=true` 才写 `core.worktree` 与 `core.bare=false`，而同一个 flag 会调用 `repo_write_gitlink`，**在用户工作区里种一个 `.git` 文件**。正确做法：`init_opts(bare(true).no_dotgit_dir(true).external_template(false))` → 自己写 `core.worktree` + `core.bare=false` → `set_workdir(.., false)`。测试 `no_gitlink_is_planted_in_the_user_workspace` 锁定「用户工作区绝不出现 `.git`」。
2. **`add_ignore_rule` 是 per-handle 的内存规则。** 实测：在临时句柄上加的规则对下一次打开句柄的 snapshot 无效。`CheckpointStore` 必须在每次打开仓库时按配置重放规则。
3. **`reset(Hard, remove_untracked)` 不会删未跟踪文件。** hard reset 的 checkout 只覆盖与目标有差异的路径。`--purge` 语义要额外走一遍 `checkout_index(None, force().remove_untracked(true))`（实测）；且**不加 `remove_ignored`**，避免删掉构建产物与用户的 `.env`。
4. **libgit2 会读开发者的全局/系统配置与模板目录。** `core.autocrlf` 与全局 `core.excludesFile` 会悄悄改变快照范围与换行。影子仓库必须显式钉住：`core.autocrlf=false`、`core.excludesFile=<不存在的路径>`、`core.fsmonitor=false`、`user.name/email`，并 `external_template(false)`。测试里的 `harden()` 就是这套配置，`CheckpointStore` 照抄。

## 理由

1. **可用性优先**（用户裁决）：不能要求用户装 git。Code 模式的核心安全网（回滚）不该因为环境缺件而静默失效。
2. 实测性能不输 CLI：热路径（每次写前打快照，是常态）快约 2 倍，影子仓库体积小一个数量级（不铺 hooks/模板）。冷快照慢一倍，但那是每个工作区一次的成本。
3. 读用户仓库状态不再需要「禁止跑 `git status`」这类脆弱纪律——libgit2 的 `statuses()` 实测不改 index。
4. M0a 否决 git2 的两条理由：一条被证伪（不需要 cmake），另一条（C 工具链）在 MSYS2 ucrt64 下已装 gcc、成本可控。ADR-0010/0011 的「纯 Rust」偏好在此让位于可用性。

## 代价

- 构建期需要 C 编译器（CI 三平台已覆盖，贡献者文档已写明）。
- libgit2 与 git CLI 的行为差异要自己踩——本 ADR 的四个坑是第一批，后续踩到就补测试。
- `target-cpu=native` 使产物不可移植，发布流水线必须覆盖。
- CI 源码编译工具链更慢（cargo-nextest 首次构建数分钟），靠缓存 `~/.cargo/bin`（Windows 为 `C:/msys64/cargo/bin`）摊薄。

## 替代方案（已否）

- **CLI `git`**（M0a 的选择）：隔离与性能都合格，但把 git 二进制变成运行时硬依赖 → 用户裁决否决。
- **`gix`（gitoxide，纯 Rust）**：与 ADR-0010/0011 的纯 Rust 偏好一致，但「独立 git-dir + 外部 work-tree + 硬恢复 + 忽略规则」这套组合的成熟度**未实测（推测不如 libgit2）**。若将来要摆脱 C 依赖，这是首选替代，前提是把本文件的 11 项门槛在 gix 上重跑一遍并全绿。
- **文件快照表**（qwen-code 式）：无 diff、边界情况多，ADR-0006 已否。

## 后果

- design/capabilities.md §2 重写：git2 实现纪律、`RestoreOptions.purge_untracked` 的 checkout_index 实现、配置钉扎清单、性能数字。
- worklog/daemon.md 删掉 `git --version` 审计项；worklog/platform.md 修正 git2 构建代价那一行（cmake 说法是错的）。
- README 的运行时依赖从「git 二进制」改为「构建期 C 编译器」；MSYS2 环境清单里 gcc 由「可选」变「必需」。
- pr.yml / nightly.yml 改为源码安装工具链；`.cargo/config.toml` 加 `target-cpu=native`。
- ADR-0006 状态行标注：后端选型已由本 ADR 关闭。
- 待办（M2）：把测试里的 `Sandbox` 提炼成 `CheckpointStore`，并把 `harden()` 的配置钉扎做成正式代码 + 契约测试。
