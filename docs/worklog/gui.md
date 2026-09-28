# 工作记录：GTK 桌面端（hatchery-gui）

- 范围：libadwaita UI、tokio↔GTK 桥接、分支时间线/rewind 面板/prompt 查看器、i18n/RTL、flatpak
- 设计文档：[../design/frontends.md §3](../design/frontends.md)
- 相关 ADR：0001、0003、0008

## 当前状态

设计稿完成，未实现。M4 才开工；此前 GUI 相关协议需求（branch_tree、prompt/render、checkpoint diff）由 protocol/daemon 侧预留。

## 待办

- [ ] (M2) 向 protocol 提需求：branch_tree 查询、checkpoint diff 载荷格式（GUI 是主要消费者）
- [ ] (M4) 应用骨架：AdwApplication + NavigationSplitView + DaemonClient 桥接（glib::Sender 纪律）
- [ ] (M4) 会话列表 + 新会话流程（模式/工作区/模型选择）
- [ ] (M4) 消息列表（GtkListView 虚拟化 + markdown/代码高亮 + reasoning AdwExpanderRow + diff 视图 gtksourceview5）
- [ ] (M4) 审批 AdwAlertDialog + Toast
- [ ] (M4) 设置窗（provider/reasoning/审批规则/prompt 查看/存储与 GC/i18n/外观）
- [ ] (M4) 分支时间线 + rewind 面板
- [ ] (M4) i18n：po 工具链接线、zh/en、`GTK_TEXT_DIR=rtl` 冒烟截图
- [ ] (M4) 10k items 压测 fixture
- [ ] (M4) flatpak manifest + CI（GNOME SDK 容器）

## 开放问题

见设计文档末尾（虚拟化性能、附件输入一致性）。另：

1. diff/markdown 渲染组件选型（gtksourceview5 有 binding；markdown 考虑 gtk4 自绘 or webview——倾向自绘避免 webkit 依赖）——M4 spike。
2. 关窗后后台 turn 的指示方式（托盘图标需要 AppIndicator，flatpak 下行为待验证）——M4。

## 变更日志

### 2026-09-28
- 初稿。选型 gtk4-rs + libadwaita + gettext 由用户确认（ADR-0008）；RTL/i18n 是选 GTK 的主要动因。
