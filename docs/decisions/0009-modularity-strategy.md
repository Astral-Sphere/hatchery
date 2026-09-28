# ADR-0009: 模块化策略——编译期装配 + Cordis 纪律借鉴，不做动态插件树

状态：accepted（2026-09-28）

## 背景

deepseek-harness（dsh）的「everything is a plugin」（Cordis 框架）是四款参考项目中最激进的模块化。用户提出评估 hatchery 是否采纳。对 `vendor/cordis` 及周边设施的代码级深挖（证据细节见 references.md dsh 节）结论：

**Cordis 本体不可移植，且成本已被其自身事故记录证实**：

- 核心 2696 行（`vendor/cordis/src/` 9 文件）+ 外围 ~3100 行（loader/schemastery/hmr/include），建立在 JS 独有动态性上：ctx 是 `Proxy`（get/set trap 沿 fiber 祖先链查字符串键字典）、`Object.create` 原型链做 isolate/intercept、`Symbol.for` 跨 realm 身份、清模块缓存式 HMR（依赖 Node `--expose-internals`）。
- 类型安全只有编译期 declaration merging 假象：`inject: ['fs']` 字符串与 `Context.fs` 字段无编译期连接，拼错得到永久 PENDING 的 fiber；`ctx.get()` 返回 `any`。
- `docs/postmortem/0001`：多写一行 `export default apply` 静默丢 inject，**178 个单测全绿、行覆盖 100%，生产完全不可用**。`0002`：`disabled: !!js` 从不被求值，文件系统工具永久关闭，snapshot 把回归固化成期望输出。
- 配置层 `!!js` = `new Function + eval`，本身是注入面。
- HMR 无状态迁移（dispose-and-recreate）；能成立是因为状态外置在持久会话日志 + projection。
- dsh 自己的刹车：rejected notes 明文「Don't split preemptively」（只有一个 provider 和一个 consumer 的能力，第二个出现前不拆包）；事件词汇拒绝上运行时 schema。

## 决策

进程内模块化 = **trait + 编译期/启动期装配**（维持 ADR-0004 的 capability seam 设计不变），不引入动态插件树与运行时服务字典。吸收 Cordis 五条**语言无关**的纪律：

1. **Disposer/effect 纪律**：一切有副作用的注册（runtime 装配、hub 订阅、MCP 连接、adapter 注册、检查点仓库句柄）必须返回 disposer（Rust 形态：Drop guard 或显式 dispose 方法）；teardown 严格逆序执行。测试锁定逆序性。
2. **反预拆分刹车**：trait 在第二个实现出现前不做 provider 层抽象；crate 在第二个消费者出现前不拆。对现有 12-crate 结构的后续演进有约束力。
3. **注册句柄 + 原子替换**：注册表 `register()` 返回 `Handle`（持 disposer + 原子 `replace()`），与 kernel 的 Turn Tool Snapshot（回合开始冻结、整表原子替换）衔接。ToolRegistry 与 LLM provider 注册表正式采用。
4. **Profile 化装配 + 启动期 fail-loud 审计**：以命名 profile 声明组件捆绑（能力后端绑定、工具集、监听器）；daemon 启动完成装配后审计——必需组件未 resolve 则**拒绝服务并输出错误清单**（dsh `auditStartupEntries` 语义）。「运行时静默 PENDING」是明确的反模式。
5. **状态外置 + projection**：与核心不变量 2（model-visible = logged）互证——任何组件可 dispose/重建，因为状态在 store 而不在组件内。这也是「daemon 重启即恢复」（而非 HMR）成立的前提。

**第三方扩展面**：v1 = MCP（工具）+ ACP（agent/subagent）双通道，与 dsh 对外的实际扩展面一致。**WASM 工具插件（wasmtime + WASI）列为 M5 评估占位**——仅评估，实施前必须过新 ADR。

## 明确拒绝（附理由）

| 机制 | 拒绝理由 |
|---|---|
| Proxy ctx / 字符串键运行时服务字典 | Rust 无等价物；类型安全靠编译器泛型/trait bound，不靠运行时查找 |
| 类型化服务注册表（TypeId map + resolve 循环） | v1 组件集合固定，启动期 builder 装配已足够；额外抽象层收益为负。出现「运行时换组件」真实需求时再评估 |
| `!!js` 式配置表达式（eval） | 注入面；hatchery 配置是纯数据 + per-key schema 校验（platform.md） |
| HMR | 编译语言不可行且 dsh 自身无状态迁移；替代品 = 配置/prompt 热重载 + 「状态外置使 daemon 重启便宜」 |
| 可用性驱动激活 + epoch 重载 | 启动期 resolve + fail-loud 审计覆盖同一需求，失败模式从「永久 PENDING」变为「启动报错」 |

## 替代方案（已否）

- 完整移植 Cordis：见背景，机制不可移植、成本有事故实证。
- 类型化服务注册表（讨论中的 B 选项）：用户裁决不采纳。
- dylib 动态插件（libloading）：ABI 脆弱、Rust 无稳定 ABI、版本地狱。

## 后果

- architecture.md §3 分层纪律追加三条（disposer、反预拆分、fail-loud 装配）。
- daemon.md 正式引入 profile 概念与启动审计；capabilities.md/llm.md 采用注册句柄模式。
- testing.md 补两条代表用例：启动审计 fail-loud、teardown 逆序。
- WASM 评估占位进 roadmap M5；本 ADR 是其评估的前置依据。
