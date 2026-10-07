# 工作记录：测试体系（横切）

- 范围：测试纪律与分层、hatchery-testkit、fixture 与录制、CI 门禁、覆盖率、fuzz/属性/基准、手动实测清单
- 设计文档：[../design/testing.md](../design/testing.md)
- 相关 ADR：全部（每条 ADR 的「后果」节都有对应测试面）；门禁实现在 `scripts/ci.sh` 与 `.github/workflows/`

## 当前状态

**M0a 的门禁与分组基建已落地并本地验证**：`scripts/ci.sh`（本地与 CI 同一条命令）、`.config/nextest.toml`（default/ci/invariants/slow/gui/live 六个 profile）、`clippy.toml`（不变量 4 的编译期强制）、`xtask coverage` 包装。两个 spike 的结论已经沉淀为常驻测试（store 12 项、capabilities 11 项，`cargo nextest list` 实测）。三平台 CI **首跑已失败一次**（windows job 的 rustup 安装命令写错，已修，见「实测记录」），等待重跑。

**M2 已重新规划（2026-10-07，Phase 0–8）**：本文件「待办」里的 M2 条目已按 Phase 重标，勘察依据见 [../roadmap.md](../roadmap.md) M2 节的「勘察更正」。两条**门禁完整性**发现要先说，因为 M2 的 DoD 写着「硬门测试全绿」而今天的门禁证明不了这句话：① `--profile invariants` 在 ci.sh 与两个 workflow 里**从未被调用**，是死配置（不变量测试只是搭 `ci` 继承 `default` 的车跑到，没有可单独报告或阻塞的门禁）；② `check_i18n` 是一条 printf 空操作，此前被算作通过的门禁步骤。加上**不变量 5 的测试在任何形态下都不存在**，三者都在 Phase 0/Phase 2 收口，细节见下面同日变更日志。

## 待办

