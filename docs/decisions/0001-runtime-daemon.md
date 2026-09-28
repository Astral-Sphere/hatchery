# ADR-0001: 全协议化 runtime daemon，前端皆瘦客户端

状态：accepted（2026-09-28）

## 背景

hatchery 要同时支持 CLI、GTK 桌面端、headless 脚本与 ACP 宿主四种前端。核心问题：agent runtime 跑在哪个进程、前端如何与之通信。参考项目分两派：

- 协议派：codex（TUI 也是 app-server 的 JSON-RPC 客户端，可 Embedded 或 LocalDaemon）、atomcode（单一 runtime + Live View Hub，多前端复用）。
- 进程内派：qwen-code（CLI 直接消费事件 AsyncGenerator，仅远程前端走 daemon）。

## 决策

daemon 持有全部 agent runtime 与数据；所有前端通过 `hatchery-protocol`（JSON-RPC 2.0 over UDS/stdio）连接 daemon：

- CLI 采用 attach-or-spawn：探测 UDS，无 daemon 则 spawn 后 attach；`--embedded` 可在同进程内起 daemon（协议不变，仅省 IPC）。
- GTK、headless exec、ACP server 均为 daemon 客户端。
- 会话生命周期独立于前端：前端断开，turn 继续执行；重连经 replay window 恢复视图。

## 理由

1. 多前端同时观察/接管同一会话（Live Hub 扇出）只有 runtime 常驻才可能。
2. GTK 若进程内嵌 tokio runtime，UI 线程与 agent 生命周期耦死（崩溃连带、退出确认复杂）。
3. ACP server 天然是长连接多会话，与 daemon 模型同构。
4. qwen-code 的双接线（CLI 进程内 + 远程走 daemon）被证明要维护两套路径。

## 替代方案（已否）

- 仅存储 daemon、runtime 内嵌前端：终端关闭任务即死，多前端无法共享会话。
- 混合接线（qwen-code 式）：两套事件消费路径长期维护成本高。

## 后果

- 需要处理 daemon 生命周期：单实例发现（XDG runtime dir + 锁文件）、崩溃恢复（存储层 WAL + 代际号）、空闲退出策略。
- 协议成为公共 API，需版本化与兼容策略（见 design/protocol.md）。
- IPC 序列化开销：流式 delta 高频，事件需批量/合并策略（coalescing）。
