# 工作记录：CLI（hatchery-cli）

- 范围：ratatui TUI、headless exec、子命令族（daemon/doctor/sessions/acp 入口）
- 设计文档：[../design/frontends.md §2](../design/frontends.md)
- 相关 ADR：0001、0008（i18n 部分）

## 当前状态

M1 Phase 4 交付：TUI/exec/daemon 子命令/doctor 全部接线，attach-or-spawn（D1 spawn 半边）就位。评审④已过、评审⑤自查轮的加固已入库（见下面 2026-10-01 条目）；手动 live 验收**进行中**——已产出 2026-10-04/10-05 三轮 TUI 修正（widget 层与双主题、滚动条与滚轮、词级换行与点击折叠），但 worklog/testing.md 的六项清单尚未勾选，M1 的收口归入 M2 Phase 0。（原句「评审④待通过」已被本文件后面的条目超过，2026-10-07 更正。）

**M2 已重新规划（2026-10-07，Phase 0–8）**：本方向的活集中在 **Phase 6（TUI 与 headless）**，另有图片输入路径挂在 Phase 5。下面的待办已按 Phase 展开；对账结论（哪些地基能复用、哪些是空的）见同日变更日志。

## 待办

- [x] (M1) **TUI 渲染 spike**:markdown/diff 渲染选型(termimad vs syntect 自绘);结论写回(2026-09-30,D5,见变更日志)
- [x] (M1) 最小 TUI:消息流 + 输入区 + reasoning 折叠 + 状态栏(mode/model/effort);Ctrl+R 切换折叠,`ui.show_reasoning` 经 `config/get` 播种
- [x] (M1) 斜杠命令 v1:/mode /effort /model /prompt /quit(纯函数层 `commands`,逐命令「输入→断言发出的协议方法」测试)
- [x] (M1) headless exec(纯文本流 + --json JSONL + 退出码语义:0 完成 / 1 失败 / 2 取消;Ctrl-C 经 `session/cancel` 服务端取消)
- [x] (M1) daemon 子命令 + attach-or-spawn 接线 + doctor(`start` 脱离终端 spawn、`run` 前台、`status`、`stop`;doctor 环境审计 + provider 实测探测)
- [x] (M1) doctor --provider 真实两家探测留痕(2026-09-30 两轮:首轮旧模型一轮制;模型校准后新模型双轮制,结论见变更日志)
- [ ] (M2 Phase 6) **弹层架构裁决先行（D18）**：模态要么给 `layout()` 加第七个约束、要么在 `draw()` 之后加一次居中 `Clear` pass（ratatui 0.30 的 `Clear` 在默认 feature 里，不需要新依赖）；`Model` 上要有 pending 审批字段与选项游标；`key_input` 要有模态分支；`Esc` 的所有权要交接（`chat.rs:273` 已经把这个交接写成注释：「while an approval waits, M2's dialog owns Esc, and idle Esc is a mispress」）。这与「`Model` 只是事件流的投影、无本地状态机」（tui/mod.rs:3）正面冲突——`/branch` `/rewind` 本质是交互式选择器（选目标 item、选 scope、`confirm: true` 确认破坏性删除），必须显式裁决，不能让它长出第二套状态机（ADR-0001 要规避的正是双接线）
- [ ] (M2 Phase 6) 审批内联弹层：展示 `ApprovalRequest`（含 diff/命令预览；预览载荷的形状是 Phase 2 的 D14）+ 快捷键 `1`-`4` 对应四个 option + `Esc` 交接。今天 `push_event` 的 `ApprovalRequested` 臂（tui/mod.rs:262-265）只把 `status.state` 改成 `"awaiting approval"`，`request_id` 与整个 `ApprovalRequest` 都丢掉；`Model` 上没有 pending 审批字段、没有选项表、没有选择游标；`key_input`（chat.rs:400-437）没有模态分支，`1`-`4` 会落进 `(_, KeyCode::Char(c)) => model.input.push(c)` 被 composer 吞掉
- [ ] (M2 Phase 6) diff 视图（**D11 已裁决**：`similar` 算 hunk + 现有主题语义色渲染，**不引 syntect**）。今天一点 diff 渲染都没有——没有解析器、没有 +/- 着色、没有 gutter；要用的主题角色（accent/text/dim/faint/success/warn/error/code/tool/thinking）已在 `theme.rs:56-125`。`markdown.rs:6` 把代码块高亮标为 M2 的 syntect 议题、`markdown.rs:43` 把表格列对齐写成「M2 diff 视图的活」——前者按 D11 另议，后者随本条落地
- [ ] (M2 Phase 6) 六个缺的斜杠命令：`/rewind`、`/branch(list|switch|delete)`、`/edit`、`/approval`、`/export`、`/clear`（frontends.md §2.2 声明的命令里今天只有 `/effort /model /prompt /mode /theme /quit`）。每条都要动六处：`SlashCommand` 新变体、`parse` 的名字匹配臂、参数解析器（挨着 `effort()`/`model()`/`theme()`）、`actions()` 臂、`local_reply()` 的用法臂、`submit_line` 的 match
- [ ] (M2 Phase 6) **`actions()` 的 fire-and-forget 形状要改**：`submit_line` 的共享循环是 `for (method, params) in commands::actions(&command, *session) { attached.client.call_raw(method, params).await?; }`——回复被丢弃。而 `session/branch/list` 返回的 `Vec<BranchNode>` 必须渲染、`session/rewind` 返回的 `RewindReport` 用户必须看见
- [ ] (M2 Phase 6) `hatchery sessions {list|resume|export|delete}` 子命令族：`Command`（args.rs:13-27）今天只有 Chat | Exec | Daemon | Doctor | Help | Version，`usage()`（args.rs:96-115）也没列
- [ ] (M2 Phase 6) **exec 的 Code 模式（D16）**：加 `--mode` + 审批策略标志，或明确「exec 拒绝 Code 会话」。今天 `open_subscribed`（exec.rs:163-206）硬编码 `SessionModeId::chat()`（:187），`ExecArgs`（args.rs:42-56）只有 prompt/json/session/workspace/model/state_dir，也没有 stdin prompt 路径；`ApprovalRequested` 在 `--json` 的转发子集里（`is_item_level`，:230-244），所以脚本看得见审批、然后这一轮永久挂住（直到下一次调用撞上 120s `CALL_TIMEOUT`，或事件流结束）。frontends.md §2.3 对此一字未规定
- [ ] (M2 Phase 5) 图片输入路径（frontends 开放问题 3，2026-10-07 用户裁决留在 M2）：协议侧 `ContentPart::Image { mime_type, data }` 已在，缺的是 llm 侧到 wire 的翻译（Phase 5）+ 本方向的输入路径；两半必须一起落，否则图片进得了库却发不出去
- [ ] (M3) `hatchery acp` 子命令（attach 与 standalone 两态，design/acp.md）——今天 `Command` 里没有它
- [ ] (M4) 文案全部进 gettext catalog

