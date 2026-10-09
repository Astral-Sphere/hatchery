# 工作记录：配置 / 提示词 / i18n（横切）

- 范围：分层配置、prompt section 管线、i18n catalog 与工具链、术语表
- 设计文档：[../design/platform.md](../design/platform.md)
- 相关 ADR：0005（模式 prompt 变体）、0008（GUI/RTL）、**0011**（i18n 用 fluent）

## 当前状态

**M1 交付（2026-10-01）**：分层配置加载与 prompt 管线 v1 已落地（代码在 `hatchery-daemon`，细节与测试见 worklog/daemon.md）；AGENTS.md 发现与注入随决策记录排 **M2 Phase 2**（见待办）。**i18n 方案已实测定型（fluent，ADR-0011）**，落地在 M4。

**口径限定（2026-10-07，M2 重新规划）**：「prompt 管线 v1 已落地」指的是**装配 + 透明性**——四节有序装配、`{{var}}` 插值、per-section 来源标注、`prompt/render` 与 CLI `/prompt` 可查看；**不含注入到任何一次 turn**。`render_chat`（prompt.rs:57）的唯一非测试调用方是服务 `prompt/render` 的 `DaemonCore::render_prompt`（core.rs:227），`ChatOptions` 无 system prompt 字段、`StoreHistory::view()` 不产 system 消息，且 runtime.rs:618 有一条测试主动断言消息里没有 `Role::System`。注入随 **M2 Phase 0**（决策点 **D15**），实证与理由记在 worklog/daemon.md 的 2026-10-07 条目。

代码归属已定：配置加载与 prompt 装配都落在 `hatchery-daemon`（唯一消费者——前端一律经 `config/get|set`、`prompt/render` 协议访问），**不新建 `hatchery-platform` 或 `hatchery-prompts` crate**（ADR-0009 反预拆分刹车）。

## 待办

