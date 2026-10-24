# ADR-0013: Windows 的本地套接字 = 命名管道（interprocess），unix 保持 tokio UDS

状态：accepted（2026-10-09）

Supersedes: 无。**补充 ADR-0001** 的传输层——它写的是「JSON-RPC 2.0 over UDS/stdio」，而 UDS 只在 unix 存在；Windows 那一半当时没有定义，也没有任何代码在 windows-gnu 上编译过它。

## 背景

三平台门禁是 2026-09-28 的项目决定，Windows 走 MSYS2 UCRT64 + `x86_64-pc-windows-gnu`（不是 MSVC，理由见 ADR-0010/0012）。

UDS 传输在 `db7c2ed`（2026-10-01，M1 Phase 1–4）落地。M0 的 windows job 绿过，那时 `crates/hatchery-protocol/src/client.rs` 与 `crates/hatchery-daemon/src/server.rs` 还不存在——**这条平台分歧从未被跑到**，直到 2026-10-09 的 PR：windows job 在 `hatchery-protocol` 编译期失败 14 条，第一条是

```text
error[E0432]: unresolved import `tokio::net::UnixStream`
note: found an item that was configured out
  ::: tokio-1.53.1\src\macros\cfg.rs:377:23
377 |             #[cfg(all(unix, feature = "net"))]
```

机制：`x86_64-pc-windows-gnu` 不是 `cfg(unix)`。UCRT64 里 `gcc` 产的是**原生 Windows 二进制**，不是 msys 子系统里的 POSIX 程序，所以既拿不到 tokio 的 `net::unix`，也拿不到 msys2 runtime 的 AF_UNIX 垫片。后面 10 条 `the size for values of type str cannot be known` 是同一处的连锁（`BufReader<缺失类型>` 推不出 `AsyncBufRead`，`next_line()` 的返回类型塌成 `Option<str>`），不是独立故障。

本机就是 windows-gnu（rustc 1.98.1 + ucrt64 gcc），**下面所有结论都是这台机器上实测的**，不是照文档写的。

## 决策

1. **传输收敛到一个模块**：新增 `crates/hatchery-protocol/src/transport.rs`，对外只有 `connect` / `bind` / `Listener::accept` / `discard` 与两个 boxed 半端 `ReadHalf`、`WriteHalf`。除这个模块外，全仓库不再出现任何平台套接字类型（`tokio::net::unix::*`、`UnixListener`、管道类型都不再被手写 import）。
2. **unix 侧继续用 tokio 的 UDS**，一行行为不改——那是 M1 以来 linux/macos 两个 job 天天在跑的代码；**Windows 侧用 `interprocess` 的 local socket**（在 Windows 上就是命名管道，`pipe_mode::Bytes`，无消息边界）。依赖各挂在自己那侧：`[target.'cfg(unix)'.dependencies] tokio = ["net"]`、`[target.'cfg(windows)'.dependencies] interprocess`，于是 Linux/macOS 的构建图里 interprocess 一个字节都不出现，Windows 的构建图里没有 tokio `net`。
3. **端点是 locator，不是文件**：`DaemonInfo.uds_path` 改名 `endpoint`（`daemon.json` 的字段名随之变），值仍是状态目录里那个 `hatchery.sock` 路径字符串。unix 拿它当 socket 文件；Windows 把它 percent-escape 成一个扁平管道名 `\\.\pipe\hatchery-<escaped>`（管道名不允许含反斜杠）。映射是确定的，daemon 与 client 各算一次得到同一个名字，`--state-dir` 覆盖因此天然分得开。
4. **权限对等**：创建管道时带 DACL `D:P(A;;GA;;;OW)`（只有 owner 能连），作为 unix `0700` 的对等物。`discover::restrict_to_owner` 在 Windows 上仍是空操作——那里没有文件可 chmod，`server::serve_local` 的注释写明这条腿在谁身上。
5. `serve_uds` → `serve_local`（它不再只服务 UDS）；`TestDaemon` 的就绪判定从「轮询 socket 文件出现」改成「轮询一次真实 connect 成功」——Windows 没有文件可轮询，而「答不答应连接」本来就是更强的问题。

## 实测（`x86_64-pc-windows-gnu`，2026-10-09）

| 项 | 结果 | 影响 |
|---|---|---|
| connect 到无人监听的名字 | `NotFound`，**56 µs 返回，不挂起** | 就绪轮询与 attach-or-spawn 的「连不上就 spawn」成立 |
| 只丢客户端的**写**半 | 服务端**读不到 EOF** | 见下条与代价节：router 任务必须能自己退出 |
| 两个半端都丢 | 服务端立刻 EOF | `dropping_the_last_client_ends_the_connection` 靠 `Inner::drop` 取消 router 才成立 |
| `D:P(A;;GA;;;ME)` | **连自己的 owner 都拒**（os error 5） | Windows 不在管道 DACL 里解析 `ME` |
| `D:P(A;;GA;;;OW)` | 通 | 用它，owner-only |
| 同名二次 bind | `PermissionDenied`（不是 `AddrInUse`） | `entry.rs` 的 bind 失败测试在 Windows 改成「先占住端点」，unix 仍用「目录占位」 |
| 转义后 522 字符的名字 | 系统答 `NotFound`(3)，长得像「没人监听」 | 转义后 >255 在本地先拒并报 `InvalidInput`，否则 CLI 会误判成没 daemon 而去 spawn 第二个 |
| `Command::new("x.cmd")` 带多余参数 | 能起，stderr 透传，退出码传出 | CLI 测试的 `#!/bin/sh` 替身在 Windows 换成 `.cmd`，断言的文字一条不改 |
| `symlink_dir` / `symlink_file` | **os error 1314**（这台机器既非管理员也没开 developer mode） | 符号链接类测试在造不出 fixture 的机器上明说并跳过，不再 `expect` |
| `kill` 二进制 | 存在的是 msys 的 kill，对原生 pid 无意义；`taskkill` 在，但没有 SIGTERM 语义 | `daemon stop` 的 Windows 实现另立待办，本条测试腿只在 unix 跑 |

