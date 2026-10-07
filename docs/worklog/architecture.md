# 工作记录：总体架构 / 跨方向协调

- 范围：crate 分层纪律、核心不变量、进程模型、跨方向决策协调、workspace 与 CI 基建
- 设计文档：[../architecture.md](../architecture.md)、[../references.md](../references.md)
- 相关 ADR：全部

## 当前状态

**M1 代码完成（2026-10-01）**：Phase 1–5 全部落地（llm adapter + 能力表、capabilities/tools、daemon 全栈、cli TUI/exec、e2e 与不变量收口），评审⑤自查轮的加固已入库；本地 `./scripts/ci.sh` 全绿、覆盖率门槛全过（数字见 worklog/testing.md 日期化条目）。手动 live 验收进行中——已产出 2026-10-04/10-05 三轮 TUI 修正，worklog/testing.md 的六项清单尚未勾选；**M1 的收口归入 M2 Phase 0**。三平台 CI 自 M0 收口后尚未见过新代码（push 由用户执行）。M0 历史结论：M0b 三 crate + testkit 于 2026-09-28 落地、评审后两轮修复（09-29/09-30）入库，M0 DoD 达成（门禁三平台绿 + kill -9 崩溃恢复 + 三 spike 落档），细节见下文变更日志。

**M2 已于 2026-10-07 重新规划**（见 [../roadmap.md](../roadmap.md) 的 M2 节）：一次三路并行的全仓库勘察发现原 M2 段是按 M0/M1 的自述写成的，与代码有 16 处不符，阶段划分重写为 Phase 0–8，跨方向的更正记在本文件下面的日期条目里。

仓库现状：`crates/`（12 个 crate = 10 个产品 crate + dev-only 的 `hatchery-testkit` 与 `hatchery-tests`）+ `xtask/` + `scripts/ci.sh` + `.github/workflows/{pr,nightly}.yml` + `docs/`。产品 crate 里 8 个已实现（protocol / kernel / store / llm / capabilities / tools / daemon / cli），`hatchery-acp` 与 `hatchery-gui` 仍是骨架（各只有文档注释，实现分别排 M3 与 M4）。（原文写「11 crate，其中 3 个已实现」是 M0b 时点的快照，2026-10-07 更正。）

## 待办

- [x] (M0) Cargo workspace 脚手架：12 member（11 crate + `xtask`）+ workspace Cargo.toml（共享依赖版本与 lints）+ rust-toolchain.toml
- [x] (M0) CI：PR 门禁全套（`scripts/ci.sh`：toolchain/fmt/clippy `-D warnings`/build/nextest ci 组/doctests/fixture 确定性/i18n 占位）+ nightly（slow/gui/audit/coverage）+ `disallowed_methods` lint 配置
- [x] (M0) xtask：`layering`（分层契约，含集成测试）、`coverage`；`i18n-extract` 与 `record-fixtures` 是 fail-loud 占位
- [x] (M0) 三个 spike：存储引擎（→ ADR-0010 turso）、影子 Git 后端（→ **ADR-0012 git2 vendored**；第一轮曾选 CLI，用户裁决后改 git2 并重测）、i18n gettext vs fluent（→ ADR-0011 fluent）
- [x] (M0) MSRV 实测：`rust-version = "1.90"`（1.85/1.88 均失败，1.90.0 编译通过）
- [x] (M0) CI 工具链改为**源码编译**（`cargo install cargo-nextest --version 0.9.146 --locked`，三平台一致；nightly 的 audit/llvm-cov 同）——用户裁决，避免预构建二进制在 MSYS2 下的不确定性
- [x] (M0) 编译 flags：`.cargo/config.toml` 加 `[build] rustflags = ["-C", "target-cpu=native"]`（用户偏好；已实测进入 rustc 调用）。**纪律**：发布产物与交叉编译必须覆盖（`RUSTFLAGS=""`），否则二进制不可跨 CPU 移植（2026-10-03 更正：该 flags 已于 10-01 停用、CI 与仓库配置归零——缓存污染事故，见同日变更日志）
- [x] (M0) 三平台 CI 跑通（第 1 次失败于 windows 的 `rustup-init --component` 参数 arity，已在 5b953fb 修复并补 windows-gnu 的 ABI 断言；细节见 worklog/testing.md「实测记录 · CI 首跑」）
- [x] (M0) 顶层 README 扩写：项目定位、快速开始、文档链接、MSYS2 ucrt64 环境清单、仓库布局；docs/README.md 加「代码布局」节
- [x] (M0b) 分层修正：protocol 沉为唯一最底层，kernel 升 L1（见下「M0b 修正」）
- [x] (M0b) protocol/kernel/store 三个 crate 实现 + testkit M0b 子集 + 各自的测试套件
- [x] (M1) 建立 docs/glossary.md 术语表（2026-10-01，M1 收口时建立）
- [x] (M1) MSRV CI job（`cargo +1.90.0 check`）加进 nightly，防止依赖升级悄悄抬高 MSRV（2026-10-01 随 Phase 5 门禁收口落地）
- [x] (M1) `hatchery-tests` 成员 crate（跨 crate e2e 与不变量套件的家；虚拟 manifest 不能有顶层 `tests/`，见 design/testing.md §1）（2026-10-01 落地，xtask layering LAYERS 表同步）