- [x] (M0) workspace 加 `hatchery-testkit` crate（dev-only，`publish = false`）；nextest/insta/proptest 进 workspace 依赖；`.config/nextest.toml` 分组
- [x] (M0) CI 骨架：`pr.yml`（三平台，全部调 `scripts/ci.sh`）+ `nightly.yml`（slow/gui/audit/coverage + 后续 TODO 注释）
- [x] (M0) clippy `disallowed_methods`/`disallowed_types` 配置 + 作用域实测（见下「实测记录」）
- [x] (M0) 两个 spike 的结论沉淀为回归测试：`crates/hatchery-store/tests/spike_engine.rs`、`crates/hatchery-capabilities/tests/spike_shadow_git.rs`（i18n spike 是纯选型裁决，M4 落地时才有可测物）
- [x] (M0) `cargo-llvm-cov` 未装时 `xtask coverage` fail-loud 报安装命令（不静默跳过）
- [x] (M0) 三平台 CI 首跑通过（第 2 次尝试；windows MSYS2 ucrt64 那条也过了）
- [x] (M0b) store 属性测试参考模型（testkit 里独立写的 `ReferenceTree`）+ kill -9 崩溃测试框架（测试二进制自重入，无需专用二进制）
- [ ] (M1) testkit 余下部分：MockWire(sse fixture)/Memory 后端三件套/TempWorkspace/TestDaemon/ClientProbe（kernel 四接缝的 fake 与 `ReferenceTree` 已于 M0b 交付）——**MockWire + fixture 加载（09-30）、MemoryFs + TempWorkspace 与 TestDaemon + ClientProbe（10-01）已交付；MemoryTerminal 随 M2 Phase 4 的 PTY（今天不存在，见下面 Phase 4 那条）**
- [x] (M1) 建 `hatchery-tests` 成员 crate（跨 crate e2e 与不变量套件的家；2026-10-01 落地，13th member + layering 表同步）
- [x] (M1) MSRV job 进 nightly（`cargo +1.90.0 check --workspace --all-targets --locked`；本地 1.90.0 工具链实测通过），防依赖升级悄悄抬高下限
- [x] (M1) fixture 录制 xtask + 脱敏（API key 扫描）+ provenance 元数据格式（2026-09-30：`cargo xtask record-fixtures`，sidecar `*.meta.json`，扫描命中即拒写）
- [x] (M1) 不变量 1/2 落地（2026-10-01）：`invariants` 分组从 4 条涨到 8 条——新增客户端代际过滤、会话租约拒绝、20 线程单实例竞态、逐字节 reasoning 回放；5（硬门）与 3/6 的 store/tool 层补充随 M2
- [x] (M1) e2e 场景 1-2 落地（2026-10-01，hatchery-tests）：最小对话 + resume + 逐字节回放、双前端扇出 + 重连补差；D7 子进程对比一并交付
- [x] (M2 Phase 0，2026-10-07 完成) **让 `invariants` profile 真被调用**：`scripts/ci.sh:134` 只跑 `--profile ci`，pr.yml/nightly.yml 也没有 `--profile invariants`（nightly 只有 slow/gui/audit/coverage + `msrv` job）——那个 profile 是死配置。不变量测试确实跑到了（`ci` 继承 `default`，`default-filter` 只排除 `live_`/`slow_`/`gui_`），但没有可单独报告或阻塞的门禁。**结果**：`scripts/ci.sh` 加了 `run_step invariants cargo nextest run --workspace --profile invariants`（在 `tests` 之后）。重复跑是有意的：命名步骤才可单独报告与单独 skip，而过滤器选空时 nextest 默认 `fail`，前缀约定一漂移就会红而不是静默空跑
- [x] (M2 Phase 0，2026-10-07 完成) **前缀对账**：design/testing.md §5 映射到不变量、却没有 `invariant_` 前缀的 5 条测试（`two_concurrent_prompts_yield_exactly_one_turn`、`a_turn_with_no_subscriber_runs_to_completion_and_persists`、`no_gitlink_is_planted_in_the_user_workspace`、`purge_restore_also_removes_never_tracked_files`、`events_below_the_session_generation_are_dropped`）改名，或把 profile 的过滤器换成显式清单——否则一旦真跑 `--profile invariants`，它们会被静默漏掉。**结果**：五条全部改名（清单与新名见 design/testing.md §5），选择改名而不是显式清单——清单会在 §5 与 `nextest.toml` 两处重复同一份知识并各自腐烂，前缀约定才是本项目本来就有的机制。改完实测 profile 选中 13 条全绿。**代价**：ADR-0012 正文引的是 `no_gitlink_is_planted_in_the_user_workspace` 旧名，ADR 不改，映射记在 worklog/capabilities.md
- [x] (M2 Phase 0，2026-10-07 完成) `check_i18n` 改为诚实的 skip 而不是门禁步骤（ci.sh:121-124 打印「i18n extraction check lands in M4 — nothing to verify yet」后返回成功）。**结果**：`check_i18n` 函数与 `run_step i18n` 一并删除，改成脚本末尾打印 `--- i18n: not gated yet (extraction lands in M4)`，`--help` 的步骤表照实列 `toolchain fmt clippy build tests invariants doctests determinism` 并单列一行「Not gated yet: i18n」
- [x] (M2 Phase 0，2026-10-07 完成) `clippy.toml` 补不变量 4 的洞：`tokio::fs::*` / `tokio::process::*` 未禁（tools crate 依赖 tokio、`LocalFs` 自己就用 tokio::fs，一句 `tokio::fs::write` 绕过整条纪律）+ 漏掉的 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`。**结果**：`tokio::fs` 的全部孪生项与那四个 `std::fs` 项已加，并逐条实测（见 worklog/capabilities.md）；`tokio::process::Command` 的孪生项**故意没加**——没有 crate 开 tokio 的 `process` feature，clippy 对这类路径回 "does not refer to a reachable function"，实测全 workspace `-D warnings` 仍退出 0（配置诊断不是 lint），但会在每次门禁留五条警告
- [x] (M2 Phase 0，2026-10-07 完成) 覆盖率表加 `hatchery-tools`（M2 四个新工具全落在那里，而 `xtask/src/coverage.rs:17-25` 只闸七个 crate）；`the_threshold_table_covers_the_seven_gated_crates`（coverage.rs:283）把「七」钉住了，加 crate 要同步改那条测试。**结果**：地板取 85%（与其他产品核心 crate 同档），**先实测再定**——加入前 `cargo xtask coverage --report-only` 报 hatchery-tools 95.6%，所以不是愿望数字。那条测试改名为 `the_threshold_table_covers_the_gated_crates`（测试名里的计数正是要从活文档里拿掉的那类东西）
- [ ] (M2 Phase 1) `TempWorkspace` 补 git init + 文件树 DSL（design §2 一直这么描述；今天只有 `new`/`path`/`write`/`fs()`/`root()`），并把不变量 6 的测试从 spike 的 `Sandbox` 迁到真 `CheckpointStore`——它要的「脏用户仓库」（staged/unstaged/untracked + 一次 commit）今天由 spike 自己的 harness 手搭
- [ ] (M2 Phase 1) `MemoryFs` 补写路径（`FsBackend` 一加 `write_text_file` 它就编译不过——是有用的 forcing function，也是工作量；今天它只实现三个读方法）
- [ ] (M2 Phase 2) **`invariant_project_config_cannot_disable_hard_gates`**（M2 DoD「硬门测试全绿」的正主，今天零命中）+ `config.rs:154` 那个空的 `STRICT_KEYS` 接线。前置是审批规则配置本身：没有它就没有「恶意项目配置」可加载，所以排 Phase 2 不是 Phase 0。工作区内硬门（`.env*`、`.git/hooks`）按裁决走 `ApprovalRequest::once_only()`（新增 `RiskLevel` 变体属协议 major bump；kernel 已拒绝不在所给选项里的答案，所以 `once_only` 真不可绕）
- [ ] (M2 Phase 2) testkit 补 **fake `ApprovalGate`**（trait 全 workspace 零实现；`testkit::Gate` 是无关的计数信号量）与**协议级审批应答器**（现有 `answer_approvals` 拿 `AgentHandle`、绕过协议，测不到 `approval/respond`；要的是驱动 `ClientProbe` 对 `ServerEvent::ApprovalRequested` 作答的等价物）
- [ ] (M2 Phase 2) 契约测试套件：`FsBackend` / `TerminalBackend` / `ApprovalGate`（`LlmProvider` / `SessionStore` 排 Phase 7）
- [ ] (M2 Phase 4) `MemoryTerminal`（design §2 与 `testkit/src/lib.rs:19` 都写着 M1–M2 交付，实际不存在；没有它 `shell` 工具没有单测后端）
- [ ] (M2 Phase 4) PTY 孤儿进程测试（`kill -0` 断言，design §3.5）随 `LocalPty` 落地；三平台（含 windows-gnu 的 Job Object 语义）的孤儿进程/取消/输出流实测是 D10 spike 的产出，spike 提前到 Phase 1 并行做
- [ ] (M2 Phase 7) e2e 场景 3-6（Chat→Code 切模式 / 编辑分叉重演 + 分支删除 / 写坏文件 → rewind 三 scope / 审批全链路含 fail-closed 超时与 AllowAlways 生效）；**前置是 toolcall SSE fixture 对 `hatchery-tests` 可达**——`sse_fixture` 按调用方 crate 的 `CARGO_MANIFEST_DIR` 解析，录制都在 `crates/hatchery-llm/tests/fixtures/`，而 `hatchery-tests` 连 `tests/fixtures/` 目录都没有，唯一的 SSE 是 `src/support.rs:27-37` 的内联字符串且不含 `tool_calls` delta
- [ ] (M2 Phase 7) 契约测试套件补齐 `LlmProvider` / `SessionStore`
- [ ] (M2 Phase 7) `disallowed_methods` 的 compile-fail 测试（trybuild 类）——人工探针复测过（10-01），且禁令已由 hatchery-tools 本地 `[lints]` 表真实生效（crate 属性压不过 Cargo lint 表，09-28 的记录写反了，见 10-01 条目）
- [ ] (M2 Phase 7) fuzz targets 上线 nightly（SSE 帧解析、JSON-RPC 帧解码、item payload serde、**LocalFs 逃逸面**——M2 让它变大了）；criterion 基线入库 + nightly 对比；cargo-mutants 试点（store/kernel）；`hatchery config schema` 导出 JSON Schema。**2026-10-07 用户裁决：这三样留在 M2 Phase 7，不再写 M2+**（今天树里没有 `fuzz/` 目录也没有 criterion 依赖，nightly.yml 里它们仍是 TODO 注释）
- [ ] (M2 Phase 7) 两个真实小洞：`walk.rs` 的注释声称「不可读子目录 skip-and-count」由真盘测试钉住，`crates/hatchery-tools/tests/assembly.rs` 里其实没有 0 权限目录的测试；`bump_generation` 在 store crate 内无直接测试（现仅 daemon 侧调用方覆盖）
- [ ] (M2) **Code 会话的事件量测量一次并记档**（`hub.rs:4` 的原意）——coalescing + replay window 已顺延 M3，M2 只产出这份测量当策略依据；相应的 coalescing/replay-window 测试（含 §6 那条虚拟时钟 proptest）一起移 M3
- [ ] (M3) fake 宿主集成 + dummy-acp-agent；e2e 场景 7-8；Zed 真机清单执行并记录；hub coalescing + replay window 的测试（2026-10-07 从 M2 顺延）
- [ ] (M4) gui 组进 CI（GNOME SDK 容器 + xvfb）；10k 性能 fixture；e2e 场景 9

## 实测记录 · M0b（2026-09-28）

测试基建在 M0b 的实测结论：

- **nextest 下「测试二进制自重入」可行**：`std::env::current_exe()` + 参数 `--exact <name> --ignored --nocapture` 起子进程，子进程跑一个 `#[ignore]` 的测试。崩溃恢复测试就是这么做的（6 项全绿），代价为零个新增 target。**这正是 design/testing.md 开放问题 1 的答案**。
- **`Child::kill()` 就是我们要的那种「杀」**：unix 发 SIGKILL、Windows `TerminateProcess`，都是不给析构机会的终止。断言里加一条「子进程必须非正常退出」以防它自己跑完（那会让实验失去意义）。
- **proptest 的失败种子要入库**：`*.proptest-regressions` 由 proptest 自动写，官方建议提交——它记录的是真实失败过的最小脚本。
- **异步代码里的谓词等待必须带超时**：`RecordingSink::wait_for` 用 `tokio::time::timeout`（5 s）+ `watch` 通道（不是 `Notify`：检查与等待之间发生的通知会丢，而版本号不会）。测试挂死比测试失败难查得多。
- **数据驱动的 golden 不能用 insta**（详见 design/testing.md §3.1）：快照名要求字面量。用纯 JSON + `UPDATE_FIXTURES=1` 生成器，并让生成器在 `INSTA_UPDATE=no`（门禁导出的变量）下拒绝运行。
- 门禁仍是一条命令：`./scripts/ci.sh` 本地 6 秒跑完全部步骤（含 219 个 nextest 测试的默认组与 7 个 doctest）。
- **覆盖率未测**：本机没有 `cargo-llvm-cov`，`cargo xtask coverage` 按设计 fail-loud 报安装命令（阈值 enforcement 仍排在 M1）。

