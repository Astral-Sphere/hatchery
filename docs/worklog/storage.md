# 工作记录：存储与分支（hatchery-store）

- 范围：SessionStore trait、libSQL schema、writer actor、分支查询、迁移、JSONL 导出
- 设计文档：[../design/storage.md](../design/storage.md)
- 相关 ADR：0002、0003、0006

## 当前状态

设计稿完成，未实现。schema v1 与 writer actor 命令表已定。

## 待办

- [ ] (M0) **libSQL spike（实测，勿靠文档推断）**：embedded WAL 多读连接并发、busy 行为、append-only 触发器、`user_version`/迁移、崩溃后 WAL 恢复；结论写回本文件
- [ ] (M0) schema v1 落地 + 迁移框架（schema_meta）
- [ ] (M0) writer actor + StoreCmd 全量实现 + 有界背压
- [ ] (M0) rebuild_history（递归 CTE + compaction 区间应用）+ 属性测试（随机编辑序列 vs 纯 Vec 参考实现）
- [ ] (M0) EditFork / SwitchBranch / DeleteBranch（级联 + active_head 校验）+ 崩溃测试（kill -9）
- [ ] (M2) checkpoints 表与 CheckpointStore 的联动（级联删除时 GC）
- [ ] (M2) JSONL 导出
- [ ] (M5) 导入

## 开放问题

见设计文档末尾 3 条（libsql crate 选型、payload 二级索引、孤儿仓库 GC）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。关键取舍：数据库做主存储（四家参考都用 JSONL，hatchery 因「历史可编辑」需求反向选择）；单写者 actor 规避 libSQL 多写者限制（用户最初的「并发写入」诉求以此方式满足，见 ADR-0002 理由节）。