## 开放问题

1. ~~crate 命名前缀与是否发布到 crates.io~~ → **已定（2026-09-28）**：前缀 `hatchery-*`；**按可发布标准写全元数据**（`license = "GPL-3.0-only"`、repository、description、readme、keywords、categories、`rust-version`），但 **M0 不 publish**；`hatchery-testkit` 与 `xtask` 标 `publish = false`。protocol crate 从第一天走 semver 纪律 + `PROTOCOL_VERSION` 常量（M0b 落地）。注意 LICENSE 是 **GPL-3.0**，将来发布 library crate 会让下游必须接受 GPL，届时需重新确认。
2. ~~MSRV 策略~~ → **已定并实测（2026-09-28）**：`rust-toolchain.toml` 用 `channel = "stable"`（浮动），`rust-version = "1.90"`。实测：1.85.1 失败（turso 的 `icu_*`/`aristo`/`home` 要求 1.88）、1.88.0 失败（`roaring 0.11.5` 要求 1.90.0）、**1.90.0 通过** `cargo check --workspace --all-targets`。GTK 生态（M4 才引入）可能再抬高下限，届时以实测为准。
3. ~~references/ 目录的 license 与体积~~ → **已由用户自行解决**：`.gitignore` 里的 `/references` 使其不入库，只保留 `references.md` 的分析结论。

## 变更日志

### 2026-10-07 · M2 Phase 0：两处门禁口径不实已修，两条不变量的口径已更正

本日勘察记下的两处「门禁说自己跑了其实没跑」都修掉了：① `--profile invariants` 现在真有步骤（`scripts/ci.sh` 的 `invariants` 步），五条映射到不变量却缺前缀的测试已改名，改完逐条核对该 profile 的选中集合并全绿；② `check_i18n` 从「返回成功的空操作」变成脚本末尾一行明示未设门禁，`--help` 的步骤表照实写。细节见 worklog/testing.md。

**不变量 2 的边界写进 architecture.md §5**：本条管辖分支历史；请求最前面那条 system prompt 是可复现的派生态（嵌入模板 + 装配时冻结的运行时事实），不是 item，由 `prompt/render` 钉——它对活着的 runtime 返回装配时冻结的那一份（D15，落地见 worklog/daemon.md）。这不是给不变量开口子：分支历史的逐字节断言强度没变，而 system prompt 现在有了一个比「混进请求体比对」更强的钉子（透明性 API 必须与模型实际收到的字节相同）。

