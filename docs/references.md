# 参考项目分析

对 `references/` 下四款代表性 harness 的深读结论（2026-09-28）。目的：为 hatchery 的设计提供证据与反面教材。所有文件路径相对于 `references/<项目>/`。

## 总览对比

| 维度 | atomcode (Rust) | codex (Rust) | deepseek-harness (TS) | qwen-code (TS) |
|---|---|---|---|---|
| 架构哲学 | 严格分层 L0–D；单一 runtime + Live View Hub | 单核心 + 全协议化（TUI 也是 app-server 客户端） | everything is a plugin（Cordis） | core 引擎 + 事件 AsyncGenerator；daemon 化 + ACP 为进程间协议 |
| 前后端解耦 | 驱动层全部经 `DriverCommand` 进同一 runtime | JSON-RPC（stdio/WS/UDS daemon） | host 进程跑插件树，client 只消费 RPC + 事件流 | CLI 进程内直连；远程前端走 daemon/ACP |
| reasoning | `StreamEvent::Reasoning` + 跨 provider `ReasoningSignature`；effort 5 档 | Responses-only；`ReasoningEffort/Summary` 按模型声明能力 | `reasoning_content` 一等公民；回放逐字节精确保 KV cache | 统一 effort 阶梯映射各 provider wire 字段 |
| 存储 | 每 session 一个 JSONL + fs2 租约 | JSONL rollout + SQLite thread-store（分页/fork/revert） | append-only JSONL（代际迁移链，从不改写） | JSONL 转录 + 写租约 + 分支检查点 |
| 历史编辑 | Rewind（对话/代码/两者，影子 Git） | resume / fork / archive / backtrack | 不可编辑，只能 fork（记录分叉点） | branch points 分叉回溯 |
| 权限 | 路径三动作模型 + 审批中间件 | 审批 4 档 × 沙箱 4 档正交 + execpolicy(Starlark) | 沙箱三模式 + fail-closed 审批瀑布 + preset | 5 档审批；auto 档双阶段 LLM 分类器 |
| ACP | server（v1+v2 双链），但**不支持 fs/terminal 委派** | 无 | server + client（subagent-acp） | server（`--acp`）+ acp-bridge |

## atomcode

Rust workspace，约 14 个 crate，自称 100% AI 生成。

- **分层**：`atomcode-kernel`（L0，中立 agent 循环：`Agent`/`AgentCommand`/`AgentEvent`、`LlmProvider` trait、`StreamEvent`）→ `atomcode-capabilities`（L1：session 持久化、tools、审批、memory、mcp、plugin、compaction）→ `atomcode-coding`（L2：`CodingRuntime` 唯一 runtime 所有者，`parts.rs` 两阶段 prepare→assemble）→ D 层驱动（CLI/daemon/clix/ACP）。旧 `atomcode-core` 已整体退役，`AGENTS.md` 写明退役判定与生命周期不变量。
- **Live View Hub**（`atomcode-daemon/src/live_hub.rs`）：TUI/webui/JetBrains/手机多路复用同一 runtime；`CONTEXT.md` 定义 Runtime Binding、View Projection、Replay Window 等统一语言。
- **reasoning**：`ReasoningSignature` 统一承载 Anthropic signature / OpenAI encrypted_content / Gemini thoughtSignature，回填 `Message.reasoning_blocks` 供下轮 echo（`kernel/src/provider.rs`）；`config.toml` 的 `providers.*.reasoning_effort`；TUI `Ctrl+T` 切换、`/effort` 命令；kernel 负责剥离 provider 的 "(no reasoning detected)" 填充。
- **Rewind**：`session/rewind.rs` 用独立影子 Git 仓库（显式 `--git-dir/--work-tree`，绝不碰用户 HEAD/index），`RewindLedger` 带版本号，`RewindScope::{Conversation, Code, ConversationAndCode}`；2GiB 磁盘熔断、500MB store 预算；`RewindTransactionGuard` RAII 失败补偿。
- **并发**：`SessionLease`（fs2 advisory 文件锁）单 session 单活跃 runtime；`RuntimeGeneration` 防迟到事件。
- **ACP**：`crates/atomcode-cli/src/acp/`（14 个模块），官方 `agent-client-protocol = "=2.0.0"`，v1+v2 双协议路由。**局限（hatchery 要解决的）**：`acp/mod.rs` L104 自述 v1 链无 `auth`/`fs`/`terminal`，L614 测试断言 client-side terminal 不得被声明——因为单一 runtime 自带 fs/shell 工具与审批链，无法把执行委派给宿主。
- **其他**：Turn Tool Snapshot（回合开始冻结工具表）；persona.rs 的 `## PRECEDENCE:` 节（用户规则优先于默认 persona，但安全审批门不可覆盖，有测试）；Goal 模式双 LLM 评审（evaluator + followup classifier）。

