# ADR-0008: GUI 用 gtk4-rs + libadwaita，i18n 用 gettext

状态：accepted（2026-09-28）。**i18n 部分已被 [ADR-0011](0011-i18n-fluent.md) 取代**：M0a 实测发现 gtk4-rs/glib 并不集成 gettext，且 `gettext-rs` 需要 vendored 编译 C 版 libintl，故应用文案改用 fluent。本 ADR 的其余决策——gtk4-rs + libadwaita、tokio↔GTK 桥接纪律、RTL 交给 Pango、flatpak 打包——继续有效。

## 背景

桌面端要原生界面。用户对 Rust GUI 生态的判断：各方案对 RTL 支持都有或多或少的问题，部分无 i18n 支持；倾向 gtk4-rs。GTK 的 BiDi/RTL 由 Pango 处理，i18n 有成熟 gettext 工具链。

## 决策

- **gtk4-rs + libadwaita**：获得暗色主题、自适应布局（桌面/移动收敛）、现成组件（AboutDialog、Toast、ViewSwitcher 等）。
- **异步模型**：GUI 进程内跑 tokio runtime（独立线程）负责协议客户端 IO；GTK 主循环与 tokio 之间经 channel 桥接（`glib::idle_add` / `gio::ListStore` 更新），严禁在 GTK 线程 block_on。
- **i18n**：gettext（`gettext-rs`），`.po` 文件管理翻译，GUI 与 CLI 的用户可见文案共用同一 catalog；RTL 交给 GTK/Pango，不自研。
- 文档翻译的配对校验方案（dsh 的 `.i18n.yaml` hash 门禁）留作 docs 国际化时参考，v1 不做。
- 打包：flatpak（freedesktop SDK + GNOME Platform）为首要分发形态；裸二进制可用即可。

## 理由

1. 用户已调研并倾向；libadwaita 大幅降低「像样桌面应用」的 UI 成本。
2. GTK 是 Rust 生态里 i18n/RTL 最完整的路径（对比 egui/iced/slint 的短板）。
3. flatpak 解决 GTK4/libadwaita 版本碎片化，也是 Linux 桌面分发主流。

## 替代方案（已否）

- Tauri/Electron（qwen-code、dsh 桌面端方案）：非原生，且引入 Node/Web 栈，与「Rust 原生」目标相悖。
- 纯 gtk4 不用 libadwaita：视觉与组件全部自建，开发量大且失去自适应布局。

## 后果

- gtk4-rs/libadwaita 版本与系统库耦合，CI 需要 GNOME SDK 容器。
- GUI 功能永远不得绕过协议直连 daemon 内部（保持瘦客户端，ADR-0001）。
- 代码渲染/diff 视图选型（gtksourceview5 有 Rust binding）留待 GUI 设计细化（worklog/gui.md）。