**不变量 4 的 §5 描述更正为真实机制**：原文写「CI 中用 lint（如 `#![deny(clippy::disallowed_methods)]`）强制」，而 crate 属性压不过 Cargo 的 lint 表（2026-10-01 实测），真正生效的是 `hatchery-tools` 自带的完整本地 `[lints]` 表 + 根目录 `clippy.toml` 的禁令表；禁令表本日补齐 `tokio::fs` 孪生项（见 worklog/capabilities.md）。

**分层未变**：Phase 0 只动 daemon 内部装配与门禁配置，没有跨层新边；删掉 daemon → `hatchery-acp` 的死依赖边后 `cargo xtask layering` 仍报 strictly downward、无环。`./scripts/ci.sh` 全绿。

### 2026-10-07 · M2 重新规划：跨方向的勘察更正

M2 开工前做了一次全仓库勘察（三路并行：capabilities/tools、kernel/store、daemon/cli/protocol/tests），逐条对着代码复核后重写了 roadmap 的 M2 段（Phase 0–8 + 决策点 D8–D18 + 顺延表）。方向内的细节在各自的 worklog，这里只记跨方向的部分。

**门禁有两处口径不实（Phase 0 修）。** ① roadmap 的 M0 DoD 写「nextest 默认组与 **invariants 组**」，而 `--profile invariants` 在 `scripts/ci.sh`、`pr.yml`、`nightly.yml` 里**从未被调用过**——ci.sh:134 只跑 `--profile ci`，nightly 只跑 slow 与 gui。不变量测试确实进了 PR 门禁（`ci` 继承 `default`，过滤器只排除 live/slow/gui），但那是巧合而不是设计：没有一个可单独报告或阻塞的门禁，而 testing.md §5 映射到不变量、却没有 `invariant_` 前缀的 5 条测试（含 `two_concurrent_prompts_yield_exactly_one_turn`）一旦真去跑那个 profile 就会被静默丢掉。② `check_i18n`（ci.sh:121-124）是一条 `printf` 空操作（"i18n extraction check lands in M4 — nothing to verify yet"），一直被算作通过的门禁步骤。

**两条不变量的实际状态与文档不符。** 不变量 4 的编译期门禁有洞：`clippy.toml` 禁了 `std::fs::*` 与 `std::process::Command`，**没禁 `tokio::fs::*` 与 `tokio::process::*`**——而 hatchery-tools 依赖 tokio、`LocalFs` 自己就用 tokio::fs，所以工具里写一句 `tokio::fs::write` 就绕过了整条纪律（另漏 `std::fs::{remove_dir, read_link, hard_link, set_permissions}`）。不变量 5 则**在任何形态下都没有测试**：testing.md 点名的 `invariant_project_config_cannot_disable_hard_gates` 全仓库零命中，唯一沾边的是 `RiskLevel::is_hard_gate()` 这个值类型谓词的两条单测，生产代码无人调用它——而「硬门测试全绿」正是 M2 的 DoD 之一。

**`hatchery-acp` 是死依赖边。** `crates/hatchery-daemon/Cargo.toml:21` 声明了它，daemon 源码零引用（`hatchery_acp` 在 `src/` 下零命中），crate 本体是 13 行文档注释 + 「implementation lands in M3」。`cargo xtask layering` 把它算作一条 build edge，于是分层契约在替一个不存在的接线背书。Phase 0 摘掉这条边（M3 真接线时再加回来）；这与 ADR-0009 的反预拆分刹车一致——依赖边也是预拆分。

**同类问题：三处文档在宣称不存在的东西。** daemon 的 `src/lib.rs:10` 与 Cargo `description` 都写「profile-based assembly」，而 `Profile` 类型不存在（grep 只命中那一条文档注释），design/daemon.md §3.1 的四个 profile 一个都没实现；`crates/hatchery-capabilities/README.md` 宣称有影子 Git 检查点与注册句柄；`crates/hatchery-tools/README.md` 列了七个工具（实际三个）。Phase 0 一并更正——「README 比代码乐观」是接手者最贵的那种误导。

