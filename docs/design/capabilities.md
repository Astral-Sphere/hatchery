# 设计：能力接缝、工具、审批与回滚（hatchery-capabilities / hatchery-tools）

> 状态：设计稿。依据 ADR-0004（capability seam）、ADR-0005（模式）、ADR-0006（影子 Git）。

## 1. hatchery-capabilities：接缝定义

三个核心 trait（完整签名见 ADR-0004）+ 检查点与工具注册表框架：

```rust
// 工具契约住在这一层：kernel 只见 ToolHost 窄接口（kernel.md §5），否则 L0 会反过来依赖 L1 成环
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;                       // ToolDef 是 kernel 类型（要进 LLM 请求）
    fn needs_approval(&self, args: &Value) -> Option<ApprovalRequest>;
    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput>;
}

pub struct ToolCtx<'a> {          // 工具能拿到的全部外界能力 = 接缝
    pub fs: &'a dyn FsBackend,
    pub terminal: &'a dyn TerminalBackend,
    pub cancel: CancellationToken,
    pub emit: &'a dyn Fn(ToolProgress),   // 进度上报 → ToolCallProgress 事件
}

pub trait FsBackend      // read_text_file / write_text_file / metadata（+M2: list/glob 支持）
pub trait TerminalBackend // create → TerminalHandle { output stream, wait_for_exit, kill, release }
pub trait ApprovalGate   // request(ApprovalRequest) → ApprovalOutcome

// ApprovalRequest / ApprovalOutcome / ToolOutput / ToolProgress 定义在 kernel（M0a 修正）：
// kernel 的 ToolHost::approval_for 与事件投影都要用它们，capabilities 只消费与构造。
pub struct ApprovalRequest {
    pub tool: String,
    pub args_digest: String,          // 人类可读摘要（如 "edit src/main.rs (+12 -3)"）
    pub risk: RiskLevel,              // ReadOnly | WritesWorkspace | WritesOutside | Executes | Network
    pub options: Vec<ApprovalOption>, // AllowOnce | AllowAlways | Deny | DenyAlways（→ 持久化规则）
}

pub struct ToolRegistry { /* name → Arc<dyn Tool>；实现 kernel::ToolHost，turn 开始冻结快照 */ }
```

**注册句柄模式**（ADR-0009 纪律 3，借鉴 dsh `registerAdapter()` → handle）：`register()` 返回 `RegistrationHandle { dispose(), replace() }`——`replace()` 用新实现整表原子替换旧实现（进行中的 turn 不受影响，因为 kernel 持有的是冻结快照），`dispose()` 摘除注册。MCP 工具、用户自定义工具、运行中换 provider adapter 全部走这一模式；禁止对注册表的原地突变。

### 本地实现（本 crate 提供）

- `LocalFs`：tokio::fs + 路径校验（工作区逃逸检查、危险路径硬门）+ **写前打影子 Git 检查点**。
- `LocalPty`：`portable-pty` 实现 TerminalBackend；PTY 会话注册表（后台进程、复用、超时杀进程）；输出环形缓冲（截断保护，借鉴 codex unified_exec）。
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
    pub fn diff(&self, from: CommitId, to: CommitId) -> Result<UnifiedDiff>;   // GUI diff 视图
    pub fn restore(&self, to: CommitId, opts: RestoreOptions) -> Result<RestoreReport>;
    pub fn gc(&self, budget: Budget) -> Result<u64>;     // 预算熔断（ADR-0006）
}
```

实现纪律（后端 = **git2 / vendored libgit2**，ADR-0012；下列结论全部来自 `tests/spike_shadow_git.rs` 的 11 项实测）：

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

## 3. hatchery-tools：内置工具

工具 = 纯逻辑 + 接缝调用，禁止直接 `std::fs`/`std::process`（CI deny lint）。

| 工具 | 模式 | 接缝 | 审批默认 |
|---|---|---|---|
| `read_file` | Chat+Code | FsBackend | 无需（工作区内）；工作区外 → Executes 级审批 |
| `glob` / `grep` | Chat+Code | FsBackend（M2 加 list 接口）| 无需 |
| `write_file` / `edit` | Code | FsBackend（写前检查点） | WritesWorkspace，规则可持久化 |
| `shell` | Code | TerminalBackend | Executes；命令摘要展示；危险模式（rm -rf、sudo、curl\|sh）→ DenyAlways 提示 |
| `web_fetch` | Chat+Code | 内置 http client（唯一例外的直连，Network 风险级） | 首次审批，可持久化域名规则 |
| `checkpoint_diff` / `rewind` | Code | CheckpointStore | 无需（只读）/ 需确认（restore）；`--purge` 额外审批 + 待删清单 |
| `subagent`（M3） | Code | ACP client | 继承父会话策略 |
| MCP 工具（M5） | 按配置 | rmcp client | 默认 Executes 级审批 |
| WASM 插件工具（M5 评估占位，ADR-0009） | 按配置 | wasmtime + WASI（沙箱化） | 默认 Executes 级审批；实施前须过新 ADR |

工具输出统一 `ToolOutput { text, artifacts?, spilled? }`；超阈值 spill 到 `~/.local/state/hatchery/tool-results/`，库存引用。

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

## 5. 安全硬门（不可覆盖，architecture.md 不变量 5）

- 路径门：`~/.ssh`、`~/.config/hatchery`（自身配置/密钥）、`.git/hooks`、`.env*` 等的写入永远需要显式审批，规则表不可 allow-always。
- 凭据门：工具参数/输出中检测到高熵密钥形态时脱敏入库（`redact` 层，M2）。
- 所有硬门有对应测试，且测试断言「项目级配置无法关闭它们」（借鉴 atomcode PRECEDENCE 测试）。

## 开放问题

1. glob/grep 走 FsBackend 还是允许只读工具直连本地 fs（ACP 委派场景下宿主 fs 可能只是单文件接口，无法高效 glob）——倾向：ACP 会话中 glob/grep 降级为 LocalFs 只读（宿主 cwd 内），M3 实测 Zed 行为后定。
2. shell 工具是否提供「unified exec」式会话复用（codex）——v1 每次 create/wait/release，M5 评估。
3. web_fetch 的 HTML→Markdown 管线选型（htmd / 自写）——M2。
4. 编辑工具的格式（精确 old/new 字符串替换 vs diff/patch）——v1 old/new 替换（各家共识），观察误配率。
