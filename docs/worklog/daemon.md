# 工作记录：runtime daemon（hatchery-daemon）

- 范围：单实例发现、监听与传输、会话管理（租约/代际）、live hub、装配、可观测性
- 设计文档：[../design/daemon.md](../design/daemon.md)
- 相关 ADR：0001、0002、0009

## 当前状态

**M1 Phase 3 代码完成（2026-10-01，评审③）**：配置分层、prompt 管线、单实例发现、UDS 传输、SessionManager、LiveHub、方法接线、审计与 disposer 全部落地；TestDaemon/ClientProbe 经真实 socket 的双前端扇出测试通过。`./scripts/ci.sh` 全绿（除两个**有意修改**的 protocol fixture 未提交导致的 determinism 步骤——提交后恢复）。

**M1 Phase 4 补齐（2026-09-30，评审④）**：生产入口 `entry`（audit → 锁 → store → serve → 信号驱动的逆序 teardown）、stdio 监听（连接循环泛化出传输）、tracing 落盘轮转（日轮转 + 14 天保留）、空闲 sweep 调度、`doctor` 探测模块。CLI 半边见 `worklog/cli.md`。

**M1 Phase 5 收口（2026-10-01，评审⑤）**：四处行为修正（见变更日志）+ e2e 场景与不变量组落地（hatchery-tests，测试面记录见 `worklog/testing.md`）。

## 待办

- [x] (M1) 单实例：daemon.lock（fs2 独占）+ daemon.json（0600、write-then-rename）+ boot token；锁独占与死 pid 判定已测（attach-or-spawn 的 CLI 半边随 Phase 4）
- [x] (M1) **daemonize spike（D1 定案，见变更日志）**：CLI spawn 分离进程；不做双 fork、不依赖 sd_notify
- [x] (M1) UDS 监听 + JSON-RPC 分发（`DaemonCore::dispatch` 无传输纯函数面，server.rs 只做帧收发）；stdio 随 Phase 4
- [x] (M1) SessionManager：runtime 装配、generation **落库（store 新增 `bump_generation`）**、事件代际过滤（信封由 daemon 附带）
- [x] (M1) LiveHub v1：per-session broadcast(4096)、隐式订阅、无 replay window（tokio broadcast 无接收者不缓冲，与设计一致：重连走 `session/load`）
- [x] (M1) 崩溃恢复：running/waiting → idle（启动时 `recover_crashed_sessions`）
- [x] (M1) 启动 fail-loud 审计（provider 缺 key / 数据目录 / 状态目录，一次列全）
- [x] (M1) 配置分层加载 + prompt 管线 v1 落本 crate；坏 key 逐条忽略 + warning
- [x] (M1) disposer 逆序 teardown + **panic 容错**（一步炸了其余照跑）+ 逆序测试
- [x] (M1) stdio 监听 + tracing 落盘轮转 + 生产入口 `entry`（2026-09-30：`serve_connection` 对读写半泛型，duplex 测试证明管道与 socket 同核；日志日轮转 + 启动时按日期字符串修剪，`RUST_LOG` 过滤）
- [x] (M1) 空闲卸载的后台 sweep 任务（`entry` 内 60s 间隔 tick `sweep_idle`，D2 判定仍在 sweep 内）
- [x] (M1) `doctor` provider 实测探测模块（`doctor::probe_provider` 走真实 `LlmProvider` 轮次；离线 wiremock 验证；真实两家探测由 CLI 触发，留痕见 `worklog/cli.md`）
- [ ] (M2) hub coalescing（16ms 窗）+ replay window
- [ ] (M2) SessionLease 文件锁（当前单 daemon 内 HashMap 槽位已防同会话双 runtime；跨进程租约随 M2 检查点一起）

## 开放问题

见设计文档末尾 4 条（daemonize 方式、空闲策略、检查点预算核算频率、token 轮换）。解决过程记录于此：

- **Phase 4 实测备注（2026-09-30）**：`pid_is_alive` 的「自述 pid = 陈旧文件」判定与 TestDaemon 的进程内 daemon 冲突（测试发布者就是测试进程）。生产语义保留不动，测试改走 `discover()`（不做存活过滤）+ `attach_to()`（跳过发现的直接握手接缝）。
- **Phase 4 实测备注（2026-09-30）**：单进程内的两次 `acquire_instance`：fs2 走 flock(LOCK_EX|LOCK_NB)，同进程异 fd 同样冲突（`a_second_start` 集成测试钉住）。

## 变更日志

### 2026-10-01 · M1 Phase 5 收口（评审⑤）

四处修正，全部先有测试再改（e2e 的失败暴露了前三处）：

1. **turn 在跑的判定换成状态机自己的话**（kernel 加 `AgentHandle::turn_running()`，watch 镜像 `TurnState::is_active`）：`SessionRuntime::is_busy` 原来的 `!handle.is_closed()` 测的是「agent 活着」——manager 的忙拒因此缺失（kernel 静默丢弃 mid-turn prompt，调用方却拿到 Ok），`sweep_idle` 的 `!is_busy` 谓词永远为假，**空闲卸载从未触发过**（D2 形同虚设）。
2. **`session/prompt` 忙拒**：turn 在途时返回 `TurnInProgress`（`core::turn_in_progress()` 终于有了调用方）；e2e 用 MockWire 延迟 5s 制造确定性在途 turn，第二 prompt 拒绝 + cancel 后原 turn 以 `Interrupted`（`TurnFinished`，不是 `TurnFailed`——中断是一种正常收束）收束。
3. **`session/load` 的 `replay_from` 语义修正**：协议文档说「该 item 之后」，实现却把它当 `rebuild_chain` 的 head（「到 head 为止」）直透——重连补差会拿到整段旧历史装作成功。现在在 manager 解析：活动分支上定位游标、只返回其后 item；游标不在活动分支上按 `InvalidRequest` 拒绝（宁可报错也不整段重放）。
4. **echo 判定与 adapter 同源**：manager 原来用 `CapabilityTable::builtin()` 决定推理是否回填历史，provider 却用 config 覆盖后的表决定请求——config 的 `echo_reasoning` 覆盖只对一半生效。`ProviderConfig::capability_table()` 成为唯一折算点，`provider_for` 一并返回 echo。

