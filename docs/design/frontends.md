# 设计：前端（hatchery-cli / hatchery-gui）

> 状态：设计稿。依据 ADR-0001（瘦客户端）、ADR-0008（GTK 栈）。两个前端都只用 `hatchery-protocol` 的 client helper，禁止触碰 daemon 内部类型。

## 1. 共用 client helper（在 hatchery-protocol 内）

```rust
pub struct DaemonClient { /* 连接管理 */ }
impl DaemonClient {
    pub async fn attach_or_spawn() -> Result<Self>;   // UDS 发现 + spawn + boot token
    pub async fn call<M: Method>(&self, params: M::Params) -> Result<M::Response>;
    pub fn events(&self) -> broadcast::Receiver<ServerEvent>; // 已做 generation 过滤与重连 replay
}
```

重连策略：断线 → 指数退避重连 → `session/load(replay_from=last_seen)` 补差 → 继续。前端只见「事件流可能重放」，不见连接细节。

## 2. CLI（hatchery-cli）

### 2.1 形态

- `hatchery`：默认进 ratatui TUI。
- `hatchery exec "task"`：headless，stdout 人类可读或 `--json` JSONL（脚本/CI/SDK 场景，借鉴 codex exec）。**Code 模式下的行为待定：D16，见 §2.3。**
- `hatchery acp`：ACP server 入口（design/acp.md）。**尚未存在**（`Command` 今天只有 `Chat | Exec | Daemon | Doctor | Help | Version`，`usage()` 也没列它），随 **M3** 交付。
- `hatchery daemon {start|stop|status}`；`hatchery doctor [--provider X]`（llm.md §7 实测探测）。
- `hatchery sessions {list|resume|export|delete}`：**尚未存在**，M2 Phase 6 加（`Command` 变体 + `usage()` 一并列出）。

### 2.2 TUI 布局（ratatui）

```
┌────────────────────────────────────────────┬─┐
│ 消息流（UI 级滚动窗口，全历史可达）：        │█│  ← 比例滚动条 gutter（常占一列）
│   hatchery · model · effort · workspace    │││
│   > 用户原文                                │││  ← accent `>` glyph
│   ◆ 助手 markdown                           │││  ← accent `◆` glyph
│   ∴ Thought for n chars (Ctrl+R to expand) │││  ← reasoning 折叠行
│   /✓/✗ 工具名 · 摘要 ▎detail ▎progress     │││  ← 工具单元格（左竖条 + 生命周期 glyph）
├────────────────────────────────────────────┴─┤
│ ✓/!/· 吐司（最多 3 条，5 秒过期）            │
│ ⠋ working 12s · esc to interrupt           │  ← turn 指示器（仅 turn 活跃时占一行）
│ ╭────────────────────────────────────────╮ │
│ │ > 输入区（多行、bracketed paste）         │ │  ← 圆角框 composer + 占位符
│ ╰────────────────────────────────────────╯ │
│ enter send · pgup/pgdn scroll · …          │  ← 键位提示行
│ chat · model · effort · state   ↑ n/m …    │  ← 状态行（底部；非 follow 时右侧显示位置）
└────────────────────────────────────────────┘
```