**ADR-0006 的枚举拼写与协议不一致，按约定不改 ADR。** ADR-0006 写 `RewindScope::{Conversation, Code, ConversationAndCode}`，协议的实际变体是 `{Conversation, Code, Both}`（`hatchery-protocol/src/session.rs:289-300`，ADR-0003 与 design/protocol.md 的修正表用的也是 `Both`）。ADR 一经 accepted 不修改，**以协议为准**，此处留痕。

**文档计数纪律照 2026-09-30 的裁决执行：删数字，不改数字。** design/protocol.md §6、design/testing.md §3.1 与 worklog/protocol.md 里写的 fixture 数「62」已经过期（实数是 79），但按裁决这类数字本就不该出现在散文里——所以是把计数删掉，而不是更新成一个新的、下次照样过期的数。

**分层图在 M2 不变。** M2 不新增 crate（反预拆分刹车：`CheckpointStore` 住 capabilities，`DaemonApproval` 住 capabilities，工具住 tools，模式装配住 daemon，都是既有归属）。新增的外部依赖待各自决策点定：`portable-pty`（D10，spike 提前到 Phase 1 并行做；证据是 codex 用 `portable-pty = "0.9.0"` 且在 Windows 侧额外挂 `winapi` 的 jobapi2/Job Object 才能保证杀进程不留孤儿——那正是 windows-gnu CI 上「cancel 后无孤儿进程」的坑）、diff 库（D11，atomcode 用 `similar = "2"`）、web_fetch 的 HTTP client 与 HTML→Markdown（D17，注意 `wiremock` 是 testkit 专属、明确「never of a product crate」）。`git2` 已在 workspace 与 capabilities 的依赖里，只是 `src/` 下零使用——spike 的 11 项门槛是唯一消费者，Phase 1 转正。

**kernel 在 M2 只改一处。** `ToolInvocation { output, is_error }` 增加携带检查点的能力，kernel 在 ToolResult item **之前**追加 Checkpoint item（决策点 D13，理由与链形状见 worklog/kernel.md 与 design/storage.md）。这是自 M1 的 `TurnInput { turn, content }` 以来第一次动 kernel 的公开形状。另两个「M2 决定」的开放问题在本里程碑内**裁决为不做**：一轮多 tool call 并行（7 处结构阻碍 + 会打破三条确定性/审批测试）与 `max_tool_retries`（「可重试失败」还没有第一个消费者）。

**两处顺延（用户裁决）。** hub coalescing + replay window → M3：`is_coalescable` 只含 text/reasoning delta，而 M2 新增的事件量主要来自 `ToolCallProgress`（不可合并），coalescing 治不了 M2 的病；replay window 已被 `session/load` + `replay_from` 取代且有 e2e 覆盖。M2 只保留一次「Code 会话事件量测量」，那正是 `hub.rs:4` 原本要的东西。`SessionLease` 跨进程文件锁 → 多 daemon 形态出现时：`--embedded` 全仓库无实现，CLI 只有 attach-or-spawn，单实例 `daemon.lock` 已挡跨进程双 runtime，会话内由 turn 闸门 + 在途 CAS 标记承担且有不变量测试；跨会话共享影子仓库要的是 daemon 内 per-workspace 互斥（ADR-0006 已写明），不是文件锁。glossary 的 lease 条目随此更正。

### 2026-10-03 · CI 编译 flags 裁决收口 + worklog 对账