### 2026-10-01 · M1 Phase 4 补齐（评审④）

1. **生产入口 `entry::run_until`**：TLS 安装 → 日志初始化 → 配置 → **审计一次列全** → 实例锁 → store → 崩溃恢复 → 发布 → serve → 信号取消 → disposer 逆序（daemon.json 先清、socket 后删）→ 锁释放。测试走 `run_until(options, token)`：把信号换成 cancellation token，生产与测试同一条路径。
2. **传输泛化**：`serve_connection` 从 `UnixStream` 改为对读/写半的泛型，UDS 与 stdio 管道同一条连接循环；`serve_stdio` 是它的 stdin/stdout 实例。duplex 双通道测试钉住「管道上同一批帧、同一个核」。
3. **日志**：tracing-appender 日轮转（`hatchery.log.YYYY-MM-DD`）+ 启动时保留 14 天修剪（`YYYY-MM-DD` 字典序即时间序，形状不对的文件不动）；`RUST_LOG` 过滤，默认 info。
4. **sweep 调度**：60s 间隔 tick `sweep_idle`；D2（忙则不卸）判定仍在 sweep 的谓词里。
5. **doctor**：环境检查（provider/env key/目录可写，报告而非拒绝）+ `probe_provider`——真实 `LlmProvider` 轮次（含重试、能力表、翻译），流上实测 reasoning 字符数、finish reason、usage、RateLimited 次数；30s 上限。
6. **测试留痕**：daemon crate 58 项（entry 集成 3 项：UDS 全生命周期 + 单实例拒绝 + 审计点名缺失 env key；stdio 1 项）。

### 2026-10-01 · M1 Phase 3 落地（评审③）

**四个 M1 决策点在此定案（D1/D2/D4 + 传输帧形）：**

1. **D1 daemonize**：CLI spawn 分离进程（unix `setsid` / Windows `DETACHED_PROCESS`），systemd 用户直接跑前台 `hatchery daemon run`。双 fork 的复杂度与 sd_notify 的依赖都不值得——握手代码就在 CLI 里，轮询 socket 出现即完成 attach。实现随 Phase 4 的 `hatchery daemon start`。
2. **D2 无订阅者在途 turn**：跑完为止。结果无论如何落库，杀掉一个没人看着的 turn 是烧掉已付费的推理。
3. **D4 boot token**：每次 daemon 启动换新 token，`daemon.json` 0600 write-then-rename；客户端 attach 时现读。死 pid 判定 best-effort 无 unsafe（Linux 走 `/proc`，其余平台保守视为活、由 hello 握手兜底——workspace `unsafe_code = deny`，为 kill(0) 开洞不值得）。
4. **事件帧形（实测发现的协议面修正）**：裸 `SessionEvent` 不是 JSON-RPC 帧（无 method/id，`classify` 判 `Unclassifiable`），client 端永远收不到。**事件现以 `session/event` notification 包装传输**（params = 信封），对分类器无特例。这条已写进 client.rs 与 server.rs 两端。

**订阅语义按协议文档实现**：`session/new`/`session/load` 成功即隐式订阅本连接（§4），server 从回复的 `session.id` 叶子提取会话——第一版把整个 Session 对象当 SessionId 反序列化，静默失败，靠「hub 直订 vs socket 断点」的二分测试定位。

**store 补了 `bump_generation`**：M0 的 `SessionPatch` 有意不含 generation（runtime 私有），但 store 也没有任何落库路径——manager 组装 runtime 前调用它，行与事件信封才一致（不变量 1 的「generation 落库」）。加性 trait 方法，无迁移。

**其他落点**：配置分层（per-key origin、坏 key 逐条忽略、Runtime 层恒存）；prompt 管线四 section + `{{var}}` 未知占位保留可见 + safety_gate 不可覆盖（override 被拒并 warning，测试断言不泄漏）；`prompt/render` 带 per-section 来源；history 重建（reasoning 按能力表 echo/drop、工具配对用 `provider_call_id`、legacy 行合成 `call-synth-N`）；audit 一次列全；`Disposers::run_reverse` 逆序且 panic 容错。

### （此前无条目）

### 2026-09-28
- 初稿。模型 = codex app-server（传输与协议纪律）+ atomcode Live Hub/租约/代际（会话生命周期纪律）的合成。
- ADR-0009 落地：daemon.md 新增 §3.1 profile 化装配（local/headless/acp-stdio/acp-standalone 四捆绑）+ 启动 fail-loud 审计（dsh `auditStartupEntries` 语义）；runtime 卸载/关闭明确 disposer 逆序规则；注册句柄模式（Handle{dispose, replace}）用于运行中换 provider/MCP 连接。