- 渲染原则：TUI 是事件流的投影（view projection），无本地状态机复制；所有动作发协议方法。`draw(&Model, Frame)` 是纯函数，TestBackend 黄金帧验证；widget 层（`tui/widgets/`）每个表面一个模块：transcript / composer / status / indicator / toasts / scrollbar。**这条纪律与 M2 的弹层需求正面冲突，Phase 6 必须先裁决 D18**：审批弹层、`/branch`、`/rewind` 本质是交互式选择器（选目标 item、选 scope、`confirm: true` 确认破坏性删除），需要 pending 请求字段、选项表与选择游标、模态键分支。要么把模态态严格定义成「协议状态的投影」（pending 审批来自事件、分支列表来自 `session/branch/list` 的回复，游标只是纯 UI 位置），要么显式承认存在一类受约束的本地 UI 状态——**不能让它悄悄长出第二套状态机**，那正是 ADR-0001 要规避的双接线。
- **滚动是 UI 级的**：TUI 占用 alternate screen，终端回滚不参与；transcript 自绘换行（unicode-width，词边界断开、宽字符之间可断、仅超宽词硬断）并缓存换行后的行表（`Model::relayout`，模型或宽度变化时重建），滚动偏移与消息跳转地址化真实渲染行。follow 态骑在尾部；任何上滚打破 follow，`End` 或滚回尾部恢复。
- **滚动条与鼠标**：transcript 最右列是常驻预留的 gutter（换行按 `width − 1`；bar 显隐不改换行宽度，缓存不在滚动中途重排——与 qwen-code `VirtualizedList`「列常驻、不 reflow」同裁决）。有溢出时画比例滚动条：thumb `█`、track `│`，`thumb = track² / total`，位置随 offset 成比例。滚轮每 notch 3 行（qwen-code 的 `WHEEL_LINES_PER_TICK`）；bar 上左键按下/拖拽按 track 行绝对定位窗口，拖到底行回 follow（sticky-bottom 同 qwen-code）。左键单击工具/思考单元格逐格展开/折叠其内容：单格覆盖（`Cell.expanded`）优先于默认（reasoning 跟 Ctrl+R 全局折叠，tool 默认展开），Ctrl+R 翻转全局折叠并清除全部单格覆盖——主开关一动，例外归零。与 qwen-code 的差异：bar 常显而非空闲自动隐藏——常驻的轨是「上面还有历史」的 affordance。鼠标捕获开启期间，终端自带的文本选择需 Shift。
- 主题：语义色层（accent/text/dim/faint/success/warn/error/code/tool/thinking）× dark/light 两套调色板；`ui.theme = auto|dark|light`（配置播种）+ `/theme` 前端本地切换；`auto` 用 OSC 11 背景查询探测（150ms 上限），回退 `$COLORFGBG`，再回退 dark。
- 动画：250ms tick 驱动 braille spinner、turn 秒数与吐司过期；仅 turn 活跃或工具 in-flight 时 tick 才脏化重排。
- 斜杠命令：`/mode /effort /model /theme /prompt(查看导出) /rewind /branch(list|switch|delete) /edit(选择历史消息编辑) /approval(规则管理) /export /clear /quit`。**今天存在的只有 `/mode /effort /model /theme /prompt /quit`**，其余六个随 M2 Phase 6。每条新命令要动六处（`SlashCommand` 变体、`parse` 的名字匹配臂、参数解析器、`actions()` 臂、`local_reply()` 的用法臂、`submit_line` 的 match）。**回复不再被丢掉**（2026-10-07）：`submit_line` 把每个回复交给 `project_reply(command, method, reply)`，它返回一个 `Reply`（`Nothing` / `Note` 吐司 / `Prompt` 转写格）——`/prompt` 渲染成 `CellKind::Prompt`（字形 `≡`，逐节 `id · source` + 正文，不可折叠：折叠一个「给你看的东西」没有意义），`/effort` 与 `/model` 吐司回报 daemon **实际生效**的值而不是用户敲的值（解码失败的回复报错而不是静默吞掉，ADR-0009）。所以 `/rewind` 与 `/branch` 只需要给 `Reply` 加变体：`session/branch/list` 的 `Vec<BranchNode>` 渲染成节点表、`session/rewind` 的 `RewindReport` 给用户看见，都不再要求先改 `actions()` 的形状。
- 审批 UI：内联弹层展示 ApprovalRequest（含 diff/命令预览），快捷键 1-4 对应四个 option（M2 Phase 6）。预览载荷的形状由 **D14** 在 Phase 2 定（建议给 `ApprovalRequest` 加可选的结构化 preview，而不是新开一个 `approval/details` 方法——ACP 的 `request_permission` 要同一份内容，放请求里一次到位）。今天的地基是空的：`Model::push_event` 的 `ApprovalRequested` 臂只把状态栏文字改成 `awaiting approval`，`request_id` 与整个 `ApprovalRequest` 都丢弃；`CellKind` 没有 Approval 变体；`layout()` 返回固定的六行分割、没有弹层槽，`draw()` 也没有 z-order/`Clear` 通道；`key_input` 没有模态分支，`1`-`4` 会被 composer 当普通字符吃掉；`Esc` 硬接 `session/cancel`，而代码注释已把「审批等待期间弹层拥有 Esc、空闲 Esc 是误触」写成约定，交接未实现。绘制本身不需要新依赖：ratatui 0.30 的 `Clear` 在默认 feature 里，居中弹层 = 一次后置的 `Clear` + widget pass（或 `layout()` 的第七个约束，取决于 D18）。
- diff 视图（M2 Phase 6，**D11 已裁决**）：用 `similar` 计算 hunk，用**现有主题语义角色**渲染（`+` 行 / `-` 行 / hunk 头 / 文件名各自映射到一个语义角色，具体配色随实现定），**不引 syntect**。理由：① diff 视图要的是 +/- 着色与 gutter，不是 token 着色，语义色层已经在；② syntect 重（一个 regex 后端 + syntax sets），而 D5 否掉 termimad 的理由之一正是这种依赖耦合；③ 房内先例——atomcode 用 `similar = "2"`（`references/atomcode/Cargo.toml`）。代码块的语法高亮是**另一个决定**，继续后排（开放问题 1）。`markdown.rs` 记的那笔表格列对齐欠账随本条一起还：它当初卡住的正是「列宽对齐需要视口宽，与单元格换行缓存的宽度无关性冲突」。
- **分支移动后的历史重建：不加新事件**。`/rewind` 与 `/branch switch` 会把活动分支头移到一个不是当前投影后继的位置。**不为此新增事件类型**（新增事件 `type` 属协议 major bump）：`SessionUpdated.state.active_branch_head` 已经在广播里，前端发现自己投影的 head 不是它的后继，就发 `session/load` 重建 transcript；发起方本来就在自己那次调用的回复里拿到新 Session，不需要额外通知。
- reasoning 展示：默认折叠为「∴ Thought for n chars (click or Ctrl+R to expand)」(M1 决策：按字符计数，逐字节纪律优先于估算)，`Ctrl+R` 展开或点击该单元格展开；尊重配置 `ui.show_reasoning`——**它的 builtin 默认是折叠**（2026-10-07 裁决：推理是过程不是答案，长链条会把答案顶出屏幕），CLI 读不到该键时的兜底与之一致。
- **item 投影（2026-10-07 补上，此前完全缺失）**：`Model::push_history` 把 `session/load` 回复里的 `items`（active 分支、oldest first）逐条投影成单元格——`UserMessage`→User、`AssistantMessage`→Assistant、`Reasoning`→Reasoning、`ToolCall`→带其记录状态的 Tool、`ToolResult`→折进对应工具格的 tail，`Checkpoint`/`Compaction`/`ModeSwitch`/`BranchNote` 跳过（它们不是对话）。live 流走 `push_live_item`，**只投影 delta 表达不了的东西**：助手正文与推理已经逐 token 在屏幕上了，再投影一次就每句话都双份；工具格由 `ToolCallStarted` 开、由 `ItemFinished` 收尾；剩下的就是**别的客户端敲的用户消息**——此前它压根不上屏，于是「两个前端消息流一致」是假的（一边有问题、一边只有答）。自己那条靠 turn id 去重（`mark_submitted(SessionPromptResult.turn)`，item 的 `turn` 与之相同即跳过），不用文本比较：两个客户端完全可以发同一句话；`turn` 为 `None` 的 item 永不被抑制。
- 已知边界（未修，各有归属）：① **投影丢掉附件**——`Content.parts` 里的 `Image`/`Resource` 不渲染，只显示 `text`，转写格没有附件的形状（M2 Phase 5 的多模态输入落地时必须一起补）；② **daemon 从不分页**——`SessionManager::load` 恒回 `next_cursor: None`（"M1 serves the whole branch in one page"），所以 CLI 那条跟随游标的循环今天在生产里不可达，它是按协议契约写的（游标是契约不是提示，忽略它的客户端会在 store 长到分页那天静默截断历史），并靠注入的分页源测试；③ **订阅与快照之间有个窗口**——`EventStream::subscribe` 先登记订阅者再取 load 快照，落在窗口里的 item 可能既在 `items` 里又以 live `ItemFinished` 到来，于是显示两次；这是 daemon 侧的顺序/游标问题（不是前端去重能干净解决的：按 item id 去重要求无界状态），记为 design/daemon.md 的开放问题。