## 实测记录（2026-09-28）

门禁与工具链的机制性结论（都影响后续写法，值得记）：

- **clippy `disallowed_methods` 属 `clippy::all`（默认 warn）**：`clippy.toml` 的清单对**全 workspace** 生效，配合 `-D warnings` 会把 xtask/daemon/store 里合法的 `std::process::Command`、`std::fs` 打成错误。可行方案 = workspace `[lints.clippy]` 里设 `disallowed_methods = "allow"` + 在 `hatchery-tools` crate 根用 `#![deny(...)]` 覆盖（crate 属性优先于命令行 lint level）。**已实测**：在 tools 里放一个 `std::fs::read_to_string` 探针 → `error: use of a disallowed method`；xtask 同样的代码干净通过。
- 作用域只限 `hatchery-tools`：`hatchery-acp` 要 spawn 外部 agent 子进程（design/acp.md §2），不在不变量 4 的「工具实现」范围内。
- **nextest 空 profile 默认失败**：`--no-tests` 的默认值是 `auto → fail`（退出码 4，实测 invariants/slow/gui/live 四个空 profile 全部 exit=4）。所以 nightly 跑空的 slow/gui 组必须显式 `--no-tests=warn`；而 `ci` 组保持默认，「一个测试都没跑」正好被当成红灯（fail-loud）。
- **Rust 无法按属性过滤测试**：原设计的 `#[live]`/`#[slow]`/`#[gui]` 标记不可行 → 改命名前缀（`live_*`/`slow_*`/`gui_*`/`invariant_*`）+ `default-filter`；live 另加 `live-tests` cargo feature 双保险。design/testing.md §1 已改写。
- **`cargo fmt --all` 只走模块树**：一个没被 `mod` 声明的孤儿 `.rs` 文件不会被格式化（实测：故意写坏的孤儿文件逃过 `fmt --check`）。写「格式违规」类测试时要用真实模块内的文件。
- **`scripts/ci.sh` 的失败路径已实测**：真实格式违规 → `--- fmt: FAILED`、脚本继续跑完其余步骤（一次暴露所有问题）、最终 exit=1；`--quick`、`SKIP_STEPS`、未知参数（exit=2）都验证过。
- bash 陷阱：`printf '--- %s'` 会被当成选项解析（格式串以 `-` 开头），必须写 `printf '%s\n' "--- …"`。ci.sh 里踩过一次。
- 虚拟 manifest 的根目录不能有顶层 `tests/` → 跨 crate e2e 与不变量套件要一个真实成员 crate（`hatchery-tests`，M1 建）。

## 实测记录 · CI 首跑（2026-09-28，windows job）

首跑在「Install rustup with the windows-gnu host」这一步 1 秒内失败：`error: unexpected argument 'clippy' found`。四条机制性结论（全部在临时 RUSTUP_HOME/CARGO_HOME 里复现过，不是读文档得来的）：

1. **`rustup-init --component` 只接受单个逗号分隔值**（1.29.1 的 `--help`：`Comma-separated list of component names to also install`）。所以 `--component rustfmt clippy` 必然报参数错误；多值语法在 `rustup component add <COMPONENT>...`，不在 init。
2. **`--default-toolchain none` 时请求的组件被静默丢弃**：`warn: ignoring requested components: rustfmt, clippy`。组件必须挂在真正安装工具链的那条命令上。
3. 拆成独立的 `rustup toolchain install "$channel-$host" --profile minimal --component rustfmt,clippy` 附带解决两个隐患：**幂等**——热缓存只恢复 `CARGO_HOME/bin` 里的 rustup 二进制（RUSTUP_HOME 故意不入缓存，否则 `stable` 会与 unix job 的新 stable 漂移），原 `if ! command -v rustup` 守卫会跳过安装；**钉住 ABI**——runner 镜像自带 rustup 的默认 host 是 msvc，`rustup toolchain install stable` 会装成 `stable-x86_64-pc-windows-msvc`，整个 job 悄悄用 MSVC 编译，与 ADR-0012 的平台决策相反。现在显式 `rustup set default-host` + 全名安装 + 断言 active toolchain 里含 `windows-gnu`（负例实测 `exit=1`）。
4. `rustup show active-toolchain` 在工具链缺失时会 **auto-install** 并打 deprecation 警告（`scripts relying on this behavior in rustup may stop working in the future`）→ 只能当断言用，不能当安装手段。

本地验证边界：把 workflow 里那段 `run` 抽出来、只替换 host 三元组后真跑 → `bash -n` 通过、整步 exit=0、`cargo fmt --version` 与 `cargo clippy --version` 都答得出。本机没有 mingw 交叉 gcc，完整的 windows-gnu 编译只能靠 runner。

教训归入 design/testing.md §0.2 同一类：**命令行参数也是第三方行为**，凭记忆写就是猜。

## 开放问题

见设计文档末尾 4 条（testkit 辅助二进制形态、mutants 投入、GUI 截图 diff、e2e 进程形态）。解决过程记录于此：

- 2026-09-28 辅助二进制形态：M0 不需要（`dummy-acp-agent` 是 M3 的事），推迟到 M3 与 ACP client 一起定；倾向 workspace member + `required-features`。

## 变更日志

### 2026-10-07 · M2 Phase 0：门禁诚实化与不变量 2 的边界

