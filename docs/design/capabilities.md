# 设计：能力接缝、工具、审批与回滚（hatchery-capabilities / hatchery-tools）

> 状态：设计稿。依据 ADR-0004（capability seam）、ADR-0005（模式）、ADR-0006（影子 Git）。

## 1. hatchery-capabilities：接缝定义

三个核心 trait（原始草图见 ADR-0004，**落地后的偏差见下面「与 ADR-0004 草图的偏差」一节，以本节代码块为准**）+ 检查点与工具注册表框架：

```rust
// 工具契约住在这一层：kernel 只见 ToolHost 窄接口（kernel.md §5），否则 kernel 会反过来依赖上层成环
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;                       // ToolDef 是 kernel 类型（要进 LLM 请求）
    fn needs_approval(&self, args: &Value) -> Option<ApprovalRequest>;
    fn summarize(&self, args: &Value) -> ToolCallSummary;   // 草图里没有；摘要需要工具语义，见下
    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError>;
}

pub struct ToolCtx<'a> {          // 工具能拿到的全部外界能力 = 接缝
    pub fs: &'a dyn FsBackend,
    pub terminal: &'a dyn TerminalBackend,
    pub cancel: CancellationToken,
    pub emit: &'a dyn Fn(ToolProgress),   // 进度上报 → ToolCallProgress 事件
    // M2 加：检查点收集器（LocalFs 写前 push；见 §2「检查点如何成为 item」）
}

// 路径一律 &str 且工作区相对（不是 PathBuf）：根由 backend 拥有，「越界」的定义也只由 backend 给
pub trait FsBackend {
    async fn read_text_file(&self, path: &str) -> Result<String, FsError>;
    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError>;
    async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError>;
    // M2 写原语：write_text_file + create_dir + remove
    //   （write_file 要建父目录，rewind 的 purge 要删文件——只有 write_text_file 不够）
    // M2 另加：glob 的模式过滤下推（read_dir 这个只读子集已提前进 M1，见下）
}

pub trait TerminalBackend {
    async fn create(&self, spec: TerminalSpec, cancel: CancellationToken)
        -> Result<Box<dyn TerminalHandle>, TermError>;
}
pub trait TerminalHandle {
    async fn wait(&mut self) -> Result<TerminalOutcome, TermError>;   // M1：进程结束后一次性全量输出
    fn kill(&mut self);
    // M2 形状：再加「输出流 + release」——没有输出流就没有 ToolCallProgress，见下
}

pub trait ApprovalGate   // request(ApprovalRequest) → ApprovalOption（批准后由 daemon 回灌 kernel）

// ToolDef / ToolInvocation / ToolProgress* 见下；值类型住在 protocol（M0b 分层修正）
pub struct ToolRegistry { /* name → Arc<dyn Tool>；实现 kernel::ToolHost，turn 开始冻结快照 */ }
pub struct Backends { pub fs, pub terminal /* M2 加 approval 与 checkpoint 两个字段 */ }
```

### 与 ADR-0004 草图的偏差（已定型）

落地时接缝与草图有若干出入，每条都是设计决定而非临时将就；草图以上面的代码块为准，出入的理由如下：

1. **路径是 `&str` 且工作区相对**，不是 `PathBuf`。根由 backend 拥有：这样绑定到 ACP 宿主的单文件接口时，「越界」由宿主定义，工具一行都不用改（ADR-0004 的初衷）。
2. **接缝上没有 `line_range`**。分页（offset/limit）是 `read_file` 的**工具参数语义**，不是文件系统能力；放进 backend 会把工具语义泄漏进接缝，而 ACP 宿主的文件接口未必给得出行范围。带内封顶因此住在工具里。
3. **`Tool` 比草图多一个 `summarize`**。摘要需要工具语义（`read_file a.txt` 与 `grep todo` 的一行摘要不同），注册表的通用回退只兜底未知工具。
4. `read_dir` 提前进 M1（草图写「M2 加 list/glob 支持」）：glob/grep 必须有目录原语才能在接缝内行走，这是它的只读子集。留在 M2 的是**模式过滤下推**（把 glob 语义交给 backend）。

