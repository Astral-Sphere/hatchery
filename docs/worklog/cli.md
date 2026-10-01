# 工作记录：CLI（hatchery-cli）

- 范围：ratatui TUI、headless exec、子命令族（daemon/doctor/sessions/acp 入口）
- 设计文档：[../design/frontends.md §2](../design/frontends.md)
- 相关 ADR：0001、0008（i18n 部分）

## 当前状态

M1 Phase 4 交付:TUI/exec/daemon 子命令/doctor 全部接线,attach-or-spawn(D1 spawn 半边)就位。评审④待通过。

## 待办

- [x] (M1) **TUI 渲染 spike**:markdown/diff 渲染选型(termimad vs syntect 自绘);结论写回(2026-09-30,D5,见变更日志)
- [x] (M1) 最小 TUI:消息流 + 输入区 + reasoning 折叠 + 状态栏(mode/model/effort);Ctrl+R 切换折叠,`ui.show_reasoning` 经 `config/get` 播种
- [x] (M1) 斜杠命令 v1:/mode /effort /model /prompt /quit(纯函数层 `commands`,逐命令「输入→断言发出的协议方法」测试)
- [x] (M1) headless exec(纯文本流 + --json JSONL + 退出码语义:0 完成 / 1 失败 / 2 取消;Ctrl-C 经 `session/cancel` 服务端取消)
- [x] (M1) daemon 子命令 + attach-or-spawn 接线 + doctor(`start` 脱离终端 spawn、`run` 前台、`status`、`stop`;doctor 环境审计 + provider 实测探测)
- [x] (M1) doctor --provider 真实两家探测留痕(2026-09-30 两轮:首轮旧模型一轮制;模型校准后新模型双轮制,结论见变更日志)
- [ ] (M2) 审批内联弹层(diff/命令预览 + 快捷键)、/rewind /branch /edit、/export
- [ ] (M2) sessions 子命令族(list/resume/export/delete)
- [ ] (M4) 文案全部进 gettext catalog
- [ ] (M2) 审批内联弹层（diff/命令预览 + 快捷键）、/rewind /branch /edit、/export
- [ ] (M2) sessions 子命令族（list/resume/export/delete）
- [ ] (M4) 文案全部进 gettext catalog

## 开放问题

见设计文档末尾（REPL 极简模式、图片输入一致性；渲染选型已裁决）。解决过程记录于此：

- **D5（2026-09-30）TUI 渲染选型：ratatui + minimad 自绘，termimad 否决，syntect 推迟 M2。**
  spike 过程（独立 scratch crate 实测，非纸上推演）：
  1. **依赖耦合**：termimad 0.35 经 coolor/crokey 拉 crossterm 0.29；ratatui 0.29 用 crossterm 0.28——双 crossterm 大版本共管一个 TTY（两套 Event 类型、raw-mode 进/出配对各管各的）。升 ratatui 0.30 后勉强对齐到 0.29，但这是巧合级耦合，任一方升级即破裂。
  2. **API 形状（决定性）**：termimad 的样式在 `Display`（ANSI 输出）时才经 skin 应用，`FmtComposite.compounds` 暴露的是**未着色**的 minimad compound——嵌进 ratatui 必须绕过它的渲染层、自己重做样式解析。termimad 想拥有整个终端（自带 Area/TextView 滚动组件），与 ratatui 的组件组合模型正面冲突。
  3. **实证替代**：minimad（termimad 底下的解析器，依赖仅 unicode-width）→ ratatui `Line<Span>` 映射约 60 行。`Paragraph::wrap` 折行后 span 样式存活（TestBackend 逐 cell 验证 bold 穿过折行）；fence 语义已摸清：`Options{keep_code_fences: true}` 下围栏标记为空 `CodeFence` 行、内容行 `CompositeStyle::Code`；`ListItem(level)` 层级 0 基。
  裁决：**自绘 = minimad 解析 + 手写 ratatui 映射**；termimad 在该集成形态下只剩解析价值而拖全套渲染栈。代码块 M1 以暗色渲染，syntect 高亮（含 M2 diff 视图）推迟。

## 变更日志

### 2026-09-30(M1 Phase 4 交付)