**门禁**：`scripts/ci.sh` 现在有 `invariants` 步（`--profile invariants`，在 `tests` 之后），`check_i18n` 从「返回成功的空操作」变成脚本末尾一行明示未设门禁；`--help` 的步骤表照实写。五条映射到不变量却缺前缀的测试已改名，改完实测 profile 选中 13 条全绿。`hatchery-tools` 进了覆盖率表（85%，实测 95.6%）。`clippy.toml` 补齐 `tokio::fs` 孪生项与四个漏掉的 `std::fs` 项，逐条用 scratch 模块实测过（解析不到的路径是静默忽略，打错字等于留洞）；`tokio::process` 那组故意没加，理由写在 clippy.toml 头部。

**不变量 2 的边界（D15 落地）**：system prompt 注入后，场景 1 的 `invariant_minimal_chat_replays_reasoning_byte_exact` 从「整个 messages 数组整表比对」改成三段——① 两次请求的 system 文本逐字节相同（装配时渲染一次并冻结）② 它等于 `prompt/render` 的 `text`（透明性 API 说的就是模型看到的那份）③ 它**后面**的 messages 数组仍与手写期望整表比对（分支历史逐字节）。子进程 e2e 的 byte-exact 腿同步。分支历史那半的强度没有降低。

**testkit**：`ScriptedToolHost::requiring_approval_with(ApprovalRequest)`（老构造器表达不出收窄选项的请求，硬门那条分支因此结构性不可测，见 worklog/kernel.md）；`hatchery-tests/tests/subprocess.rs` 的 `spawn` helper 给子进程设 `XDG_CONFIG_HOME`，让「daemon 从环境推导覆盖目录」这条生产路径可测而不依赖宿主机（`set_var` 在 edition 2024 是 unsafe 且被 clippy 禁掉，测试拥有子进程环境是唯一形状）。

### 2026-10-07 · M2 重新规划对账

roadmap 的 M2 段重写为 Phase 0–8（依据是那一节的「勘察更正」，逐条带 file:line 实证）。测试方向的对账结论如下；**前三条是门禁完整性问题**——M2 的 DoD 写着「硬门测试全绿」，而今天的门禁证明不了这句话：

1. **`--profile invariants` 从未被调用**。`scripts/ci.sh:134` 只有一条 nextest 调用（`run_step tests cargo nextest run --workspace --profile ci`）；pr.yml 全平台调 ci.sh，nightly.yml 只有 `--profile slow --no-tests=warn`（:65）、`--profile gui --no-tests=warn`（:68）、`cargo xtask coverage`（:77）与一个 `msrv` job。`.config/nextest.toml` 里那个 profile 因此是**死配置**。不变量测试**确实跑到了**——`ci` 继承 `default`，`default-filter` 只排除 `live_`/`slow_`/`gui_`——但没有一个可单独报告或阻塞的门禁，roadmap M0 DoD 那句「nextest 默认组与 invariants 组」是假的。更糟的是前缀：design §5 映射到不变量、却没有 `invariant_` 前缀的有 5 条（`two_concurrent_prompts_yield_exactly_one_turn` invariants.rs:213、`a_turn_with_no_subscriber_runs_to_completion_and_persists` invariants.rs:310、`no_gitlink_is_planted_in_the_user_workspace` spike_shadow_git.rs:464、`purge_restore_also_removes_never_tracked_files` spike_shadow_git.rs:541、`events_below_the_session_generation_are_dropped` hub.rs ~:170），一旦真跑 `--profile invariants` 就会被静默漏掉。Phase 0：ci.sh 加真正的 invariants 步骤 + 这 5 条改名（或把 profile 过滤器换成显式清单）。
2. **`check_i18n` 是一条 printf 空操作**（ci.sh:121-124：打印「i18n extraction check lands in M4 — nothing to verify yet」然后返回成功），此前一直被算作通过的门禁步骤。Phase 0 改为诚实的 skip。顺带记清 determinism 步的真实语义（ci.sh:111-119）：`git status --porcelain` 里出现 `tests/(fixtures|snapshots)/` 或 `*.snap[.new]` 即失败——协议加字段的那几个 Phase 会照旧撞上「有意修改 → 提交前保持红」这个已知摩擦。
3. **不变量 5 的测试在任何形态下都不存在**，而它正是那句 DoD 的正主。design §5 点名的 `invariant_project_config_cannot_disable_hard_gates`（§3.5 写作 `project_config_cannot_disable_hard_gates`）全仓库零命中；`hard_gate|cannot_disable|project_config` 的六处命中没有一处是它——`RiskLevel::is_hard_gate()`（protocol/src/approval.rs:34）与其两条单测（`a_normal_request_offers_every_option` :135、`a_hard_gate_offers_no_remembered_options` :139），外加影子 Git spike 里三处无关命中；唯一相邻覆盖是 prompt 级的 `the_safety_gate_cannot_be_overridden`（daemon/src/prompt.rs:230）。**原因是前置不存在**：`config.rs:154` 的 `const STRICT_KEYS: &[&str] = &[];` 是空表（:148-153 的注释说接线进 `filter_keys` 与 typed reader「是 M2 的活」），没有审批规则配置就没有「恶意项目配置」可加载——所以它排 Phase 2，不是 Phase 0。另：`is_hard_gate()` 只认 `WritesOutside`，而 design/capabilities.md §5 的硬门含**工作区内**的 `.env*` 与 `.git/hooks`；裁决是对这些路径强制 `ApprovalRequest::once_only()`（新增 `RiskLevel` 变体属协议 major bump），kernel 已经会拒绝不在所给选项里的答案，所以 `once_only` 是真不可绕。
4. **不变量 4 的编译期门禁有洞**：`clippy.toml` 禁了 `std::fs::*` 与 `std::process::Command`（+ `process::abort`、`env::set_var`），**没禁 `tokio::fs::*` / `tokio::process::*`**——而 hatchery-tools 依赖 tokio、`LocalFs` 自己就用 tokio::fs，工具里一句 `tokio::fs::write` 就绕过整条纪律；也漏 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`。Phase 0 补。`disallowed_methods` 的 compile-fail 探针仍是开放项，落 Phase 7；注意强制机制是 hatchery-tools 自己的**本地 `[lints]` 表**（Cargo.toml:33-47），因为 Cargo lint 表是命令行 flag、压得过任何源码级 `#![deny]`，而 workspace 继承不能与本地表混用。
5. **覆盖率表不闸 `hatchery-tools`**：`xtask/src/coverage.rs:17-25` 只有 kernel/store/llm/capabilities 85、protocol/daemon 80、cli 60 七个，而 M2 的四个新工具全落在 tools；`the_threshold_table_covers_the_seven_gated_crates`（coverage.rs:283）把「七」钉住了，加 crate 要同步改它。覆盖率只在 nightly 跑（PR 报告不 block，这条纪律不变）。
6. **testkit 缺四件 M2 前置**（roadmap 更正 12）：`MemoryTerminal` 不存在（design §2 与 `testkit/src/lib.rs:19` 都写着 M1–M2 交付）→ `shell` 工具没有单测后端；没有 fake `ApprovalGate`（trait 全 workspace 零实现；`testkit::Gate` 是无关的计数信号量）；`answer_approvals` 拿 `AgentHandle`、**绕过协议**，测不到 `approval/respond`，需要一个驱动 `ClientProbe` 对 `ServerEvent::ApprovalRequested` 作答的协议级等价物；`TempWorkspace` 只有 `new`/`path`/`write`/`fs()`/`root()`，design §2 声称的 git init 与文件树 DSL 都不存在，而 CheckpointStore 的不变量 6 测试要的正是「脏用户仓库」。也没有 fake/stub `CheckpointStore` 可用来测 rewind 的 Code scope。
7. **fixture 可达性**：`sse_fixture(name)`（testkit/src/wire.rs:188，按**调用方 crate** 的 `CARGO_MANIFEST_DIR` 解析 `tests/fixtures/`，sidecar 缺失即 panic）；toolcall 录制都在 `crates/hatchery-llm/tests/fixtures/`（`deepseek-toolcall.sse`、`qwen-toolcall.sse`、`synthetic-qwen-toolcall.sse` + 各自 `.meta.json`），而 `hatchery-tests` **连 `tests/fixtures/` 目录都没有**，唯一的 SSE 是 `src/support.rs:27-37` 的内联 `SSE_REASONING_OK`，不含 `tool_calls` delta。Phase 7 的场景 3–6 要么复制/链接一份 fixture 到 `hatchery-tests/tests/fixtures/`，要么换共享机制。
8. **已经就绪、别再当欠账写**：`TestDaemon` + `ClientProbe`（testkit/src/daemon.rs）是真进程内 daemon + 真 temp UDS + 真 TursoStore + 真 LayeredConfig + 真 daemon.json，正是 design §4 规定的形态，场景 1/2 已证；`ReferenceTree`（src/model.rs）独立于 store 生产代码建模 append/edit_fork/switch_branch/delete_branch，M2 的分支语义今天就能 proptest 对拍；`MockWire` 有 `replay_sse` / `replay_sse_after(body, delay)` / `refuse_then_sse` / `refuse_always` / `sse_switched` / `requests()`。`hatchery-tests` 里现有的 scenario1 / scenario2 / subprocess / invariants 与 `e2e_daemon` bin 就是 M2 的地基，场景 3–6 加在其上。
9. **文档口径更正**：design §1 与 §3.8 仍写 insta——`d057a99`（"Drop the unused insta dependency"）已把依赖删掉，真实机制是 protocol 的纯 JSON golden 与 TUI 的 `assert!(frame.contains(…))` over `TestBackend::to_string()`。但 **`INSTA_UPDATE=no` 不能跟着删**：`protocol/tests/golden_fixtures.rs:20-22` 把它当「门禁不许改写自己契约」的闸门，生成器在它下面拒绝运行。§3.1 的断言/fixture 计数按 2026-09-30 的裁决**删除而不是更新**。design §6 的 cargo-fuzz 与 §7 的 criterion 今天都不在树里（无 `fuzz/` 目录、无 criterion 依赖）。
10. **顺延与裁决**：coalescing + replay window 的测试随实现移 M3（`is_coalescable` 只含 text/reasoning delta，而 M2 新增的事件量主要来自**不可合并**的 `ToolCallProgress`；replay window 已被 `session/load` + `replay_from` 取代且有 e2e 覆盖），M2 只产出一次 Code 会话的事件量测量并记档。**cargo-fuzz / criterion / cargo-mutants 试点留在 M2 Phase 7（2026-10-07 用户裁决，不写 M2+）**，同批还有 `hatchery config schema` 的 JSON Schema 导出。