### `TerminalHandle`：M1 的形状不足以支撑 shell 工具

M1 的 handle 只有 `wait()`（进程结束后一次性返回全部输出）与 `kill()`。这意味着 shell 工具**产不出 `ToolCallProgress`**：进度要求一个在进程还活着时就能拿到字节的输出流，而 `wait()` 的语义正好相反。所以 M2 的「输出流 + `release()`」是**替换形状，不是扩展**——`src/terminal.rs:34` 那句「streaming extend it without replacing it」对流式不成立，随实现一起改掉（`unified_exec` 式的会话复用仍是扩展，那条不变，见开放问题 2）。

现在改最便宜：`TerminalBackend` 只有一个实现 `NoTerminal`，`TerminalHandle` 一个实现都没有。

### 值类型的归属（M0b 定案）

初稿把 `ApprovalRequest`/`ApprovalOutcome`/`ToolOutput`/`ToolProgress` 都写成 kernel 类型（M0a 修正）。落地时发现：它们既要进 wire（`ItemKind::ToolResult`、`ServerEvent::ApprovalRequested`/`ToolCallProgress`），又被 kernel 与 capabilities 共用，而 layering 禁止同层横向依赖——于是它们统一搬到最底层的 **protocol**（见 architecture.md §3 的 M0b 分层裁决）：

| 类型 | 归属 | 理由 |
|---|---|---|
| `ToolOutput`、`ToolProgress`、`ToolArtifact`、`SpilledOutput` | protocol | 落库 + 进事件 |
| `ApprovalRequest`、`ApprovalOption`、`RiskLevel` | protocol | 进事件 + 落 `approval_rules` |
| `ToolInvocation { output, is_error }` | kernel | 「跑失败」与「没跑成」的分野，只对 kernel 的循环有意义 |
| `ToolDef`、`ToolCallSummary` 的**产生** | kernel / ToolHost | `ToolDef` 进 LLM 请求；`ToolCallSummary` 由 `ToolHost::summarize` 构造 |
| `Tool`、`ToolCtx`、`FsBackend`、`TerminalBackend`、`ApprovalGate` | capabilities | 实现细节，kernel 不得看见 |

`ApprovalOutcome` 不再单独存在：答复必然是被提供的选项之一，`ApprovalOption { AllowOnce, AllowAlways, Deny, DenyAlways }` 兼作请求选项与答复，`ApprovalGate::request(&self, req) -> ApprovalOption`。

### 审批往返由谁发起（M0b 定案）

**kernel 发起、daemon 应答**：kernel 调 `ToolHost::approval_for` 判断是否需要审批；需要则进入 `AwaitingApproval`，发 `KernelEvent::ApprovalNeeded { request_id, request }`，等 `AgentCommand::ApprovalDecision { request_id, option }`（kernel.md §7）。daemon 收到事件后调用该会话绑定的 `ApprovalGate`（本地 `DaemonApproval` 弹给前端；`AcpPermission` 转发 `session/request_permission`），把结果作为命令回灌。

这样切的两个好处：`request_id` 与协议的 `approval/respond` 一一对应，daemon 只是翻译层；kernel 不认识任何审批后端，`ToolCtx` 里也**没有** approval 字段——工具不请求审批，审批发生在工具被调用之前。超时策略（fail-closed = deny）住在 `ApprovalGate` 实现里，不在 kernel：kernel 无从知道一个人需要多久。

**注册句柄模式**（ADR-0009 纪律 3，借鉴 dsh `registerAdapter()` → handle）：`register()` 返回 `RegistrationHandle { dispose(), replace() }`——`replace()` 用新实现整表原子替换旧实现（进行中的 turn 不受影响，因为 kernel 持有的是冻结快照），`dispose()` 摘除注册。MCP 工具、用户自定义工具、运行中换 provider adapter 全部走这一模式；禁止对注册表的原地突变。

