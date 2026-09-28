# ADR-0005: Chat/Code 模式 = 工具集 × 审批策略 × prompt 变体

状态：accepted（2026-09-28）

## 背景

用户想要 Chat 与 Code 两套模式但细节未定。参考项目里「模式」各有含义：qwen-code 的 5 档审批模式（plan/default/auto-edit/auto/yolo）、atomcode 的 Plan/Build/Goal、dsh 的 permission-presets（沙箱×审批打包成用户可见选择器）、ACP 协议自带 `session/set_mode` 与 session modes 声明。

## 决策

模式是**会话级属性**，定义为三元组的命名组合，运行中可切换（切换只影响后续 turn）：

```rust
pub struct SessionMode {
    pub id: &'static str,          // "chat" | "code" | 用户自定义
    pub tools: ToolPolicy,         // 工具注册表子集
    pub approval: ApprovalPolicy,  // 审批策略
    pub prompt_variant: PromptVariant, // system prompt 变体
}
```

内置两种：

| | Chat | Code |
|---|---|---|
| 工具 | 只读：read/glob/grep/web_fetch（+MCP 只读工具） | 全量：+edit/write/shell/MCP 写工具 |
| 工作区 | 可不绑定 | 必须绑定工作区（影子 Git 检查点随之启用） |
| 审批 | 无（只读无需审批） | ApprovalGate 全链路；规则可持久化（allow/deny always） |
| prompt | 对话助手变体（无工作区纪律段落） | coding agent 变体（工具纪律、安全门、AGENTS.md 注入点） |

- 模式注册表可扩展：用户可在 config.toml 定义自定义模式（如 `plan` = Code 工具集只读化 + 无审批），M2+ 支持。
- ACP 对接：`initialize` 响应中声明 session modes（chat/code），`session/set_mode` 直接切换；宿主 UI 的模式选择器免费获得。
- 切换语义：切到 Chat 时进行中的写工具调用需先完成或取消；切回 Code 不自动恢复被移除的工具正在执行的调用。

## 理由

1. 三元组正交分解避免了 qwen-code「审批模式与工具可用性纠缠」的 5 档膨胀，也避免两套独立 pipeline 的代码重复。
2. 与 ACP session mode 天然一一映射。
3. 自定义模式给用户留出「plan 模式」等演进空间，无需改核心。

## 替代方案（已否）

- 两套独立产品线（独立 prompt 体系/存储/UI）：复用少，维护翻倍。
- codex 式审批×沙箱正交档位直出给用户：表达力强但认知负担大；hatchery 用命名模式封装档位，高级用户仍可用 config 细调 `ApprovalPolicy`。

## 后果

- kernel 每次 turn 开始时冻结工具表（借鉴 atomcode Turn Tool Snapshot），模式切换在 turn 边界生效。
- prompt 变体机制要求提示词系统按 section 组装（design/platform.md）。