- **D5 spike 结论(ratatui + minimad 自绘)**:过程与证据见上方开放问题记录。syntect(代码块高亮、M2 diff 视图)未进树。
- **argparse**:手写解析成类型化 `Command`(子命令面小,不值得引入解析器依赖);`exec` 退出码矩阵 0/1/2 写进 `--help`。
- **attach-or-spawn(D1 spawn 半边)**:`attach_or_spawn(state, daemon_exe, workspace)` — 有 `daemon.json` 且 pid 活着 → 连接 + hello;否则 spawn 当前二进制的 `daemon run`(stdio 入 /dev/null、`process_group(0)` 脱离前台进程组),100ms 轮询发布文件,15s 超时报日志路径。子进程先于发布退出 → 立即报错,不磨完超时。`daemon_exe` 参数是测试接缝。
- **exec**:输出经 `ExecOut` trait(stdout 实现 + 测试 Vec 实现);`--json` 逐行输出 item 级事件(session/generation/event 信封,过滤 `session_updated` 等连接杂务);plain 模式只流 assistant 正文,工具活动走 stderr。
- **TUI**:渲染 = 纯函数 `draw(&Model, frame)`(TestBackend golden 帧验证);`Model` 只经 `push_event` 与按键处理变化;reasoning 折叠为 "… thinking (n chars) — Ctrl+R";markdown 按 minimad → ratatui Line 映射(`markdown.rs`)。
- **daemon 子命令**:`start` 是 D1 的 CLI 半边;`stop` 经系统 `kill` 发 SIGTERM(工作区 deny unsafe,kill(2) 无法安全直调;一行子进程优于引 syscall 绑定,M1 裁决)。
- **doctor**:环境审计(配置可载、env key、state/data 目录可写)+ `--provider` 一次真实请求探测(30s 上限;报告 reasoning 字符数/finish reason/usage/重试次数)。探测代码离线经 wiremock 验证。
- **doctor --provider 真实探测留痕(用户 shell 实测,2026-09-30,两轮)**:
  - 第一轮(模型校准前,旧模型 `deepseek-chat` 别名 + `qwen-plus`,单轮制):两轮各一条最小请求,completed,reasoning 0 字符,usage 无 reasoning 细分——当时误判为"能力表无需修正",随后用户指出模型世代已换代,该结论作废。
  - 第二轮(模型校准后:单模型双轮制,`deepseek-flash` / `qwen3.8-flash`),最小请求("Reply with exactly: ok",max_tokens=64),走完整 adapter 路径(能力表 + 重试 + 翻译):
    - `deepseek/deepseek-flash`:**default 轮 591 ms,reasoning 120 字符,usage reasoning=Some(28)**;**off 轮 512 ms,reasoning 0 字符**,usage prompt=9/completion=1。两轮 finish Stop,内容 "ok"。
    - `qwen/qwen3.8-flash`:**default 轮 2241 ms,reasoning 98 字符,usage reasoning=Some(22)**;**off 轮 742 ms,reasoning 0 字符**,usage prompt=30/completion=1。两轮 finish Stop,内容 "ok"。
  - **校准结论(定稿)**:两家的混合推理"默认开、参数可关"被实测确认;off 开关在两家都精确抑制 reasoning;**当前世代两家都在 usage 里上报 reasoning token 细分**(旧模型不报);无 RateLimited 轮次。能力表 ThinkingSwitch/QwenThinking 行与实测一致,无需再修。
- **测试留痕**:仓库默认组 443 项全绿(ci profile,含 daemon 58、cli 24)。golden:`exec --json` 行形(JSON 可解析、type 集合、text_delta 内容、信封字段)、TUI 状态栏/折叠/工具摘要帧、退出码矩阵、`/effort /model /prompt` 到协议方法的映射。
- **评审③遗留确认**:determinism 门禁的协议 fixture 两处增量(item_tool_call + provider_call_id、params_daemon_hello + boot_token)为有意变更,提交即转绿,与 Phase 3 相同。

### 2026-09-28
- 初稿。原则：TUI 是事件流投影，零本地状态机复制（吸取 qwen-code CLI 进程内直连导致双接线的教训，见 ADR-0001）。