**这一模式推到 M5**（roadmap 的顺延表）：它的消费者是跨会话存活、需要原地换 MCP 工具的注册表，M2 没有这个消费者（ADR-0009 的反预拆分刹车）。在那之前 `register()` 返回 `()`，注册表每会话一个、装配后不可变——「换工具」= 下一 turn 换一个注册表，正好接上 kernel 的 Turn Tool Snapshot。

### 本地实现（本 crate 提供）

三者中只有 `LocalFs` 的**只读路径**已落地（M1）；`LocalFs` 的写路径、`LocalPty`、`DaemonApproval` 都是 M2（Phase 1 / 4 / 2），今天是设计而非代码。

- `LocalFs`：tokio::fs + 路径校验（工作区逃逸检查、危险路径硬门）+ **写前打影子 Git 检查点**。
- `LocalPty`：实现 TerminalBackend；PTY 会话注册表（后台进程、复用、超时杀进程）；输出环形缓冲（截断保护，借鉴 codex unified_exec）。**库选型 = D10**（spike 提前到 M2 Phase 1 并行做，实现落 Phase 4）：候选 `portable-pty`（codex 用 `portable-pty = "0.9.0"`，references/codex/codex-rs/Cargo.toml:422），但它在 Windows 侧还要额外挂带 `jobapi`/`jobapi2` 的 `winapi`（references/codex/codex-rs/utils/pty/Cargo.toml）——即 Job Object，因为**杀 PTY 不留孤儿孙进程**要靠它，而那正是「cancel 后无孤儿进程（`kill -0` 断言）」这条测试在 windows-gnu CI 上的坑。spike 必须在三平台上实测孤儿进程、取消与输出流后再定。
- `DaemonApproval`：把 ApprovalRequest 经 EventSink 发 `ApprovalRequested` 协议事件，等待前端 `approval/respond`；查询/写入 `approval_rules` 持久化规则；超时策略 = deny（fail-closed，借鉴 dsh）。

### 委派实现（hatchery-acp 提供，实现同样的 trait）

`AcpClientFs` / `AcpClientTerminal` / `AcpPermission` —— 见 design/acp.md §4。这就是「换绑定不换工具」。

## 2. 影子 Git 检查点（CheckpointStore）

```rust
pub struct CheckpointStore { /* per-workspace 影子仓库，daemon 内互斥 */ }

pub struct RestoreOptions {
    /// 删除「快照之后才出现、且从未被任何快照跟踪」的文件。
    /// 默认 false：rewind 永不删用户自己的未跟踪文件。开启需要审批 + 二次确认 + 先列出待删清单。
    pub purge_untracked: bool,
}

pub struct RestoreReport { pub rolled_back: Vec<PathBuf>, pub purged: Vec<PathBuf> }

impl CheckpointStore {
    pub fn init(workspace: &Path) -> Result<Self>;       // git-dir: ~/.local/share/hatchery/checkpoints/<workspace-hash>
    pub fn snapshot(&self, kind: CheckpointKind, item: ItemId) -> Result<CommitId>;
    pub fn diff(&self, from: CommitId, to: CommitId) -> Result<UnifiedDiff>;   // 见下：返回类型名是暂定的
    pub fn restore(&self, to: CommitId, opts: RestoreOptions) -> Result<RestoreReport>;
    pub fn gc(&self, budget: Budget) -> Result<u64>;     // 预算熔断（ADR-0006）
}
```

`UnifiedDiff` **不存在**——protocol 里没有任何 diff 载荷类型（无 `UnifiedDiff`/`DiffHunk`/`FileDiff`），草图这个名字是暂定的。M2 Phase 1 给 hatchery-protocol 加一个 diff 载荷类型（TUI 的 diff 预览与 M4 的 GUI diff 视图共用同一份，ACP 的 `ToolCallContent{content=diff}` 也要它），最终以那次改动的命名为准。注意 `ApprovalRequest` 装不下 diff：它只有 `args_digest: String`，其文档明写不是原始 JSON；审批要预览 diff 得走 D14 的结构化 preview 字段。