## 理由

1. **语义对等优先**。命名管道是 Windows 上唯一与 UDS 同构的原语：按名字连接的本地流、不占端口、可按用户设 DACL。design/daemon.md §2 写的用户级隔离（「UDS 权限已挡，token 是纵深防御」）在管道上有直接对应物；换成正则没有。
2. **否掉 tokio 自带的命名管道**：`tokio::net::windows_named_pipes` 在 `cfg(tokio_unstable)` 后面，要它就得给 CI 加 `RUSTFLAGS`。`.cargo/config.toml` 里 2026-10-01 那条纪律禁的是「带 flag 的产物被缓存复用」，`--cfg tokio_unstable` 不属于那类风险，但它把**产品代码**绑到 tokio 的不稳定面上：补丁版就可能变形，而且每个开发者本地都得配同一条 flag 才编得过——为省一个依赖把这两件事请回来不值。
3. **否掉 `uds_windows`**：它给的是裸 `SOCKET` 句柄，而 tokio 在 Windows 没有 `UnixStream` 可以包（正是本 ADR 的起因），要自己接 IOCP。
4. interprocess 是纯 Rust（`windows-sys` / `libc`，已在锁文件里），MSRV 1.75 < 本仓库 1.90，不需要 C 工具链（与 ADR-0010 选 turso、ADR-0011 选 fluent 同一条理由链），许可 `0BSD OR Apache-2.0`，与 GPL-3.0-only 工程兼容。它只在 `transport.rs` 一处被引用。

## 代价

- 平台分歧没有消失，只是被**收进一个模块**，并且两条分歧各有测试钉住、在模块文档里写明：**EOF 要两个半端都丢**（unix 丢一个就行）、**bind 冲突的判定不同**（unix 先 unlink 再绑，Windows 拒绝同名）。前者逼出了 `Inner` 上的 `CancellationToken`——client 的 router 停在安静的读上时，Windows 永远不会知道客户端已经没了。
- 管道 ACL 只证明了「owner 连得上、别人被拒」里能被单机测的那一半；**同机另一个用户确实连不上**没有第二个账号可测，未实测。
- `daemon stop`（SIGTERM）与 D1 承诺的 `DETACHED_PROCESS` 在 Windows 上仍未实现——`discover.rs`/`attach.rs` 的注释写着「setsid on unix, DETACHED_PROCESS on Windows」，代码里只有 unix 那半。这是本次改动**发现**的文档-代码漂移，不在本次修，记为 worklog/daemon.md 待办。
- Windows 上状态目录过深会直接被拒（`InvalidInput`，见实测表）；unix 的 `sun_path` 上限（Linux 108 / macOS 104）本来就有同类约束。

## 替代方案（已否）

- **TCP 环回 `127.0.0.1:0`**：零新依赖、三平台一套代码，但把「同机其他用户连不上」这一层整个去掉（Windows 上 `restrict_to_owner` 本来就是空操作，届时只剩 boot token——而 `DaemonCore::dispatch` 今天并不要求先握手），端点还得从路径变成 host:port 并处理端口抢占。安全模型不是传输层该顺手改的东西。
- **`--cfg tokio_unstable` + tokio 自带管道**：见理由 2。
- **`uds_windows`**：见理由 3。
- **Windows 暂不支持 daemon，只让它编得过**：门禁会绿，但 2026-09-28 的三平台决定不是传输层该推翻的东西；而且真跑起来才知道上面那一堆分歧是什么，不如现在测。

## 后果

- design/daemon.md §1、§2，design/protocol.md §1，architecture.md 的分层图与 L4：「UDS」改为「本地套接字（unix UDS / Windows 命名管道）」；`daemon.json` 的字段清单改为 `endpoint`。
- ADR-0001 的「over UDS/stdio」由本 ADR 补上 Windows 那一半（0001 状态不变）。
- `hatchery-daemon` 与 `hatchery-tests` 的 Cargo.toml 不再需要 tokio `net`——「谁都能手写一个平台套接字」这件事从此由编译期挡住。
- 测试可移植性：5 处 `std::os::unix::fs::symlink` 改成两侧都对+权限缺失时明说跳过（`local_fs.rs` 原有的 Windows 臂本来就是会 panic 的假臂）；CLI 测试的 `/bin/true`、`#!/bin/sh`、`/bin/sleep` 换成平台替身；`daemon stop` 那条腿 cfg 到 unix。
- worklog/daemon.md 记实现、两条语义分歧与上面那条文档-代码漂移。
