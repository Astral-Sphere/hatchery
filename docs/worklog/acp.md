# 工作记录：ACP（hatchery-acp）

- 范围：ACP server（agent 侧，含 fs/terminal 委派）、ACP client（subagent 编排）
- 设计文档：[../design/acp.md](../design/acp.md)
- 相关 ADR：0004、0005
- 依赖：官方 crate `agent-client-protocol`（atomcode 锁 `=2.0.0`，含 unstable_protocol_v2/elication features——来源：references/atomcode Cargo.toml 查证）

## 当前状态

设计稿完成，未实现。方法映射表、委派降级矩阵、审批 option 映射已定初稿。

## 待办

- [ ] (M2) 后端绑定机制在本地工具下先行验证（为委派铺路，roadmap M2 项）
- [ ] (M3) server：initialize 能力协商（含 client fs/terminal 能力探测）、session/new|load|prompt|cancel、session/update 投影（含 agent_thought_chunk ← reasoning）
- [ ] (M3) AcpClientFs / AcpClientTerminal / AcpPermission 实现 + 降级矩阵测试
- [ ] (M3) minimal test client（模拟宿主）集成测试
- [ ] (M3) **Zed 真机实测**：编辑/终端/审批全链路；宿主行为差异记录于此
- [ ] (M3) client：spawn 外部 agent、subagent 工具、权限上浮策略、深度限制
- [ ] (M3) 自举测试（hatchery 编排 hatchery）
- [ ] (M3) `hatchery acp` 子命令（attach 常驻 daemon / --standalone 两态）

## 开放问题

见设计文档末尾 4 条（v2 draft 跟进、replay 裁剪、嵌套深度、宿主 fs 失败回退）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。用户特别指出 atomcode v1 缺 fs/terminal 委派且根因是单一 runtime 绑定本地工具（已在 references/atomcode/crates/atomcode-cli/src/acp/mod.rs L104/L575/L614 查证）——hatchery 以 capability seam + 会话级后端绑定解决（ADR-0004），此为项目核心差异点，验收以 Zed 真机为准。
