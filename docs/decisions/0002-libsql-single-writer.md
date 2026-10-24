# ADR-0002: 存储用 daemon 内嵌 libSQL（WAL）+ 单写者 actor

状态：accepted（2026-09-28）。**引擎选型部分已被 [ADR-0010](0010-storage-engine-turso.md) 取代**：M0a 实测后选定 `turso` 0.7.2（纯 Rust）而非 `libsql` crate。本 ADR 的其余决策——daemon 内嵌、WAL、单写者 actor、`SessionStore` trait 隔离、JSONL 导出逃生门——继续有效，下文「引擎」相关表述以 ADR-0010 为准。

## 背景

四款参考项目的会话转录主存储全部是 append-only JSONL，没有一家用数据库做主存储（codex 的 SQLite thread-store 是后加的索引/查询层）。用户最初设想「turso + 全局 daemon 负责数据并发读写」。需要确定：存储引擎、进程形态、并发模型、与「历史可编辑」需求的关系。

事实基础（SQLite/libSQL 既定行为，非本项目实测）：WAL 模式支持多读并发 + 单写；写锁互斥，跨线程/进程以 busy_timeout 排队；不支持多写者并行。

## 决策

- 引擎：libSQL（turso 的开源引擎），以 embedded 方式链接进 daemon，数据库文件 `~/.local/share/hatchery/hatchery.db`，WAL 模式，`synchronous=NORMAL`。
- 写路径：daemon 内所有持久化事件汇入 mpsc channel，由**专职 writer actor task 串行提交**；item/turn 边界即 commit（及时性），流式 delta 不落库、内存聚合。
- 读路径：多个只读连接并发查询（UI 列表、历史重建、分支切换），不与写者互斥（WAL）。
- 进程形态：单 daemon 即唯一写进程；CLI/GTK 不直接打开数据库文件，一律经协议访问。
- 抽象：`SessionStore` trait 隔离引擎；预留独立 sqld/turso server（HTTP）后端实现位，不在 v1 做。
- 可选 JSONL 导出（审计/迁移），导入不在 v1。

## 理由

1. 「历史可编辑（分叉+删除）」需要关系查询与引用完整性，纯 JSONL 做分支树是逆水行舟；dsh 干脆放弃编辑只留 fork。
2. agent 会话写入吞吐极低（每秒几十 item 封顶），单写者串行不构成瓶颈；actor 模式反而免去写锁竞争、天然保序、可批量。
3. daemon 已是全系统唯一常驻进程（ADR-0001），存储并入同进程不增加部署面。
4. 「model-visible = logged」不变量由 append-only items 表 + active 分支重建保证（与 JSONL 派同等强度）。

## 替代方案（已否）

- JSONL 主存储 + libSQL 索引（codex 式）：两份真相需要同步，编辑分叉时索引一致性复杂。
- 独立 turso/sqld server 进程：多一层部署与运维，v1 收益为零；trait 已留逃生门。
- 多写连接 + busy_timeout：可行但乱序与重试逻辑上移到业务层，不如 actor 干净。

## 后果

- daemon 崩溃时最多丢失「已开始未 commit」的当前 item；WAL 保证已提交数据完整。
- schema 迁移需要版本管理（libSQL migration 表），从 M0 就定好 `user_version` 纪律。
- 数据库文件是单点：备份 = 复制文件（WAL checkpoint 后）；导出 JSONL 作为逃生通道。
