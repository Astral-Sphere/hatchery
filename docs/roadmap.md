# 路线图（M0–M5）

> 每个里程碑的完成定义（DoD）都包含：`cargo test`/`clippy` 全绿 + 列出的端到端验证 + worklog 更新。测试分层、CI 门禁与「不变量 → 测试」映射见 [design/testing.md](design/testing.md)；各里程碑的测试交付物已列入其范围与 [worklog/testing.md](worklog/testing.md)。当前处于 **M0 之前**：文档定稿，等待 M0 细化规划。

## M0 — 地基（scaffold + 核心类型）

**范围**
- Cargo workspace 脚手架：12 个 workspace member（11 个 crate = 10 个产品 crate + dev-only 的 `hatchery-testkit`，外加 `xtask`；见 architecture.md §3）+ CI PR 门禁全套（testing.md §8）+ nextest 分组。
- `hatchery-protocol`：Thread/Turn/Item/事件/方法的完整类型定义 + JSON fixture 测试。
- `hatchery-kernel`：Turn 状态机 + trait 定义 + fake provider 单测。
- `hatchery-store`：schema v1 + writer actor + rebuild_history + 分支操作（分叉/切换/级联删）+ 属性测试。
- spike（各半天，结论写进对应 worklog）：**三个全部完成**——存储引擎 → turso 0.7.2（ADR-0010，12 项门槛测试常驻）；影子 Git 后端 → git2 vendored（ADR-0012，11 项门槛测试常驻）；i18n → fluent（ADR-0011）。
- 本文档体系随代码入库。

**DoD**：`./scripts/ci.sh` 三平台全绿；store 的 kill -9 崩溃恢复测试通过；三个 spike 结论落档。

## M1 — 最小对话闭环（Chat 模式端到端）

**范围**
- `hatchery-llm`：ChatCompletions adapter（deepseek/qwen 两家真实探测录制 fixture）、effort 映射表 v1、reasoning 采集/回放。
- `hatchery-daemon`：UDS/stdio 监听、attach-or-spawn、单会话 runtime、live hub（无 coalescing 优化）、generation。
- `hatchery-cli`：TUI 最小版（消息流 + 输入 + reasoning 折叠 + `/effort` `/model` `/prompt`）+ headless exec。
- Chat 模式：只读工具 `read_file`/`glob`/`grep`（LocalFs 直读）。
- 配置分层（platform.md §1）+ prompt 管线 v1（identity + mode_variant + environment + safety_gate）。
- `hatchery doctor`。

**DoD**：真实 provider 端到端对话（流式 + reasoning 展示与回放命中验证）；关终端重开会话 resume；两前端同时 attach 扇出一致。

## M2 — Code 模式（工具、审批、回滚、编辑分叉）

**范围**
- capabilities 本地实现全量：LocalFs（写前检查点）、LocalPty、DaemonApproval、CheckpointStore（影子 Git + 预算熔断）。
- 工具：write/edit/shell/web_fetch + spill + 凭据脱敏。
- 会话级：edit_item 分叉、branch switch/delete、rewind（三 scope）、模式切换。
- ACP fs/terminal 委派所需的后端绑定机制（不含 ACP 协议本身）——接缝在真实工具下验证。
- GUI 不开工；TUI 审批 UI + diff 渲染 + `/rewind` `/branch`。

**DoD**：Code 会话改坏文件后 rewind 恢复；编辑历史消息分叉重演；审批规则持久化生效；硬门测试全绿。

## M3 — ACP server + client

**范围**
- `hatchery-acp`：server 全量（design/acp.md §1，含 fs/terminal 委派、审批映射、session modes、load replay、config options）；client + `subagent` 工具（§2）。
- `hatchery acp` 子命令（attach 与 standalone 两态）。

**DoD**：Zed 真机全链路（编辑/终端/审批/thought chunk）；自举测试（hatchery 把 hatchery 当 subagent）；能力降级矩阵测试全绿。

## M4 — GTK 桌面端

**范围**
- `hatchery-gui`：frontends.md §3 全部（会话列表、消息流、审批弹层、设置窗、分支时间线、rewind 面板、prompt 查看器）。
- i18n 落地：po 工具链、zh/en 两语、RTL 冒烟。
- flatpak 打包。

**DoD**：GUI 完成一次完整 Code 会话（含审批与 rewind）；10k items 会话滚动流畅；`GTK_TEXT_DIR=rtl` 冒烟通过。

## M5 — 生态与打磨

**范围**：MCP client（rmcp，会话级配置透传兑现）、上下文压缩（compaction item + side-query 摘要）、沙箱（landlock/bwrap，接口已留）、自定义模式开放、JSONL 导入、otel（过 ADR 后）、**WASM 工具插件评估**（wasmtime + WASI，仅评估：能力边界/性能/生态调研，实施须过新 ADR——ADR-0009）、文档英文化评估。

**DoD**：按范围逐项验收。

## 依赖关系

```
M0 ─▶ M1 ─▶ M2 ─▶ M3 ─▶ M4 ─▶ M5
            │            ▲
            └── 后端绑定机制为 M3 委派铺路
```

M4（GUI）只依赖协议稳定（M1）+ 分支/rewind 语义（M2），可与 M3 并行——若人力允许。