**CI 缓存污染的完整定案（用户裁决补齐根因）**。时间线：2026-09-30 在 pr.yml 设 `RUSTFLAGS=x86-64-v3`（提速实验，88f2561）→ 同日移除（6c4e005）；2026-10-01 注释掉 `.cargo/config.toml` 的 `-C target-cpu=native`（1c75e69）并给 Gate 加 `cargo clean`（4ae1042）。机制：仓库配置对 CI 同样生效，native/baseline+ 的缓存产物只对构建它的那颗 CPU 合法，rust-cache 把它们恢复到不同世代的 runner 上就出非法指令。`cargo clean` 是当时有效的应急，但让 rust-cache 整套缓存白干。**最终形态**：仓库与 CI 一律零 flags（可移植为默认）；本地想提速把 flag 写进**用户级** `~/.cargo/config.toml`（CI 永远读不到它，而 runner 缓存只由 CI 自己的构建写入，故不可能再污染）；加固轮删掉的 `cargo clean` 维持删除，缓存价值恢复。无需手工失效缓存：`add-rust-environment-hash-key: "true"` 把 `.cargo/config.toml` 内容与 RUST*/CARGO*/CC_* 环境都算进 key，任何 flags 变更即换 key。`.cargo/config.toml` 与 pr.yml 的注释已按此改写；上面 M0 那条「已实测进入 rustc 调用」的记录保留为历史。

**worklog 对账**（M1 代码完成后的全表核对）：勾掉已完成但仍开着的框——本文件（glossary/MSRV job/hatchery-tests）、platform 的（配置分层/prompt 管线/prompt-render/git plumbing/glossary）、llm 的 doctor 子命令；改里程碑标注（附日期与理由）——AGENTS.md 注入、上下文组装 v1、hub coalescing 调参 → (M2)，store 只读连接池 → (M3)；修正 platform.md 过期的「未实现」现状句、cli.md 重复的两组待办框、nightly.yml 的 TODO(M1) fuzz 注释（worklog 中 fuzz 本就标 M2）。kernel 的「max_rounds 在真实对话下验证」框留待 live 验收后勾。

### 2026-09-30 · M0b 评审后的两轮修复

M0b 的三个 crate 按范围分块评审了一遍（protocol 源码 / protocol 测试 / kernel 源码 / kernel 测试与 testkit / store 源码 / store 测试 / 跨 crate 一致性），约 50 条发现逐条对着代码复核后分成四组：① 协议健壮性，② 行为与契约缺陷，③ 测试可信度，④ 打磨与前瞻。用户裁决「②③ 全修，④ 留到 M1」（2026-09-29 落地），随后又裁决「④ 里能修的都修掉」（2026-09-30 落地，本条）。crate 内的细节在各自的 worklog，这里只记跨方向的部分。

**四个跨 crate 的决策**

1. **接缝枚举不加 `#[non_exhaustive]`**（`KernelEvent`/`AgentCommand`/`StreamEvent`/`TurnCompletion`/`TurnState`）。消费方全部在本 workspace 内，穷尽匹配正是「加一个变体时编译器替你找齐所有消费方」的机制；加上它只会逼出 `_ =>`，把新变体藏起来。wire 层的开放性由 protocol 承担，而且规则更强：枚举值在 major 内冻结、未知值一律硬失败（design/protocol.md §6）。反方理由是「第三方前端会 match 我们的类型」，但按同一条冻结规则，加变体本身就是 major bump，`#[non_exhaustive]` 换不来在 minor 里加变体的自由。
2. **LLM 接缝改借用**：`chat_stream(&ChatOptions, &[Message])`（原为按值）。adapter 在发起请求时就地把两者序列化进 body，返回的流仍是 `'static`，所以按值传只是让长会话每轮多拷一份全量上下文。**在 M1 写 adapter 之前定案比之后改便宜**——这是本轮唯一一处主动改动公开签名。
3. **显式 `"id": null` 一律拒绝**（`FrameError::NullId`），与「字段缺失 = 通知」严格区分（worklog/protocol.md）。
4. **`ItemIdRange` 不提供成员判定**：span 是位置语义，只有持有链的一方能解析（worklog/protocol.md）。解析函数留到 M5 与它的第一个消费者一起落地，避免又造一个没有调用方的 API。

**分层契约**：`cargo xtask layering` 现在检查全部三种依赖（normal / build / dev）。构建图 = normal + build，要求无环且严格向下；dev 边也必须向下，只有指向 Dev 层 crate 的例外（kernel 的 dev-dependency 指向 testkit，而 testkit 正常依赖 kernel，这条边不参与环检测）。

