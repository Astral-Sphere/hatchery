# 工作记录：能力接缝 / 工具 / 审批 / 回滚（hatchery-capabilities, hatchery-tools）

- 范围：Fs/Terminal/Approval trait 与本地实现、影子 Git、内置工具、安全硬门、模式装配
- 设计文档：[../design/capabilities.md](../design/capabilities.md)
- 相关 ADR：0004、0005、0006

## 当前状态

设计稿完成，未实现。trait 草图与工具清单已定。

## 待办

- [ ] (M0) **git spike（实测）**：CLI `git --git-dir --work-tree` 全操作集（init/snapshot/diff/restore/大文件排除/预算 GC）性能与边界；对比 git2；结论写回本文件并更新 ADR-0006 后果节
- [ ] (M0) trait 定型（FsBackend/TerminalBackend/ApprovalGate/TerminalHandle 取消与流语义）
- [ ] (M1) LocalFs 只读路径 + read_file/glob/grep 工具（Chat 模式用）
- [ ] (M2) LocalFs 写路径 + CheckpointStore + write/edit 工具
- [ ] (M2) LocalPty + shell 工具（输出环形缓冲、超时杀、危险命令模式表）
- [ ] (M2) DaemonApproval + approval_rules 持久化 + 硬门测试（断言项目配置不可关闭）
- [ ] (M2) web_fetch + spill + 凭据脱敏
- [ ] (M2) disallowed_methods lint 配置（工具代码禁 std::fs/std::process）
- [ ] (M3) 与 AcpClientFs/AcpClientTerminal 的绑定矩阵联测

## 开放问题

见设计文档末尾 4 条（glob/grep 在 ACP 会话的降级、unified exec、HTML→MD 选型、edit 格式）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。核心设计输入：dsh capability seam 三角色模型 + atomcode 影子 Git 实现细节（预算熔断、RAII 补偿）+ atomcode ACP 缺 fs/terminal 的反面教材（ADR-0004）。
