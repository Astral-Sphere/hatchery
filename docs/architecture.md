# 总体架构

> 状态：设计定稿（2026-09-28），未开始实现。关键决策的依据见 [decisions/](decisions/README.md)。

## 1. 目标与非目标

### 目标

- 一个 **Rust 编写的 AI Agent Harness**，同一核心引擎驱动多种前端：
  - CLI（ratatui TUI + headless exec 模式）
  - 原生桌面端（gtk4-rs + libadwaita，gettext i18n，RTL 交给 GTK/Pango）
  - ACP agent server（被 Zed 等宿主驱动）
- **Chat / Code 两种会话模式**：同一 runtime，按模式装配不同的工具集、审批策略与 prompt 变体。
- **reasoning_content 一等公民**：流式展示、历史回放逐字节精确（KV-cache 友好）、effort 为用户可配置项。
- **历史可编辑**：编辑即分叉（append-only + 分支指针），允许物理删除分支；代码可回滚（影子 Git 检查点）。
- **提示词透明**：用户可查看、覆盖、导出当前生效的完整 system prompt。
- **ACP 完整适配**：包括 atomcode 未做的两个方向——宿主侧文件读写（`fs/read_text_file`、`fs/write_text_file`）与宿主侧终端（`terminal/*`），通过 capability seam 把执行后端委派给 ACP 客户端。
- ACP client：以 ACP 协议编排外部 harness 作为 subagent。

### 非目标（v1）

- Web UI（协议已为其预留：任何进程都能做 JSON-RPC 客户端，但不实现）。
- 沙箱执行（landlock/bwrap/seatbelt）：留接口，M5+ 实现。
- 非 OpenAI 兼容的原生 Anthropic/Gemini wire 格式：LLM 层构建在 `openai-interface` 之上，只覆盖 Chat Completions 与 Responses 两种 wire。
- 多用户 / 远程多租户 daemon。

## 2. 进程模型

```
┌────────────┐   ┌────────────┐   ┌────────────┐   ┌──────────────────┐
│ CLI (TUI)  │   │ GTK 桌面端 │   │ headless   │   │ ACP 宿主 (Zed…)  │
└─────┬──────┘   └─────┬──────┘   └─────┬──────┘   └───────┬──────────┘
      │ JSON-RPC (IPC) │ JSON-RPC (IPC) │ JSON-RPC(stdio)  │ stdio (ACP)
      └────────────────┴───────┬────────┴──────────────────┘
                        ┌──────▼────────────────────────────┐
                        │        hatchery daemon            │
                        │  ┌─────────────────────────────┐  │
                        │  │ Session Manager (租约/代际) │  │
                        │  │  ┌─────────┐  ┌─────────┐   │  │
                        │  │  │ Runtime │  │ Runtime │   │  │   ACP server 是 daemon 内
                        │  │  │(会话 A) │  │(会话 B) │   │  │   的一个前端适配层；ACP
                        │  │  └─────────┘  └─────────┘   │  │   client (subagent) 由
                        │  └─────────────────────────────┘  │   runtime 作为工具驱动
                        │  ┌─────────────────────────────┐  │
                        │  │ Store: writer actor(turso)  │  │
                        │  └─────────────────────────────┘  │
                        └───────────────────────────────────┘
```

- **单 daemon 单写者**：每个用户一个 daemon 实例（attach-or-spawn），全系统只有 daemon 一个进程写数据库，规避跨进程写竞争（ADR-0002；引擎选型见 ADR-0010）。
- **前端是瘦客户端**：CLI TUI、GTK、headless exec 都通过 `hatchery-protocol` 定义的 JSON-RPC 协议与 daemon 通信，不内嵌 agent runtime（ADR-0001）。会话生命周期独立于任何前端：关掉终端，任务继续跑。
- **CLI 的启动路径**：`hatchery` 命令先探测 daemon 的本地套接字（unix = UDS，Windows = 命名管道，ADR-0013）；daemon 不在则 spawn 一个（daemonize），再 attach。也支持 `--embedded` 在同进程内起 daemon（测试与单机简化场景）。
- **ACP server** 是 daemon 内的一个适配层：每个 ACP 连接映射到一个 runtime 会话；stdio 传输。`hatchery acp` 子命令可独立以 stdio 方式起一个单连接进程（宿主直接 spawn 的场景）。

