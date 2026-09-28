# 工作记录：CLI（hatchery-cli）

- 范围：ratatui TUI、headless exec、子命令族（daemon/doctor/sessions/acp 入口）
- 设计文档：[../design/frontends.md §2](../design/frontends.md)
- 相关 ADR：0001、0008（i18n 部分）

## 当前状态

设计稿完成，未实现。

## 待办

- [ ] (M1) **TUI 渲染 spike**：markdown/diff 渲染选型（termimad vs syntect 自绘）；结论写回
- [ ] (M1) 最小 TUI：消息流 + 输入区 + reasoning 折叠 + 状态栏（mode/model/effort）
- [ ] (M1) 斜杠命令 v1：/mode /effort /model /prompt /quit
- [ ] (M1) headless exec（纯文本流 + --json JSONL + 退出码语义）
- [ ] (M1) daemon 子命令 + attach-or-spawn 接线 + doctor
- [ ] (M2) 审批内联弹层（diff/命令预览 + 快捷键）、/rewind /branch /edit、/export
- [ ] (M2) sessions 子命令族（list/resume/export/delete）
- [ ] (M4) 文案全部进 gettext catalog

## 开放问题

见设计文档末尾（渲染选型、REPL 极简模式、图片输入一致性）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。原则：TUI 是事件流投影，零本地状态机复制（吸取 qwen-code CLI 进程内直连导致双接线的教训，见 ADR-0001）。