**文档计数纪律（用户裁决）**：README、roadmap 与设计文档不再写测试数与 fixture 数——这轮修复本身就是证据：同一个 M0b 在四处写着 219 / 198 / 197 三个互不相同的数。数字只留在 worklog 的日期化条目里；覆盖率改由测试自己机器检查（protocol 的 fixture 覆盖率有三条测试兜着，见 design/protocol.md §6）。

**门禁（本轮实测）**：`cargo nextest list --workspace` 默认组共 258 项、全绿——protocol 109、kernel 46、store 73、testkit 5、capabilities 11、xtask 13、cli 1；另有 1 项 `#[ignore]`（崩溃测试重入的子进程入口），invariants 组 4 项，doctest 7 个。`./scripts/ci.sh` 全绿，`cargo xtask layering` 报 12 members、22 build edges + 8 dev edges，严格向下无环。三平台 CI 仍未看到 M0b 之后的代码（push 由用户执行）。

### 2026-09-28 · M0b（分层修正 + 三个 crate 落地）

**M0b 修正：把 protocol 沉为唯一最底层。** M0a 把 `hatchery-protocol` 与 `hatchery-kernel` 并列在 L0，layering 契约禁止同层横向依赖（`from_layer <= to_layer` 即违规）。但 protocol 的数据模型必须引用 `ToolOutput`/`ApprovalRequest`/`Content`/`Usage`——这些值既要进 wire、又被 kernel 与 capabilities 共用。两个选择：镜像约 10 个类型 + 在 daemon 里加一层翻译，或者让 protocol 成为共享词汇表。选后者：

- protocol = L0（共享词汇表 + wire 契约）；kernel = L1；llm/store/capabilities = L2；tools/acp = L3；daemon = L4；cli/gui = frontend。
- 连带改动：`xtask::layering::LAYERS`、`Layer` 枚举 + `L4`、8 个 crate 的 lib.rs 层号注释、architecture.md §3 的分层图与纪律条目。
- **ADR-0004 的「kernel 不得依赖 capabilities」不受影响**：那条边向上，仍然禁止。这条纪律的措辞也从「L0 反过来依赖 L1」改为「kernel 反过来依赖上层」。
- 没有新增 ADR：层号是 architecture.md 内部的表述，推翻记录留在本文件（依约定，ADR 才需要 supersede 链）。
- 用户四项裁决同时落定：① protocol 沉底（本条）；② store 只出有序 `Vec<Item>`（storage.md §4）；③ 审批由 kernel 发起、daemon 应答（kernel.md §7 / capabilities.md §1）；④ 崩溃测试用测试二进制自重入（testing.md 开放问题 1）。

**三个 crate 的实现**（各自的 worklog 有细节）：protocol 93 测试 + 62 golden fixture；kernel 40 测试；store 64 测试（含属性测试与 kill -9）。`cargo xtask layering` 报 12 members / 22 edges，严格向下。