### 2026-10-01 · 评审⑤前的全面自查（对抗性审计 → 修复 + 补测）

评审⑤等待期间做了一轮全面自查：五路并行审计（llm / daemon 核心 / daemon 基建+协议客户端 / cli+tools+capabilities / kernel+e2e+门禁），逐条核实后修缺陷、补测试。**最重要的三个发现都不在任何既有测试的视野里**：

1. **并发 prompt 竞窗（不变量 1 的第四条腿）**：manager 的 busy 检查与 submit 之间无串行化，且 `turn_running()` 要等 kernel 真正开闸才翻真——两条连接的 prompt 可以双双通过检查，第二条被 kernel 静默丢弃（返回 Ok）。修法是两层：同一会话的 prompt 路径走 per-session turn 闸门串行化；接受即以 CAS 在 runtime 上打在途标记（sink 投影 `TurnEnded` 时清除，submit 失败回滚）。`two_concurrent_prompts_yield_exactly_one_turn` 钉住；同一闸门顺带消灭了「并发冷启动双重组装」（两代 generation、两条分叉链）。
2. **订阅计数活不过 runtime 重组装**：`attach` 在无 slot 时是空操作，而正常流程是先 `session/new` 后首个 prompt（此时才装配，subscribers=0）——30 分钟后空闲清扫会卸掉一个正被观看的会话。计数挪到 slots 之外的 `watchers` 表；清扫在卸载前于锁内复核 busy/watched（清扫自身的 check-then-act 竞态一并关掉），agent 已死的 slot 也纳入清扫。manager 测试：busy 不扫、被看的不扫、看过再松手才扫。
3. **不变量 4 的编译期禁令实际处于关闭状态**：workspace `[lints]` 里 `disallowed_methods/types = "allow"`，tools 的 crate 属性从未写上；更糟的是实测证明 **crate 属性根本压不过 Cargo lint 表**（命令行 flag 优先）——09-28 那条「已实测：探针报错」的记录是在 allow 落位前做的，结论写反了。真正的修法：hatchery-tools 自带一份**完整的本地 `[lints]` 表**（workspace 继承与本地表不能混用，cargo 直接拒载 manifest——报错信息极具误导性，会指向别的 crate），其中两项 deny。探针复测：违规即 error，干净树零报错。trybuild 自动化探针仍是 M2。

**行为修复**（每条都有测试钉住）：daemon 崩溃恢复现在先用新的 `open_turns` 把未结 turn 行以失败形态关闭（此前永远停在"running"）；`store_error` 直通 `StoreError::to_event_error()`，`session_not_found` 不再被压成通用 StoreError（原测试名与断言互相矛盾，测试随行为修正）；cancel 空闲会话如实报 `cancelled:false`；`SessionPromptResult.turn` 不再是永远对不上的占位 id——TurnInput 携带调用方 TurnId 贯穿 kernel，回复与事件同一名；hub 侧代际过滤落地且装配以 `GenerationBumped` 开场（协议里一直有、daemon 从未发过）；commit 失败不再向 hub 发布 ItemFinished（invariant 2 的"先提交后发布"在失败半边也成立）；provider 注册表每次装配都整表替换（config/set 后端点/密钥变更能到达下一个 runtime）；bind 失败也走逆序拆锁（否则留下指向未服务 socket 的 daemon.json）；spawn 轮询容忍"发布先于 bind"的连接失败。