- [x] (M0) **i18n spike**：gettext-rs vs fluent（复数、上下文标注、GTK 集成度、构建代价）→ 结论 fluent，见下「实测记录」，已落 ADR-0011 + platform.md §3
- [x] (M0) prompts 存放位置定案：各 crate 自己的 `prompts/` 目录 + `include_str!`，不新建 crate
- [x] (M0) 配置 schema 校验失败降级策略定案：逐 key 忽略 + warning（安全 key 取最严格默认值并升 error 日志）
- [x] (M1) 分层配置加载 + per-key origins + 项目级安全边界（覆盖硬门时忽略 + warning）（2026-10-01 落地，见 worklog/daemon.md）
- [x] (M1) prompt 管线 v1：identity/mode_variant/environment/safety_gate 四 section + `{{var}}` 插值 + PRECEDENCE 声明（2026-10-01 落地，见 worklog/daemon.md）——**交付范围限定（2026-10-07 勘察）**：交付的是**装配与透明性，不是注入**。实际发出的节序是 `["identity", "mode_chat", "environment", "safety_gate"]`，由 `all_four_sections_assemble_in_order`（prompt.rs:181）与 dispatch 级的 `prompt_render_lists_four_sections`（core.rs:527）钉住；design/platform.md §2.1 列的 **7 节里只有这 4 节存在**，嵌入文件是 `crates/hatchery-daemon/prompts/{identity,mode-chat,environment,safety-gate}.md`——**没有 `mode-code.md`**
- [x] (M1) `prompt/render` + CLI `/prompt`（随 daemon Phase 3 / cli Phase 4）
- [x] (M2 Phase 0，2026-10-07 完成) **把装配结果注入 turn**（+ **D15**）：由 daemon 的 `HistorySource` 实现在 `view()` 里前置一条 system `Message`，`ChatOptions` 不加 system 字段；D15 的两条建议是「runtime 装配时渲染一次并冻结整个 runtime 生命周期」（每轮重渲染会让 environment 节的日期/cwd 破坏前缀稳定性，那正是 KV cache 友好性反复强调的东西；模式切换与 config 变更本来就 bump generation 重组装）与「不变量 2 只管分支历史，system prompt 是可复现派生态、由 `prompt/render` golden 单独钉」。细节与实证见 worklog/daemon.md。**结果**：注入与覆盖目录都已接线（`prompt::prompts_dir` / `default_prompts_dir` + `entry.rs` → `SessionManager`），`prompt/render` 对活着的 runtime 返回冻结那一份；**D15 的理由①「模式切换与 config 变更本来就 bump generation 重组装」经实测不成立**（唯一卸载路径是空闲清扫），更正写回 design/platform.md §2.1 与 design/kernel.md §6，暴露出的「`/model` 改完当轮不生效」记为 design/daemon.md 开放问题 5
- [ ] (M2 Phase 2) AGENTS.md 发现与注入（层级向上发现 + project_context section + 来源标注 + 发现 golden 测试）——决策已定（2026-10-01）：**AGENTS.md 为主文件名、HATCHERY.md 兼容认读**，并存时 AGENTS.md 优先并 warning 一次（design/platform.md 开放问题 1 的决策记录）；实现在 **M2 Phase 2**
- [ ] (M2 Phase 2) **`tool_discipline` 节**（§2.1 第 6 节，按 Turn Tool Snapshot 生成）——本文件此前完全没有这一项；它与 `safety_gate` 同样**不接受用户覆盖**，而 §2.1 那句「`safety_gate` 与 `tool_discipline` 不接受覆盖」今天只对前者有代码（后者连节都不存在）
- [ ] (M2 Phase 2) **`mode-code.md`** 提示词文件 + Code 变体纪律段（ADR-0005 的 mode_variant 在 Code 侧）——嵌入文件今天只有 chat 变体，`render_chat` 也只装配 `mode_chat`；Code 节要随 Phase 2 的模式装配（`assemble(mode, backends)`）一起才有意义，否则渲染出来的 Code prompt 没有消费者
- [x] (M1) **environment 节只准用 git plumbing 命令**：实测 `git status` 会重写用户的 `.git/index`（worklog/capabilities.md），prompt 装配属于后台行为，绝不能有这种副作用（git2 `statuses()` 落地，不碰用户 index）——实现是 `git_summary`（prompt.rs:129）：`git2::Repository::discover` + `statuses()`，符合 ADR-0012
- [x] (M1) docs/glossary.md 术语表初版（2026-10-01）
- [ ] (M2) **`user_override` 作为 §2.1 的第 3 节**——**先把两件被混为一谈的事分开（2026-10-07 对账）**：① **per-section 覆盖机制已在 M1 落地**：`render_chat` 的 `override_dir` 参数（prompt.rs:57）逐节读 `<id>.md`、命中则来源标为 `user:prompts/<id>.md`（prompt.rs:59-84），`safety_gate` 的覆盖被**拒绝并 `tracing::warn!`**（prompt.rs:85-91），由 `the_safety_gate_cannot_be_overridden`（prompt.rs:230）钉住——所以「覆盖不可越权测试」这半句已经是既成事实，不该继续挂在未勾选的框里；② **没落地的是 `user_override` 作为节序里的一个编号节**：`render_chat` 发出的四节里没有它，覆盖是「替换某一节的来源」而不是「追加一节用户内容」，§2.1 的 7 节序因此对不上代码。此外还有一处未接线：生产调用方传的 `override_dir` 是 `None`（core.rs:227），且全仓库无任何地方构造 `~/.config/hatchery/prompts` 这个路径（只有 prompt.rs:5 的模块文档提到它）——**用户覆盖目录今天从不被读取**，接线随本节。附带要定的命名口径：覆盖查找用 `{id}.md`，而节 id 是 `mode_chat`（下划线），§2.2 举的例子却写 `mode-code.md`（连字符），二者对不上，加 Code 变体时一并统一
- [ ] (M2 Phase 7) `hatchery config schema` 导出 JSON Schema（roadmap 把它排在 Phase 7；前置是 Phase 2 的 `STRICT_KEYS` 接线与 `[modes.*]`/`[approval_rules]`/工具策略/检查点预算四类 key 进 schema——今天这些 key 一个都不存在，导出的 schema 会缺 M2 的整个安全面）
- [ ] (M4) fluent catalog 落地（FTL 嵌入 + 用户级覆盖）+ `xtask i18n-extract`（id 对账 + 未包裹字符串 lint）+ zh/en 两语 + RTL 冒烟

## 实测记录（2026-09-28，Linux x86_64）

