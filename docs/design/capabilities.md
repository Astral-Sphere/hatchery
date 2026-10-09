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
    // 检查点收集器**没有**进这里（原草图打算加）：工具不需要它，而本节的原则是「没有字段就没有能力」。
    // 收集器由 kernel 造、借给 ToolHost::invoke，注册表在包装 fs 时把它交给装饰器；见 §2「检查点如何成为 item」
}

// 路径一律 &str 且工作区相对（不是 PathBuf）：根由 backend 拥有，「越界」的定义也只由 backend 给
pub trait FsBackend {
    async fn read_text_file(&self, path: &str) -> Result<String, FsError>;
    async fn read_dir(&self, path: &str) -> Result<Vec<FsEntry>, FsError>;
    async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError>;
    // M2 写原语（已落地）：只有 write_text_file，它自己建缺失的父目录
    async fn write_text_file(&self, path: &str, contents: &str) -> Result<(), FsError>;
    // create_dir / remove **没有加**，草图给它们指名的消费者都不存在：
    //   write_file 要的父目录由 write_text_file 建；rewind 的 purge 是 CheckpointStore::restore 里的
    //   checkout_index(remove_untracked)，从不经过接缝。无消费者的原语 = 无人跑过的死代码（ADR-0009）
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
pub struct Backends {
    pub fs,                                   // Phase 1 起是 CheckpointedFs 的内层
    pub terminal,
    pub checkpointer: Option<Arc<dyn Checkpointer>>,  // Phase 1 已加；None = Chat，没有写就没有 undo 点
    // M2 Phase 2 再加 approval
}
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

**`release()` 的含义要在 Phase 4 之前定下来**（PTY 探针实测，`spikes/pty/`）。上面那段把 `release()` 与 `kill()` 并列，读起来像是一对可选的收尾动作，而在 PTY backend 上它不可能是「把活着的进程交还给调用方」：实测关掉 master/slave 之后 **500ms 子进程仍然活着**，也仍然没有任何人能再观察或停止它。所以 PTY 上的 `release()` 只有两种诚实的读法——要么它就是 `kill()` 的别名，要么它交回的是一个**没人再看管**的进程（对 `unified_exec` 式的会话复用是唯一合理的读法：所有权转移给注册表，而不是消失）。两种都能实现，但语义必须写进 trait 文档，否则第一个实现者会随手选一个。

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

**Phase 1 已落地**（`src/checkpoint.rs` + `src/checkpointed_fs.rs`）。下面是落地后的形状；草图里被实现推翻的部分在每条后面写明。

```rust
pub struct CheckpointStore { /* 一个工作区的影子仓库；每次操作开一个新句柄 + 内部 tokio Mutex 串行化 */ }
pub struct CheckpointPool  { /* 根目录 + 选项 + per-workspace 缓存；daemon 持一个，两个会话共享一个仓库 */ }

pub struct CheckpointOptions { pub ignore_rules: Vec<String>, pub max_file_bytes: u64 }
pub struct SnapshotReport { pub checkpoint: Checkpoint, pub oversized: Vec<String> }

pub struct RestoreOptions {
    /// 删除「快照之后才出现、且从未被任何快照跟踪」的文件。
    /// 默认 false：rewind 永不删用户自己的未跟踪文件。开启需要审批 + 二次确认 + 先列出待删清单。
    pub purge_untracked: bool,
}
pub struct RestoreReport { pub safety: SnapshotReport, pub rolled_back: Vec<PathBuf>, pub purged: Vec<PathBuf> }

impl CheckpointStore {
    pub fn open(workspace: &Path, git_dir: &Path, options: &CheckpointOptions) -> Result<Arc<Self>>;
    pub fn recorded_workspace(git_dir: &Path) -> Option<PathBuf>;  // 孤儿判定唯一可行的入口
    pub async fn snapshot(&self, kind: CheckpointKind) -> Result<SnapshotReport>;
    pub async fn diff(&self, from: &str, to: &str) -> Result<Diff>;
    pub async fn restore(&self, to: &str, options: RestoreOptions) -> Result<RestoreReport>;
    pub async fn untracked(&self) -> Result<Vec<PathBuf>>;  // purge 的待删清单，审批要它
    pub async fn bytes(&self) -> u64;                        // 预算量的就是这个
    pub async fn count(&self) -> Result<u64>;                // 从 HEAD 可达的快照数
    pub async fn destroy(&self) -> Result<()>;               // 唯一的回收原语，只对孤儿 sound
}
```

与草图的四处出入，每处都有理由：

- **没有 `init(workspace)`**，git-dir 由调用方给：`<data>/checkpoints/<uuid5(NAMESPACE, workspace)>` 由 `CheckpointPool::git_dir_for` 算。目录名用 uuid v5（`uuid` 已有的 crate 加 `v5` feature，只多 `sha1_smol` 一个依赖）而不是自己写哈希；仓库 config 里另记一个 `hatchery.workspace` 标记，`open` 每次校验，所以名字碰撞或数据目录被搬走都是**报错**而不是悄悄检查点错树。
- **没有 `gc(budget)`**：预算熔断需要 `checkpoints` 表，而那张表在同层的另一个 crate（L2 之间禁止横向依赖），所以**策略住在 daemon**（`daemon/src/checkpoints.rs`），本 crate 只提供机制。libgit2 也**没有对象级 GC**（`Repository` 只有 `odb()` 读写与 `cleanup_state()`），所以「回收」只有整个仓库这一种粒度——见 D9 那段。
- **`snapshot` 的签名里没有 `item: ItemId`**：item 是 kernel 在工具返回**之后**才造的，打点那一刻没有 id 可给。关联由 `checkpoints` 行（`item_id`）与 Checkpoint item 自己的载荷（`commit_id`）承担，两者都比 commit message 里塞一个字符串强。
- **commit id 是 `String` 不是新类型 `CommitId`**：protocol 的 `Checkpoint.commit_id` 已经是 `String`，再引入一个同值类型只会在边界上添一次可失败的转换。

`Diff` **已在 Phase 1 落地**（protocol 的 `Diff/DiffFile/DiffStatus/DiffHunk/DiffLine/DiffLineKind`），草图那个暂定名 `UnifiedDiff` 没有采用：载荷是**结构化 hunk**，不是 unified 文本。理由是 D11 已经裁决 TUI 用 `similar` 算 hunk、git2 侧也原生产出 hunk，两个生产者喂同一个类型，两个前端都不必写 unified-diff 解析器（codex 走文本那条路，为此有 2745 行 `tui/src/diff_render.rs`）。注意 `ApprovalRequest` 仍装不下 diff：它只有 `args_digest: String`，其文档明写不是原始 JSON；审批要预览 diff 得走 D14 的结构化 preview 字段——那时 preview 的类型就直接是这个 `Diff`。

**目前没有 golden fixture 钉 `Diff`，这是对的**：fixture 注册表只覆盖三类东西（`ItemKind` 载荷、方法结果、事件），`Diff` 一类都不是。第一个返回它的方法（Phase 3 的 rewind，或 `checkpoint_diff` 工具）落地时补上。serde 拼写由 protocol 内的单测钉住。

另有一处 diff **仍没有来源**：`git2` 出的是检查点之间（commit → commit）的 diff，而 `write_file`/`edit` 的**预览** diff 是「旧缓冲 vs 新内容」、此时还没有 commit。这需要独立的 diff 能力（D11 的证据：atomcode 用 `similar = "2"`，references/atomcode/Cargo.toml:40），**目前仍不在任何依赖表里**——Phase 2 的 `edit` 预览或 Phase 6 的 diff 视图谁先到谁引入，产出同一个 `Diff` 类型。

### D9：预算熔断（用户裁决 2026-10-08）

裁决是「GC 最旧，仍超则跳过打点」。前半句只能按**整个影子仓库**的粒度兑现，原因是两条既成事实而不是一句「太麻烦」：

1. libgit2 没有对象级 GC，回收字节的唯一办法是重建仓库；
2. 重建就要重提交幸存者，而**重提交的 commit id 会变**——`ItemKind::Checkpoint.commit_id` 已经写进 append-only 的 `items` 表（`items_no_update` 触发器拒绝那次修正），所以任何改写历史的 GC 都会让既有 item 指向不存在的 commit。

于是 daemon 侧的阶梯是（`daemon/src/checkpoints.rs`，全部有测试）：

| 情况 | 动作 | 为什么 |
|---|---|---|
| 全局预算超了 | 先扫孤儿；仍超 → **跳过打点 + 告警** | 全局那条保护的是**用户的磁盘**，不是我们的索引 |
| 本工作区超预算、快照数 >1 | **删库重来** + 删该工作区的行 | 保住*将来*的可回滚；只跳过会让工作区因为一个配置数字永久失去 undo |
| 本工作区超预算、单个快照就超 | **跳过打点 + 告警** | 删库重来只会腾出地方装同一个快照，每次写都重复一遍 |
| git/IO 真故障 | **拒绝写入**（`FsError::Checkpoint`，不是 `Io`） | 拿不到 undo 点的 agent 写入比不写更糟；错误变体分开，模型才听得见「缺的是检查点」 |

「删库重来」的代价是诚实且有界的：指向被丢弃 commit 的 rewind 报 `UnknownCommit`，测试钉住了这句话。ADR-0006 要的「熔断可配置」落成 `[checkpoints]` 四个键：`workspace_budget_mb`（500）、`global_budget_mb`（2048）、`max_file_mb`（10）、`ignore_rules`（换行分隔的 gitignore 语法；用字符串不用数组，因为 `config/set` 的标量机器已经在，而 gitignore 本来就是按行写的）。`_mb` 一律按 MiB——ADR-0006 原文混用了 500MB 与 2GiB。

### 孤儿影子仓库（storage 开放问题 3，已关闭）

删会话会级联删掉它的 `checkpoints` 行，而**没有任何东西会回头看那些行指向的目录**，所以孤儿只能在清扫时被注意到：启动扫一次 + 每次预算检查时扫。判定 = `CheckpointStore::recorded_workspace(dir)` 反查归属 + `WHERE workspace = X` 无行。**认不出属于谁的一律保留**（「cannot tell」绝不能等于「delete」）。

这里有一个会**删掉用户数据**的坑，已在实现里堵掉并有测试：影子仓库记录的是**规范化后**的工作区路径，而 HubSink 写行用的是会话里的原始拼写。两者不一致时，清扫会认为一个活着的仓库没有主。修法是把拼写规则收成一个函数 `recorded_workspace()`，两处写入方（sink 与 checkpointer）都用它。


实现纪律（后端 = **git2 / vendored libgit2**，ADR-0012；下列结论全部来自 `tests/spike_shadow_git.rs` 的门槛实测——该套件常驻，升级 git2 必须重跑）：

- **不依赖用户的 git 二进制**（这是选它的首要理由：很多用户机器上没有 git）。构建期需要一个 C 编译器——libgit2-sys 用 `cc` 编译 vendored libgit2 与 pcre2，**不需要 cmake**（实测 build.rs 未调用它）。daemon 启动审计因此不检查 git，但仍要检查数据目录可写。
- **打开影子仓库的固定配方**（`CheckpointStore::open`）：
  1. 首次创建：`Repository::init_opts(git_dir, opts)`，opts = `no_dotgit_dir(true)` + `bare(true)` + `external_template(false)`（不读开发者机器上的模板目录）。
  2. 自己写 config：`core.worktree = <用户工作区>`、`core.bare = false`。**必须手写**——libgit2 的 `set_workdir(path, update_gitlink=false)` 只改内存句柄；而 `update_gitlink=true` 会在用户工作区里种一个 `.git` gitlink 文件（`repository.c:3259`，读源码 + 实测双确认）。
  3. `set_workdir(<用户工作区>, false)` 让当前句柄生效；之后重新 `Repository::open(git_dir)` 靠第 2 步的 config 恢复 work tree。
  4. 钉住配置以隔绝开发者机器：`core.autocrlf=false`、`core.excludesFile=<不存在的路径>`（否则用户的全局 excludes 会悄悄缩小快照范围）、`core.fsmonitor=false`、`user.name/email`。测试里的 `harden()` 就是这份清单，实现照抄。
- **不变量 6 已实测**：快照 + 恢复全程，用户仓库的 HEAD、分支、refs、`.git/index` mtime、`.git` 目录条目全部不变，且工作区里不会出现 `.git`（`invariant_shadow_git_never_touches_user_repo`、`invariant_no_gitlink_is_planted_in_the_user_workspace`）。**Phase 1 把这三条 `invariant_` 测试从 spike 的私有 `Sandbox` 迁到了 `tests/checkpoint.rs`，跑真的 `CheckpointStore`**（名字未变；spike 只留 libgit2 后端事实）。启动断言落在 `open()`：`assert_disjoint` 拒四种重叠——影子 git-dir 就是用户 `.git`、在它内部、在工作区内部（那会把自己快照进去并每次变大）、工作区在它内部；另外每次打开都校验仓库自己记的 `hatchery.workspace`，所以「拿别人的 git dir」也是报错。
- **读用户仓库是安全的**：libgit2 的 `statuses()` 实测**不会**重写用户的 `.git/index`（CLI 的 `git status` 会）。prompt 的 environment 节（platform.md §2.1）因此可以直接读分支/脏状态，不再需要「禁用 status」那类脆弱纪律。
- **忽略规则是 per-handle 的**：`add_ignore_rule` 只作用于当前 `Repository` 句柄（实测：临时句柄上加的规则对下一次打开无效）。`CheckpointStore` 每次打开都要按配置重放规则；`.git/` 恒在规则里；大文件（默认 >10MB）走 `max_file_bytes` 排除、由 `Index::add_all` 的回调逐个 stat 决定，**排除不等于删除**（实测文件本身保留），被排除的路径通过 `SnapshotReport.oversized` 回报并由 daemon 记日志——ADR-0006 那句「跳过快照并记录」的「记录」就是这个。
- **影子仓库尊重工作区自己的 `.gitignore`（Phase 1 实测，非显然）**：即便它是 bare + 外部 work tree + `core.excludesFile` 指向一个不存在的路径，libgit2 仍会读 work tree 里的 `.gitignore`。所以 git 工作区的 `target/` 天然不入快照，**不需要我们自造一份构建产物排除表**；`checkpoints.ignore_rules` 是给非 git 工作区用的。测试 `a_workspace_gitignore_is_honoured_by_the_shadow_repository` 两面都钉（有 `.gitignore` 与没有）。
- **restore 语义（实测）**：`reset(Hard)` 会回滚已跟踪文件、并删除「被后续快照跟踪过的新文件」，但**不会**删除从未被任何快照跟踪的文件；给它传 `remove_untracked` 也无效（hard reset 的 checkout 只覆盖与目标有差异的路径）。所以 `purge_untracked=true` 要额外走一遍 `checkout_index(None, force().remove_untracked(true))`，且**不加 `remove_ignored`**（否则会删构建产物与用户的 `.env`）。默认 `false`：rewind 永不删用户自己的未跟踪文件；开启需审批 + 二次确认 + 先列出待删清单——那份清单就是 `untracked()`，它读的是 `include_ignored=false` 的 status 列表，所以 `.git/` 与构建产物**结构上**就不可能出现在里面（有测试）。
- **restore 前的安全快照有两条反直觉的约束**（Phase 1 实测，两条都是测试跑红才发现的）：它**不能移动 HEAD**，也**不能留下已 stage 的 index**。原因是 `reset(Hard)` 按 index 决定删什么——安全快照把整个工作区 stage 进去之后，「从未被任何快照跟踪」的用户文件看起来就是已跟踪的，于是每次 rewind 都变成一次 purge，正是上一条默认 `false` 要防的事。实现：`commit_snapshot(.., update_head: false)`（提交对象不挂 ref，靠对象库 + `checkpoints` 行仍可寻址；libgit2 不做对象 GC，所以它不会自己消失）+ 快照后把 index 读回 HEAD 的树。变异验证过。
- 用户工作区不是 git 仓库时一切照常（实测通过）——影子仓库不依赖用户仓库存在。
- **`delta.flags()` 的 `BINARY` 位只在 `Patch::from_diff` 之后才有**（Phase 1 实测：之前是 `DiffFlags(0x0)`，之后是 `DiffFlags(BINARY)`）。libgit2 要加载内容才判定二进制，所以 diff 的实现必须**先建 patch 再读 flags**；顺序反了会把每个二进制文件报成「文本文件、零 hunk」。
- 预算核算：`revwalk` 数快照数（**从 HEAD 可达的**——restore 之后目标之后的那些仍在对象库里、仍被 item 指着，但已不在这条链上，所以预算量的是 `bytes()` 而不是 `count()`），影子 git-dir 占用直接遍历目录求和（实测 5 次快照仅 3457 B，因为 libgit2 不铺 hooks/模板）。
- 性能（实测，Linux x86_64 / 24 核）：500 文件冷快照 48.9 ms、10 处改动的热快照 6.0 ms、硬恢复 2.8 ms。热路径（每次写前打点）是常态，比 CLI 后端快约 2 倍。**48.9 ms 足以停住一个 runtime worker**，所以 `CheckpointStore` 的每个操作都经 `spawn_blocking`——与 `LocalFs` 用 `tokio::fs` 是同一个理由。
- **树未变则不新建提交**：`snapshot` 发现新树与 HEAD 的树相同就复用 HEAD 的 commit id。写同样两遍字节不是一个新状态，而写循环否则会堆出大量空提交，让每个预算计数都得先解释一遍。

### 检查点如何成为 item（D13，Phase 1 落地）

链路：**kernel 造一个 `CheckpointCollector` 借给 `ToolHost::invoke`** → 注册表把 `Backends.fs` 包进 `CheckpointedFs`（装饰器），它在每次写之前 `Checkpointer::pre_write()` 并 push 进收集器 → **kernel 在 ToolResult item 之前追加 Checkpoint item** → daemon 的 HubSink 在 Checkpoint item 落库之后补写 `checkpoints` 行。

三个设计理由（原样成立）：

- 追加位置由 kernel 负责而不是 daemon：kernel 本来就在造 ToolCall/ToolResult item，用同一套机器，顺序天然正确，链变成 `… → ToolCall → Checkpoint → ToolResult`。
- 这个顺序**安全**，因为工具结果靠 `ToolResult.call` 与调用配对，**不靠父子关系**——在两者之间插 item 不会拆散配对。
- `checkpoints` 行写失败**只记日志**：commit_id 已由 item 自己携带（`ItemKind::Checkpoint { commit_id, kind }`），行只是索引，丢了可以从 item 重建。

**两处与草图不同**，都是实现时发现的：

1. **收集器不在 `ToolCtx` 上，而在 `ToolHost::invoke` 的参数里**。原写法「`ToolInvocation` 把 `Vec<Checkpoint>` 带出工具调用」在**取消路径上会丢掉它们**：kernel 的工具 select 是 cancel-first，被中断的 invocation 直接 drop 且不再被 poll（这条事实本来就写在 testkit 假 host 的注释里，它为此专门写了 drop guard）。后果是具体的——取消的 `write_file` 留下半截文件而没有任何 item 指向它的 undo 点，Code rewind 向后扫会跳过它、恢复出**包含损坏**的状态。借进去的收集器由 kernel 拥有，所以完成/失败/中断三条出口都能 drain。工具也不需要看见它：`ToolCtx` 的原则是「没有字段就没有能力」。
2. **打点是 `FsBackend` 的装饰器（`CheckpointedFs`），不是 `LocalFs` 的字段**。三条理由：local backend 保持「只是个文件系统」；收集器天然按**调用**划分而不是按会话，两个调用永不混检查点；任何 backend 都能被包住，包括 `MemoryFs`——testing.md 要的「写序列 vs 检查点记录对齐」因此有得测。接缝外看，行为与「`LocalFs` 写前打检查点」一模一样。

`Checkpointer` 是 trait 而不是直接用 `CheckpointStore`：「要不要打点」是需要 `checkpoints` 表的策略（D9 的预算阶梯），而那张表在同层的另一个 crate 里，所以 trait 的实现住在 daemon，capabilities 只认接口。它有两个成功分支——`Taken(checkpoint)` 与 `Skipped { reason }`——因为「没有检查点」有时是**决定**而不是失败（D9 的第③档），把两者混成一个 `Result::Err` 会让「跳过但照样写」表达不出来。

**写路径自己需要一条解析规则**：`resolve` 靠 `canonicalize`，对「还不存在的文件」必然失败。`resolve_write` 锚定**最近的可解析祖先**再往下拼，所以 `create_dir_all` 不会在符号链接祖先的另一侧建目录（`link/` 指向 `/etc` 时写 `link/deeper/x` 会先建 `/etc/deeper`）；目标本身若已存在则整体 canonicalize，**悬空符号链接被拒**——穿过去写会在链接目标处创建文件，落在工作区外、也落在所有检查点之外。两条都有测试。

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