## 3. Crate 分层

依赖方向严格单向，下层不知道上层存在：

```
L0  hatchery-protocol      共享词汇表 + wire 契约：id 新类型、Content / ToolOutput /
                           ApprovalRequest / Usage、Session / Item、JSON-RPC 方法与事件、代际号
L1  hatchery-kernel        中立 agent 循环：Turn 状态机、LlmProvider / ToolHost / HistorySource /
                           EventSink trait、StreamEvent（复用 protocol 的词汇表类型）
L2  hatchery-llm           openai-interface 之上的 provider adapter（实现 kernel 的 trait）
    hatchery-store         SessionStore trait + turso 实现（分支模型、writer actor）
    hatchery-capabilities  capability seam：Tool / ToolCtx 与 FsBackend / TerminalBackend /
                           ApprovalGate、影子 Git 检查点、工具注册表框架（实现 kernel 的 ToolHost）
L3  hatchery-tools         内置工具（read/write/edit/glob/grep/shell/web_fetch/MCP client）
    hatchery-acp           ACP server + client（实现 capabilities 的 trait，绑定宿主后端）
L4  hatchery-daemon        runtime 宿主：会话管理、监听本地套接字/stdio、事件扇出（live hub）
                           配置加载与 prompt 装配也在这里（前端一律经协议访问，无第二个消费者）
D   hatchery-cli           ratatui TUI + headless exec
    hatchery-gui           gtk4-rs + libadwaita 桌面端

dev hatchery-testkit       测试基建：fake 后端 / ScriptedProvider / TestDaemon / fixture 加载，
                           仅作 dev-dependency，不发布（见 design/testing.md §2）
    hatchery-tests         跨 crate 的 e2e 场景与核心不变量套件（design/testing.md §4/§5），
                           不发布、无产品代码（M1 Phase 5 落地）
    xtask                  开发者任务：layering 契约检查、coverage、i18n 提取、fixture 录制
```

**13 个 workspace member = 上面 12 个 crate（10 个产品 crate + dev-only 的 testkit 与 hatchery-tests）+ `xtask`**。`cargo xtask layering`（以及同名集成测试）把这张图当契约检查，normal/build/dev **三种依赖边都查**：成员清单必须与 `xtask/src/layering.rs` 的 `LAYERS` 表一致；所有指向产品 crate 的边必须严格向下，dev 边也不例外——cargo 允许 dev 依赖向上甚至成环，方向规则是契约唯一的防线；产品 crate 不得以 normal/build 依赖 dev crate（`[dev-dependencies]` 是 testkit 的合法用法，而 build 脚本在用户机器上跑，等同交付测试代码）；构建图（normal + build）不得有环——dev 边不进环检测，因为 kernel dev→testkit、testkit normal→kernel 是合法模式，dev 边不在 cargo 实际编译的构建图里。改这张图就要同时改那张表。

分层纪律（借鉴 atomcode 的 L0–D 分层与 codex 的「TUI 也是协议客户端」）：

