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

**现状对账（2026-10-07）**：以上是目标结构。**今天被读取的 key 只有三类**——`ui.{show_reasoning,theme,language,response_language}`、`daemon.idle_timeout_min`、`providers.<id>.*` 子树（`is_known_key`，config.rs:158，是唯一判定点）。`[store]` 的检查点预算、`[modes.*]`、`[approval_rules]` 与工具策略（capabilities.md §4 的 `ToolPolicy`、§2 的 `Budget`）**全部是 M2 新增，当前 schema 里一个都没有表示**；`[acp_agents.*]` 属 M3。所以 M2 Phase 2 要成对地做两件事：① 把这些 key 加进 schema；② 给它们配上安全边界——`STRICT_KEYS`（config.rs:154，「解析失败不得降级」的那份清单）**现在是空表**，其注释直说「接进 `filter_keys` 与 typed reader 是 M2 的任务」。在它接上之前，§1.1 那句「项目级配置不得覆盖安全硬门与审批持久规则」还只是意图：今天没有任何一个安全 key 存在，所以也没有任何东西可被项目级配置覆盖掉。

## 2. 提示词系统

### 2.1 组装管线

```
sections（有序）:                                                    现状（2026-10-07 勘察）
  1. identity        默认 persona（include_str! 嵌入二进制）          M1 已有
  2. mode_variant    Chat/Code 变体纪律段（ADR-0005）                Chat 侧 M1 已有（id 实为 `mode_chat`）；
                                                                     Code 侧 `mode-code.md` 随 M2 Phase 2
  3. user_override   ~/.config/hatchery/prompts/<section>.md 逐 section 覆盖
                                                                     覆盖机制 M1 已有，目录 Phase 0 已接线；
                                                                     作为编号「节」M2 Phase 2
  4. project_context AGENTS.md（工作区层级向上发现 + 项目根，qwen-code memoryDiscovery 语义）
                                                                     M2 Phase 2
  5. environment     cwd、平台、日期、工作区是否 git 仓库等运行时事实   M1 已有
  6. tool_discipline 当前工具表的使用纪律（按 Turn Tool Snapshot 生成） M2 Phase 2
  7. safety_gate     安全门声明（不可覆盖，见下）                     M1 已有（含不可覆盖 + warning + 测试）
```

**节序的现状对账（2026-10-07）**：`render_chat` 今天发出且只发出 `["identity", "mode_chat", "environment", "safety_gate"]` 四节（由 `all_four_sections_assemble_in_order` 与 dispatch 级的 `prompt_render_lists_four_sections` 钉住），嵌入文件是 `crates/hatchery-daemon/prompts/{identity,mode-chat,environment,safety-gate}.md`——**没有 `mode-code.md`**。缺的三项是 `project_context`、`tool_discipline`，以及作为编号节的 `user_override`。第三项要拆开说：**per-section 覆盖机制在 M1 已落地**（逐节读 `<id>.md`，命中则该节来源标为 `user:prompts/<id>.md`；`safety_gate` 的覆盖被拒绝并 `tracing::warn!`，`the_safety_gate_cannot_be_overridden` 断言不泄漏），没落地的是「用户内容作为节序里独立的一节」——今天的覆盖是**替换某节的来源**，不是追加一节。**覆盖目录已接线（2026-10-07，Phase 0）**：此前生产调用方恒传 `None`、`~/.config/hatchery/prompts` 在代码里从未被构造，机制因此只有单测能碰到。现在 `prompt::prompts_dir(config_home, home)` 按 `config::LoadPaths::detect` 的同一套规则算出 `$XDG_CONFIG_HOME/hatchery/prompts`（否则 `~/.config/hatchery/prompts`），`prompt::default_prompts_dir()` 读环境，`entry.rs` 把它交给 `SessionManager`，装配时传进 `render_chat`。纯函数那半由单测钉住两条分支；**生产路径**由 e2e 的子进程测试钉住（`a_prompt_override_in_the_standard_location_reaches_the_request`：子进程自己读 `XDG_CONFIG_HOME`，而 `set_var` 在 edition 2024 是 unsafe 且被 clippy 禁掉，所以「测试拥有子进程环境」是唯一能测真路径的形状），同一测试也断言 `safety_gate.md` 的覆盖尝试经真路径依然被拒。测试侧的 `SessionManager` 一律传 `None`，不继承跑测试那台机器的配置——与 `LayeredConfig` 用注入层是同一个理由。覆盖文件名取节 id（`mode_chat.md`，下划线），而 §2.2 的例子写 `mode-code.md`（连字符），加 Code 变体时要统一。

**注入已落地（2026-10-07，Phase 0）。** 此前 M1 装配出来的 prompt 只被 `prompt/render` 消费过，从未进入任何一次模型请求。现在的链路是：`SessionManager::assemble` 渲染一次 → `RuntimeParts.prompt` 带进 `SessionRuntime`（冻结，同时供 `prompt/render` 回报）→ `StoreHistory::view()` 把 `join()` 后的文本前置成一条 `Role::System` 消息。**`ChatOptions` 有意不加字段**：llm 的 `wire_message` 本来就映射 `Role::System`（`translate.rs:134`），所以 adapter 一行没改，注入完全住在 daemon 这个装配器里（kernel.md §6 的分工）。原来那条断言「消息里没有 `Role::System`」的测试改为直接断言 checkpoint 不产消息（`checkpoints_are_not_provider_visible`，判据换成 commit id 不出现在任何消息里）。`prompt/render` 的实现也不再自己拼 `Environment`——它调 `SessionManager::render_prompt`，与装配走同一条路，两边不可能漂移；顺带删掉了那个把会话 model 读出来又 `let _ = model;` 丢掉的死绑定。

