# ADR-0003: 历史编辑 = 编辑即分叉，允许物理删除分支

状态：accepted（2026-09-28）

## 背景

用户要求「历史记录要可以自由修改（很多 Agent 做不到）」。两种语义：

- 原地覆写：直接改历史记录。UI 直觉，但破坏可审计性——「模型当时看到的上下文」与库中记录产生分歧，压缩/回放/ACP replay 全部要特判。
- 编辑即分叉：修改任意历史 item 后从其 parent 派生新分支重新生成，旧分支保留。qwen-code branch-points、codex fork、dsh fork 都是此范式的变体。

用户已确认：分叉为主，**同时允许物理删除**（隐私需求）。

## 决策

- items 表 append-only：每条 item 带 `parent_item_id` 构成树；session 持 `active_branch_head` 指针。
- 编辑 item = 以该 item 的 parent 为基点 append 新 item（新分支），切换 active 指针；可选联动影子 Git 回滚代码（ADR-0006，`RewindScope::{Conversation, Code, Both}`）。
- 分支切换：`active_branch_head` 指向任意历史节点即完成，历史重建 = 从 head 沿 parent 链回溯。
- 删除分支：显式级联删除子树 items + 关联检查点记录 + 影子 Git 对应 commit 的清理（或标记 unreachable 交由 GC）；删除不可恢复，UI 需二次确认。
- 上下文压缩（compaction）产生的摘要也是一种 item（kind=compaction），同样不可变、可分叉。

## 理由

1. 保住不变量「模型看到的 = active 分支重建的」，LLM 请求上下文永远可从库中确定性重建。
2. 分叉免费获得 undo：改错了切回旧分支即可。
3. 与 ACP `session/load` replay、多前端视图投影兼容——它们都只需要「按 active 分支重放 items」。

## 替代方案（已否）

- 原地覆写：见背景。
- dsh 式「只 fork 不可删」：不满足隐私删除需求。

## 后果

- item 树的级联删除靠外键 `ON DELETE CASCADE`，树遍历在 Rust 内做（引擎无 `WITH RECURSIVE`，见 ADR-0010）；两者已于 M0a 实测成立（`crates/hatchery-store/tests/spike_engine.rs`），M0b 补完 schema 落地与 store 层查询测试。
- 删除分支时若 `active_head` 仍在子树内，引擎的外键会直接拒绝——应用层校验之外还有一层数据库兜底。
- 删除分支与影子 Git 检查点的引用完整性：检查点记录挂在 item 上，级联删除时一并处理。
- UI 需要分支可视化（树/时间线），GTK 端工作量增加，但这也是差异化卖点。