另有一处 diff **没有来源**：`git2` 出的是检查点之间（commit → commit）的 diff，而 `write_file`/`edit` 的**预览** diff 是「旧缓冲 vs 新内容」、此时还没有 commit。这需要独立的 diff 能力（D11 的证据：atomcode 用 `similar = "2"`，references/atomcode/Cargo.toml:40），目前不在任何依赖表里。

实现纪律（后端 = **git2 / vendored libgit2**，ADR-0012；下列结论全部来自 `tests/spike_shadow_git.rs` 的门槛实测——该套件常驻，升级 git2 必须重跑）：

- **不依赖用户的 git 二进制**（这是选它的首要理由：很多用户机器上没有 git）。构建期需要一个 C 编译器——libgit2-sys 用 `cc` 编译 vendored libgit2 与 pcre2，**不需要 cmake**（实测 build.rs 未调用它）。daemon 启动审计因此不检查 git，但仍要检查数据目录可写。
- **打开影子仓库的固定配方**（`CheckpointStore::open`）：
  1. 首次创建：`Repository::init_opts(git_dir, opts)`，opts = `no_dotgit_dir(true)` + `bare(true)` + `external_template(false)`（不读开发者机器上的模板目录）。
  2. 自己写 config：`core.worktree = <用户工作区>`、`core.bare = false`。**必须手写**——libgit2 的 `set_workdir(path, update_gitlink=false)` 只改内存句柄；而 `update_gitlink=true` 会在用户工作区里种一个 `.git` gitlink 文件（`repository.c:3259`，读源码 + 实测双确认）。
  3. `set_workdir(<用户工作区>, false)` 让当前句柄生效；之后重新 `Repository::open(git_dir)` 靠第 2 步的 config 恢复 work tree。
  4. 钉住配置以隔绝开发者机器：`core.autocrlf=false`、`core.excludesFile=<不存在的路径>`（否则用户的全局 excludes 会悄悄缩小快照范围）、`core.fsmonitor=false`、`user.name/email`。测试里的 `harden()` 就是这份清单，实现照抄。
- **不变量 6 已实测**：快照 + 恢复全程，用户仓库的 HEAD、分支、refs、`.git/index` mtime、`.git` 目录条目全部不变，且工作区里不会出现 `.git`（`invariant_shadow_git_never_touches_user_repo`、`no_gitlink_is_planted_in_the_user_workspace`）。启动时仍要断言影子 git-dir ≠ 用户任何 `.git`（含向上查找）。
- **读用户仓库是安全的**：libgit2 的 `statuses()` 实测**不会**重写用户的 `.git/index`（CLI 的 `git status` 会）。prompt 的 environment 节（platform.md §2.1）因此可以直接读分支/脏状态，不再需要「禁用 status」那类脆弱纪律。
- **忽略规则是 per-handle 的**：`add_ignore_rule` 只作用于当前 `Repository` 句柄（实测：临时句柄上加的规则对下一次打开无效）。`CheckpointStore` 每次打开都要按配置重放规则；`.git/` 恒在规则里；大文件（默认 >10MB）与构建产物走配置化排除，且**排除不等于删除**（实测文件本身保留）。
- **restore 语义（实测）**：`reset(Hard)` 会回滚已跟踪文件、并删除「被后续快照跟踪过的新文件」，但**不会**删除从未被任何快照跟踪的文件；给它传 `remove_untracked` 也无效（hard reset 的 checkout 只覆盖与目标有差异的路径）。所以 `purge_untracked=true` 要额外走一遍 `checkout_index(None, force().remove_untracked(true))`，且**不加 `remove_ignored`**（否则会删构建产物与用户的 `.env`）。默认 `false`：rewind 永不删用户自己的未跟踪文件；开启需审批 + 二次确认 + 先列出待删清单。
- restore 前先 snapshot 当前状态（回滚也可回滚）。
- 用户工作区不是 git 仓库时一切照常（实测通过）——影子仓库不依赖用户仓库存在。
- 预算核算：`revwalk` 数快照数，影子 git-dir 占用直接遍历目录求和（实测 5 次快照仅 3457 B，因为 libgit2 不铺 hooks/模板）；GC 与熔断逻辑 M2。
- 性能（实测，Linux x86_64 / 24 核）：500 文件冷快照 48.9 ms、10 处改动的热快照 6.0 ms、硬恢复 2.8 ms。热路径（每次写前打点）是常态，比 CLI 后端快约 2 倍。

