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
- `hatchery exec "task"`：headless，stdout 人类可读或 `--json` JSONL（脚本/CI/SDK 场景，借鉴 codex exec）。
- `hatchery acp`：ACP server 入口（design/acp.md）。
- `hatchery daemon {start|stop|status}`；`hatchery doctor [--provider X]`（llm.md §7 实测探测）。
- `hatchery sessions {list|resume|export|delete}`。

### 2.2 TUI 布局（ratatui）

```
┌────────────────────────────────────────────┐
│ 消息流（UI 级滚动窗口，全历史可达）：        │
│   hatchery · model · effort · workspace    │  ← 开场一行会话摘要
│   > 用户原文                                │  ← accent `>` glyph
│   ◆ 助手 markdown                           │  ← accent `◆` glyph
│   ∴ Thought for n chars (Ctrl+R to expand) │  ← reasoning 折叠行
│   /✓/✗ 工具名 · 摘要 ▎detail ▎progress     │  ← 工具单元格（左竖条 + 生命周期 glyph）
├────────────────────────────────────────────┤
│ ✓/!/· 吐司（最多 3 条，5 秒过期）            │
│ ⠋ working 12s · esc to interrupt           │  ← turn 指示器（仅 turn 活跃时占一行）
│ ╭────────────────────────────────────────╮ │
│ │ > 输入区（多行、bracketed paste）         │ │  ← 圆角框 composer + 占位符
│ ╰────────────────────────────────────────╯ │
│ enter send · pgup/pgdn scroll · …          │  ← 键位提示行
│ chat · model · effort · state   ↑ n/m …    │  ← 状态行（底部；非 follow 时右侧显示位置）
└────────────────────────────────────────────┘
```

- 渲染原则：TUI 是事件流的投影（view projection），无本地状态机复制；所有动作发协议方法。`draw(&Model, Frame)` 是纯函数，TestBackend 黄金帧验证；widget 层（`tui/widgets/`）每个表面一个模块：transcript / composer / status / indicator / toasts。
- **滚动是 UI 级的**：TUI 占用 alternate screen，终端回滚不参与；transcript 自绘换行（unicode-width，词边界断开、宽字符按两列）并缓存换行后的行表（`Model::relayout`，模型或宽度变化时重建），滚动偏移与消息跳转地址化真实渲染行。follow 态骑在尾部；任何上滚打破 follow，`End` 或滚回尾部恢复。
- 主题：语义色层（accent/text/dim/faint/success/warn/error/code/tool/thinking）× dark/light 两套调色板；`ui.theme = auto|dark|light`（配置播种）+ `/theme` 前端本地切换；`auto` 用 OSC 11 背景查询探测（150ms 上限），回退 `$COLORFGBG`，再回退 dark。
- 动画：250ms tick 驱动 braille spinner、turn 秒数与吐司过期；仅 turn 活跃或工具 in-flight 时 tick 才脏化重排。
- 斜杠命令：`/mode /effort /model /theme /prompt(查看导出) /rewind /branch(list|switch|delete) /edit(选择历史消息编辑) /approval(规则管理) /export /clear /quit`。
- 审批 UI：内联弹层展示 ApprovalRequest（含 diff/命令预览），快捷键 1-4 对应四个 option（M2）。
- reasoning 展示：默认折叠为「∴ Thought for n chars」（M1 决策：按字符计数，逐字节纪律优先于估算），`Ctrl+R` 展开；尊重配置 `ui.show_reasoning`。

键位（M1 定稿，2026-10-04）：

| 键 | 动作 |
|---|---|
| `Enter` / `Alt`/`Shift+Enter` | 发送 / 换行 |
| `Ctrl+R` | reasoning 折叠切换 |
| `PgUp` / `PgDn` | 上/下翻一页（打破 follow；滚回尾部恢复） |
| `Home` / `End` | 顶部 / 恢复 follow |
| `Ctrl+↑` / `Ctrl+↓` | 跳到上/下一条用户消息边界并对齐窗口顶部 |
| `Esc` | turn 进行中发 `session/cancel`（空闲为误触，无副作用） |
| `Ctrl+C` | 退出 TUI |

- 键位与交互细节在 M1 实现中定稿；TUI 文案全部走 gettext catalog（platform.md，M4 落地）。

### 2.3 headless exec

- 无 TTY 时自动降级为流式纯文本输出。
- `--json`：逐行输出协议事件子集（item 级），供 SDK/脚本消费；退出码区分 completed/failed/cancelled。

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

1. ~~TUI 的 markdown/diff 渲染库选型（termimad? syntect 自绘?）~~ **已裁决（2026-09-30，D5）**：ratatui + minimad 自绘。实证记录见 `docs/worklog/cli.md`；syntect（代码块/diff 高亮）推迟 M2。
2. GTK 消息列表在超长会话（10k items）下的虚拟化与增量渲染性能——M4 用 fixture 压测。
3. 图片附件的输入路径（粘贴/拖拽/文件引用）CLI 与 GUI 的一致性——M2。
4. CLI 是否需要 REPL 极简模式（无 TUI 依赖，SSH 友好）——倾向 M1 顺手做（exec 已覆盖大半）。
