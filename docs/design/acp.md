# 设计：ACP（hatchery-acp）

> 状态：设计稿。依据 ADR-0004。v1 同时实现 **server**（被 Zed 等宿主驱动）与 **client**（编排外部 harness 作 subagent）。协议库：官方 Rust crate `agent-client-protocol`（atomcode 用 `=2.0.0`，hatchery 跟随最新稳定版）。规范：<https://agentclientprotocol.com/>。

## 1. Server（agent 侧）

### 1.1 入口形态

- `hatchery acp`：stdio 单连接进程，宿主（Zed）直接 spawn。内部仍走 daemon 架构：默认 attach 常驻 daemon（会话持久、可被其他前端同时观察）；`--standalone` 时进程内嵌 DaemonCore（宿主期望 agent 进程自包含的场景）。
- 能力声明（`initialize` 响应）：

```rust
AgentCapabilities {
    load_session: true,             // 支持载入既有会话（映射 session/load + replay）
    prompt_capabilities: PromptCapabilities { image: true, audio: false, embedded_context: true },
    mcp_capabilities: Some(McpCapabilities { http: true, sse: true }),  // 会话级 MCP 配置透传
}
```

### 1.2 方法映射

| ACP 方法 | hatchery 内部 |
|---|---|
| `initialize` | 握手 + 协商协议版本 + 交换能力；记录 **client 的 fs/terminal 能力**（关键！见 §1.3） |
| `authenticate` | v1 返回 method-not-found（密钥走环境变量，不做 OAuth agent 侧） |
| `session/new` | 创建 Thread（mode=code，workspace=cwd）+ runtime；返回 sessionId |
| `session/load` | 按 active 分支重放 items → `session/update` 序列（replay.rs 语义，借鉴 atomcode） |
| `session/prompt` | content blocks → `Content`（text/image/resource_link/embedded resource）→ `session/prompt` 协议方法 → turn |
| `session/cancel` | `session/cancel`（→ kernel Interrupt） |
| `session/set_mode` | Chat/Code 切换（ADR-0005）；modes 在 `session/new` 响应中声明 |
| `session/request_permission`（我方发起） | 见 §1.4 |
| `fs/read_text_file`、`fs/write_text_file`（我方发起） | 见 §1.3 |
| `terminal/*`（我方发起） | 见 §1.3 |
| MCP servers 配置（`session/new` 参数） | 注入该会话的 MCP client 列表（M5 前接受但忽略，并 warning） |

turn 期间的所有进展经 `session/update` 通知流投影：`agent_message_chunk`、`agent_thought_chunk`（**reasoning_content 映射到这里**，展示与否尊重宿主）、`tool_call` / `tool_call_update`（状态 pending→in_progress→completed/failed，含 diff/content 附件）、`plan`（M5）。

### 1.3 fs/terminal 委派（atomcode 缺的两块，hatchery 的核心差异点）

`initialize` 拿到 `ClientCapabilities`：

```
client.fs.read_text_file / write_text_file == true
  → 该会话绑定 AcpClientFs（ LocalFs 不再用于宿主可见路径 ）
client.terminal == true
  → 绑定 AcpClientTerminal
任一为 false → 回退 Local 后端，审批仍走 AcpPermission
```

- `AcpClientFs`：`read_text_file` → ACP `fs/read_text_file`（带 line range 透传）；`write_text_file` → ACP `fs/write_text_file`。**写入不进影子 Git**（宿主自己管 buffer/undo），checkpoint 工具在该会话降级为 no-op 并在工具描述中移除。
- `AcpClientTerminal`：`TerminalHandle` 各操作直译 ACP `terminal/create`、`terminal/output`（宿主推送 output chunk → handle 的 output stream）、`terminal/wait_for_exit`、`terminal/release`、`terminal/kill`。create 参数映射：cwd、env、command 数组。
- 路径语义：ACP 用 URL（`file://`）；边界处统一转换并校验 scheme。
- 宿主能力不齐时的降级矩阵要在 `session/new` 时确定并固定整个会话（避免 turn 中途换后端）。

### 1.4 审批映射

`ApprovalGate` 的 ACP 实现 `AcpPermission`：

```rust
ApprovalRequest { tool, args_digest, risk, options }
  → session/request_permission {
      session_id,
      tool_call: ToolCallContent（展示用：kind=edit|execute|fetch…, title=args_digest, content=diff/终端命令）,
      options: [
        { option_id: "allow_once",   kind: allow_once },
        { option_id: "allow_always", kind: always_allow },   // → 持久化 approval_rules
        { option_id: "deny_once",    kind: reject_once },
        { option_id: "deny_always",  kind: always_reject },
      ],
    }
```

- risk 级别映射到 ACP option kind 的建议排序；宿主取消（cancel）→ deny（fail-closed）。
- 硬门（capabilities.md §5）在委派场景同样生效：先过硬门再问宿主。

### 1.5 会话配置项

`session/new` 响应可带 config options（借鉴 atomcode 把 model/effort 做成 `SessionConfigOption`）：模型选择、reasoning effort、模式。宿主 UI 直接渲染成设置面板。

## 2. Client（subagent 编排）

参照 dsh `subagent-acp`：把任意外部 ACP agent（包括另一个 hatchery、codex 适配器、qwen-code 等）当作工具驱动。

```toml
# config.toml
[acp_agents.claude-code]
command = ["claude-code-acp"]
[acp_agents.qwen]
command = ["qwen", "--acp"]
tools_hint = ["edit", "shell"]   # 呈现给模型的用途描述素材
```

- `subagent` 工具：参数 `{ agent, task, mode? }`；实现 = spawn 进程 → `initialize`（**我方作为 client 声明 fs/terminal 能力：true**，让子 agent 的执行回到 hatchery 的接缝里，统一审批与检查点）→ `session/new`（workspace 继承或指定 worktree）→ `session/prompt(task)` → 转发子 agent 的 `session/update` 为 ToolCallProgress → 汇总结果为 ToolOutput。
- 子 agent 的 `session/request_permission` → 经父会话 ApprovalGate 上浮（附「来自子代理 X」上下文）；策略可配：上浮 / 自动 allow_once（危险级除外）/ 自动 deny。
- 生命周期：turn 结束即 cancel + release；崩溃的子进程标记工具失败。
- 隔离选项（M3+）：`worktree = true` 时在 git worktree 里跑子 agent，结果以 diff 呈现。

## 3. 测试与验证

- **单元**：能力协商矩阵（fs/terminal 有无 × 降级路径）、方法映射往返（fixture 驱动）。
- **集成**：用官方 crate 写一个 minimal test client（模拟宿主，声明全能力），驱动完整 turn：prompt → 委派写文件 → 委派终端 → 审批 → 完成。
- **实测**（M3 验收）：Zed 真机接入，验证 edit/terminal/permission 全链路与 `agent_thought_chunk` 展示；记录宿主实际行为差异进 worklog/acp.md。
- client 侧：以 `hatchery acp` 自己作为子 agent 跑通自编排（自举测试）。

## 开放问题

1. ACP v2/draft 演进（atomcode 做了 v1+v2 双链）：hatchery v1 只跟稳定版，crate 升级窗口内评估。
2. `session/load` replay 的事件量裁剪（超长会话重放性能）——M3。
3. 子 agent 嵌套深度限制与环检测（hatchery spawn hatchery spawn…）——M3，倾向深度上限 2 + 配置可调。
4. 宿主声明 fs 能力但读写失败率高的情况是否需要「回退本地」的运行时开关——先不做，观察实测。
