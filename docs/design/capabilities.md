# 设计：能力接缝、工具、审批与回滚（hatchery-capabilities / hatchery-tools）

> 状态：设计稿。依据 ADR-0004（capability seam）、ADR-0005（模式）、ADR-0006（影子 Git）。

## 1. hatchery-capabilities：接缝定义

三个核心 trait（完整签名见 ADR-0004）+ 检查点与工具注册表框架：

```rust
pub trait FsBackend      // read_text_file / write_text_file / metadata（+M2: list/glob 支持）
pub trait TerminalBackend // create → TerminalHandle { output stream, wait_for_exit, kill, release }
pub trait ApprovalGate   // request(ApprovalRequest) → ApprovalOutcome

pub struct ApprovalRequest {
    pub tool: String,
    pub args_digest: String,          // 人类可读摘要（如 "edit src/main.rs (+12 -3)"）
    pub risk: RiskLevel,              // ReadOnly | WritesWorkspace | WritesOutside | Executes | Network
    pub options: Vec<ApprovalOption>, // AllowOnce | AllowAlways | Deny | DenyAlways（→ 持久化规则）
}

pub struct ToolRegistry { /* name → Arc<dyn Tool>；turn 开始冻结快照（kernel.md §5） */ }
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

impl CheckpointStore {
    pub fn init(workspace: &Path) -> Result<Self>;       // git-dir: ~/.local/share/hatchery/checkpoints/<workspace-hash>
    pub fn snapshot(&self, kind: CheckpointKind, item: ItemId) -> Result<CommitId>;
    pub fn diff(&self, from: CommitId, to: CommitId) -> Result<UnifiedDiff>;   // GUI diff 视图
    pub fn restore(&self, to: CommitId) -> Result<()>;   // work-tree 恢复到检查点
    pub fn gc(&self, budget: Budget) -> Result<u64>;     // 预算熔断（ADR-0006）
}
```

实现纪律：
- 所有 git 调用显式 `--git-dir=<影子> --work-tree=<用户工作区>`；启动时断言影子 git-dir ≠ 用户任何 `.git`（含向上查找），测试覆盖。
- 快照范围默认全工作区，排除项可配（`.gitignore` 语义 + hatchery 自己的 exclude 文件）；单文件超过阈值（默认 10MB）跳过并记录。
- restore 前先 snapshot 当前状态（回滚也可回滚）。
- git 后端选型：优先 CLI `git`（行为可预期、无 libgit2 绑定风险）；`git2` 仅在性能证明必要时引入——M0 spike 决定（worklog/capabilities.md）。

## 3. hatchery-tools：内置工具

工具 = 纯逻辑 + 接缝调用，禁止直接 `std::fs`/`std::process`（CI deny lint）。

| 工具 | 模式 | 接缝 | 审批默认 |
|---|---|---|---|
| `read_file` | Chat+Code | FsBackend | 无需（工作区内）；工作区外 → Executes 级审批 |
| `glob` / `grep` | Chat+Code | FsBackend（M2 加 list 接口）| 无需 |
| `write_file` / `edit` | Code | FsBackend（写前检查点） | WritesWorkspace，规则可持久化 |
| `shell` | Code | TerminalBackend | Executes；命令摘要展示；危险模式（rm -rf、sudo、curl\|sh）→ DenyAlways 提示 |
| `web_fetch` | Chat+Code | 内置 http client（唯一例外的直连，Network 风险级） | 首次审批，可持久化域名规则 |
| `checkpoint_diff` / `rewind` | Code | CheckpointStore | 无需（只读）/ 需确认（restore） |
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
