# 设计：测试体系（横切）

> 状态：设计稿。目标：**所有代码都能如期运行**——每条不变量、每个协议方法、每种降级路径都有对应测试；测试是「实测优先于推测」纪律的执行载体。配套 crate：`hatchery-testkit`（测试基建，见 §2）。

## 0. 测试纪律（全项目强制）

1. **不变量必测**：architecture.md §5 的 6 条核心不变量，每条至少一个专属集成测试（§5 给出映射表）；CI 中归入 `invariants` 分组，永不 skip。
2. **实测优先**：涉及第三方行为的断言（libSQL WAL/触发器、git CLI 边界、provider SSE 怪癖、ACP 宿主行为）必须来自真实执行或真实录制 fixture，禁止按文档/记忆推断后直接写死预期。spike 结论（worklog 各方向）沉淀为回归测试。
3. **Bug 修复必附回归测试**：先写复现测试（红），再修（绿）；worklog 条目引用测试名。
4. **文档示例可运行**：公共 API 的 rustdoc 示例必须是 doctest 且进 CI；不可运行的示例不写「ignore」了事——要么改成可运行，要么改成 `text` 代码块并说明原因。
5. **测试不碰真实用户环境**：一切文件/进程/数据库操作在 tempdir、内存或专用 fixture 工作区内；网络默认禁止，live 测试单独标记（§8）。

## 1. 测试分层

```
                 少 ▲            手动实测（Zed 真机、GUI 走查、doctor 探测）
                    │            live 测试（真实 provider 网络调用，标记 #[live]）
                    │            e2e 全栈（TestDaemon + 协议客户端 + mock wire）
                    │            集成（跨模块：store+分支、daemon+hub、acp+委派）
                    │            契约（trait 级共享测试套件，各实现必须通过）
                    │            属性（proptest：item 树操作、协议 serde）
                 多 ▼            单元（状态机、翻译层、纯函数）
```

组织与命名约定：

- 单元测试：crate 内 `#[cfg(test)]`，文件名 `测试对象::行为::预期`（如 `edit_fork_on_tool_result_starts_new_turn`）。
- 集成测试：各 crate `tests/`；跨 crate e2e 与不变量套件在 workspace 顶层 `tests/`（独立 test crate，dev-depend 全家）。
- runner：**cargo-nextest**（分组、重试标记、JUnit 输出）；快照断言：**insta**（golden file，`INSTA_UPDATE=always` 审阅流）；属性测试：**proptest**。
- 标记：`#[live]`（真实网络，需 secrets）、`#[slow]`（>5s）、`#[gui]`（需显示环境）。PR CI 只跑默认组；live/slow/gui 进 nightly 或手动。

## 2. 测试基建：hatchery-testkit（dev-only crate）

所有 fake/harness 集中在此，各 crate 以 dev-dependency 引用，避免 fake 散落重复：

```rust
// LLM
pub struct ScriptedProvider { rounds: Vec<Vec<StreamEvent>> }  // kernel 状态机测试用
pub struct MockWire;        // wiremock 装配：按 fixture 回放 SSE 字节流（llm crate HTTP 级测试）

// 能力后端（capabilities 接缝的内存实现）
pub struct MemoryFs;        // HashMap<PathBuf, Vec<u8>>，支持断言读写序列
pub struct MemoryTerminal;  // 脚本化进程行为：输出序列/退出码/挂起（测取消）
pub enum ScriptedApproval { AutoAllow, AutoDeny, Script(Vec<ApprovalOutcome>) }

// 环境
pub struct TempWorkspace;   // tempdir + 可选 git init + 文件树 DSL
pub struct TestDaemon;      // 进程内 DaemonCore + 内存 transport；暴露 ClientProbe
pub struct ClientProbe;     // 协议客户端：call/收集事件/断言事件序列（e2e 主驱动）

// fixture
pub fn fixture(name: &str) -> Bytes;          // tests/fixtures 加载 + 元数据校验
pub fn assert_golden(value: impl Debug);      // insta 封装，统一快照命名
```

**契约测试套件**：对 `LlmProvider`、`SessionStore`、`FsBackend`、`TerminalBackend`、`ApprovalGate` 各定义一组 trait 级共享测试（宏或泛型 fn），任何实现（本地/ACP 委派/内存/未来远程）必须整套通过——这是「换绑定不换工具」（ADR-0004）的机器保证。

## 3. 各方向测试设计

### 3.1 protocol（hatchery-protocol）

风险：wire 兼容破坏、serde 表示漂移。

- 每个方法/事件的 **serde 往返测试** + golden JSON（insta）：字段增删一目了然。
- **版本兼容**：`tests/fixtures/protocol-v<N>/` 存各版本 golden；测试断言当前代码能反序列化 N-1 的全部 fixture（只增不改语义的机器检查）。
- fixture 确定性：JSON 序列化字段顺序稳定（serde 结构体序），CI 校验 fixture 无 diff。

