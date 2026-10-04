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

## 开放问题

见设计文档末尾（REPL 极简模式、图片输入一致性；渲染选型已裁决）。解决过程记录于此：

- **D5（2026-09-30）TUI 渲染选型：ratatui + minimad 自绘，termimad 否决，syntect 推迟 M2。**
  spike 过程（独立 scratch crate 实测，非纸上推演）：
  1. **依赖耦合**：termimad 0.35 经 coolor/crokey 拉 crossterm 0.29；ratatui 0.29 用 crossterm 0.28——双 crossterm 大版本共管一个 TTY（两套 Event 类型、raw-mode 进/出配对各管各的）。升 ratatui 0.30 后勉强对齐到 0.29，但这是巧合级耦合，任一方升级即破裂。
  2. **API 形状（决定性）**：termimad 的样式在 `Display`（ANSI 输出）时才经 skin 应用，`FmtComposite.compounds` 暴露的是**未着色**的 minimad compound——嵌进 ratatui 必须绕过它的渲染层、自己重做样式解析。termimad 想拥有整个终端（自带 Area/TextView 滚动组件），与 ratatui 的组件组合模型正面冲突。
  3. **实证替代**：minimad（termimad 底下的解析器，依赖仅 unicode-width）→ ratatui `Line<Span>` 映射约 60 行。`Paragraph::wrap` 折行后 span 样式存活（TestBackend 逐 cell 验证 bold 穿过折行）；fence 语义已摸清：`Options{keep_code_fences: true}` 下围栏标记为空 `CodeFence` 行、内容行 `CompositeStyle::Code`；`ListItem(level)` 层级 0 基。
  裁决：**自绘 = minimad 解析 + 手写 ratatui 映射**；termimad 在该集成形态下只剩解析价值而拖全套渲染栈。代码块 M1 以暗色渲染，syntect 高亮（含 M2 diff 视图）推迟。

## 变更日志

### 2026-10-05 · 滚动条与滚轮：UI 级滚动窗口的鼠标半边

live 验收反馈：上一轮翻新做了 UI 级滚动，但没有滚动条、也没有滚轮。补 `tui/widgets/scrollbar.rs`，chat 循环启用鼠标捕获。

- **gutter 常占一列**：`relayout` 按 `width − 1` 换行，`transcript_areas` 把 transcript 行切成内容列 + bar 列；bar 显隐不改换行宽度，缓存不在滚动中途重排（与 qwen-code `VirtualizedList`「列常驻、不 reflow」同裁决）。有溢出时画比例滚动条：thumb `█`、track `│`，`thumb = max(1, round(track²/total))`、`top = round(offset/max·(track−thumb))`，几何照抄 qwen-code；track 行反解 offset 用同一比例，**拖到底行落 sticky-bottom（follow）**，与滚轮/按键滚到底同语义。与 qwen-code 的差异只有一处：bar 常显而非空闲自动隐藏（它的 flash 式 auto-hide 在空闲态截图里等于没有滚动条，而用户要的是看得见的轨）。
- **鼠标**：`EnableMouseCapture`（退出配对 disable）；滚轮每 notch 3 行（qwen-code `WHEEL_LINES_PER_TICK`）；bar 上左键按下 = Grab，Drag 离开该列仍继续抓（同 qwen-code），Up 释放；文本区的按下/移动一律忽略。事件→意图是纯函数 `mouse_input`，`apply` 的视口半边拆成同步 `viewport(input, model, height, bar)`，测试不需要终端也不需要 daemon。hints 行加 wheel。
- **坑留痕**：ratatui 0.30 的 `TestBackend::to_string` 给每行包双引号（`buffer_view` 要标多宽字符 overwrite）；既有黄金帧全用 `contains` 所以没碰过它，这次的行尾断言（`ends_with('█')`）头一回撞上，测试先 `trim_matches('"')`。
- 实测：cli 85 项全绿（75 + 新 10：scrollbar 几何/渲染 5、gutter 换行宽与 bar 黄金帧 2、鼠标映射/滚轮/拖拽 3）；`./scripts/ci.sh` 全门禁绿。真实终端的滚轮手感与拖拽仍挂 live 验收。

### 2026-10-04 · TUI 翻新：widget 层、融合视觉、双主题、UI 级滚动

单文件 `tui.rs`（402 行）退役为 `tui/` 模块树：`theme`（语义色层 × dark/light 调色板）、`scroll`（follow/偏移状态）、`widgets/{transcript, composer, status, indicator, toasts}`，`mod.rs` 留 Model 与帧组合。渲染不变量不变：`draw(&Model, Frame)` 纯函数、TestBackend 黄金帧。

