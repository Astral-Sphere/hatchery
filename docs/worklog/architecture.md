# 工作记录：总体架构 / 跨方向协调

- 范围：crate 分层纪律、核心不变量、进程模型、跨方向决策协调
- 设计文档：[../architecture.md](../architecture.md)、[../references.md](../references.md)
- 相关 ADR：全部

## 当前状态

**设计定稿（2026-09-28），零代码**。仓库现状：README、LICENSE、.gitignore、references/（四款参考项目源码，只读参考，不参与构建）、本文档体系。

## 待办

- [ ] (M0) Cargo workspace 脚手架：12 crate 空壳（含 dev-only `hatchery-testkit`）+ workspace Cargo.toml（共享依赖版本）+ rust-toolchain.toml
- [ ] (M0) CI：PR 门禁全套（design/testing.md §8：fmt + clippy -D warnings + nextest 默认组 + doctests + fixture 确定性）+ nightly 占位；`disallowed_methods` lint 配置（不变量 4 的强制）
- [ ] (M0) xtask：i18n-extract、录制回放 fixture 工具的骨架
- [ ] (M0) 三个 spike 并按结论更新对应文档：libSQL、git CLI vs git2、gettext vs fluent
- [ ] (M0) 顶层 README 扩写：项目定位、快速开始占位、文档链接
- [ ] (M1) 建立 docs/glossary.md 术语表

## 开放问题

1. crate 命名前缀最终确认（`hatchery-*`）与是否发布到 crates.io（发布则 protocol crate 是公共 API，语义化版本纪律从 M0 开始）。
2. MSRV 策略：跟 gtk4-rs/libadwaita 的 MSRV 走（它们通常最激进），M0 脚手架时定。
3. references/ 目录体积较大且是第三方源码——确认 license 合规性（各项目均为 MIT/Apache-2.0 系，收录为学习参考应无碍，但发布仓库时考虑用 submodule 或移除，只留 references.md 结论）。

## 变更日志

### 2026-09-28
- 项目启动设计：深读四款参考项目（分析结论存 ../references.md），与用户对齐 8 项关键决策（ADR-0001~0008），产出 architecture/roadmap + 9 份方向设计文档 + 本 worklog 体系。
- 用户明确的核心差异化诉求：完整 ACP（含 fs/terminal 委派，atomcode 的反面教材）、reasoning_content 可配置回传、历史可编辑（分叉+删除）、提示词透明、CLI+GTK 双前端。
- 下一步：单独对 M0 做细化规划（用户明确要求先停下来对齐）。
- 补充测试体系设计（用户要求「详尽的测试，确保所有代码都能如期运行」）：新增 design/testing.md + worklog/testing.md；crate 清单加入 dev-only 的 `hatchery-testkit`（12 个）；architecture.md 不变量节与 roadmap DoD 挂接测试文档。
- dsh/Cordis 模块化二次深读（用户问「能否采纳其激进模块化」，探索代理做了 vendor/cordis 代码级分析）：结论 = 运行时机制不移植（Proxy ctx/字符串键字典/`!!js` eval/HMR，postmortem 0001/0002 事故实证），吸收五条语言无关纪律 → **ADR-0009**；architecture.md §3 分层纪律 +3 条（disposer 逆序/反预拆分/fail-loud 装配）；第三方扩展面定为 MCP + ACP，WASM 工具插件列 M5 评估占位；证据细节补进 references.md dsh 节。
