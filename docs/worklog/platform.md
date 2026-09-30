# 工作记录：配置 / 提示词 / i18n（横切）

- 范围：分层配置、prompt section 管线、i18n catalog 与工具链、术语表
- 设计文档：[../design/platform.md](../design/platform.md)
- 相关 ADR：0005（模式 prompt 变体）、0008（GUI/RTL）、**0011**（i18n 用 fluent）

## 当前状态

设计稿完成 + **i18n 方案已实测定型（fluent，ADR-0011）**。配置加载与 prompt 管线未实现（M1）；i18n 落地在 M4。

代码归属已定：配置加载与 prompt 装配都落在 `hatchery-daemon`（唯一消费者——前端一律经 `config/get|set`、`prompt/render` 协议访问），**不新建 `hatchery-platform` 或 `hatchery-prompts` crate**（ADR-0009 反预拆分刹车）。

## 待办

- [x] (M0) **i18n spike**：gettext-rs vs fluent（复数、上下文标注、GTK 集成度、构建代价）→ 结论 fluent，见下「实测记录」，已落 ADR-0011 + platform.md §3
- [x] (M0) prompts 存放位置定案：各 crate 自己的 `prompts/` 目录 + `include_str!`，不新建 crate
- [x] (M0) 配置 schema 校验失败降级策略定案：逐 key 忽略 + warning（安全 key 取最严格默认值并升 error 日志）
- [ ] (M1) 分层配置加载 + per-key origins + 项目级安全边界（覆盖硬门时忽略 + warning）
- [ ] (M1) prompt 管线 v1：identity/mode_variant/environment/safety_gate 四 section + `{{var}}` 插值 + PRECEDENCE 声明
- [ ] (M1) `prompt/render` + CLI `/prompt`
- [ ] (M1) AGENTS.md 发现与注入（层级向上 + 与 HATCHERY.md 的兼容策略定案）
- [ ] (M1) **environment 节只准用 git plumbing 命令**：实测 `git status` 会重写用户的 `.git/index`（worklog/capabilities.md），prompt 装配属于后台行为，绝不能有这种副作用
- [ ] (M1) docs/glossary.md 术语表初版
- [ ] (M2) user_override section（`~/.config/hatchery/prompts/`）+ 覆盖不可越权测试
- [ ] (M2) `hatchery config schema` 导出 JSON Schema
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

### 2026-09-28
- 初稿。prompt 系统合成三家经验：dsh section 注册表 + atomcode PRECEDENCE/安全门不可覆盖（含测试）+ qwen-code AGENTS.md 层级发现；透明性（可查看/导出最终 prompt）是用户明确需求。
- **M0a i18n spike 完成**：新增 **ADR-0011**（supersedes ADR-0008 的 i18n 部分），platform.md §3 重写为 fluent 方案，testing.md §3.9 的 i18n 检查项改写（pot/po fuzzy → FTL 解析 + id 对账 + 未包裹字符串 lint），开放问题 2/3 关闭。
- 顺带确认代码归属：配置与 prompt 都落 daemon，crate 总数维持 11 + xtask（见 worklog/architecture.md 的「12 个 crate」修正）。
