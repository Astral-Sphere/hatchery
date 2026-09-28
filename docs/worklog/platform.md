# 工作记录：配置 / 提示词 / i18n（横切）

- 范围：分层配置、prompt section 管线、gettext 工具链、术语表
- 设计文档：[../design/platform.md](../design/platform.md)
- 相关 ADR：0005（模式 prompt 变体）、0008（i18n）

## 当前状态

设计稿完成，未实现。

## 待办

- [ ] (M0) **i18n spike**：gettext-rs vs fluent（复数、上下文标注、GTK 集成度）；结论写回并更新 platform.md 开放问题 3
- [ ] (M0) prompts 存放位置定案（独立 crate vs 各 crate prompts/ 目录）
- [ ] (M1) 分层配置加载 + per-key origins + 项目级安全边界（覆盖硬门时忽略 + warning）
- [ ] (M1) prompt 管线 v1：identity/mode_variant/environment/safety_gate 四 section + {{var}} 插值 + PRECEDENCE 声明
- [ ] (M1) `prompt/render` + CLI /prompt
- [ ] (M1) AGENTS.md 发现与注入（层级向上 + 与 HATCHERY.md 的兼容策略定案）
- [ ] (M1) docs/glossary.md 术语表初版
- [ ] (M2) user_override section（~/.config/hatchery/prompts/）+ 覆盖不可越权测试
- [ ] (M2) `hatchery config schema` 导出 JSON Schema
- [ ] (M4) xtask i18n-extract + po 工作流

## 开放问题

见设计文档末尾 3 条（HATCHERY.md 双文件名、schema 校验降级、gettext vs fluent）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。prompt 系统合成三家经验：dsh section 注册表 + atomcode PRECEDENCE/安全门不可覆盖（含测试）+ qwen-code AGENTS.md 层级发现；透明性（可查看/导出最终 prompt）是用户明确需求。