键位与鼠标（M1 定稿，2026-10-05）：

| 键 | 动作 |
|---|---|
| `Enter` / `Alt`/`Shift+Enter` | 发送 / 换行 |
| `Ctrl+R` | reasoning 折叠切换 |
| `PgUp` / `PgDn` | 上/下翻一页（打破 follow；滚回尾部恢复） |
| `Home` / `End` | 顶部 / 恢复 follow |
| `Ctrl+↑` / `Ctrl+↓` | 跳到上/下一条用户消息边界并对齐窗口顶部 |
| 滚轮上 / 滚轮下 | 上/下滚 3 行（打破 follow；滚回尾部恢复） |
| 滚动条左键按下 / 拖拽 | 按 track 行绝对定位窗口；拖到底行恢复 follow |
| 左键单击工具 / 思考单元格 | 展开/折叠该单元格内容（单格覆盖；Ctrl+R 为 reasoning 全局开关并清除覆盖） |
| `Esc` | turn 进行中发 `session/cancel`（空闲为误触，无副作用）。**M2 Phase 6 起交接**：有审批等待时 Esc 归弹层（具体语义随 D18 定），不再直接发 cancel |
| `Ctrl+C` | 退出 TUI |

- 键位与交互细节在 M1 实现中定稿；TUI 文案全部走 gettext catalog（platform.md，M4 落地）。