**传输层三处**：连接读循环改为整块喂 decoder（`read_line` 会先把超限行整个缓冲进内存，4MiB 上限形同虚设）；decoder 的错误路径改为 `Scan{frames, error}`——一条坏线不再吞掉同块到达的完好帧（旧语义下这正是新读循环的死锁源）；**发现并修掉每条回复后的双换行**（`encode_frame` 自带换行、server 又补了一个，所有严格逐行读者都得靠 decoder 跳空行活着）。

**cli**：turn 进行中打字不再整窗退出（调用错误降级为 notes，被拒的 prompt 文本退回输入框）；多行输入落地（Alt/Shift+Enter 换行 + bracketed paste，输入区随行数增高）；resume 用 load 回复里的真 session 播种状态栏（此前 model 空白、effort 写死 medium）；`//` 转义命令区、命令名大小写不敏感、空输入不发送；状态栏随 `SessionUpdated` 显示 thinking/awaiting approval；attach 轮询容忍 connect-before-bind。

**config**：provider 子树按 key 降级（此前一个坏子键把整个 provider 重置成内置默认）；已知 key 的类型不符在过滤层丢弃并告警、`config/set` 拒绝类型不符（此前 `ui.show_reasoning = "banana"` 被接受且 typed 视图静默取默认）；`insert_dotted` 穿过叶子时拒绝而非静默空操作；审计把"设置但为空"的 env 视同缺失；doctor 的目录检查做真实写探针；daemon.json 的 tmp 文件以 0600 创建（原为写后 chmod，崩溃窗口内可读）。

**llm**：`resolve_key` 去除粘贴进来的首尾空白；`resolve_model` 平局改判 None（HashMap 迭代序随机，平局选择=同一配置不同 run 打不同 provider）；第二个 Done 不再覆盖被延迟的第一个；`wire = "responses"` fatal 拒绝（此前配置被静默无视）；重试耗尽的报错改为真实尝试次数。

**门禁/工具**：xtask 覆盖率折叠锚定 `crates/hatchery-` 右侧（外层路径含 `crates/` 的 checkout 此前会折叠出不存在的 crate 并静默全过）+ 门禁 crate 缺报告即报错；pr.yml 删掉 `cargo clean`（它让上面的整套 rust-cache 白干）——2026-10-03 用户补齐根因后正式定案：`cargo clean` 当初（4ae1042）是对缓存被非可移植 flags 产物污染（跨代 runner 非法指令）的应急，正式修法 = 仓库与 CI 的 flags 归零、本地提速走用户级 `~/.cargo/config.toml`，细节见 worklog/architecture.md 同日条目。

**测试面**（本轮 +63）：`two_concurrent_prompts_yield_exactly_one_turn`、`a_turn_with_no_subscriber_runs_to_completion_and_persists`（D2 全链路）、信封全序 + turn id 贯穿（scenario1）、kernel 三条（RateLimited 直通、max_rounds=0 引信、句柄全掉仍收尾）、daemon 侧 sweep 双守卫 + 恢复关闭 turn + 幂等 + waiting_approval、server 三条（超限行拒绝且连接存活、坏 UTF-8 跳行、二次订阅停旧流）、entry 的 bind 失败拆锁、protocol client 七条（乱序回包按 id 路由、孤儿回包不偷包、错误对象、**最后一个 client 掉线即断连**——router 强持有 Arc 的泄漏一并修掉、陈旧代际丢弃、订阅回复与事件共连接）、llm 五条（qwen 空 id 续传钉进真录制、畸形块跳过、流末冲刷 Done、5xx 退避、responses wire 拒绝）+ registry 平局 + ModelSwitch + 温度/上限落体 + key 修剪、config 三条、tools 三条（read_file 边界、grep 超限计数、walk 跳过计数）+ TempWorkspace 真盘三条（含符号链接逃逸与自环）、capabilities 两条（目录读两后端一致、空路径）、store 的 `open_turns` 往返、覆盖率路径加固两条。

**覆盖率地板首次真实过线**（用户跑 `cargo xtask coverage` 的实测数字）：首轮 **capabilities 83.0% / cli 57.8% 低于 85/60 的地板**，其余七闸全过。空洞不在测试盲区就在真实功能上：daemon_cmd（start/status/stop，114 行 0 覆盖）与 doctor_cmd 全裸，chat.rs 的 TUI 主循环是仅剩的诚实空白（需要真终端）。补测后（本机已装 cargo-llvm-cov，enforcement 实跑）：**capabilities 86.9%、cli 65.4%，门禁全过**——cli 的新测试是 daemon_cmd 的发布真相对照表（存活 pid 绿、死 pid 标 stale、stop 发 SIGTERM 等进程消失，发现两件事：`discover_alive` 刻意把自己的 pid 读成 stale；SIGTERM 后的僵尸在 reap 前仍会被读成存活）、优雅停机下的 exec 行为钉住（daemon 先取消 turn → TurnFinished(Interrupted) → exec 计 Completed，退出码矩阵以客户端动作为中心，M2 可再议）、--json 透传 generation_bumped、args 解析边缘；capabilities 是 LocalFs 的构造/查找边缘（文件根、缺失根、目录读的 NotFound/WrongKind）。仍未覆盖且如实记录：chat.rs 主循环（TTY）、main.rs 派发（bin 入口，子进程测试随 M2）。

**本轮实测计数**（`cargo test -p <crate>` 汇总，2026-10-01，覆盖率补测后）：全 workspace **531** 项全绿（protocol 119、daemon 75、store 75、llm 59、kernel 52、tools 38、cli 37、capabilities 27、xtask 25、tests 11、testkit 11），另有 1 项 `#[ignore]` 照旧 skipped；覆盖率 ci 组实跑 519 项全绿。`./scripts/ci.sh` 八闸门全绿；clippy 零告警、fmt 干净。手动 live 验收清单不变（见 Phase 5 节）。

### 2026-10-01 · M1 Phase 5（e2e + 门禁收口）

**`hatchery-tests` 落地（第 13 member，Dev 层）**：场景 1/2、invariants 组、D7 子进程对比共用一个 lib target（SSE fixture 常量、provider 配置层、事件收集助手）。共享 fixture 走 lib 而不是 `tests/support/mod.rs`，是为了不让 clippy 的 dead_code 规则跟「这个测试二进制只用了一半助手」较劲。

**e2e 场景 1（最小对话 + 不变量 2）**：`invariant_minimal_chat_replays_reasoning_byte_exact`——prompt → 流式（事件序与 kernel 文档化序列的 wire 投影逐项比对）→ 落库（session/load 三类 item）→ 第二 turn 请求体**整表逐字节**命中：`messages` 数组与手写期望做 `Value` 相等（serde 字符串相等即字节相等，fixture 的 reasoning 含首尾空格/中文/换行/制表符）。配套 `a_reconnecting_frontend_gap_fills_from_the_store` 钉 `replay_from` 的补差语义。

