# 工作记录：总体架构 / 跨方向协调

- 范围：crate 分层纪律、核心不变量、进程模型、跨方向决策协调、workspace 与 CI 基建
- 设计文档：[../architecture.md](../architecture.md)、[../references.md](../references.md)
- 相关 ADR：全部

## 当前状态

**M0a 完成（2026-09-28）**：workspace 脚手架（12 member）、门禁脚本、三平台 CI、xtask layering 契约、三个 spike 全部落地并实测通过。M0b（protocol/kernel/store 的实现）未开工。

仓库现状：`crates/`（11 crate）+ `xtask/` + `scripts/ci.sh` + `.github/workflows/{pr,nightly}.yml` + `docs/`。本地 `./scripts/ci.sh` 全绿；三平台 CI **尚未验证**（需要 push 才能触发，见待办）。

## 待办

- [x] (M0) Cargo workspace 脚手架：12 member（11 crate + `xtask`）+ workspace Cargo.toml（共享依赖版本与 lints）+ rust-toolchain.toml
- [x] (M0) CI：PR 门禁全套（`scripts/ci.sh`：toolchain/fmt/clippy `-D warnings`/build/nextest ci 组/doctests/fixture 确定性/i18n 占位）+ nightly（slow/gui/audit/coverage）+ `disallowed_methods` lint 配置
- [x] (M0) xtask：`layering`（分层契约，含集成测试）、`coverage`；`i18n-extract` 与 `record-fixtures` 是 fail-loud 占位
- [x] (M0) 三个 spike：存储引擎（→ ADR-0010 turso）、影子 Git CLI vs git2（→ CLI）、i18n gettext vs fluent（→ ADR-0011 fluent）
- [x] (M0) MSRV 实测：`rust-version = "1.90"`（1.85/1.88 均失败，1.90.0 编译通过）
- [ ] **(需要用户 push)** 三平台 CI 首次验证：windows MSYS2 ucrt64 + `x86_64-pc-windows-gnu` 那条 job 风险最高（rustup-in-MSYS2、prebuilt MSVC nextest、actions/cache 路径）；失败日志回传后修正
- [x] (M0) 顶层 README 扩写：项目定位、快速开始、文档链接、MSYS2 ucrt64 环境清单、仓库布局；docs/README.md 加「代码布局」节
- [ ] (M1) 建立 docs/glossary.md 术语表
- [ ] (M1) MSRV CI job（`cargo +1.90.0 check`）加进 nightly，防止依赖升级悄悄抬高 MSRV
- [ ] (M1) `hatchery-tests` 成员 crate（跨 crate e2e 与不变量套件的家；虚拟 manifest 不能有顶层 `tests/`，见 design/testing.md §1）

## 开放问题

1. ~~crate 命名前缀与是否发布到 crates.io~~ → **已定（2026-09-28）**：前缀 `hatchery-*`；**按可发布标准写全元数据**（`license = "GPL-3.0-only"`、repository、description、readme、keywords、categories、`rust-version`），但 **M0 不 publish**；`hatchery-testkit` 与 `xtask` 标 `publish = false`。protocol crate 从第一天走 semver 纪律 + `PROTOCOL_VERSION` 常量（M0b 落地）。注意 LICENSE 是 **GPL-3.0**，将来发布 library crate 会让下游必须接受 GPL，届时需重新确认。
2. ~~MSRV 策略~~ → **已定并实测（2026-09-28）**：`rust-toolchain.toml` 用 `channel = "stable"`（浮动），`rust-version = "1.90"`。实测：1.85.1 失败（turso 的 `icu_*`/`aristo`/`home` 要求 1.88）、1.88.0 失败（`roaring 0.11.5` 要求 1.90.0）、**1.90.0 通过** `cargo check --workspace --all-targets`。GTK 生态（M4 才引入）可能再抬高下限，届时以实测为准。
3. ~~references/ 目录的 license 与体积~~ → **已由用户自行解决**：`.gitignore` 里的 `/references` 使其不入库，只保留 `references.md` 的分析结论。

## 变更日志

### 2026-09-28
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