### 检查点如何成为 item（D13）

链路：**`ToolCtx` 带一个检查点收集器** → `LocalFs` 在每次写之前 snapshot 并 push 进去 → **`ToolInvocation` 把收集到的 `Vec<Checkpoint>` 带出工具调用** → **kernel 在 ToolResult item 之前追加 Checkpoint item** → daemon 的 HubSink 在 Checkpoint item 落库之后补写 `checkpoints` 行。

三个设计理由：

- 追加位置由 kernel 负责而不是 daemon：kernel 本来就在造 ToolCall/ToolResult item，用同一套机器，顺序天然正确，链变成 `… → ToolCall → Checkpoint → ToolResult`。
- 这个顺序**安全**，因为工具结果靠 `ToolResult.call` 与调用配对，**不靠父子关系**——在两者之间插 item 不会拆散配对。
- `checkpoints` 行写失败**只记日志**：commit_id 已由 item 自己携带（`ItemKind::Checkpoint { commit_id, kind }`），行只是索引，丢了可以从 item 重建。

### rewind 如何定位 commit（更正）

**rewind 不查 `checkpoints` 表。** `Checkpoint` **item** 自己带着 `commit_id`，且 `ItemKind::is_conversation()` 不含 Checkpoint（它是代码侧的锚点，不进模型请求）。Code scope 的算法是：

`rebuild_chain` 从旧 head 建链 → 定位 `target_item` → **向后扫第一个 Checkpoint item** → 读它的 `commit_id` → `restore`。

pre-write 快照恰好等于 `target_item` 时刻的工作区状态，因此扫不到 Checkpoint 就是 no-op（那之后没写过东西）。

`checkpoints` **表**（其 `item_id` 列可空）服务的是**跨会话的预算核算与 GC**，不是 rewind 的主索引——早先文档把它写得像主索引，那是错的。`item_id` 可空正是为 restore 前的安全快照留的：那次快照是 undo-of-undo，不属于对话历史，所以记进表、`item_id = NULL`、**不建 item**。

## 3. hatchery-tools：内置工具

工具 = 纯逻辑 + 接缝调用，禁止直接 `std::fs`/`std::process`（CI deny lint）。

| 工具 | 模式 | 接缝 | 审批默认 |
|---|---|---|---|
| `read_file` | Chat+Code | FsBackend | 工作区内无需审批。工作区外分模式：**Code** → Executes 级审批；**Chat** → 无审批（ADR-0005），越界读取直接以错误结果返回给模型 |
| `glob` / `grep` | Chat+Code | FsBackend（`read_dir` 已进 M1；M2 加模式过滤下推）| 无需 |
| `write_file` / `edit` | Code | FsBackend（写前检查点） | WritesWorkspace，规则可持久化 |
| `shell` | Code | TerminalBackend | Executes；命令摘要展示；危险模式（rm -rf、sudo、curl\|sh）→ DenyAlways 提示 |
| `web_fetch` | Chat+Code | 内置 http client（唯一例外的直连，Network 风险级） | 首次审批，可持久化域名规则 |
| `checkpoint_diff` / `rewind` | Code | CheckpointStore | 无需（只读）/ 需确认（restore）；`--purge` 额外审批 + 待删清单 |
| `subagent`（M3） | Code | ACP client | 继承父会话策略 |
| MCP 工具（M5） | 按配置 | rmcp client | 默认 Executes 级审批 |
| WASM 插件工具（M5 评估占位，ADR-0009） | 按配置 | wasmtime + WASI（沙箱化） | 默认 Executes 级审批；实施前须过新 ADR |