## 开放问题

见设计文档末尾（REPL 极简模式、图片输入一致性；渲染选型已裁决，diff 渲染于 2026-10-07 另裁为 D11）。解决过程记录于此：

- **图片输入（frontends 开放问题 3）2026-10-07 用户裁决留在 M2**，并与 llm 侧的多模态工作（Phase 5）耦合：协议侧 `ContentPart::Image { mime_type, data }` 已在，缺的是 llm 侧到 wire 的翻译 + 本方向的输入路径。两半必须一起落，否则图片进得了库却发不出去。
- **D5（2026-09-30）TUI 渲染选型：ratatui + minimad 自绘，termimad 否决，syntect 推迟 M2。**
  spike 过程（独立 scratch crate 实测，非纸上推演）：
  1. **依赖耦合**：termimad 0.35 经 coolor/crokey 拉 crossterm 0.29；ratatui 0.29 用 crossterm 0.28——双 crossterm 大版本共管一个 TTY（两套 Event 类型、raw-mode 进/出配对各管各的）。升 ratatui 0.30 后勉强对齐到 0.29，但这是巧合级耦合，任一方升级即破裂。
  2. **API 形状（决定性）**：termimad 的样式在 `Display`（ANSI 输出）时才经 skin 应用，`FmtComposite.compounds` 暴露的是**未着色**的 minimad compound——嵌进 ratatui 必须绕过它的渲染层、自己重做样式解析。termimad 想拥有整个终端（自带 Area/TextView 滚动组件），与 ratatui 的组件组合模型正面冲突。
  3. **实证替代**：minimad（termimad 底下的解析器，依赖仅 unicode-width）→ ratatui `Line<Span>` 映射约 60 行。`Paragraph::wrap` 折行后 span 样式存活（TestBackend 逐 cell 验证 bold 穿过折行）；fence 语义已摸清：`Options{keep_code_fences: true}` 下围栏标记为空 `CodeFence` 行、内容行 `CompositeStyle::Code`；`ListItem(level)` 层级 0 基。
  裁决：**自绘 = minimad 解析 + 手写 ratatui 映射**；termimad 在该集成形态下只剩解析价值而拖全套渲染栈。代码块 M1 以暗色渲染，syntect 高亮（含 M2 diff 视图）推迟。