### 2026-09-28 · M0a
- 项目启动设计：深读四款参考项目（分析结论存 ../references.md），与用户对齐 8 项关键决策（ADR-0001~0008），产出 architecture/roadmap + 9 份方向设计文档 + 本 worklog 体系。
- 用户明确的核心差异化诉求：完整 ACP（含 fs/terminal 委派，atomcode 的反面教材）、reasoning_content 可配置回传、历史可编辑（分叉+删除）、提示词透明、CLI+GTK 双前端。
- 补充测试体系设计（用户要求「详尽的测试，确保所有代码都能如期运行」）：新增 design/testing.md + worklog/testing.md；crate 清单加入 dev-only 的 `hatchery-testkit`；architecture.md 不变量节与 roadmap DoD 挂接测试文档。
- dsh/Cordis 模块化二次深读 → **ADR-0009**（吸收五条语言无关纪律，拒绝运行时机制）；第三方扩展面 = MCP + ACP，WASM 工具插件列 M5 评估占位。
- **M0 细化规划**（用户四轮裁决）：M0 拆 M0a/M0b；CI 用 bash 编排（`scripts/ci.sh` 本地=CI 同一条命令）+ xtask 只做专属检查；PR 与 main 都跑三平台，Windows 走 **MSYS2 ucrt64 + windows-gnu**；ItemId = **UUIDv7**；三平台跑全量 default 组 + 强缓存；dev 分支分阶段 commit 不 push。
- **M0a 执行**（6 个 commit）：脚手架 + 门禁 + CI；存储引擎 spike → **ADR-0010**；影子 Git spike → 定 CLI；i18n spike → **ADR-0011**；MSRV 实测 1.90；文档同步。
- M0a 修正的两处设计矛盾（都是脚手架一落地就暴露的）：
  - **「12 个 crate」与 architecture.md §3 只列 11 个不符** → 定为 **11 crate（10 产品 + testkit）+ xtask = 12 member**；配置/提示词代码落 daemon（前端经协议访问，无第二个消费者 → 按 ADR-0009 反预拆分不新建 `hatchery-platform`/`hatchery-prompts` crate）。
  - **kernel(L0) 与 capabilities(L1) 依赖环**：原设计里 kernel 的 `ToolCtx` 直接引用 capabilities 的 `FsBackend`/`TerminalBackend` → 改为 kernel 只暴露窄接口 **`ToolHost`**（snapshot/approval_for/invoke），`Tool`/`ToolCtx`/三个 backend trait 全部归 capabilities；`ToolDef`/`ToolOutput`/`ToolProgress`/`ApprovalRequest` 留 kernel（组装 LLM 请求与投影事件要用）。`cargo xtask layering` 的 LAYERS 表把这条规则变成机器检查。
- 术语统一：wire 类型 `Thread` → **`Session`**（与方法名 `session/*`、表名 `sessions` 一致），避免一物两名。
- **用户三项裁决后的第二轮**（M0a 收尾）：
  - 影子 Git 后端从 CLI git 改为 **git2（vendored libgit2）** → 新增 **ADR-0012**，理由是可用性（很多用户机器没有 git）；11 项门槛在 git2 上重测全绿，热路径反而快约 2 倍。同时更正第一轮的错误记录「git2 需要 cmake」（实测 libgit2-sys 用 `cc`，不用 cmake）——教训写进 design/testing.md §0.2：**自己写下的结论也要复核**。
  - CI 工具链一律 `cargo install --locked` 源码编译（pr.yml 三平台 + nightly 的 audit/llvm-cov），不再下载预构建二进制。
  - `.cargo/config.toml` 加 `-C target-cpu=native`（实测已进入 rustc 调用）；连带纪律：发布产物与交叉编译必须用 `RUSTFLAGS=""` 覆盖，README 与 docs/README.md 都写明了。
  - 运行时依赖变化：不再需要用户装 git；构建期改为需要一个 C 编译器。README、worklog/daemon.md（删掉 `git --version` 审计项）、worklog/platform.md（构建代价对照行）已同步。
- **CI 第 1 次实跑（用户 push 后）失败并修正**：windows job 的 `rustup-init --component rustfmt clippy` 不是合法参数（实测 1.29.1：该选项只接受单个逗号分隔值，且 `--default-toolchain none` 时组件被静默忽略）。改成 `rustup set default-host` + `rustup toolchain install "$channel-$host" --component rustfmt,clippy` + 「active toolchain 必须是 windows-gnu」的断言——顺带堵掉两个同源隐患：热缓存下 `command -v rustup` 命中镜像自带的 msvc rustup 会跳过安装、并让 job 悄悄按 MSVC 编译（与 ADR-0012 的平台决策相反）。README 里给开发者的同一条命令也改了。验证与教训：worklog/testing.md「实测记录 · CI 首跑」。