- **布局**（对照 qwen-code 与 codex 的 TUI）：状态从顶部蓝底条移到底部一行（`mode · model · effort · state`，状态段带色；非 follow 时右侧 `↑ top/total · end follows`）；输入进圆角框 composer（`>` prompt glyph + 占位符 + 框下键位提示行）；turn 活跃时 composer 上方一行 braille spinner 指示器（`⠋ working 12s · esc to interrupt`，250ms tick 驱动）；notes 改为吐司（图标按 kind、5 秒过期、最多 3 条）；transcript 顶部一行会话摘要（model · effort · workspace）。
- **角色 glyph**（qwen-code 词汇）：用户 `>`、助手 `◆`、reasoning 折叠 `∴ Thought for n chars (Ctrl+R to expand)`；工具单元格带生命周期 glyph（in-flight spinner / `✓` / `✗` / denied `−`），名字在 `ItemFinished` 到达后补上，detail 与 progress 尾行走左竖条。
- **滚动是 UI 级的**：transcript 自绘换行（unicode-width 进 workspace 依赖；词边界断开、折点空格丢弃、宽字符两列、硬断超宽词），换行后行表缓存于 Model（`relayout(width)` 在模型或宽度变化时重建），滚动偏移与 `Ctrl+↑/↓` 消息跳转地址化真实渲染行—— PgUp 可回到会话第一行，终端回滚不参与（alternate screen）。ratatui 的 `Paragraph::wrap` 否决：它不报告折行落点，偏移与跳转无从算起（`line_count` 在 0.30 仍是 unstable feature，不引）。
- **主题**：`ui.theme = auto|dark|light` 进 daemon 配置 schema（`is_known_key` 同步），`/theme` 前端本地切换（纯命令层照 `/effort` 的用法回复纪律）；`auto` = OSC 11 背景查询（raw mode 后、事件流前的 150ms 窗口，/dev/tty + 线程读，无 unsafe）→ `$COLORFGBG` → dark。解析与亮度判定全纯函数单测（含截断/异槽回复为 None）。
- **markdown**：皮肤改走主题角色（标题 accent 粗体——旧纯白在亮底不可见）；代码块左竖条；**表格从渲染成空行改为 pipe 行**（列宽对齐需要视口宽，与单元格换行缓存的宽度无关性冲突，留给 M2 diff 视图）。
- **键位**：新增 PgUp/PgDn/Home/End、Ctrl+↑/Ctrl+↓（用户消息边界跳转）、Esc（turn 进行中发 `session/cancel`，空闲无副作用）；流式 delta 不再把已上滚的窗口拽回尾部（只有用户发送新消息才 follow）。
- 实测：cli 75 项全绿（原 24+31 两轮计数口径合并后的现值），workspace 默认组 557 项全绿（1 skipped 为既有）；`./scripts/ci.sh` 全门禁绿。真实 provider 的手动 live 验收仍挂 M1 收口清单（roadmap）。

### 2026-10-04 · live 验收反馈：已知命令答用法，不说 unknown

TUI 实测打出 `unknown command /effort; try /effort, /model, …`——回复在推荐刚输入的那个命令。根因：`/effort` 与 `/model` 的缺参/坏参路径被折进 `Unknown` 变体，而 Unknown 按「命令名不认识」措辞。新增 `SlashCommand::Usage { command, argument }`：裸 `/effort` 回 `effort is high; set it with /effort off|low|medium|high|max`（捎上状态栏现值，裸命令本来就是一问）；参数错回 `not an effort: banana; usage: …`；只有真正不认识的名字才说 unknown。顺带：斜杠后的空白不再吞掉命令（`/ effort off` 可解析——实测就有人这么打）。清单一处笔误一并更正：关推理是 `/effort off`，不是裸 `/effort`。

### 2026-10-01 · 评审⑤自查轮（cli）

交互正确性四处：turn 进行中打字不再整窗退出（submit 错误降级为 notes，连接断开仍由事件流收尾；被拒 prompt 文本退回输入框可改再发）；多行输入落地（Alt/Shift+Enter 换行、bracketed paste 启停与整块粘贴、输入区随行数 1..8 行增高）；resume 播种改读 `session/load` 回复里的真 session（exec.rs 曾伪造最小回复，导致 model 空白、effort 写死 medium）；状态栏从 `SessionUpdated.status` 投影 thinking/awaiting approval（此前整个 turn 期间显示 idle）。解析层：`//` 转义、命令名大小写不敏感、Unknown 回显去尾随空格、空输入不发送；`actions` 的序列化失败从静默空表改为 expect。attach 轮询容忍发布先于 bind 的连接失败（与预检路径同一裁决）。计数见 worklog/testing.md 本日条目（cli 31 项）。

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