- **D11（2026-10-07）M2 的 diff 渲染：`similar` 算 hunk + 现有主题语义色，不引 syntect。** D5 当初把「syntect 高亮（含 M2 diff 视图）」整体推迟，M2 重新规划时又评估了一次，结论是 diff 视图**不要**语法高亮：① 它要的是 +/- 着色与 gutter，不是 token 着色，`theme.rs` 的语义角色（accent/text/dim/faint/success/warn/error/code/tool/thinking）已经够用；② syntect 重（一个 regex 后端 + syntax sets），而 D5 否掉 termimad 的理由之一正是依赖耦合；③ 房内先例：atomcode 用 `similar = "2"`（references/atomcode/Cargo.toml:40）。代码块的语法高亮是**另一个决定**，继续往后排。

## 变更日志

### 2026-10-07 · M2 重新规划对账

roadmap 的 M2 段重写为 Phase 0–8；本方向的活集中在 **Phase 6（TUI 与 headless）**，图片输入路径挂 Phase 5。勘察（roadmap 更正 10/11）对 CLI 的结论如下。

**widget 层可以作为「惯例」复用，但不能作为「框架」复用。** 直接拿来用的是那套写法：每个 widget 是借用 `&Model` 的纯函数、实现 ratatui 的 `Widget`、height 以自由函数暴露好让 `layout()` 算约束、全部有 `TestBackend` 黄金帧。另外三件现成料：`composer.rs` 的 `Block::default().borders(ALL).border_type(Rounded)`（模态外框）、`transcript::wrap_lines`（逐 cell 与宽度无关、保样式、CJK 正确——三次重写才对；diff 正文与审批请求文本正好要用）、`toasts.rs` 那种「`height()` 驱动的可变行」（可变高度弹层的样板）。

**弹层与 diff 的地基是空的**，五处都得新建：

1. `layout()`（tui/mod.rs:515-528）是**固定的六行竖向分割**——transcript(Min(1)) / toasts / indicator / composer / hints(1) / status(1)，外加 `transcript_areas`（:536-544）切出来的一列滚动条 gutter；返回硬编码的 `[Rect; 6]`，`draw()`（:546-580）把它们逐个渲染进各自区域。**没有弹层槽、没有 z-order/`Clear` 通道**（ratatui 0.30 的 `Clear` 在默认 feature 里，居中弹层不需要新依赖）。
2. `CellKind`（:83-95）只有 Banner/User/Assistant/Reasoning/Tool 五个变体——没有 Approval、没有 Diff、没有 Branch。
3. `Model::push_event`（:228-320）以 `_ => {}` 收尾，`ItemStarted`/`ModeSwitched`/`GenerationBumped` 被静默丢弃；`ApprovalRequested` 臂（:262-265）只有两行，把 `status.state` 改成 `"awaiting approval"` 再置脏——**`request_id` 与整个 `ApprovalRequest` 都丢了**。`Model` 上没有 pending 审批字段、没有选项表、没有选择游标。
4. `key_input`（chat.rs:400-437）没有模态分支，`1`-`4` 会落进 `(_, KeyCode::Char(c)) => model.input.push(c)` 被 composer 吞掉；`Esc` 硬接 `Input::Cancel` → `session/cancel`，而 chat.rs:273 明写「审批等待期间 M2 的弹层拥有 Esc，空闲时按 Esc 是误触」——**交接写在注释里，没有实现**。
5. diff 渲染为零：没有解析器、没有 +/- 着色、没有 gutter、没有 syntect。`markdown.rs:43` 那句「表格列对齐是 M2 diff 视图的活」也因此还欠着。

**headless exec 跑不了 Code 会话**：`open_subscribed`（exec.rs:163-206）硬编码 `SessionModeId::chat()`（:187）；`ExecArgs`（args.rs:42-56）只有 prompt/json/session/workspace/model/state_dir——没有 `--mode`、没有任何审批策略标志、也没有 stdin prompt 路径。而 `ApprovalRequested` **在** `--json` 的转发子集里（`is_item_level`，:230-244），所以脚本会看到审批事件、然后这一轮**永久挂住**（直到下一次调用撞上 120s `CALL_TIMEOUT`，或事件流结束）。exec 对 M1 是完整的（`Outcome` = Completed(0)/Failed(1)/Cancelled(2)，:48-67；Ctrl-C → `session/cancel` → Cancelled，:127-141），但 frontends.md §2.3 对 Code 模式一字未规定。

**三个决策点**：

- **D11（Phase 6，已裁决）**：diff = `similar` + 主题语义色，不引 syntect（理由见上面「开放问题」）。
- **D16（Phase 6）**：exec 的 Code 模式——加 `--mode` + 审批策略标志，还是明确拒绝 Code 会话。今天的行为是「静默挂住」，两个选项都比它好。
- **D18（Phase 6，且要先定）**：弹层/选择器与「`Model` 只是事件流的投影、无本地状态机」（tui/mod.rs:3）的和解方式。它决定上面 1–4 处怎么改：让 `/branch` `/rewind` 各自长一套本地状态机，正是 ADR-0001 要规避的双接线。