- **protocol 是共享词汇表，位于最底层**：id 新类型、`Content`/`ToolOutput`/`ApprovalRequest`/`Usage` 这些既要进 wire、又被 kernel 与 capabilities 使用的值类型只定义一次，住在这里。kernel 依赖 protocol；反过来不行。**修正记录**：M0a 把 protocol 与 kernel 并列在 L0，但 M0b 落地时发现 protocol 的数据模型必须引用 kernel 声明的 `ToolOutput`/`ApprovalRequest`，而同层横向依赖被 layering 契约禁止——于是把 protocol 沉淀为唯一的最底层，其余各层顺次 +1（见 worklog/architecture.md 2026-09-28 变更日志）。
- **kernel 零业务语义**：不知道 Chat/Code 模式、不知道工作区、不知道存储格式。它只驱动「LLM 请求 → 流事件 → 工具调用 → 结果回填」循环，通过 trait 与外界交互。
- **kernel 只见窄接口 `ToolHost`**（defs 快照 / 是否需要审批 / 调用）：`Tool`、`ToolCtx` 与 `FsBackend`/`TerminalBackend`/`ApprovalGate` 都住在 L2 的 capabilities。否则 kernel 的 `ToolCtx` 要引用 capabilities 的 trait，kernel 就反过来依赖上层成环（M0a 修正，见 worklog/architecture.md）。审批的往返由 kernel 发起（`ApprovalNeeded` 事件 + `ApprovalDecision` 命令），daemon 收到事件后调用注入的 `ApprovalGate` 应答——kernel 因此不需要认识任何审批后端（M0b 定，见 design/kernel.md §3/§5）。
- **capabilities 定义接缝，tools/acp 提供实现**：工具永远通过 `FsBackend`/`TerminalBackend`/`ApprovalGate` trait 操作外界，绝不直接 touch 文件系统或进程。这是 ACP 完整适配（ADR-0004）与将来远程执行后端（docker/ssh）的前提。
- **daemon 是唯一的 runtime 所有者**：前端不重建任何生命周期状态，只投影事件流（view projection）。
- **Disposer 纪律**（ADR-0009）：一切有副作用的注册（runtime 装配、hub 订阅、adapter 注册、检查点句柄）必须返回 disposer（Drop guard 或显式 dispose），teardown 严格逆序；有测试锁定。
- **反预拆分刹车**（ADR-0009）：trait 在第二个实现出现前不做 provider 层抽象，crate 在第二个消费者出现前不拆。
- **Fail-loud 装配**（ADR-0009）：daemon 按 profile 装配组件，启动期审计——必需组件缺失即拒绝服务并输出错误清单；「运行时静默 PENDING」是反模式。
- 新增跨 crate 依赖必须在 ADR 或 worklog 里说明理由；禁止环。

## 4. 数据流

### 4.1 一次对话轮（turn）

```
前端 ──session/prompt──▶ daemon
  daemon 校验租约/代际 ──▶ kernel.submit(TurnInput)
  kernel: 组装上下文（prompt sections + active 分支历史重建）
        ──▶ LlmProvider.chat_stream(&ChatOptions, &[Message])   // 借用：长会话不必每轮重拷一份上下文
        ◀── StreamEvent::{TextDelta, ReasoningDelta, ToolCall, Done…}
  每个事件:
    ├─▶ store writer actor（item 边界落库；delta 内存聚合）
    └─▶ live hub 扇出给所有 attach 的前端（含迟加入者的 replay window）
  ToolCall ─▶ 工具注册表 ─▶ ApprovalGate（本地弹窗 / ACP request_permission）
                        ─▶ FsBackend / TerminalBackend（本地 / ACP 宿主委派）
                        ─▶ 写类工具执行前：影子 Git checkpoint
  turn 结束 ──turn/finished──▶ 前端；TurnCompletion 落库
```

### 4.2 历史编辑（编辑即分叉，ADR-0003）

```
前端 ──session/edit_item{item_id, new_content}──▶ daemon
  daemon: 以 item 的 parent 为基点插入新分支首 item（append-only）
        ├─ 更新 session.active_branch_head 指向新分支
        ├─ 若该 turn 之后有代码写入且用户选择回滚 → 影子 Git restore
        └─ 旧分支保留，可 session/branch/switch 切回；可 session/branch/delete 级联删除
```

## 5. 核心不变量（所有实现必须遵守）

每条不变量都有专属测试锁定，映射表见 [design/testing.md §5](design/testing.md)。