## codex (OpenAI Codex CLI)

约 150 crate 的巨型 Rust workspace + TS 启动壳。

- **协议化最彻底**：`protocol` crate 定义 SQ/EQ（提交/事件队列）异步协议（`Op`/`EventMsg`，`codex-rs/protocol/src/protocol.rs`）；`app-server` + `app-server-protocol` 提供面向前端的 JSON-RPC v2（Thread/Turn/Item 为中心，`app-server-protocol/src/protocol/v2/thread.rs`），传输含 stdio/WebSocket/UDS daemon。**TUI 也是 app-server 客户端**（`tui/src/app_server_connection.rs`：`Embedded` 进程内或 `LocalDaemon` UDS）。TS SDK 直接 spawn `codex exec --experimental-json` 解析 JSONL（`sdk/typescript/src/exec.ts`）。
- **LLM**：Chat Completions 已移除，`WireApi` 只剩 Responses（`model-provider-info/src/lib.rs`）；SSE 解析为 `ResponseEvent`（含 `ReasoningSummaryDelta`、`RateLimits`）；模型目录按模型声明 `supported_reasoning_efforts`（`protocol/src/openai_models.rs`）。**base instructions 随远端模型目录下发**（`ModelInfo.model_messages.instructions_template`），本地仅剩 compact/review 等模板；`model_instructions_file` 可覆盖。
- **存储**：JSONL rollout（`~/.codex/sessions/YYYY/MM/DD/`）+ 新增 SQLite thread-store（分页历史、fork、revert、rollout 迁移）；配置 8 级分层并输出 per-key origins（`config/src/loader/README.md`）。
- **权限**：`AskForApproval`（UnlessTrusted/OnRequest/Granular/Never）× `SandboxPolicy`（DangerFullAccess/ReadOnly/WorkspaceWrite/ExternalSandbox）正交组合；沙箱实现 seatbelt/landlock+seccomp/bwrap/Windows mxc；execpolicy crate 提供 Starlark 规则语言。
- **其他**：unified_exec（PTY 会话复用 + 沙箱拒绝自动降级重试）；多代理 `Op::InterAgentCommunication`；guardian 审查；otel 遥测；MCP 双向（rmcp client + `codex mcp` server）。

## deepseek-harness (dsh)

DeepSeek 官方开源（MIT），TS monorepo，60+ 插件包。