### 2.3 headless exec

- 无 TTY 时自动降级为流式纯文本输出。
- `--json`：逐行输出协议事件子集（item 级），供 SDK/脚本消费；退出码区分 completed/failed/cancelled（`Outcome` = 0/1/2；Ctrl-C 经 `session/cancel` 落到 cancelled）。
- **Code 模式：M2 Phase 6 必须裁决（D16）。** 今天 exec 只能跑 Chat 会话——`open_subscribed` 硬编码 `SessionModeId::chat()`，`ExecArgs` 只有 `prompt / json / session / workspace / model / state_dir`：没有 `--mode`，没有任何审批策略标志（`--approve`/`--deny` 一类），也没有从 stdin 读 prompt 的路径。而 `ApprovalRequested` **在** `--json` 的转发子集里（item 级），所以脚本会看到审批事件、然后这一轮**永久挂住**（直到下一次调用撞上 120 s `CALL_TIMEOUT`，或事件流结束）。二选一：① 加 `--mode` + 审批策略标志，策略要能表达 allow-once / allow-always / deny 与超时行为（fail-closed 是审批管线本身的纪律，exec 不能例外）；② 明确「exec 拒绝 Code 会话」——被要求以 code 模式起会话时报错退出，而不是挂住。两者都比现状好：现状是最坏的一种失败形态（静默、无退出码、脚本只能等超时）。
- 审批进了 exec 之后，退出码矩阵要不要为「审批被拒」单开一档（testing.md §3.8 已预留这一格），随 D16 一起定。

## 3. GTK 桌面端（hatchery-gui）

### 3.1 窗口结构（libadwaita）