1. **单一 runtime 所有者**：一个会话在任意时刻至多绑定一个 runtime 实例；`SessionLease`（advisory 文件锁）+ 单调代际号（generation）保证旧实例的迟到事件不会污染新实例（借鉴 atomcode `RuntimeGeneration`）。
2. **模型可见 = 已记录**（借鉴 deepseek-harness "model-visible means logged"）：发给 LLM 的上下文必须能从数据库 active 分支完整重建；不允许存在只活在内存里的历史。**边界（2026-10-07，D15）**：本条管辖**分支历史**；请求最前面那条 system prompt 是可复现的派生态（嵌入模板 + 装配时冻结的运行时事实），不是 item，由 `prompt/render` 钉住——它对活着的 runtime 返回装配时冻结的那一份。
3. **Items append-only**：已提交的 item 永不原地修改；编辑产生新分支，删除是显式的级联操作（ADR-0003）。
4. **工具只经接缝**：工具实现不得直接调用 `std::fs`/`std::process`，必须经 `FsBackend`/`TerminalBackend`（ADR-0004）。强制机制是 clippy 的 `disallowed_methods`/`disallowed_types` + 根目录 `clippy.toml` 的禁令表，**`tokio::fs` 的孪生项一并禁掉**（2026-10-07 补：只禁 `std` 等于留了一条绕过路径，而 tools crate 本来就依赖 tokio）。生效方式不是 crate 属性 `#![deny(...)]`——实测（2026-10-01）crate 属性压不过 Cargo 的 lint 表，真正的机制是 `hatchery-tools` 自带一份完整的本地 `[lints]` 表（细节见 design/testing.md §5）。
5. **安全门不可覆盖**：危险路径保护、审批硬门不可被项目级配置 / AGENTS.md / prompt 覆盖（借鉴 atomcode 的 PRECEDENCE 节 + 测试保证）。
6. **影子 Git 不碰用户仓库**：检查点仓库使用独立 `--git-dir`，绝不操作用户的 HEAD/index/refs（ADR-0006）。

## 6. 术语表

| 术语 | 含义 |
|---|---|
| **Session** | 一次会话。含元数据（模式、工作区、模型配置）与 item 树。类型名统一为 `Session`（方法 `session/*`、表 `sessions`）；早期文档里的 "Thread" 已废弃，避免一物两名。 |
| **Turn** | 一次「用户输入 → agent 完成响应」的完整轮次，可含多次 LLM 请求与工具调用。 |
| **Item** | 历史的最小单元：message / tool_call / tool_result / reasoning 块 / checkpoint 记录，带 `parent_item_id` 构成树。 |
| **Branch** | item 树上从某节点分叉出的一条链；`active_branch_head` 决定当前生效历史。 |
| **Runtime** | 驱动一个会话的 kernel Agent 实例及其绑定的能力后端。 |
| **Capability seam** | kernel/tools 与外界（文件系统、终端、审批）之间的 trait 边界。 |
| **Live hub** | daemon 内的事件扇出层：多前端订阅同一会话的事件流，支持迟加入 replay。 |
| **Generation** | 会话 runtime 的单调代际号，用于丢弃旧实例的迟到事件。 |
| **Shadow Git** | 每工作区一个的独立 git 仓库（`--git-dir` 指向应用数据目录，`--work-tree` 指向用户工作区），用于代码检查点与回滚。 |

## 7. 目录与路径约定（XDG）

```
~/.config/hatchery/config.toml        # 用户配置（见 design/platform.md）
~/.config/hatchery/prompts/           # 用户 prompt 覆盖
~/.local/share/hatchery/hatchery.db   # turso 主数据库（ADR-0010）
~/.local/share/hatchery/checkpoints/<workspace-hash>/  # 影子 Git 仓库
~/.local/state/hatchery/daemon/       # 套接字 locator、daemon 锁与 daemon.json（ADR-0013）
<project>/.hatchery/config.toml       # 项目级配置
<project>/AGENTS.md                   # 项目级 prompt 注入
```