### 3.2 kernel

风险：状态机死角、取消语义、上下文组装错误。

- 状态机全迁移矩阵：ScriptedProvider 脚本化每条路径（含错误注入），代表用例：
  - `interrupt_during_streaming_yields_finished_interrupted`
  - `interrupt_during_tool_execution_kills_terminal`（MemoryTerminal 断言 kill 被调）
  - `max_rounds_fuse_trips_at_limit`
  - `tool_snapshot_frozen_when_mode_switches_mid_turn`
- 组装：reasoning 块按能力表保留/丢弃；compaction 区间替换；token 裁剪顺序（先旧 round 的工具结果）。
- 确定性：同一脚本输入两次运行产生完全相同的事件序列（事件序断言）。

### 3.3 llm

风险：wire 翻译错误、reasoning 回放被污染、重试风暴。

- **HTTP 级**（MockWire + 录制 fixture）：SSE 逐 chunk 翻译、跨包分片（tool call arguments 被切断在两个 chunk）、`[DONE]`、半帧缓冲、非法 JSON 容错。
- **reasoning 逐字节纪律**（ADR-0007 的机器保证）：
  - `reasoning_passback_is_byte_exact`：fixture 含首尾空白/unicode/换行的 reasoning_content，入库→重建→断言 wire 请求体中逐字节相等（比较序列化后的 JSON 字段原文）。
  - 能力表驱动矩阵：每个 provider 族 × {echo, signature, 丢弃} 行为。
  - 填充剥离只发生在入库前（`(no reasoning detected)` 类），有正反用例。
- effort 映射表：每族 golden 请求体（deepseek Off→换模型、qwen→enable_thinking+budget…）。
- 重试：429 + `Retry-After` → RateLimited 事件与退避时序（tokio::time::pause 虚拟时钟）；401 不重试；流中断→TurnFailed。
- **live**（`#[live]`，手动跑）：`hatchery doctor --provider X` 即 live 测试的 CLI 形态；新 provider 接入时录制 fixture + 把实测行为写进 worklog/llm.md。

### 3.4 store

风险：分支树 SQL 错误、并发丢失、崩溃损坏——数据层错误最不可原谅，测试最重。

- **属性测试（核心投入）**：随机操作序列（append/edit_fork/switch/delete_branch/compaction）作用于 libSQL 实现与纯 `Vec`/树参考模型，每步后断言 `rebuild_history(active_head)` 与参考模型一致。参考模型是显式写出的第二实现（不复用生产代码）。
- 级联删除：`delete_branch_cascades_items_and_checkpoints`、`delete_branch_refuses_when_active_head_inside`、共享祖先不被误删。
- append-only：`items_update_trigger_aborts`（直接发 UPDATE 断言 RAISE）。
- 并发：N reader + writer actor 压测（1k items）无 busy 错误、顺序保证；背压（channel 满时 await 而非丢弃）。
- **崩溃测试**：spawn 真实子进程写库，`kill -9`，重启断言「已提交 item 全在、至多丢当前 item、WAL 自动恢复」；每种 StoreCmd 各一次。
- 迁移：v(N-1) 库文件 fixture → 自动迁移 → schema 断言。

### 3.5 capabilities + tools

风险：影子 Git 碰用户仓库（灾难级）、硬门被绕过、PTY 泄漏进程。

- **影子 Git 安全**（不变量 6 专属）：`shadow_git_never_touches_user_repo`——TempWorkspace 先 git init + 造脏状态（staged/unstashed/HEAD 位置/detached），跑一轮 snapshot+restore，断言用户仓库 `status --porcelain`、`rev-parse HEAD`、index mtime、refs 全部不变。
- 检查点语义：写前必有 snapshot（MemoryFs 写序列 vs checkpoint 记录对齐）；restore 可回滚（restore 前自动 snapshot）；大文件跳过；预算熔断触发 GC（构造超预算 fixture）；非 git 工作区可用。
- 硬门（不变量 5 专属）：`project_config_cannot_disable_hard_gates`——加载恶意项目配置后断言危险路径写仍需审批、规则不可 allow-always。
- LocalFs：路径逃逸（`../`、symlink 出工作区、绝对路径）全部拦截；行范围读取边界。
- LocalPty：真实进程（`sh -c echo/sleep/kill -0`）——输出完整性、超时杀、cancel 后无孤儿进程（`kill -0` 断言）、环形缓冲截断。
- 工具单测：每个工具 × MemoryFs/MemoryTerminal 的行为矩阵；spill 阈值；凭据脱敏（高熵串 fixture 正反例）。
- 契约套件：LocalFs/AcpClientFs 过同一组 FsBackend 契约测试（§2）。

