# ADR-0011: i18n 用 fluent（纯 Rust），不用 gettext

状态：accepted（2026-09-28）

Supersedes: ADR-0008 的 **i18n** 部分（gettext / `gettext-rs` / `.po`）。ADR-0008 的其余决策——gtk4-rs + libadwaita、tokio↔GTK 桥接纪律、RTL 交给 Pango、flatpak 打包——继续有效。

## 背景

ADR-0008 选 gettext 的两条理由是「GTK 工具链原生」与「成熟工具链」；design/platform.md 开放问题 3 要求 M0 做一次 spike（复数、上下文标注）后锁定。M0a 实测（2026-09-28，Linux x86_64，本机 gettext-tools 0.26）：

- **glib 0.22.10 与 gtk4 0.11.5 的 crate 元数据里没有任何 gettext 依赖或 feature**（crates.io 索引实测）→「GTK 工具链原生」对 Rust 绑定不成立。GTK 的 C 内部仍用 gettext 翻译它自己的字符串，但那与我们应用文案的方案选择无关。
- `gettext-rs 0.8` → `gettext-sys 0.27`：**vendored 编译 C 版 libintl**（实测产出 `out/lib/libintl.a` 853 KB + `out/include/libintl.h`），单独构建 **37.64 s**。它另有 `gettext-system` feature 可改链系统 libintl，但 macOS/Windows 没有系统 libintl → 需要按平台条件化 feature，正是 ADR-0010 为 turso 避开的那种负担。
- `fluent-bundle 0.16`：纯 Rust，单独构建 **3.45 s**；复数实测可用（`n=1` → `1 file`，`n=5` → `5 files`）；**默认给插值加 Unicode bidi 隔离符**（实测输出 `"\u{2068}5\u{2069} files"`，即 FSI/PDI），可用 `set_use_isolating(false)` 关闭——对 ADR-0008 的 RTL 目标是加分项。
- `xgettext 0.26` 能从 Rust 源码抽取，但**只认函数调用形式**：`gettext("…")`、`ngettext(…)`、`pgettext(ctx, msg)` 都抽到了（含 `msgctxt` 与 `msgid_plural`），而 `gettext!("…")` 宏形式抽不出来（实测 0 命中，因为 `-L C++` 解析器不接受 `!`）→ platform.md 原方案「用 `gettext!`/`gettext_noop!` 宏标记」不成立。**无论选哪个方案，`xtask i18n-extract` 都得自己写。**
- `msgfmt`/`msgmerge` 本机可用，UTF-8 中文 `.po` → `.mo` 往返实测通过（146 字节）。

## 决策

- 应用文案 i18n 用 **fluent**：`fluent-bundle` + `unic-langid`，语言协商需要时加 `fluent-langneg`。
- catalog 是 FTL 文件：默认随二进制嵌入（`include_str!`），用户级覆盖放 `~/.config/hatchery/locales/<lang>/*.ftl`（M4 落地）。
- 上下文用 FTL 的 message id 命名约定表达（如 `approval-allow-button = Allow`）+ 注释；复数/选择性用 FTL 的 `->` selector（表达力强于 gettext 的 plural forms）。
- 查表层在**前端进程内**（CLI 与 GUI 各自渲染文案），共用同一个薄封装 crate 内模块；daemon 只透传语言设置（`[ui] language`），不参与文案格式化。
- bidi 隔离默认**开启**（服务 RTL 目标）；golden 测试里显式写出 U+2068/U+2069，避免被当成噪声清理掉。
- 提取与校验走 `xtask i18n-extract`（M4）：扫描源码中的 id 引用，与 FTL catalog 对账，报告缺失/多余/未包裹字符串。

## 理由

1. **三平台 CI 零系统依赖**——与 ADR-0010 选 turso 同一条理由链：不引入 C 构建，不要求 macOS 装 brew gettext、不要求 Windows MSYS2 装 mingw gettext。
2. **「GTK 原生集成」这条理由经实测不成立**，gettext 在本项目里没有额外杠杆。
3. 构建代价实测差一个数量级（3.45 s vs 37.64 s）。
4. fluent 的 selector 表达力与默认 bidi 隔离，正对「Rust GUI 的 RTL/i18n 有问题」这一原始关切（ADR-0008 背景）。
5. 抽取工具两边都得自写，gettext 的「标准工具链」优势被实测削弱。

## 代价

- **译者生态变窄**：`.po` 有 Weblate/poedit/Transifex 的成熟习惯与工具，FTL 主要靠 Pontoon 支持。对希望社区翻译的开源项目这是真实代价（已向用户说明并由用户裁决）。缓解：FTL 是纯文本、格式简单，写贡献指南即可上手；必要时提供一次性 FTL→po 导出脚本（非 v1 目标）。
- 提取/校验工具自建（gettext 有 xgettext/msgmerge/msgfmt 现成链路，本机实测可用，但只能用到函数形式）。
- fluent 的 Rust 生态小于 gettext，上游响应不确定。缓解：查表层极薄（一个 bundle + `format_pattern`），必要时可整体替换实现而不影响调用点。

## 替代方案（已否）

- **gettext（`gettext-rs`）**：见背景与代价。若 M4 起 flatpak 已经强依赖 GNOME 运行时，gettext 的系统依赖成本会下降，可重新评估——但纯 Rust 与 bidi 隔离两项优势仍在。
- **不做 i18n（只英文）**：违背用户明确需求（i18n/RTL 正是选 GTK 的动因）。
- **自研查表 + JSON catalog**：CLDR 复数规则极难写对，重复造轮子。

## 后果

- design/platform.md §3 重写（gettext → fluent：catalog 布局、查表层归属、语言协商、RTL 说明）。
- design/testing.md §3.9 的 i18n 检查项改写：不再是 pot/po fuzzy 检查，而是「FTL 解析无错 + 每个 id 在所有语言都存在 + 未包裹字符串的启发式 lint」。
- ADR-0008 状态行标注其 i18n 部分被本 ADR 取代；ADR 索引同步。
- `xtask i18n-extract` 的输出目标从 `.pot` 改为「FTL id 清单 + 缺失/多余报告」（M4）。
- worklog/platform.md 记录实测数据与本次裁决；开放问题 3 关闭。
