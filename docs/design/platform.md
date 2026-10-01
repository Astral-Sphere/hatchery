# 设计：配置、提示词与 i18n（横切）

> 状态：设计稿。横切关注点，落点在多个 crate；本文档统一约定。

## 1. 配置系统

### 1.1 分层（低 → 高，借鉴 codex 8 级简化为 5 级）

```
1. 内置默认（编译进二进制的 Default impl）
2. 系统级   /etc/hatchery/config.toml
3. 用户级   ~/.config/hatchery/config.toml
4. 项目级   <workspace>/.hatchery/config.toml
5. 运行时覆盖：CLI -c key=value / 协议 config/set（会话级 config_patch 落库）
```

- 每个 key 记录来源（origin），`config/get` 返回 `{ value, origin }`，GUI 设置页展示「此值来自项目级配置」。
- 合并语义：表深度合并，数组整体替换（不做元素级 merge，避免不可预期）。
- **安全边界**：项目级配置**不得**覆盖安全硬门与审批持久规则（capabilities.md §5）；尝试覆盖时忽略并 warning。
- schema：用 `serde` + 手写校验（M0），JSON Schema 导出供编辑器补全（M2+，`hatchery config schema` 子命令）。

### 1.2 config.toml 结构总览

```toml
[ui]                language = "zh-CN"  theme = "auto"  show_reasoning = true
[daemon]            idle_timeout_min = 30
[store]             checkpoint_budget_mb = 500  disk_fuse_gb = 2
[providers.X]       # llm.md §2
[modes.custom-name] # ADR-0005 自定义模式：tools include/exclude、approval、prompt variant
[acp_agents.name]   # acp.md §2
[approval_rules]    # 与 DB 中 approval_rules 表同步展示（DB 为准，config 只读快照）
```

## 2. 提示词系统

### 2.1 组装管线

```
sections（有序）:
  1. identity        默认 persona（include_str! 嵌入二进制）
  2. mode_variant    Chat/Code 变体纪律段（ADR-0005）
  3. user_override   ~/.config/hatchery/prompts/<section>.md 逐 section 覆盖
  4. project_context AGENTS.md（工作区层级向上发现 + 项目根，qwen-code memoryDiscovery 语义）
  5. environment     cwd、平台、日期、工作区是否 git 仓库等运行时事实
  6. tool_discipline 当前工具表的使用纪律（按 Turn Tool Snapshot 生成）
  7. safety_gate     安全门声明（不可覆盖，见下）
```

- section 注册表模式（借鉴 dsh system-prompt）：每 section 有 id、默认内容、是否可覆盖、排序权重；`{{var}}` 插值。
- **PRECEDENCE 声明**（借鉴 atomcode）：identity section 开头明确「用户与项目注入的规则优先于默认 persona，但 safety_gate 不可被任何注入覆盖」。
- `safety_gate` 与 `tool_discipline` 不接受用户覆盖；覆盖尝试被忽略并 warning；有测试断言（不变量 5）。
- **透明性**：`prompt/render` 协议方法 + CLI `/prompt` + GUI prompt 查看器，输出最终拼装的完整 prompt 并标注每 section 来源。默认 persona 源文件同时放在 `prompts/`（仓库内）供直接阅读。

### 2.2 存放

```
仓库:   各 crate 自己的 prompts/ 目录 + include_str! 嵌入（不新建 hatchery-prompts crate，
        理由见开放问题 4：唯一消费者是 daemon）
用户:   ~/.config/hatchery/prompts/{identity.md, mode-code.md, …}
项目:   AGENTS.md（兼容生态惯例；同时识别 HATCHERY.md？——开放问题 1）
```

## 3. i18n

> 方案已由 **ADR-0011** 定为 fluent（原 ADR-0008 的 gettext 方案被取代）。M0a 实测依据：gtk4-rs/glib 并不集成 gettext；`gettext-rs` 需 vendored 编译 C 版 libintl（构建 37.64 s），fluent 纯 Rust（3.45 s）；xgettext 抽取 Rust 时只认函数形式，宏形式抽不出来。