工具链与 crate 元数据（crates.io 索引 + 本机命令）：

- 本机 gettext-tools **0.26** 齐全（xgettext/msgfmt/msgmerge），intltool 0.51.0 也在。
- **glib 0.22.10 与 gtk4 0.11.5 没有任何 gettext 依赖或 feature**（索引实测）→ platform.md 原来「gettext 胜在 GTK 工具链原生」这条理由对 Rust 绑定**不成立**。GTK 的 C 内部仍用 gettext 翻自己的字符串，但与我们应用文案的方案无关。
- `gettext-rs 0.8`（lib 名实际是 `gettextrs`）→ `gettext-sys 0.27`，build-deps 是 `cc` + `temp-dir`；实测**vendored 编译 C 版 libintl**：产出 `out/lib/libintl.a`（853 KB）与 `out/include/libintl.h`。另有 `gettext-system` feature 可改链系统 libintl，但 macOS/Windows 无系统 libintl → 需按平台条件化。
- `fluent-bundle 0.16`：纯 Rust（fluent-syntax/intl_pluralrules/unic-langid/…）。

构建代价（一次性探测，/tmp 里做，未进仓库；24 核）：

| 探测 | 构建耗时 | 备注 |
|---|---|---|
| 仅 fluent-bundle + unic-langid | **3.45 s** | 纯 Rust |
| 仅 gettext-rs | **37.64 s** | 含 vendored libintl 的 C 编译 |
| git2（对照，见 worklog/capabilities.md 与 ADR-0012） | 4.58 s（`default-features=false`）/ 10.5 s（`vendored-libgit2`，含本项目 crate） | 需 C 编译器；**不需要 cmake**——M0a 曾误记为需要，已在 ADR-0012 更正 |

功能实测：

- fluent 复数：`files = { $n -> [one] 1 file *[other] { $n } files }`，`n=1` → `1 file`，`n=5` → `5 files`，errors 为空。
- fluent **默认给插值加 bidi 隔离符**：`n=5` 实际返回 `"\u{2068}5\u{2069} files"`（U+2068 FSI / U+2069 PDI）。对 RTL 目标是加分项，但 golden 测试必须显式保留这两个码点；可用 `set_use_isolating(false)` 关闭（我们不关）。
- gettext-rs 链接与调用正常（未绑 catalog 时返回 msgid 原文，符合预期）。
- xgettext 抽取 Rust：`-L C++ --keyword=gettext --keyword=ngettext:1,2 --keyword=pgettext:1c,2` 能抽到 `gettext("…")`、`ngettext(…)`（含 `msgid_plural`）、`pgettext(ctx, msg)`（含 `msgctxt`）；但 **`gettext!("…")` 宏形式抽不出来**（实测 0 命中，`!` 让 C++ 解析器跳过）→ 原设计的「用宏标记」方案不成立，抽取工具无论如何都得自己写。
- msgfmt UTF-8 中文 `.po` → `.mo` 往返通过（146 字节）。

裁决：**fluent**（用户确认）。决定性理由是「三平台 CI 零系统依赖」——与 ADR-0010 选 turso、影子 Git 选 CLI 同一条理由链；其次是 GTK 集成那条理由被实测推翻。已知代价（.po 译者生态更成熟）记录在 ADR-0011 的「代价」节。

## 开放问题

见设计文档末尾（1 = HATCHERY.md 双文件名，M1 定）。2 与 3 已关闭：

- 2026-09-28 配置 schema 降级策略 → 逐 key 忽略 + warning；安全相关 key 解析失败取最严格默认值并记 error。
- 2026-09-28 gettext vs fluent → fluent（ADR-0011），依据见上「实测记录」。
- 2026-09-28 prompts 存放 → 各 crate `prompts/` 目录，不新建 crate。

## 变更日志

### 2026-10-08 · `cargo xtask coverage` 在报告步骤上根本跑不完

