# ADR-0004: capability seam 支撑完整 ACP（含 fs/terminal 委派）

状态：accepted（2026-09-28）

## 背景

用户明确要求 v1 即完整支持 ACP，包括 atomcode 缺失的两个方向：宿主侧文件读写与宿主侧终端。证据：atomcode `crates/atomcode-cli/src/acp/mod.rs` L104 自述 v1 链无 `auth`/`fs`/`terminal`，L614 测试断言 client-side terminal 不得被声明——根因是其单一 runtime 直接绑定了本地 fs/shell 工具与审批链，无法按会话把执行委派给 ACP 宿主。

ACP 规范中，client（宿主，如 Zed）可声明 `fs.readTextFile` / `fs.writeTextFile` / `terminal` 能力；agent 应优先使用宿主的文件与终端（这样编辑走宿主的 buffer/权限体系、终端在宿主 UI 中可见），权限经 `session/request_permission` 询问。

## 决策

kernel 的工具层不直接操作外界，一切经 `hatchery-capabilities` 定义的 trait（借鉴 dsh capability seam 三角色模型）：

```rust
#[async_trait]
pub trait FsBackend: Send + Sync {
    async fn read_text_file(&self, path: PathBuf, line_range: Option<LineRange>) -> Result<String>;
    async fn write_text_file(&self, path: PathBuf, content: String) -> Result<()>;
    async fn metadata(&self, path: PathBuf) -> Result<FsMetadata>;
}

#[async_trait]
pub trait TerminalBackend: Send + Sync {
    async fn create(&self, opts: TerminalOpts) -> Result<TerminalHandle>; // run / output stream / wait_for_exit / release
}

#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome;
}
```

每个会话在创建时**绑定一组后端实现**：

| 会话来源 | Fs | Terminal | Approval |
|---|---|---|---|
| 本地（CLI/GTK/headless） | `LocalFs`（写入前打影子 Git 检查点） | `LocalPty`（portable-pty） | `DaemonApproval`（弹给前端 + 持久化规则） |
| ACP server（宿主声明 fs/terminal 能力） | `AcpClientFs`（转发 `fs/read_text_file`、`fs/write_text_file`） | `AcpClientTerminal`（转发 `terminal/create|output|wait_for_exit|release`） | `AcpPermission`（转发 `session/request_permission`） |
| ACP server（宿主未声明） | 回退 LocalFs/LocalPty + AcpPermission | 同左 | 同左 |

工具（read/edit/shell/…）只依赖 trait；`hatchery-tools` 提供工具逻辑，后端注入决定执行位置。CI 用 `clippy::disallowed_methods` 禁止工具代码直接调用 `std::fs`/`std::process`。

## 理由

1. ACP fs/terminal 委派从「架构级改造」降为「换绑定」，正是 atomcode 被卡住的点。
2. 同一接缝免费获得：docker/ssh 远程执行后端、测试用内存后端（kernel 测试无需真实 fs）。
3. 影子 Git 检查点挂在 `LocalFs` 写入路径上，ACP 委派场景由宿主管编辑历史，职责清晰。

## 替代方案（已否）

- atomcode 式单 runtime 绑定本地工具 + ACP 只做子集：不满足需求。
- 每工具各自处理委派分支：委派逻辑散落，必然漏。

## 后果

- trait 边界的 async 生命周期与取消语义要设计好（`TerminalHandle` 的 output 流、release 时机）。
- ACP 会话的工具行为受宿主能力制约（如宿主终端不支持 resize），需要能力探测与降级路径。
- 审批语义映射（hatchery ApprovalRequest ↔ ACP permission options）需要一张明确的翻译表（design/acp.md）。