- **文案**：fluent（`fluent-bundle` + `unic-langid`）。catalog 是 FTL 文件，默认 `include_str!` 嵌入二进制；用户级覆盖放 `~/.config/hatchery/locales/<lang>/*.ftl`（M4）。CLI 与 GUI 共用同一套 catalog 与同一个查表模块。
- **查表位置**：在**前端进程内**（文案渲染发生在前端）；daemon 只透传语言设置，不做文案格式化。
- **上下文与复数**：上下文用 message id 命名约定表达（`approval-allow-button = Allow`）+ FTL 注释；复数/选择性用 FTL 的 `->` selector（`{ $n -> [one] … *[other] … }`）。
- **bidi 隔离**：fluent 默认给插值加 U+2068/U+2069 隔离符（实测），**保持开启**以服务 RTL；golden 测试里显式写出这两个码点，避免被误当噪声清理。
- **提取与校验**：`xtask i18n-extract`（M4）扫描源码中的 id 引用，与 catalog 对账，报告缺失/多余/未包裹的用户可见字符串。
- **语言选择**：`[ui] language` 覆盖 `LANGUAGE`/`LC_*` 环境推断；协商用 `fluent-langneg`（需要时引入）。
- **RTL**：GTK 端交给 Pango（ADR-0008）；TUI 端 RTL 不做（终端生态现状），仅保证不主动破坏 BiDi 文本。
- **LLM 输出语言**：不是 i18n 问题，是 prompt/配置问题——`[ui] response_language` 注入 environment section（如「以简体中文回复」）。
- **文档翻译**：v1 只写中文；将来若做英文文档，采用 dsh 的配对校验思路（`.i18n.yaml` hash + CI 门禁），届时另立 ADR。
- 术语表：`docs/glossary.md`（M1 建立，翻译与 UI 文案统一用词，如 turn=轮次、branch=分支、checkpoint=检查点）。

## 4. 日志与遥测

- tracing：daemon/CLI/GUI 统一 subscriber；daemon 落盘轮转，前端 stderr。
- 遥测（otel）：feature gate，v1 默认关闭且不编译进发布二进制；任何遥测必须先过 ADR（隐私）。

## 开放问题

1. ~~项目级 prompt 文件是否兼容识别 `HATCHERY.md`（自有品牌）与 `AGENTS.md`（生态）双文件名~~ → **决策记录（2026-10-01，M1 收口）**：**`AGENTS.md` 为主文件名，`HATCHERY.md` 兼容认读**——两处同名并存时 AGENTS.md 优先、发出一次 warning。理由：AGENTS.md 已是多家 agent 工具的事实惯例，用户的同一个文件应能同时喂给 hatchery 与其他工具；自有双文件名只增加「该写哪个」的犹豫，不增加表达力。实现（工作区层级向上发现 + project_context section 注入 + 来源标注 + 发现 golden 测试）排 M2，M1 只落此决策。
2. ~~配置 schema 校验失败的降级策略~~ → **已定（2026-09-28）**：**逐 key 忽略 + warning**，不整文件拒绝。理由：一个坏 key 不该让整个 daemon 起不来（与 ADR-0009 的 fail-loud 不冲突——fail-loud 指「必需组件缺失要拒绝服务」，配置里的可选 key 缺失只需报告）；每条 warning 带 key 路径、来源层级与原因，`config/get` 能查到「此 key 被忽略」。安全相关 key 解析失败时**取最严格默认值**并升级为 error 级日志。
3. ~~gettext vs fluent~~ → **已定（2026-09-28，ADR-0011）**：fluent。实测依据见 §3 开头与 worklog/platform.md。
4. prompt 文件存放：~~独立 crate vs 各 crate prompts/ 目录~~ → **已定（2026-09-28）**：各 crate 自己的 `prompts/` 目录 + `include_str!` 嵌入，**不新建 `hatchery-prompts` crate**——prompt 装配的唯一消费者是 daemon（前端经 `prompt/render` 协议查看），按 ADR-0009 的反预拆分刹车，第二个消费者出现前不拆。