### 3.6 daemon

风险：竞态（双实例/双 runtime）、事件错序、代际污染。

- 单实例：两进程并发 attach-or-spawn → 恰一胜者（重复 20 次抓竞态）；陈旧 daemon.json（pid 已死）自愈。
- 租约与代际（不变量 1 专属）：`stale_runtime_events_are_dropped`——旧 generation 事件注入 hub，断言订阅者收不到；`session_lease_blocks_second_runtime`。
- hub：两订阅者收到同序事件；迟加入者 replay 完整；慢消费者被踢且不阻塞他人；coalescing 合并 delta 但控制事件不合并、不乱序。
- 崩溃恢复：status=running 的会话重启后标记 interrupted 且发过 TurnFailed 存档事件。
- e2e 主干（§4）大部分在此层驱动。

### 3.7 acp

风险：能力协商矩阵漏分支、委派语义与本地语义漂移。

- 协商矩阵测试：client caps {fs,terminal} × {有,无} 四组合 → 断言绑定的后端类型与降级行为（用 testkit 的 fake ACP 连接）。
- **fake 宿主集成**：用官方 crate 写 minimal test client，进程内驱动完整 turn：prompt → `fs/write_text_file` 委派往返 → `terminal/create..release` 委派往返 → `session/request_permission`（断言 option 映射与 allow_always 落规则表）→ 完成；`agent_thought_chunk` 携带 reasoning。
- `session/load` replay：长会话重放的 update 序列与 active 分支一致（golden）。
- client 侧：fake 子 agent 进程（testkit 提供 `dummy-acp-agent` 二进制 target）→ subagent 工具全流程；权限上浮策略三档；深度限制与环检测（自举：hatchery spawn hatchery，深度 2 熔断）。
- **手动实测清单**（M3 验收，结果记 worklog/acp.md）：Zed 真机——编辑/终端/审批/thought 展示/取消/多会话；宿主行为与规范不符处记录并加防御测试。

### 3.8 cli

- **TUI golden 帧**：ratatui `TestBackend` + insta——喂脚本化事件流，断言关键帧（消息渲染、reasoning 折叠展开、审批弹层、分支列表）。布局改动的 diff 审阅即视觉回归。
- headless：`exec --json` 输出 golden（JSONL 逐行）；退出码矩阵（completed/failed/cancelled/审批拒绝）。
- 斜杠命令：每个命令一个「输入命令 → 断言发出的协议方法」单测（命令层与渲染层分离，命令层纯函数可测）。

### 3.9 gui

GUI 是测试最薄弱层，策略 = 「逻辑出 GTK，GTK 只做投影」+ 分层兜底：

- **view-model 单测**（主力）：所有状态变换（事件→列表模型、分支树→可视化结构、审批弹层状态机）为不依赖 GTK 的纯 Rust 类型，全覆盖单测。
- GTK 冒烟（`#[gui]`，CI 用 GNOME SDK 容器 + xvfb-run）：窗口构建、导航、空会话渲染不 panic；每个 milestone 扩一条路径。
- 性能 fixture：10k items 会话的 ListStore 构建耗时基准（criterion）+ 滚动冒烟。
- RTL/i18n：`GTK_TEXT_DIR=rtl` + `LANGUAGE=ar` 截图冒烟（人工比对存档）；gettext 完整性——xtask 断言源码无未包裹的用户可见字符串（启发式 lint）+ pot 无 fuzzy。
- 手动走查清单（M4 验收）：完整 Code 会话、审批、rewind 面板、分支时间线、设置窗全项。

### 3.10 platform（配置/提示词/i18n）

- 配置：分层合并 golden（五级 fixture 叠加）、per-key origins 正确、数组整体替换语义、项目级越权忽略+warning、坏 TOML 逐 key 降级。
- 提示词：`prompt/render` golden（含 section 来源标注）；user_override 生效但 safety_gate/tool_discipline 不可覆盖（正反用例）；AGENTS.md 层级发现（嵌套工作区 fixture）。
- i18n：见 §3.9 的提取完整性检查。

## 4. e2e 全栈主干

每个里程碑的 DoD 落到一组 e2e 场景测试（TestDaemon + ClientProbe + MockWire，无真实网络）：