工具输出统一 `ToolOutput { text, artifacts?, spilled? }`；超阈值 spill 到 `~/.local/state/hatchery/tool-results/`，库存引用。阈值与落盘位置 = **D12**；「谁来 spill」——工具自己还是注册表统一做——**尚未裁决**（protocol 侧的 `ToolOutput.spilled`/`SpilledOutput` 值类型已在，但还没有任何构造方；今天各工具是在带内封顶：`read_file` 的字节帽、glob/grep 的条数上限）。

### Chat 模式的审批真空（M2 Phase 2 裁决）

ADR-0005 说 Chat 的审批策略是「无（只读无需审批）」，而上表给两个 **Chat+Code** 工具标了审批默认：`read_file` 的工作区外读取（Executes 级）与 `web_fetch`（首次审批 + 可持久化域名规则）。Chat 会话没有应答方，所以这两格在 Chat 下是**真空**。

M1 已经在 `read_file` 上撞到过它，裁决是「越界读取直接以错误结果返回给模型，否则会挂在无人应答的审批等待上」（见 worklog/capabilities.md 的 M1 只读层条目，2026-10-01）。`web_fetch` 进 Chat 时会撞上同一个问题，三个处置：

1. **Chat 的工具表不含 `web_fetch`**（倾向）——ADR-0005 把它列进 Chat 的理由是「只读」，而 Network 风险级本身就已经承认它不只是只读；移出 Chat 比给 Chat 造一个审批应答方省事，也不削弱 Chat 的定位。
2. 照 `read_file` 的先例：未获批的域名以错误结果返回。
3. Chat 也绑一个 gate（与 ADR-0005 的「无审批」直接冲突，等于给 Chat 加一档）。

若选 1，这是对 ADR-0005 模式表的一处偏离——按约定**不改 ADR**，在本文档与 worklog 留痕即可（幅度不足以另立 ADR；若 Chat 的工具面后续继续变动，再合并成一份新 ADR 并标 supersedes）。

### 依赖前提（M2 要先补的洞）

- **`web_fetch` 需要自己的 HTTP client 依赖**：今天**没有任何产品 crate 有通用 HTTP client**（`reqwest` 只是经 `openai-interface` 传递进 hatchery-llm），而 `wiremock` 是 testkit 专属、明确「never of a product crate」。HTML→Markdown 同样一个 crate 都没有（无 `htmd`/`html2md`）。两半合并为 **D17**（Phase 4）——不能分开选，因为选型同时决定新依赖面的大小。
- **`write_file`/`edit` 的预览 diff 没有来源**：`git2` 出的是检查点之间的 diff，而预览要的是「旧缓冲 vs 新内容」、此时还没有 commit（见 §2）。

## 4. 模式装配（daemon 侧）

```rust
fn assemble(mode: &SessionMode, backends: Backends) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    for spec in mode.tools.enabled() { reg.register(builtin(spec, &backends)); }
    reg  // turn 开始时被 kernel 冻结快照
}
```

- `ToolPolicy` 支持 include/exclude 列表与 per-tool 配置（如 shell 的 timeout、web_fetch 的域名白名单）。
- 用户自定义模式在 config.toml 声明三元组（ADR-0005）。