```
AdwApplication
└── AdwNavigationSplitView（自适应：宽屏双栏 / 窄屏单栏）
    ├── 侧栏：会话列表（搜索、置顶、删除）＋「新会话」（选 Chat/Code、工作区、模型）
    └── 主区：
        ├── HeaderBar：模式切换器（AdwViewSwitcher）、模型/effort 菜单、分支指示器
        ├── 消息列表（GtkListView + 虚拟滚动）：markdown 渲染、代码高亮、
        │    reasoning 折叠区（AdwExpanderRow）、工具卡片（含 diff 视图 gtksourceview5）
        ├── 输入区：GtkTextView + 附件/引用
        └── 弹层：审批 Dialog（AdwAlertDialog，展示 diff/命令）、Toast 通知
设置窗（AdwPreferencesWindow）：
  Provider/模型、reasoning（effort+展示）、审批规则管理、prompt 查看与编辑（入口）、
  存储（导出 JSONL、检查点磁盘占用、GC）、i18n（语言选择）、外观
```

### 3.2 异步桥接（ADR-0008）

```
tokio worker 线程: DaemonClient 事件循环
      │ glib::Sender（跨线程 channel）
      ▼
GTK 主循环: 收事件 → 更新 gio::ListStore / AdwExpanderRow 等模型 → 视图自动刷新
```

- 纪律：GTK 线程只做 UI 模型更新，不 await；tokio 线程不 touch GTK 对象。
- delta 事件在 tokio 侧按 16ms 聚合再进 glib channel（与 daemon hub coalescing 二级配合，避免 UI 抖动）。
- 应用退出 ≠ 会话结束：关窗后 daemon 继续跑 turn，重开窗口 replay 恢复；托盘/后台指示 M4+。

### 3.3 特色视图（对应差异化功能）

- **分支时间线**：树状可视化 item 分叉（worklog 里的 M4 任务），右键切换/删除/标注分支。
- **rewind 面板**：选 turn → 展示该 turn 的检查点 diff → 选 `RewindScope` 执行。
- **prompt 查看器**：渲染 `prompt/render` 结果，分 section 展示来源（默认/用户覆盖/AGENTS.md），可一键复制。

### 3.4 i18n / RTL / a11y

- 全部用户可见文案 `gettext!("…")`；`.po` 位于 `po/`，与 CLI 共用 catalog（platform.md）。
- RTL：不做任何硬编码方向；布局用 libadwaita 方向感知组件；测试用 `GTK_TEXT_DIR=rtl` 跑截图冒烟。
- a11y：组件设置 accessible roles/labels（libadwaita 默认较好），M4 检查清单。

## 开放问题

1. ~~TUI 的 markdown/diff 渲染库选型（termimad? syntect 自绘?）~~ **已裁决（2026-09-30，D5）**：ratatui + minimad 自绘。实证记录见 `docs/worklog/cli.md`；syntect（代码块/diff 高亮）推迟 M2。**2026-10-07 更新（D11）**：M2 的 diff 视图又重新评估了一次 syntect，**在 diff 这一项上否决**——改用 `similar` 算 hunk + 现有主题语义角色渲染（§2.2，理由三条记在那里）。于是 syntect 只剩下「代码块语法高亮」这一个用途，作为一个**独立的后续决定**继续挂着，不在 M2 范围内。
2. GTK 消息列表在超长会话（10k items）下的虚拟化与增量渲染性能——M4 用 fixture 压测。
3. 图片附件的输入路径（粘贴/拖拽/文件引用）CLI 与 GUI 的一致性——**M2（2026-10-07 用户裁决保留在 M2）**，且与 llm 侧的多模态工作（Phase 5）耦合：协议侧 `ContentPart::Image { mime_type, data }` 已经在，缺的是 llm 侧到 wire 的翻译 + CLI 侧的图片输入路径。**两半必须一起落**，否则图片进得了库却发不出去。GUI 侧的一致性仍随 M4。
4. CLI 是否需要 REPL 极简模式（无 TUI 依赖，SSH 友好）——倾向 M1 顺手做（exec 已覆盖大半）。