**e2e 场景 2（双前端 + 重连）**：两 probe 全序列相等；断线后 `session/load(replay_from)` 补差 + 重订阅续流。

**invariants 组 4 → 8**：客户端代际过滤（假 UDS 服务端推 5/3/7，前端只见 5/7）、会话租约（MockWire 延迟 5s 制造在途 turn，第二 prompt 拒绝 + cancel 后原 turn 以 Interrupted 收束）、20 线程单实例竞态（恰一胜者）、逐字节回放（场景 1 兼任）。

**D7 定案（design/testing.md 开放问题 4）**：同一场景两种形态对比后，e2e 默认维持进程内过真 socket；真子进程形态保留一条常驻对比测试（`e2e_daemon` bin + 真实 daemon.json 发现）。详见设计文档条目。

**四个行为修正（e2e 落地前先补齐语义，均有 crate 内测试）**：
1. kernel `AgentHandle::turn_running()`（watch 镜像状态机）——此前 `is_busy` 用 `!is_closed()` 把「agent 活着」当「turn 在跑」，manager 的忙拒缺失、空闲 sweep 谓词永远为假（D2 形同虚设）。
2. `session/prompt` 忙拒：第二 prompt 返回 `TurnInProgress`（此前 kernel 静默丢弃、调用方却拿到 Ok）。
3. `session/load` 的 `replay_from` 从 store 直透改为「活动分支上该 item 之后」——协议语义与 `rebuild_chain` 的「到 head 为止」不一致是潜伏 bug；游标不在活动分支上返回 `InvalidRequest`。
4. manager 的 echo 判定改读 resolved provider config 的折叠能力表（`ProviderConfig::capability_table`，与 adapter 同源）——此前只读 built-in 表，config 的 `echo_reasoning` 覆盖只对 wire 生效、对历史回填无效。

**门禁**：`cargo xtask coverage` 升级为按 crate 折算 + 阈值 enforce（`--report-only` 逃生），执行位在 nightly；nightly 新增 `msrv` job。折算规则（`crates/<name>/src` 前缀、tests 目录不计）与表结构由 xtask 单元测试钉住；llvm-cov 未装时照旧 fail-loud。

**本轮实测计数**（`cargo nextest run --workspace`，2026-10-01）：默认组 **458** 项全绿（另有 1 项崩溃重入入口 skipped）——protocol 109、store 73、daemon 60、llm 50、kernel 47、tools 30、cli 24、capabilities 22、xtask 24、testkit 11、tests 8；invariants 组 8 项。`cargo xtask layering`：13 members、32 build edges + 8 dev edges。

**M1 手动 live 验收清单（评审⑤前由用户执行，结果记本文件）**：
- [ ] `hatchery doctor --provider deepseek` / `--provider qwen`：两轮探测（默认出推理 / 关掉推理）均 ok；
- [ ] `hatchery exec "你好，介绍一下你自己"`（真实模型）：流式纯文本、退出码 0；`--json` 每行 item 级事件、含 reasoning；
- [ ] TUI：`hatchery chat` 发起对话，reasoning 默认折叠、Ctrl+R 展开；`/effort off` 后下一条不再出现推理；
- [ ] resume：`hatchery exec --session <id> "继续"`（关掉先前终端重开），第二 turn 正常续上历史；
- [ ] 双前端扇出：两个终端同时 attach 同一会话，一边 prompt，两边消息流一致；
- [ ] 真实 provider 的回放命中：TUI 中对同一会话追问一轮，观察无上下文丢失（逐字节断言的 mock 已覆盖，live 侧以对话连贯性佐证）。

### 2026-10-01 · M1 Phase 3（daemon 的测试面）

**TestDaemon 起真 socket**：TestDaemon = 真 turso 库（tempdir）+ 真 LayeredConfig + 真 UDS 监听；ClientProbe = hello（带 boot token）+ 类型化调用 + `EventStream` 订阅。design/testing.md 开放问题 4（e2e 形态）**就此定默认：进程内但走真 socket**——一个 tempdir 的代价，换来 UDS/分帧/路由/扇出全链路覆盖。真子进程形态（覆盖 daemon.json 生命周期与进程崩溃）留给需要它的场景再对比。

**二分测试的用法被验证了一次**：双前端扇出测试超时，先直订 hub（过）再断 socket 半段（断）→ 定位到 server 的会话提取把整个 Session 对象当 id 反序列化。失败面大的时候，「在链路上逐段放探针」比读代码猜快得多。

**determinism 步骤与有意修改的摩擦**：门禁的 determinism 检查把「工作树里有未提交的 fixture 改动」一律当测试改写 golden。本批有两个**有意**的 protocol fixture 更新（provider_call_id、boot_token），在提交前该步骤会保持红——这是步骤设计与「分阶段交付未提交」的已知摩擦，不是回归；提交即恢复。

### 2026-10-01 · M1 Phase 2（capabilities/tools 的测试面）

**MemoryFs 与 LocalFs 行为对齐是这条测试线的地基**：`..`/绝对路径拒绝、NUL 嗅探拒绝二进制——grep 的「跳过二进制并计数」路径就是靠 MemoryFs 的 `Binary` 错误测出来的；两个 backend 行为不一致时，工具会长出只在其中一个上成立的习惯，而测试看不见。

**`TempWorkspace` 补上真磁盘那半边**：symlink 逃逸只有真盘测得了（LocalFs 的 `canonicalize` 前缀检查），MemoryFs 造不出符号链接。两条路径各有归属。

**工具测试走 `ToolCtx` 直连而不是绕道 kernel**：单元层（每个工具 happy/越界/截断/取消）用 `ToolCtx` 直连；装配层（`tests/assembly.rs`）走 `ToolHost` 三方法验证「广告目录 = 分发表」、Chat 无审批、未知工具报错。两层各测各的合同。

**walker 形态实测**：回调式（`AsyncFnMut` 闭包）与借用检查打了三轮（AsyncFnOnce 逃逸、Send 不成立、higher-ranked lifetime），换成「walk 返回路径 Vec、调用方自己 for 循环」后一次通过——取消与 result cap 都住在调用方的循环里，反而更诚实。教训：异步遍历的接缝宁收 `Vec` 不收闭包。

### 2026-09-30 · M1 Phase 1（llm 的测试面）

**MockWire 与 fixture 加载进 testkit。** `MockWire::replay_sse`/`refuse_then_sse`/`refuse_always` 覆盖流回放与「先拒后服务」两种 wire；`requests()` 返回展平的 `RecordedWireRequest`（小写 header 名 + body），请求体 golden 断言直接对它做。fixture 约定：`tests/fixtures/<name>.sse`（原始字节）+ `<name>.meta.json`（provenance：recorded/synthetic、provider、model、shape、时间）——**sidecar 缺失即拒载**，无出处的录算是负债。合成 fixture 六个先顶位（deepseek 文本/推理、qwen 工具调用、401/402/429 错误体），真实录制后同名替换。