- **everything is a plugin**：基于 Cordis（vendored），agent loop、模型适配器、会话日志皆可替换；运行形态即 profile（web/headless/sdk/acp/desktop）。
- **Cordis 机制深挖（2026-09-28 二次深读，ADR-0009 的证据基础）**：核心 2696 行（`vendor/cordis/src/` 9 文件）+ 外围设施 ~3100 行（loader/schemastery/hmr/include）。ctx 是 `Proxy`（`reflect.ts` get/set trap 沿 fiber 祖先链查字符串键字典）；类型安全只有 declaration merging（163 个文件增强 `Context`），`inject: ['fs']` 与之无编译期连接，拼错 = 永久 PENDING fiber。事故记录 `docs/postmortem/0001`：多写一行 `export default apply` 静默丢 inject，**178 单测全绿、行覆盖 100%，生产完全不可用**；`0002`：`disabled: !!js`（`new Function + eval`）从不被求值，fs 工具永久关闭。HMR（`packages/boot/hmr/`）依赖 Node `--expose-internals`，且**无状态迁移**（dispose-and-recreate，状态本就外置在持久会话日志 + projection）。dsh 自己的刹车：`.agents/notes/rejected/simplification/2026-07-19-fold-compaction-package-split.md` 明文「**Don't split preemptively**」；事件词汇拒绝上运行时 schema。capability seam 三角色（Definition 抽象 Service / Provider 子类或注册式 / Consumer 只 inject）权威定义在 `.agents/notes/implemented/architecture/2026-06-13-capability-seams.md`；`docs/capability-seams.md` 是生成物，~180 个 ctx 服务的 owner/impl/consumer 图。
- **Capability seam 三角色模型**（Definition/Provider/Consumer，`docs/capability-seams.md`）：换一个 fs/subprocess provider，Bash、PTY、LSP 全部跟着迁移到远程沙箱。**hatchery capabilities 层的直接 inspiration。**
- **reasoning**：`packages/llm/llm-deepseek/src/config.ts:85` 暴露 `reasoningEffort: 'off'|'low'|'high'|'max'`（volatile 即时生效）；回放**逐字节精确**以命中 KV cache（决策记录 `.agents/notes/archived/bug-fix/2026-08-19-deepseek-reasoning-passback-every-turn.md`）；`assembler.ts` 把 `reasoning-delta` 累积为 `reasoning` 块。独有 wire 扩展 `dsh_session_log`（会话日志随请求增量上传，watermark at-least-once）。
- **存储**：append-only JSONL（`session.vN.jsonl[.zstd]`，代际迁移链、独占发布、从不改写，`docs/persistence-catalog.md`）；历史不可编辑但支持 fork（`inheritedEventCount` 记录分叉点）；非会话数据走 `packages/storage`（JSON/SQLite 双后端 + 类型化 KV）。
- **权限**：`packages/sandbox`（read-only/workspace-write/danger-full-access；bwrap→Landlock/Seatbelt/restricted token；被拒可一次性升权）+ `packages/interaction`（fail-closed 审批瀑布；permission-presets 打包成用户可见选择器）。
- **ACP 双向**：server `packages/acp/acp`（`dsh --profile acp`）；client `packages/subagent/subagent-acp`（把别的 harness 当 subagent）。**hatchery ACP client 的参照。**
- **i18n 双层方案**：文档层每个 `.md` 配 `.zh.md` + `.i18n.yaml`（按 heading 记录内容 hash），CI 门禁 `verify-translation-pairing` 检测漂移；UI 层 `packages/client/locale`。
- **不变量**："model-visible means logged"（`packages/core/agent-loop/src/invariant.ts`）。

## qwen-code

TS monorepo（0.24.6），远超「Gemini CLI fork」；a2a-server 已移除，ACP + acp-bridge + channels 取而代之。

