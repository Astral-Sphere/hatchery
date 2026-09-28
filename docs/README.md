# Hatchery 文档索引

Hatchery 是一个用 Rust 编写的开源 AI Agent Harness，支持 CLI 与原生桌面端（GTK4 + libadwaita），完整支持 [ACP](https://agentclientprotocol.com/)（server + client）。

## 阅读路线

**新接手者**请按此顺序阅读：

1. [architecture.md](architecture.md) — 总体架构：进程模型、crate 分层、数据流、核心不变量
2. [references.md](references.md) — 四款参考项目（atomcode / codex / deepseek-harness / qwen-code）的分析结论与借鉴要点
3. [decisions/](decisions/README.md) — 架构决策记录（ADR），了解每个关键选择的理由与被否掉的替代方案
4. [roadmap.md](roadmap.md) — 里程碑 M0–M5 与验收标准
5. 你负责方向的设计文档（见下）+ 对应 [worklog/](worklog/README.md) 工作记录

## 设计文档（按方向）

| 方向 | 设计文档 | 工作记录 | 对应 crate |
|---|---|---|---|
| Wire 协议与数据模型 | [design/protocol.md](design/protocol.md) | [worklog/protocol.md](worklog/protocol.md) | `hatchery-protocol` |
| Agent 循环 / Turn 状态机 | [design/kernel.md](design/kernel.md) | [worklog/kernel.md](worklog/kernel.md) | `hatchery-kernel` |
| LLM Provider 层 | [design/llm.md](design/llm.md) | [worklog/llm.md](worklog/llm.md) | `hatchery-llm` |
| 存储与会话分支 | [design/storage.md](design/storage.md) | [worklog/storage.md](worklog/storage.md) | `hatchery-store` |
| 能力接缝 / 工具 / 审批 / 回滚 | [design/capabilities.md](design/capabilities.md) | [worklog/capabilities.md](worklog/capabilities.md) | `hatchery-capabilities`, `hatchery-tools` |
| Runtime Daemon | [design/daemon.md](design/daemon.md) | [worklog/daemon.md](worklog/daemon.md) | `hatchery-daemon` |
| ACP（server + client） | [design/acp.md](design/acp.md) | [worklog/acp.md](worklog/acp.md) | `hatchery-acp` |
| 前端（CLI TUI + GTK 桌面端） | [design/frontends.md](design/frontends.md) | [worklog/cli.md](worklog/cli.md), [worklog/gui.md](worklog/gui.md) | `hatchery-cli`, `hatchery-gui` |
| 配置 / 提示词 / i18n | [design/platform.md](design/platform.md) | [worklog/platform.md](worklog/platform.md) | 横切 |

## 约定

- 文档语言为中文，代码标识符、协议方法名、技术术语保留英文。
- 设计文档中的 Rust 代码块是**接口草图**（type sketch），不是最终 API；以实际代码为准。
- 每项不可逆或高影响的决策必须落一份 ADR；ADR 一经 accepted 不修改，推翻旧决策时新增 ADR 并标注 supersedes。
- 工作记录（worklog）按方向维护，格式见 [worklog/README.md](worklog/README.md)；做完一件事就更新，不要攒。
- 每个设计文档末尾有「开放问题」节；解决问题后把结论移入正文或 ADR，并在 worklog 留痕。
