# 工作记录：测试体系（横切）

- 范围：测试纪律与分层、hatchery-testkit、fixture 与录制、CI 门禁、覆盖率、fuzz/属性/基准、手动实测清单
- 设计文档：[../design/testing.md](../design/testing.md)
- 相关 ADR：全部（每条 ADR 的「后果」节都有对应测试面）；门禁实现在 `scripts/ci.sh` 与 `.github/workflows/`

## 当前状态

**M0a 的门禁与分组基建已落地并本地验证**：`scripts/ci.sh`（本地与 CI 同一条命令）、`.config/nextest.toml`（default/ci/invariants/slow/gui/live 六个 profile）、`clippy.toml`（不变量 4 的编译期强制）、`xtask coverage` 包装。两个 spike 的结论已经沉淀为常驻测试（store 12 项、capabilities 11 项，`cargo nextest list` 实测）。三平台 CI **首跑已失败一次**（windows job 的 rustup 安装命令写错，已修，见「实测记录」），等待重跑。

## 待办

- [x] (M0) workspace 加 `hatchery-testkit` crate（dev-only，`publish = false`）；nextest/insta/proptest 进 workspace 依赖；`.config/nextest.toml` 分组
- [x] (M0) CI 骨架：`pr.yml`（三平台，全部调 `scripts/ci.sh`）+ `nightly.yml`（slow/gui/audit/coverage + 后续 TODO 注释）
- [x] (M0) clippy `disallowed_methods`/`disallowed_types` 配置 + 作用域实测（见下「实测记录」）
- [x] (M0) 两个 spike 的结论沉淀为回归测试：`crates/hatchery-store/tests/spike_engine.rs`、`crates/hatchery-capabilities/tests/spike_shadow_git.rs`（i18n spike 是纯选型裁决，M4 落地时才有可测物）
- [x] (M0) `cargo-llvm-cov` 未装时 `xtask coverage` fail-loud 报安装命令（不静默跳过）
- [x] (M0) 三平台 CI 首跑通过（第 2 次尝试；windows MSYS2 ucrt64 那条也过了）
- [x] (M0b) store 属性测试参考模型（testkit 里独立写的 `ReferenceTree`）+ kill -9 崩溃测试框架（测试二进制自重入，无需专用二进制）
- [ ] (M1) testkit 余下部分：MockWire(sse fixture)/Memory 后端三件套/TempWorkspace/TestDaemon/ClientProbe（kernel 四接缝的 fake 与 `ReferenceTree` 已于 M0b 交付）
- [ ] (M1) 建 `hatchery-tests` 成员 crate（跨 crate e2e 与不变量套件的家）
- [ ] (M1) MSRV job 进 nightly（`cargo +1.90.0 check`），防依赖升级悄悄抬高下限
- [ ] (M1) fixture 录制 xtask + 脱敏（API key 扫描）+ provenance 元数据格式
- [ ] (M1) 不变量套件 `invariants` 分组填满（现在 4 条：store 的 3 条 + 影子 Git 的 1 条；4 编译期已保；1/2/5 随 M1-M2；6 已有引擎级实测，M2 补 store/tool 层）
- [ ] (M1) e2e 场景 1-2 落地（最小对话、双前端扇出）
- [ ] (M2) 影子 Git 安全测试补全（硬门、PTY 孤儿进程）；fuzz targets 上线 nightly
- [ ] (M2) e2e 场景 3-6；契约测试套件（LlmProvider/SessionStore/FsBackend/TerminalBackend/ApprovalGate）
- [ ] (M2) `disallowed_methods` 的 compile-fail 测试（trybuild 类）——M0a 只做了人工实测，未自动化
- [ ] (M3) fake 宿主集成 + dummy-acp-agent；e2e 场景 7-8；Zed 真机清单执行并记录
- [ ] (M4) gui 组进 CI（GNOME SDK 容器 + xvfb）；10k 性能 fixture；e2e 场景 9
- [ ] (M2+) criterion 基线入库 + nightly 对比；cargo-mutants 试点（store/kernel）

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