本节全是 **M2 Phase 2 的交付物**：`assemble(mode, backends)`、`builtin(spec, &backends)` 与 `ToolPolicy` 都还不存在，`register()` 目前返回 `()`（注册句柄按 ADR-0009 的反预拆分刹车推到 M5，见 §1）。今天的装配是硬编码在 daemon 里的 `SessionManager::chat_tools()`（crates/hatchery-daemon/src/manager.rs:612-628）：内联造 `LocalFs` + `NoTerminal`，循环 `hatchery_tools::chat_tools()`；而 `SessionManager::assemble`（manager.rs:523）**从不读 `session.mode`**，所以本节描述的「按模式装配」今天是**模式盲**的——code 会话拿到的是与 chat 完全相同的只读三件套 + `NoTerminal`。`Backends` 也要在这里扩出 `approval` 与 `checkpoint` 两个字段。

## 5. 安全硬门（不可覆盖，architecture.md 不变量 5）

- 路径门：`~/.ssh`、`~/.config/hatchery`（自身配置/密钥）、`.git/hooks`、`.env*` 等的写入永远需要显式审批，规则表不可 allow-always。
- 凭据门：工具**输出**在离开 backend 的那一刻检测高熵密钥形态并脱敏（`redact` 层，M2 Phase 4）——**脱敏发生在接缝处，不是入库时**，理由见下。
- 所有硬门有对应测试，且测试断言「项目级配置无法关闭它们」（借鉴 atomcode PRECEDENCE 测试）。

### 「规则不可 allow-always」怎么表达（不动 `RiskLevel`）

四条硬门路径里有两条（`.env*`、`.git/hooks`）在**工作区内**，所以风险级本身表达不了它们：`RiskLevel::is_hard_gate()`（crates/hatchery-protocol/src/approval.rs:34）只认 `WritesOutside`。**不新增枚举变体**——新增枚举值属协议 major bump。改用已有机制：`ApprovalRequest::once_only()` 已经用「不提供 always 选项」表达了「不可记忆」，路径门只需要能为工作区内的敏感路径**强制 `once_only`**。于是「规则表不可 allow-always」的准确含义是：这类请求**根本不提供 AllowAlways 这个选项**，而不是让人存了一条规则再在求值时拦下来。

### 凭据脱敏的位置：接缝，不是存储（更正）

本节此前写的是「脱敏入库」。那是错的——它破坏不变量 2（模型看到的 == 落库的）：入库内容若与实际发给模型的字节不同，e2e 的 `invariant_minimal_chat_replays_reasoning_byte_exact`（拿重建上下文与实际发出的请求体逐字节对比）就会失败。更正后的位置：

- **输出侧**：脱敏发生在**工具输出离开 backend 的那一刻**，于是模型、存储、事件三方看到同一份字节，不变量 2 不受影响。
- **参数侧**：**不在存储里脱敏**（同一个不变量 2 问题）。检出凭据 → **触发审批 / 告警**，交给人决定，而不是悄悄改写历史记录。

真正的防线不在脱敏动作本身，而在于**密钥根本到不了模型视野**：它在读取环节出门时就已经被脱敏了。

## 开放问题

1. glob/grep 走 FsBackend 还是允许只读工具直连本地 fs（ACP 委派场景下宿主 fs 可能只是单文件接口，无法高效 glob）——倾向：ACP 会话中 glob/grep 降级为 LocalFs 只读（宿主 cwd 内），**仍挂 M3**（实测 Zed 行为后定）。
2. shell 工具是否提供「unified exec」式会话复用（codex）——v1 每次 create/wait/release，**M5 评估**（roadmap 的顺延表已收）。注意这里说的 `release` 本身是 M2 Phase 4 才进 `TerminalHandle` 的（见 §1）；M5 要评估的只是「复用」那一层。
3. web_fetch 的 HTML→Markdown 管线选型（htmd / 自写）——**已升级为决策点 D17**，与 HTTP client 选型合并成一个决策，落 **M2 Phase 4**（理由与现状见 §3「依赖前提」）。不再是本文档能自己关掉的问题。
4. 编辑工具的格式（精确 old/new 字符串替换 vs diff/patch）——**已裁决：v1 用 old/new 精确替换**（各家共识），M2 Phase 2 按此实现 `edit`；观察误配率后再议 diff/patch。留在这里只为记录裁决，不再是开放项。
