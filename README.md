# hatchery
An open source AI harness -- hatchery for your code!

用 Rust 写的 AI Agent Harness：同一个核心引擎驱动三种前端——CLI（ratatui TUI + headless exec）、
原生桌面端（GTK4 + libadwaita）、以及 ACP agent server（可被 Zed 等宿主驱动）。会话跑在常驻 daemon 里，
关掉终端任务也继续。

## 这个项目想解决什么

- **reasoning_content 是一等公民**：流式展示、可配置 effort，历史回放**逐字节精确**（provider 侧 KV-cache 友好）。
- **历史可编辑**：编辑即分叉（append-only item 树 + 分支指针），允许物理删除分支——多数 agent 做不到。
- **代码可回滚**：影子 Git 检查点，`--git-dir` 独立，绝不碰用户自己的仓库。
- **提示词透明**：可查看、覆盖、导出生效的完整 system prompt，并标注每段来源。
- **完整的 ACP**：包括宿主侧文件读写与宿主侧终端委派——这正是同类实现最常缺的两块。

## 当前状态

**M0（地基）进行中**：设计文档定稿（10 份方向设计 + 11 份 ADR + 12 份 worklog），workspace 脚手架、
门禁与三平台 CI 已就绪，三个技术 spike 已实测收口（存储引擎、影子 Git、i18n）。核心类型与 agent 循环
尚未实现。里程碑与验收标准见 [docs/roadmap.md](docs/roadmap.md)。

## 文档

从 [docs/README.md](docs/README.md) 开始（含阅读路线）。几个常用入口：

| 想看什么 | 去哪 |
|---|---|
| 总体架构、进程模型、6 条核心不变量 | [docs/architecture.md](docs/architecture.md) |
| 每个关键决策的理由与被否方案 | [docs/decisions/](docs/decisions/README.md) |
| 里程碑 M0–M5 与验收标准 | [docs/roadmap.md](docs/roadmap.md) |
| 测试纪律、分层、CI 门禁、不变量→测试映射 | [docs/design/testing.md](docs/design/testing.md) |
| 接手某个方向的开发 | [docs/worklog/](docs/worklog/README.md) |
| 四款参考项目的分析结论 | [docs/references.md](docs/references.md) |

## 构建与测试

前置：`rustup`（`rust-toolchain.toml` 会自动装 stable + rustfmt + clippy）、`git`、`cargo-nextest`
（`cargo install cargo-nextest --locked`）。**MSRV 1.90**（实测，非抄依赖声明）。

```bash
cargo build --workspace
./scripts/ci.sh            # 与 CI 完全相同的门禁序列
./scripts/ci.sh --quick    # 只跑 fmt + clippy + build，本地快速迭代
cargo xtask layering       # 分层契约检查（docs/architecture.md §3）
cargo nextest run --profile invariants   # 核心不变量套件
```

`scripts/ci.sh` 是门禁的唯一真相：CI 与本地跑的是同一条命令，所以「本地绿」和「CI 绿」是同一件事。

## 平台

Linux / macOS / Windows 三平台都进 PR CI。Windows 走 **MSYS2 UCRT64 + `x86_64-pc-windows-gnu`**
（不用 MSVC）。Windows 开发环境：

```bash
pacman -S --needed mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-pkg-config \
                   mingw-w64-ucrt-x86_64-make git unzip curl
curl -sSfL https://sh.rustup.rs | sh -s -- \
     --default-host x86_64-pc-windows-gnu --default-toolchain stable \
     --component rustfmt clippy
# cargo-nextest 上游只发 windows-msvc 预构建包；那是原生 exe，在 MSYS2 bash 下可直接调用
```

存储引擎（turso）与 i18n（fluent）都是纯 Rust 实现，三平台都不需要 C 工具链——这是 ADR-0010/0011
的共同理由。唯一的运行时外部依赖是 `git` 二进制（影子 Git 检查点，ADR-0006）。

## 仓库布局

```
crates/          11 个 crate：protocol/kernel（L0）、llm/store/capabilities（L1）、
                 tools/acp（L2）、daemon（L3）、cli/gui（D）、testkit（dev-only）
xtask/           开发者任务：分层契约、coverage、（M1/M4）fixture 录制与 i18n 提取
scripts/ci.sh    门禁序列（本地 = CI）
docs/            设计文档、ADR、路线图、各方向工作记录
```

分层与依赖方向由 `cargo xtask layering` 机器检查，改架构图必须同步改那张表。

## License

GPL-3.0-only — 见 [LICENSE](LICENSE)。