先前就存在的问题，Phase 1 要读覆盖率读数时才撞上：xtask 把 JSON 报告路径写死成 `target/llvm-cov/coverage.json`，而 cargo-llvm-cov 的插桩构建树叫 **`target/llvm-cov-target/`**——`target/llvm-cov/` 从来不存在，而 `--output-path` **不创建父目录**。于是整个覆盖率门禁在**跑完全部插桩测试之后**才失败，报一句 `failed to create file ... No such file or directory`，看起来像覆盖率问题、其实是缺一个 `mkdir`。修法是把路径构造提成 `report_path_under(base)` 并先 `create_dir_all`；收 base 作参数是为了可测（否则单测会在真 target 目录里留东西），测试用 `temp_dir` + pid 自己清干净。

它属 xtask 的范围（「与本仓库自身相关的检查」），不是 CI 平台运维。

### 2026-10-07 · M2 重新规划对账

roadmap 的 M2 段被一次全仓库勘察重写（Phase 0–8 + 决策点 D8–D18），本方向按它重新对账。三件事：

1. **「prompt 管线 v1 已落地」被限定为装配 + 透明性**。勘察发现装配结果从未进过任何一次模型请求：`render_chat`（prompt.rs:57）的唯一非测试调用方是服务 `prompt/render` 的 `DaemonCore::render_prompt`（core.rs:227），`ChatOptions`（kernel/src/message.rs:257-279）没有 system prompt 字段，`StoreHistory::view()` 不产 system 消息，而 `checkpoints_are_not_provider_visible` 在 runtime.rs:618 主动断言消息里没有 `Role::System`。注入归 **M2 Phase 0**、由 **D15** 定渲染时机（建议：runtime 装配时渲染一次并冻结——每轮重渲染会让 environment 节的日期/cwd 破坏前缀稳定性，那正是 KV cache 友好性要的东西；不变量 2 只管分支历史，system prompt 由 `prompt/render` golden 单独钉）。完整实证记在 worklog/daemon.md 的同日条目，本文件不重复。
2. **§2.1 的 7 节里只有 4 节存在**，本文件的待办因此补两项：`tool_discipline`（第 6 节，按 Turn Tool Snapshot 生成；它与 `safety_gate` 一样不接受覆盖，但今天连节都没有）与 `mode-code.md`（嵌入文件今天只有 `identity`/`mode-chat`/`environment`/`safety-gate` 四个）。`project_context`（第 4 节，AGENTS.md 层级发现）本来就在待办里，阶段明确为 **Phase 2**——开放问题 1 的决策 2026-10-01 已定，此前只写「排 M2」，不含阶段。
3. **`user_override` 的文档/代码分歧被拆开**：覆盖**机制**是 M1 既成事实（prompt.rs:59-84 逐节读覆盖文件并标注来源；`safety_gate` 覆盖被拒 + `tracing::warn!`，prompt.rs:85-91，`the_safety_gate_cannot_be_overridden` 钉住），没落地的是它作为 §2.1 节序里的**一个编号节**。顺带查出两处未接线：生产调用方传的 `override_dir` 是 `None`（core.rs:227）、全仓库无处构造 `~/.config/hatchery/prompts`（只有 prompt.rs:5 的文档提到），所以**用户覆盖目录今天从不被读取**；以及覆盖文件名用节 id（`mode_chat.md`，下划线）而 §2.2 的例子写 `mode-code.md`（连字符），加 Code 变体时要一并统一。

另：`hatchery config schema` 按 roadmap 改挂 **Phase 7**，并注明它的前置是 Phase 2 的 `STRICT_KEYS` 接线与四类新 key 进 schema（`[modes.*]`、`[approval_rules]`、工具策略、检查点预算今天都不存在于 `is_known_key`）。

### 2026-09-28
- 初稿。prompt 系统合成三家经验：dsh section 注册表 + atomcode PRECEDENCE/安全门不可覆盖（含测试）+ qwen-code AGENTS.md 层级发现；透明性（可查看/导出最终 prompt）是用户明确需求。
- **M0a i18n spike 完成**：新增 **ADR-0011**（supersedes ADR-0008 的 i18n 部分），platform.md §3 重写为 fluent 方案，testing.md §3.9 的 i18n 检查项改写（pot/po fuzzy → FTL 解析 + id 对账 + 未包裹字符串 lint），开放问题 2/3 关闭。
- 顺带确认代码归属：配置与 prompt 都落 daemon，crate 总数维持 11 + xtask（见 worklog/architecture.md 的「12 个 crate」修正）。
