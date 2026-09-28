# 工作记录：runtime daemon（hatchery-daemon）

- 范围：单实例发现、监听与传输、会话管理（租约/代际）、live hub、装配、可观测性
- 设计文档：[../design/daemon.md](../design/daemon.md)
- 相关 ADR：0001、0002

## 当前状态

设计稿完成，未实现。

## 待办

- [ ] (M1) 单实例：daemon.lock + daemon.json + boot token；attach-or-spawn 竞态测试
- [ ] (M1) **daemonize spike**：双 fork vs sd_notify socket activation；结论写回
- [ ] (M1) UDS + stdio 监听、JSON-RPC 分发框架（方法路由表生成自 protocol 类型）
- [ ] (M1) SessionManager：runtime 装配、SessionLease（fs2）、generation 落库与事件过滤
- [ ] (M1) LiveHub v1：per-session broadcast、订阅管理、`session/load` replay（先不做增量 replay window）
- [ ] (M1) 崩溃恢复：status=running → interrupted 标记
- [ ] (M2) hub coalescing（16ms 窗）+ replay window + 慢消费者踢出
- [ ] (M2) 空闲卸载与 daemon 退出策略
- [ ] (M1) tracing 落盘轮转 + `daemon status`

## 开放问题

见设计文档末尾 4 条（daemonize 方式、空闲策略、检查点预算核算频率、token 轮换）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。模型 = codex app-server（传输与协议纪律）+ atomcode Live Hub/租约/代际（会话生命周期纪律）的合成。