**两个实测结论，都写进了测试注释：**

1. **wiremock + 连接池会吞一个请求**：keep-alive 连接被服务端关掉恰逢重试复用时，reqwest 报 `SendError`（按网络错误分类、判可重试——行为正确），但「恰好 N 个请求」的断言会差一。llm 测试统一用 `pool_max_idle_per_host(0)` 的客户端（`provider()` helper）。
2. **`start_paused` 可以测真实的退避时间**：429 两次 + SSE 一次的测试在暂停时钟下断言 `elapsed == 1500ms`——`RateLimited` 事件里的毫秒数不是装饰，adapter 真的等了。要求 `jitter_percent = 0`（否则时间带内随机）。

**脱敏扫描是门不是擦除器**：录制器扫到录制 key 本身或任何 `sk-` 形长串就拒写——改写会背叛「字节精确」这条录制存在的理由，泄漏意味着重录。xtask 的单元测试只测参数校验路径（`--provider bogus`），**绝不在测试里裸调 record-fixtures**：机器上有真 key 时那会真的花钱打 API。

**TLS provider 的测试面**：`reqwest::Client` 在无 crypto provider 的进程里构造即 panic；llm 测试与 registry 单测都先 `install_tls_provider()`（先装者胜、幂等，可并行调用）。

### 2026-09-30 · M0b 评审后的两轮修复（测试面）

**「测试通过」不等于「测试在测」。** 这轮抓出三处结构上不可能失败的测试，都是同一类错误——测试观察的位置与被测行为发生的位置错开了：

1. `an_interrupt_with_no_turn_running_is_ignored` 在当前线程 runtime 上**空转**：`mpsc::send` 在有容量时不挂起，所以 spawn 出去的 agent 任务在断言之前根本没被 poll，随后又被 abort。改成走同一条 FIFO 通道驱动一个真实 turn，空闲中断才真的排在前面被处理。
2. 工具取消**不可观测**：fake 在 gate 之后才记录调用，而 kernel 的 select 是取消优先、并且会 **drop 掉 invoke future**——所以 `cancelled` 永远不可能是 true，测试里那句「让被取消的工具返回」描述的是一个不可能发生的顺序。改成进门即记录 + drop guard 写结论。
3. 工具进度的丢失被 gate **掩盖**：`tool_progress_is_forwarded_while_the_tool_runs` 用的是 gated host，而放开 gate 会多给 select 一轮，进度分支就赢了；不带 gate 时「同一次 poll 内发进度并返回」的工具，它最后那条进度是确定性丢弃的。补的不带 gate 的测试在改 kernel 之前先跑了一次，确认是红的。

**纪律**：本轮所有「修行为」的新测试都先证明它会红（进度排空、跨会话批次、会话缺失的错误分类），再改实现。「先绿后红」的测试只能证明它当时没在测东西。

**共享的断言面搬进 testkit。** `completion`/`reason`/`error`/`finished_items`/`kinds`/`states`/`tool_result_texts` 原本定义在 kernel 的集成测试文件里，现在是 `hatchery_testkit::events`——M1 的 daemon 测试要对同一批事件问同样的问题，第二份 `completion()` 就是第二个忘记「一个 turn 恰好一个 `TurnEnded`」的地方。`RecordingSink` 的 `wait_for_from`/`wait_for_end_from`（2026-09-29 加）同理：`wait_for` 从头扫，多 turn 测试等第二次结束时会重新匹配到第一次，拿到一份过期快照。

**未使用的 API：能给真调用方的补上，其余删掉（用户裁决）。** `ScriptedApproval::{AllowOnce,DenyAlways,Script}` 原本没有调用方，现在有一个：新增 kernel 测试「一轮里两次工具调用，第一次允许、第二次拒绝」——它顺带补上了此前没有覆盖的「一轮多审批」路径（两个 request id 必须不同、状态序列要出现两组 `awaiting_approval→executing`）。`Gate::available` 与 `ScriptedProvider::last_messages` 删掉（测试都用 `requests()[n].messages`）；kernel 的 `AgentHandle::try_submit` 同样删掉（把「队列满」和「agent 已停」都报成 `AgentGone`，零调用方）。

**文档里的计数一律去掉（用户裁决）**：README、roadmap 与设计文档不再写测试数与 fixture 数。证据就是这轮修复本身——同一个 M0b 在四处写着 219 / 198 / 197 三个互不相同的数，而且每改一次代码它们就再错一次。数字只留在 worklog 的日期化条目里；覆盖率交给测试自己机器检查（protocol 的 fixture 覆盖率有三条测试兜着）。

**本轮实测计数**（`cargo nextest list --workspace`，2026-09-30）：默认组共 **258** 项、全绿——protocol 109、kernel 46、store 73、testkit 5、capabilities 11、xtask 13、cli 1；另有 1 项 `#[ignore]`（崩溃测试重入的子进程入口，运行时计为 skipped）。invariants 组 4 项，doctest 7 个。`./scripts/ci.sh` 全绿；`cargo xtask layering` 报 12 members、22 build edges + 8 dev edges。三平台 CI 仍未看到 M0b 之后的代码（push 由用户执行）。

### 2026-09-28

**M0b 落地**：不变量分组从 1 条涨到 4 条（新增 `invariant_items_are_never_rewritten`、`invariant_the_database_refuses_to_update_an_item`）；默认组从 23 项涨到 219 项（其中 protocol 94、store 64、kernel 40）。testkit 交付 kernel 四接缝的 fake、`ScriptedApproval`/`answer_approvals`、`Gate` 与 store 的参考模型 `ReferenceTree`；`fixture`/`assert_golden` 推迟到 M1（protocol 的 golden 是纯 JSON，SSE fixture 要等 llm crate）。崩溃测试的形态按实测定案（测试二进制自重入），design/testing.md 开放问题 1 关闭。详见上面「实测记录 · M0b」。

### 2026-09-28（M0a）
- 初稿（应用户要求补充：「这个项目需要详尽的测试，确保所有代码都能如期运行」）。设计要点：测试纪律 5 条；不变量 2 的锁死方式（MockWire 记录实际请求体字节 vs store 重建逐字节比对）；store 是最重投入；GUI 策略 =「逻辑出 GTK」；live 测试不进 CI。
- **M0a 落地**：门禁脚本 + 三平台 workflow + nextest 分组 + clippy 作用域方案；两个 spike 的 23 项测试成为常驻回归网。design/testing.md 相应改写三处（§0.2 加「上游文档也会过时」、§1 分组机制与 nextest 实测语义、§3.4/§3.9 更新为 turso 与 fluent、§5 测试名统一加 `invariant_` 前缀）。
- **CI 首跑失败并修正**（windows job）：`rustup-init --component` 的参数 arity 写错；同时补上幂等的工具链安装、`rustup set default-host` 与 windows-gnu ABI 断言。细节见上面「实测记录 · CI 首跑」。
