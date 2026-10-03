# 设计：Runtime Daemon（hatchery-daemon）

> 状态：设计稿。依据 ADR-0001（全协议化）、ADR-0002（单写者）。

## 1. 职责

daemon 是系统唯一的 runtime 所有者与数据写者：

- 监听 UDS + stdio，说 `hatchery-protocol`。
- 会话管理：创建/加载 runtime、租约、代际号。
- Live hub：事件扇出到多前端，迟加入 replay。
- 装配：按模式组装工具注册表、按会话来源绑定能力后端（ADR-0004 的表）。
- 持有 store writer actor 与 CheckpointStore。

## 2. 单实例与发现

```
~/.local/state/hatchery/daemon/
├── daemon.lock        # advisory 文件锁（fs2），持锁者即活跃 daemon
├── daemon.json        # { pid, uds_path, protocol_version, started_at }
└── hatchery.sock      # UDS
```

- **attach-or-spawn**：客户端先读 daemon.json + 尝试连接；失败则抢锁 spawn（fork/daemonize 或 systemd socket activation，二选一 M1 定）再 attach。竞态由锁保证。
- UDS 权限 0700（用户级隔离）；`daemon.json` 里带随机 boot token，客户端连接时出示（防同机其他用户进程伪装前端——UDS 权限已挡，token 是纵深防御）。
- CLI `--embedded`：同进程内起 DaemonCore，走内存 transport（协议不变）。

## 3. 内部结构

```rust
pub struct DaemonCore {
    sessions: SessionManager,
    hub: LiveHub,
    store: StoreHandle,          // writer actor 的发送端
    checkpoints: CheckpointRegistry, // per-workspace CheckpointStore
    config: Arc<LayeredConfig>,
}

struct SessionSlot {
    runtime: Arc<SessionRuntime>, // kernel Agent 句柄 + 任务 + 在途标记
    generation: u64,              // 装配即 +1 并落库（不变量 1）
    last_activity: Instant,       // 空闲清扫的比较基准
}

// 订阅计数在 slots 之外单独记账：订阅先于首个 prompt 到达（先 session/new 后 prompt），
// 计数必须活过它挂靠的那个 runtime（否则空闲清扫会掐掉正被观看的会话）。
// 规划中的 per-session 文件锁（SessionLease）在 M1 不存在：单 daemon 内由
// turn 闸门 + 在途标记 + TurnInProgress 拒绝承担，跨 daemon 由单实例锁承担 —— 文件锁随
// 多 daemon 形态（M2+）再引入。
```

- **runtime 生命周期**：runtime 在**首个 prompt 时惰性装配**（M1 实现与规划不同点：`session/new|load` 只建会话行），builder 注入 provider/tools/history/sink；turn 之间 runtime 常驻内存；空闲超时（默认 30min 无订阅者且无进行中 turn）落盘卸载；清扫在卸载前于锁内复核 busy/watched，清扫对象也包括 agent 已死的 slot。
- **代际号**：runtime 每次（重）建 generation+1 并落库；所有事件带 generation，hub 与前端各自丢弃旧代事件；装配以 `GenerationBumped` 开场通知前端重置视图。
- **并发接受**：同一会话的 prompt 路径（装配→busy 检查→提交）走 per-session turn 闸门串行化，接受即在 runtime 上打在途标记（kernel TurnEnded 投影时清除）——第二条 prompt 无论落在装配期还是提交与开闸之间的窗口，都得到 `TurnInProgress` 拒绝，绝不静默丢弃或排队。
- **崩溃恢复**：daemon 重启后先以 `open_turns` 找出所有未结 turn 行、以失败形态关闭（`ended_at` 置位、`stop_reason` 保持 NULL —— "仍在跑"与"失败"可区分，不造假原因），再把 status=running/waiting_approval 的会话标记回 idle。不自动续跑（v1）。

### 3.1 Profile 化装配与启动审计（ADR-0009）

daemon 启动按命名 **profile** 装配组件捆绑，装配表是显式数据（不是散落的 if/else）：

| profile | 监听器 | 能力后端 | 工具集 | 场景 |
|---|---|---|---|---|
| `local` | UDS | Local 三件套 | 按模式 | 常驻 daemon（CLI/GTK attach） |
| `headless` | stdio | Local 三件套 | 按模式 | `hatchery exec` embedded |
| `acp-stdio` | stdio(ACP) | 按宿主能力协商（ADR-0004 矩阵） | Code 全量 | `hatchery acp` attach 常驻 daemon |
| `acp-standalone` | stdio(ACP) | 同上 | 同上 | 宿主 spawn 的自包含进程 |

- **启动审计（fail-loud）**：装配完成后核对 profile 声明的必需组件（provider 可用、密钥环境变量存在、监听器绑定成功、store 迁移完成）；任一未 resolve → 拒绝服务，输出缺失清单后退出（非零码）。dsh `auditStartupEntries` 语义，杜绝「运行时永久 PENDING」。
- 注册句柄：profile 内的组件注册（provider adapter、MCP 连接等）返回 `Handle`（disposer + 原子 `replace()`），运行中换 provider 走整表原子替换（见 capabilities.md §1）。

## 4. Live Hub（事件扇出）

- per-session broadcast channel（tokio::sync::broadcast，容量如 4096）；订阅者 = 协议连接。
- **coalescing**（规划，M1 未实现）：TextDelta/ReasoningDelta 在 hub 入口按时间窗（~16ms）合并成批量事件；ItemFinished 等控制事件立即发。M1 有意不做（roadmap），实测数据决定 M2 策略；`ServerEvent::is_coalescable` 已预留分类。
- **replay window**（规划，M1 未实现）：hub 保留最近 N 事件供瞬时断线重连补发；M1 的重连前端走 `session/load` 全量重建（replay_from 补差在 manager 层）。
- 慢消费者：broadcast lag 时断开该订阅者并让其走 load 重建（不阻塞其他前端）。

## 5. 与其他 crate 的接线

| 交互 | 机制 |
|---|---|
| kernel → daemon | `EventSink` 实现：事件 → hub 扇出 + store 落库（item 边界） |
| kernel → 外界 | 装配好的 Backends（本地或 ACP 委派） |
| ACP server | daemon 内的适配层：ACP 连接 ↔ 一个 SessionSlot；`hatchery acp` 子命令 = 单连接精简 daemon（stdio） |
| ACP client | 作为 `subagent` 工具的实现被 runtime 调用，生命周期挂在该 turn 上 |
| CLI/GTK | 纯协议客户端；共用 `hatchery-protocol` 里的 client helper（连接、重连、订阅、generation 过滤） |

## 6. 可观测性

- 结构化日志（tracing），级别/输出可配；默认 `~/.local/state/hatchery/daemon.log` 轮转。
- `hatchery daemon status` 子命令：版本、uptime、会话数、DB 大小、检查点磁盘占用。
- otel 遥测：接口预留（feature gate），v1 默认关。

## 开放问题

1. daemonize 方式：双 fork vs `sd_notify` socket activation vs 前台进程 + CLI spawn 等待握手——M1 spike 后定（影响 systemd 集成体验）。
2. daemon 空闲退出策略与「进行中 turn 但无订阅者」的取舍（跑完 vs 暂停）——M1 定，倾向跑完并保留结果。
3. 多工作区并发会话共享 CheckpointStore 的锁粒度（per-workspace mutex 已定，跨 workspace 的全局磁盘预算核算频率）——M2。
4. boot token 的轮换时机（daemon 重启即换 vs 定期）——M1。
