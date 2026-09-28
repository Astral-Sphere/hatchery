# 工作记录（worklog）使用说明

每个方向一个文件，是该方向的**单一事实来源**：现状、待办、开放问题的解决过程、给接手者的提示。设计文档描述「应该是什么样」，worklog 记录「现在到哪了、为什么」。

## 约定

- **做完一件事就更新**，不要攒；条目短没关系，断更才是问题。
- 每条日志格式：`### YYYY-MM-DD` + 要点（做了什么 / 发现了什么 / 决定了什么 / 踩了什么坑）。
- 决定如果影响架构 → 升级为 ADR，并在 worklog 条目里链接。
- 开放问题解决后：结论写进设计文档正文，worklog 记录解决过程与证据（**实测的标"实测"，推测的标"推测"**——本项目纪律）。
- 待办用 checkbox，标注目标里程碑，如 `- [ ] (M1) …`。

## 新接手者流程

1. 读 [../README.md](../README.md) 的阅读路线；
2. 读你方向的 worklog（本目录）→ 对应设计文档 → 相关 ADR；
3. 从「待办」认领，从「开放问题」里挑能推进的；
4. 动手前跑 `cargo test`（有代码后），确认基线绿。

## 方向索引

| 文件 | 方向 | 设计文档 |
|---|---|---|
| [architecture.md](architecture.md) | 总体架构 / 跨方向协调 | ../architecture.md |
| [protocol.md](protocol.md) | wire 协议 | ../design/protocol.md |
| [kernel.md](kernel.md) | agent 循环 | ../design/kernel.md |
| [llm.md](llm.md) | LLM provider 层 | ../design/llm.md |
| [storage.md](storage.md) | 存储与分支 | ../design/storage.md |
| [capabilities.md](capabilities.md) | 接缝/工具/审批/回滚 | ../design/capabilities.md |
| [daemon.md](daemon.md) | runtime daemon | ../design/daemon.md |
| [acp.md](acp.md) | ACP server + client | ../design/acp.md |
| [cli.md](cli.md) | CLI TUI + headless | ../design/frontends.md §2 |
| [gui.md](gui.md) | GTK 桌面端 | ../design/frontends.md §3 |
| [platform.md](platform.md) | 配置/提示词/i18n | ../design/platform.md |
| [testing.md](testing.md) | 测试体系 | ../design/testing.md |
