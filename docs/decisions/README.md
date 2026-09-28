# 架构决策记录（ADR）

记录 hatchery 每一项不可逆或高影响的设计决策：背景、决策、理由、被否掉的替代方案、后果。

约定：
- 编号递增，永不复用；推翻旧决策时新增 ADR 并标注 `Supersedes: ADR-XXXX`。
- 状态：`accepted`（已定）/ `proposed`（讨论中）/ `superseded`（被推翻）。
- 模板见任意现有 ADR，新增时复制结构即可。

## 索引

| 编号 | 标题 | 状态 |
|---|---|---|
| [0001](0001-runtime-daemon.md) | 全协议化 runtime daemon，前端皆瘦客户端 | accepted |
| [0002](0002-libsql-single-writer.md) | 存储：daemon 内嵌 libSQL + 单写者 actor | accepted（引擎选型部分被 0010 取代） |
| [0003](0003-edit-as-fork.md) | 历史编辑 = 编辑即分叉，允许删除分支 | accepted |
| [0004](0004-capability-seam-acp.md) | capability seam 支撑完整 ACP（含 fs/terminal 委派） | accepted |
| [0005](0005-chat-code-modes.md) | Chat/Code 模式 = 工具集 × 审批 × prompt 变体 | accepted |
| [0006](0006-shadow-git-rewind.md) | 代码回滚用影子 Git 检查点 | accepted |
| [0007](0007-llm-openai-interface.md) | LLM 层基于 openai-interface，reasoning 逐字节回放 | accepted |
| [0008](0008-gui-gtk4-libadwaita.md) | GUI 用 gtk4-rs + libadwaita，i18n 用 gettext | accepted（i18n 部分被 0011 取代） |
| [0009](0009-modularity-strategy.md) | 模块化：编译期装配 + Cordis 纪律借鉴，不做动态插件树 | accepted |
| [0010](0010-storage-engine-turso.md) | 存储引擎用 turso 0.7.2（纯 Rust），递归 CTE 改内存走树 | accepted |
| [0011](0011-i18n-fluent.md) | i18n 用 fluent（纯 Rust），不用 gettext | accepted |