**顺带记清两处口径**：`hatchery sessions` 与 `hatchery acp` 都不存在（`Command`，args.rs:13-27，只有 Chat | Exec | Daemon | Doctor | Help | Version，`usage()`，args.rs:96-115，两个都没列）；**历史移动后不需要新事件**（新增事件 `type` 属协议 major bump）——`SessionUpdated.state.active_branch_head` 已经在广播里，前端发现自己投影的 head 不是它的后继就 `session/load` 重建，发起方本来就在自己的回复里拿到新 Session。

### 2026-10-05 · 滚动条与滚轮：UI 级滚动窗口的鼠标半边

live 验收反馈：上一轮翻新做了 UI 级滚动，但没有滚动条、也没有滚轮。补 `tui/widgets/scrollbar.rs`，chat 循环启用鼠标捕获。

- **gutter 常占一列**：`relayout` 按 `width − 1` 换行，`transcript_areas` 把 transcript 行切成内容列 + bar 列；bar 显隐不改换行宽度，缓存不在滚动中途重排（与 qwen-code `VirtualizedList`「列常驻、不 reflow」同裁决）。有溢出时画比例滚动条：thumb `█`、track `│`，`thumb = max(1, round(track²/total))`、`top = round(offset/max·(track−thumb))`，几何照抄 qwen-code；track 行反解 offset 用同一比例，**拖到底行落 sticky-bottom（follow）**，与滚轮/按键滚到底同语义。与 qwen-code 的差异只有一处：bar 常显而非空闲自动隐藏（它的 flash 式 auto-hide 在空闲态截图里等于没有滚动条，而用户要的是看得见的轨）。
- **鼠标**：`EnableMouseCapture`（退出配对 disable）；滚轮每 notch 3 行（qwen-code `WHEEL_LINES_PER_TICK`）；bar 上左键按下 = Grab，Drag 离开该列仍继续抓（同 qwen-code），Up 释放；文本区的按下/移动一律忽略。事件→意图是纯函数 `mouse_input`，`apply` 的视口半边拆成同步 `viewport(input, model, height, bar)`，测试不需要终端也不需要 daemon。hints 行加 wheel。
- **坑留痕**：ratatui 0.30 的 `TestBackend::to_string` 给每行包双引号（`buffer_view` 要标多宽字符 overwrite）；既有黄金帧全用 `contains` 所以没碰过它，这次的行尾断言（`ends_with('█')`）头一回撞上，测试先 `trim_matches('"')`。
- 实测：cli 85 项全绿（75 + 新 10：scrollbar 几何/渲染 5、gutter 换行宽与 bar 黄金帧 2、鼠标映射/滚轮/拖拽 3）；`./scripts/ci.sh` 全门禁绿。真实终端的滚轮手感与拖拽仍挂 live 验收。

### 2026-10-05 · 词级换行与点击折叠

live 验收反馈两条：换行把单词劈开（截图里 `check t / he workspace`）；工具/思考区域不能点开看或收起来。

- **词级换行**：旧 `wrap_line` 是字符级贪心（doc 注释写着词边界，代码不是——live 截图抓出来的谎）。重写为 atom 贪心：窄字符粘成词、空格成段、**宽字符单独成 atom**（CJK 任意两字之间是断点，中文照旧按字折），整词放不下才换行，只有比整行还宽的词才硬断；行首缩进保留、断点空格两边都不留。样式随 atom 走，折行穿样式不变。
- **点击折叠**：`Cell.expanded: Option<bool>` 单格覆盖；reasoning 默认跟 Ctrl+R 全局折叠，tool 默认展开（折叠后 header 留 `· ⋯` 提示有藏起来的内容）。`relayout` 顺带记录 row→cell（`cell_of`），左键单击 transcript 行经 `offset + 行` 地址化到 cell 再 `toggle_cell`。**与 Ctrl+R 的关系**：Ctrl+R 翻转全局折叠并清除全部 reasoning 单格覆盖——主开关一动例外归零，避免「全局说折、单格说开」的两套真值打架；折叠行文案改为 `(click or Ctrl+R to expand)`。bar 命中优先于折叠（点在 gutter 上是抓滚动条）。
- 实测：cli 89 项全绿（85 + 新 4：词级换行 1、点击地址化 1、reasoning 单格/全局关系 1、tool 折叠黄金帧 1）；`./scripts/ci.sh` 全门禁绿。

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
