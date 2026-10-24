# ADR-0006: 代码回滚用影子 Git 检查点

状态：accepted（2026-09-28）。**后端选型已由 [ADR-0012](0012-shadow-git-git2-vendored.md) 关闭**：影子 Git 用 `git2`（vendored libgit2），不依赖用户机器上的 git 二进制。本 ADR 的其余决策——独立 git-dir、检查点时机、预算熔断、`RewindScope`——继续有效；下文凡提到 `git --git-dir=… --work-tree=…` 的地方，实现形态以 ADR-0012 为准。

## 背景

Code 模式下 agent 可能改坏工作区文件；对话分叉（ADR-0003）回退了「说过的话」，还需要能回退「改过的文件」。参考实现三种：atomcode 影子 Git（独立 `--git-dir/--work-tree`）、qwen-code 文件快照表（fileHistoryService）、不做（依赖用户 git）。用户已确认选影子 Git。

## 决策

- 每个绑定工作区的会话对应一个影子仓库：`--git-dir = ~/.local/share/hatchery/checkpoints/<workspace-hash>/`，`--work-tree = 用户工作区`。**绝不**读写用户仓库的 HEAD/index/refs；用户工作区是否 git 仓库无关紧要。
- 检查点时机：`LocalFs` 每次写入/删除前、shell 工具执行前（粗粒度快照），commit 进影子仓库，commit message 携带 item_id 关联。
- 检查点记录（commit hash + item_id + 时间）入 items 表（kind=checkpoint），随分支级联删除（ADR-0003）。
- 回滚：`RewindScope::{Conversation, Code, ConversationAndCode}`；Code 回滚 = `git --git-dir=… --work-tree=… checkout/restore` 到目标检查点。
- 预算与熔断（借鉴 atomcode）：单工作区检查点存储预算（默认 500MB）+ 全局磁盘熔断（默认 2GiB），超限自动 GC 最旧检查点；大文件（>阈值）跳过快照并记录。
- 并发：同一工作区多会话共享一个影子仓库，写入经 daemon 内 per-workspace 互斥锁串行化。

## 理由

1. 天然获得 diff 视图（GUI 展示「这个 turn 改了什么」= 两个检查点间 git diff）。
2. 处理新建/删除/改名等边界情况是 git 的本职，文件快照表要自己重造。
3. 与用户自己的 git 完全隔离，不会污染 status/stash/HEAD。

## 替代方案（已否）

- 文件 before 镜像表：无 diff、边界情况多。
- 依赖用户 git：风险转嫁用户，且非 git 工作区不可用。

## 后果

- 需要 careful 的 git2 或 CLI-git 选型（见 worklog/capabilities.md 开放问题）。
- 影子仓库 GC 与磁盘预算逻辑必须有测试；熔断行为要可配置。
- shell 工具可以在检查点粒度之间做任意多次写入，回滚精度是「工具调用级」而非「系统调用级」——文档需向用户言明。