**D15 已定稿并实现（2026-10-07，Phase 0）**：

① **每次 runtime 装配渲染一次，冻结该 runtime 的整个生命周期。** 理由**不是**规划时写的「模式切换与 config 变更本来就 bump generation 并重组装」——那句经实测不成立：全仓库唯一的卸载路径是空闲清扫（`sweep_after` → `unload`），`session/set_config` 与 `config/set` 都不重组装活着的 runtime，`session/set_mode` 甚至还没被路由。所以 prompt 是「装配时冻结、直到下次装配才换」，冻结让它与既有语义一致，而不是它引入了陈旧。（`/model` 与 effort 同日按 **D19** 改为**跟着 turn 走**，见 design/daemon.md 开放问题 5；prompt 是唯一有意留在 runtime 上的东西。）（**effort 曾比这更糟，同日已修**：`config_patch.reasoning_effort` 在 daemon 侧没有读者，`ChatOptions::new` 恒把它留成 `None`、`apply_effort` 遇 `None` 直接 return，所以 effort 一度**从不进入任何一次 turn 请求**，全仓库唯一喂过它的是 doctor 的探测两轮；2026-10-07 live 实测 `/effort off` 之后状态栏变了、推理照旧。D19 之后 effort 由 `turn_options` 在 submit 时解析，`/effort off` 才真的意味着「下一条不推理」。）真正的理由是每轮重渲染会让 environment 节的日期/cwd 破坏请求前缀的稳定性，而那正是本项目为 KV cache 反复强调的东西。由此暴露的「`/model` 改完当轮不生效」记为 design/daemon.md 开放问题 5；Phase 3 的 `session/set_mode` 必须先回答它——换模式要换工具表与 prompt 变体，非重组装不可。

② **不变量 2 只管辖分支历史。** system prompt 是可复现的派生态（模板 + 装配时冻结的运行时事实），不是 item，由 `prompt/render` 钉：该方法对活着的 runtime 返回它装配时冻结的那一份，而不是重新渲染一份可能已经不同的。e2e 的 `invariant_minimal_chat_replays_reasoning_byte_exact` 因此断言三件事——两次请求的 system 文本逐字节相同（冻结）、它等于 `prompt/render` 的 `text`（透明性说的就是模型看到的那份）、它后面的 messages 数组仍与手写期望整表比对（分支历史逐字节）。

- section 注册表模式（借鉴 dsh system-prompt）：每 section 有 id、默认内容、是否可覆盖、排序权重；`{{var}}` 插值。
- **PRECEDENCE 声明**（借鉴 atomcode）：identity section 开头明确「用户与项目注入的规则优先于默认 persona，但 safety_gate 不可被任何注入覆盖」。
- `safety_gate` 与 `tool_discipline` 不接受用户覆盖；覆盖尝试被忽略并 warning；有测试断言（不变量 5）。**现状**：`safety_gate` 那半是真的（prompt 层的覆盖拒绝 + warning + 测试）；`tool_discipline` 连节都还不存在（M2 Phase 2），它的不接受覆盖要到那时才有对象。而「不变量 5」那条端到端断言（`invariant_project_config_cannot_disable_hard_gates`）今天全仓库零命中，随 M2 Phase 2 与 `STRICT_KEYS`（§1.2）接线一起补。
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

1. ~~项目级 prompt 文件是否兼容识别 `HATCHERY.md`（自有品牌）与 `AGENTS.md`（生态）双文件名~~ → **决策记录（2026-10-01，M1 收口）**：**`AGENTS.md` 为主文件名，`HATCHERY.md` 兼容认读**——两处同名并存时 AGENTS.md 优先、发出一次 warning。理由：AGENTS.md 已是多家 agent 工具的事实惯例，用户的同一个文件应能同时喂给 hatchery 与其他工具；自有双文件名只增加「该写哪个」的犹豫，不增加表达力。实现（工作区层级向上发现 + project_context section 注入 + 来源标注 + 发现 golden 测试）**排 M2 Phase 2**——与 `mode-code.md`、`tool_discipline`、模式装配同阶段（roadmap 的 Phase 2；此前本文只写「排 M2」，未定阶段），M1 只落此决策。
2. ~~配置 schema 校验失败的降级策略~~ → **已定（2026-09-28）**：**逐 key 忽略 + warning**，不整文件拒绝。理由：一个坏 key 不该让整个 daemon 起不来（与 ADR-0009 的 fail-loud 不冲突——fail-loud 指「必需组件缺失要拒绝服务」，配置里的可选 key 缺失只需报告）；每条 warning 带 key 路径、来源层级与原因，`config/get` 能查到「此 key 被忽略」。安全相关 key 解析失败时**取最严格默认值**并升级为 error 级日志。
3. ~~gettext vs fluent~~ → **已定（2026-09-28，ADR-0011）**：fluent。实测依据见 §3 开头与 worklog/platform.md。
4. prompt 文件存放：~~独立 crate vs 各 crate prompts/ 目录~~ → **已定（2026-09-28）**：各 crate 自己的 `prompts/` 目录 + `include_str!` 嵌入，**不新建 `hatchery-prompts` crate**——prompt 装配的唯一消费者是 daemon（前端经 `prompt/render` 协议查看），按 ADR-0009 的反预拆分刹车，第二个消费者出现前不拆。