| 场景 | 里程碑 | 断言要点 |
|---|---|---|
| 最小对话：prompt → 流式响应 → 落库 → resume | M1 | 事件序、DB items、reasoning 回放（第二 turn 请求体逐字节） |
| 双前端扇出一致性 | M1 | 两 probe 事件序列相同；断线重连 replay 补齐 |
| Chat→Code 切模式 | M2 | 工具表变化在 turn 边界生效 |
| 编辑分叉重演 + 分支删除 | M2 | 新旧分支 rebuild、级联删除、active_head 校验 |
| 写坏文件 → rewind 三 scope | M2 | 工作区文件恢复、对话回退、检查点联动 |
| 审批全链路（本地 + 规则持久化） | M2 | fail-closed 超时、ProceedAlways 生效 |
| ACP 委派 turn（fake 宿主） | M3 | §3.7 场景在真实 daemon 进程形态下复跑 |
| subagent 自举 | M3 | 深度限制、权限上浮 |
| GUI view-model 场景回放 | M4 | 同一 e2e 事件流喂 view-model 断言 UI 状态 |

## 5. 不变量 → 测试映射（CI `invariants` 分组）

| 不变量（architecture.md §5） | 专属测试 |
|---|---|
| 1 单一 runtime 所有者 | `stale_runtime_events_are_dropped`、`session_lease_blocks_second_runtime`、单实例竞态 |
| 2 模型可见=已记录 | e2e 每场景收尾断言「重建上下文 == MockWire 实际收到的请求体」（逐 turn） |
| 3 items append-only | `items_update_trigger_aborts`、store 属性测试 |
| 4 工具只经接缝 | clippy `disallowed_methods`（编译期）+ 工具单测只注入 Memory 后端（运行期证明） |
| 5 安全门不可覆盖 | `project_config_cannot_disable_hard_gates` + prompts 覆盖正反用例 |
| 6 影子 Git 不碰用户仓库 | `shadow_git_never_touches_user_repo` |

不变量 2 的实现方式值得单列：TestDaemon 里 MockWire 记录**实际发出的请求体字节**，测试收尾从 store rebuild 上下文并序列化，两者必须逐字节一致——这一条测试同时锁死了组装、存储、回放三层。

## 6. fuzz 与边界

- **cargo-fuzz**（M2+，nightly 短跑）：SSE 帧解析、JSON-RPC 帧解码（半帧/超长/嵌套）、item payload serde、路径校验（LocalFs 逃逸面）。
- proptest 除 store 外还覆盖：effort 映射全定义域、token 裁剪单调性、coalescing 时间窗（虚拟时钟随机事件流不丢不乱）。

## 7. 性能基准（criterion）

`rebuild_history`(1k/10k items)、hub 扇出吞吐、prompt 组装、SSE 解析吞吐、GUI ListStore 构建。基线存仓库（criterion baseline），nightly 对比，劣化 >20% 标红（人工裁决，不自动 block）。

## 8. CI 门禁

**PR（必须全绿，<10min 目标）**：fmt → clippy `-D warnings`（含 disallowed_methods）→ nextest 默认组（单元+契约+属性+集成+e2e+invariants）→ doctests → fixture 确定性（git diff 为空）→ i18n 提取检查。

**nightly**：slow 组、gui 组（GNOME SDK 容器 + xvfb）、fuzz 短跑（10min/target）、criterion 基线对比、cargo-audit、`cargo mutants`（可选，先只跑 store/kernel 两个高风险 crate）。

**live 组（不进 CI，手动/自托管）**：需要真实 provider 密钥；`cargo nextest run -E 'test(live_)'`；触发时机 = 新 provider 接入、上游 openai-interface 升级、能力表改动。结果记 worklog/llm.md。

**覆盖率**：cargo-llvm-cov，PR 报告不 block；阈值（line）：kernel/store/llm/capabilities ≥ 85%，protocol/daemon ≥ 80%，cli ≥ 60%，gui 豁免（view-model 部分 ≥ 80%）。覆盖率是指标不是目标——不变量套件与属性测试的通过优先于数字。

## 9. 手动实测清单（模板）

每次里程碑验收与真机测试按清单执行，结果（含失败与宿主怪癖）记入对应 worklog：

- M1：真实 provider（deepseek/qwen 各一）对话、reasoning 展示与回放命中、resume、双终端扇出。
- M2：真工作区改坏→rewind、审批规则持久化、非 git 目录检查点。
- M3：Zed 真机全链路（§3.7）。
- M4：GUI 走查（§3.9）+ flatpak 安装冒烟。

## 开放问题

1. testkit 的 `dummy-acp-agent`、`dummy-provider` 等辅助二进制以 workspace member（`[[bin]]` + `required-features = ["testkit"]`）还是独立小 crate 存在——M0 定。
2. cargo-mutants 的投入产出（跑一次全 workspace 很慢）——先 nightly 只对 store/kernel，M2 评估。
3. GUI 快照测试（截图 diff）是否引入（GTK 渲染跨环境像素不稳定，倾向只做 RTL/i18n 人工存档）——M4 评估。
4. e2e 是否需要「真实 daemon 子进程」形态（当前 TestDaemon 是进程内；进程形态额外覆盖 UDS/序列化层，代价是测试变慢）——M1 各做一条对比后定默认形态。