- **分层**：`core`（无 UI 引擎：`client.ts` LlmClient → `llm-chat.ts` → `turn.ts` Turn.run() AsyncGenerator → `coreToolScheduler.ts` 状态机）；`cli`（Ink/React，进程内直连事件生成器）；多前端（serve REST/SSE、web-shell、desktop=Tauri 2 壳、channels=11 个 IM、三语 SDK、zed-extension）经 daemon/ACP 复用引擎。
- **reasoning**：`openaiContentGenerator/converter.ts` 解析 delta.`reasoning_content`（含 `?? message.reasoning` 回退）并在回放历史时保留；统一 `ReasoningEffort = low|medium|high|xhigh|max`（`core/reasoning-effort.ts`），每模型声明 `disableField: 'enable_thinking'|'reasoning_effort'|'thinking'`——**canonical effort → 各 provider wire 字段的映射表**（设计文档 `docs/design/2026-06-30-unified-reasoning-effort-cli.md`）。hatchery LLM 层直接借鉴。
- **存储**：`~/.qwen/tmp/<projectHash>/`（chats/checkpoints/plans/tool-results）；JSONL 转录 + `session-writer-lease.ts` 多进程写租约 + `branch-points.ts` 分叉回溯 + `fileHistoryService.ts` 文件快照；`SettingScope` 四层配置。
- **权限**：5 档审批（plan/default/auto-edit/auto/yolo，`config/approval-mode.ts`）；auto 档双阶段 LLM 分类器（`permissions/classifier.ts`，Stage1 快速 ~300ms + Stage2 复核，fail-closed）；工具级 `shouldConfirmExecute` + `ToolConfirmationOutcome`（含 ProceedAlways 持久化规则）。
- **提示词**：`core/prompts.ts`（1676 行）按 `interactive|headless|acp` 三态变体；QWEN.md/AGENTS.md 层级加载；自动记忆系统（remember/recall/forget/dream）。
- **ACP**：`cli/src/acp-integration/acpAgent.ts`（16k 行，`@agentclientprotocol/sdk`）；acp-bridge 把 ACP 会话再桥接为 daemon 通道（journal 重放、permissionMediator、compaction）。
- **其他**：subagent（markdown+frontmatter 定义，五级解析优先级）；Goals（结构化目标协议+验证器）；cron/loop 自唤醒；压缩（侧查询摘要尊重 prefix caching + microcompaction）；MCP 传输连接池 + OAuth；git worktree 会话隔离。

## 对 hatchery 的结论

**采纳**：
1. codex 的全协议化（前端皆客户端）+ atomcode 的单 runtime/Live Hub/代际号 → daemon 设计（ADR-0001）。
2. dsh 的 capability seam + qwen-code 的 ACP 实践 → `FsBackend`/`TerminalBackend`/`ApprovalGate` trait，ACP fs/terminal 委派（ADR-0004，atomcode 的反面教材）。
3. qwen-code 的统一 effort 阶梯映射 + dsh 的逐字节回放 + atomcode 的签名块 → LLM 层 reasoning 设计（ADR-0007）。
4. atomcode 影子 Git rewind + qwen-code branch points → 历史分叉 + 代码回滚（ADR-0003/0006）。
5. dsh "model-visible means logged" 不变量 → 存储设计（ADR-0002）。
6. codex 配置分层 + per-key origins；atomcode PRECEDENCE 安全门不可覆盖。
7. dsh/Cordis 的五条语言无关纪律（disposer 逆序、反预拆分刹车、注册句柄原子替换、profile 化装配 + 启动 fail-loud 审计、状态外置 + projection）→ 模块化策略（ADR-0009）。

**规避**：
1. atomcode 单 runtime 强绑定本地工具导致 ACP 适配残缺。
2. dsh 的 Cordis 运行时机制不移植：Proxy ctx/字符串键服务字典/declaration-merging 假类型安全/`!!js` eval/HMR/epoch 重载依赖 JS 动态性且有事故实证（见上 Cordis 深挖）；hatchery 用 trait + 编译期装配，只吸收其纪律层（ADR-0009）；第三方扩展面走 MCP/ACP，WASM 插件 M5 评估。
3. qwen-code 的「CLI 进程内直连 + 远程前端走 daemon」双接线：维护两套路径，hatchery 统一走协议（保留 `--embedded` 进程内 daemon 作为优化，但协议不变）。
4. codex 移除 Chat Completions 押注 Responses-only：hatchery 面向第三方 OpenAI 兼容生态，必须双 wire 并存。
