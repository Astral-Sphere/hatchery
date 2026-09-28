# 工作记录：runtime daemon（hatchery-daemon）

- 范围：单实例发现、监听与传输、会话管理（租约/代际）、live hub、装配、可观测性
- 设计文档：[../design/daemon.md](../design/daemon.md)
- 相关 ADR：0001、0002、0009

## 当前状态

设计稿完成，未实现（M1）。M0a 的两项决定落在这里：**配置加载与 prompt 装配的代码归属 daemon**（前端一律经 `config/get|set`、`prompt/render` 协议访问，没有第二个消费者 → 按 ADR-0009 反预拆分不新建 platform/prompts crate）；启动审计要多查一类东西——外部二进制。

## 待办

- [ ] (M1) 单实例：daemon.lock + daemon.json + boot token；attach-or-spawn 竞态测试
- [ ] (M1) **daemonize spike**：双 fork vs sd_notify socket activation；结论写回
- [ ] (M1) UDS + stdio 监听、JSON-RPC 分发框架（方法路由表生成自 protocol 类型）
- [ ] (M1) SessionManager：runtime 装配、SessionLease（fs2）、generation 落库与事件过滤
- [ ] (M1) LiveHub v1：per-session broadcast、订阅管理、`session/load` replay（先不做增量 replay window）
- [ ] (M1) 崩溃恢复：status=running → interrupted 标记
- [ ] (M1) profile 化装配表 + 启动 fail-loud 审计（daemon.md §3.1，ADR-0009）+ 审计测试
- [ ] (M1) 启动审计包含**外部依赖检查**：`git --version`（影子 Git 是 Code 模式硬依赖，M0a 实测选定 CLI 后端而非 git2，见 worklog/capabilities.md）、数据目录可写、UDS 目录存在；缺失即拒绝服务并列出清单
- [ ] (M1) 配置分层加载（platform.md §1）+ prompt 管线 v1（§2）落本 crate；坏 key 逐条忽略 + warning（platform.md 开放问题 2 的裁决）
- [ ] (M1) disposer 逆序 teardown（订阅者→runtime→检查点→store→监听器）+ 逆序测试
- [ ] (M2) hub coalescing（16ms 窗）+ replay window + 慢消费者踢出
- [ ] (M2) 空闲卸载与 daemon 退出策略
- [ ] (M1) tracing 落盘轮转 + `daemon status`

## 开放问题

见设计文档末尾 4 条（daemonize 方式、空闲策略、检查点预算核算频率、token 轮换）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。模型 = codex app-server（传输与协议纪律）+ atomcode Live Hub/租约/代际（会话生命周期纪律）的合成。
- ADR-0009 落地：daemon.md 新增 §3.1 profile 化装配（local/headless/acp-stdio/acp-standalone 四捆绑）+ 启动 fail-loud 审计（dsh `auditStartupEntries` 语义）；runtime 卸载/关闭明确 disposer 逆序规则；注册句柄模式（Handle{dispose, replace}）用于运行中换 provider/MCP 连接。
