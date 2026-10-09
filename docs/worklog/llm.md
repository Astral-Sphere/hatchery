# 工作记录：LLM provider 层（hatchery-llm）

- 范围：openai-interface adapter、effort 能力表、reasoning 回放、重试/限流
- 设计文档：[../design/llm.md](../design/llm.md)
- 相关 ADR：0007
- 上游：`openai-interface`（用户自研，crates.io，当前 0.14.0；已确认建模 chat completions 流式、responses、reasoning_content/reasoning_effort——来源：docs.rs 页面，2026-09-28 查证）

## 当前状态

M1 Phase 1 代码完成：ChatCompletions adapter、能力表 v1、重试/退避、注册表、录制工具与测试基建全部落地，`./scripts/ci.sh` 全绿。真实录制与 live 探测**已完成**（2026-09-30 的 `doctor --provider` 两家双轮实测、2026-10-01 的八条真实 fixture 重录与 toolcall 分片补录，见下面两个日期条目），**D3 随之定案：ChatCompletions 不需要缓冲一个事件**。（原「待真实录制与 live 探测，D3 决策随之定案」的现状句已过期，2026-10-07 更正。）

M2 侧本方向有两项，都排在 **Phase 5**（2026-10-07 重新规划）：Responses adapter 与多模态 image 输入。该阶段与 Phase 1–4（检查点/写路径/工具/审批/会话级方法）**无依赖**，可并行或任意插位。

## 待办

- [x] (M0) 确认 openai-interface feature 组合与 hatchery feature 表的映射（2026-09-30：`default = [reasoning]`，`deepseek`/`qwen` 各自蕴含 `reasoning`；workspace 取 `deepseek`+`qwen`+`ferritls`，`ferritls-rustls` 必须 `default-features = false`——其默认 `simd` feature 依赖 nightly 的 `portable_simd`，与上游 openai-interface 的声明一致）
- [x] (M1) ChatCompletions adapter：请求翻译 + SSE chunk → StreamEvent + tool call delta 累积
- [x] (M1) effort 能力表 v1（deepseek/qwen/通用兼容三族）+ config 覆盖机制
- [x] (M1) reasoning 逐字节回放（`reasoning_content` 按能力表 echo/drop；签名块无 Chat Completions 线上字段，Responses adapter 落地时再补存储表示——开放问题 1 保持开放）
- [x] (M1) 重试/退避/429 + RateLimited 事件（kernel 新增 `StreamEvent::RateLimited` 透传通道；见 kernel.md 2026-09-30 条目）
- [x] (M1) fixture 录制工具（xtask record-fixtures：原始字节 + provenance sidecar + 脱敏扫描）
- [x] (M1) deepseek/qwen 真实探测录制（2026-10-01 首轮：两家 text/reasoning/401 共六流 + 双 401 错误体；toolcall 流首轮被模型拒绝，录制器改为 `tool_choice` 强制 + `must_contain` 字节校验后**同日第二轮重录成功**，见下面「重录后的 toolcall 分片」条。原框内「待重录」已过期，2026-10-07 更正）
- [x] (M1) `hatchery doctor --provider` 实测子命令（2026-09-30 随 CLI Phase 4 落地；两家双轮实测留痕见 worklog/cli.md 与本文件「模型世代校准」条）
- [x] (M1) D3：ReasoningDone 是否需缓冲一个事件（**2026-10-01 定案：ChatCompletions 不需要**——实测两家 reasoning 值都严格先于 content 值，且该 wire 无签名块，迟到签名无从发生；adapter 在 reasoning→text 值边界补发 `ReasoningDone`。风险整体移交给 Responses wire 的 M2 adapter。流在 reasoning 中途结束则不发 Done，kernel 的 `close_open` 收尾——无签名可丢，等价）
- [ ] (M2, Phase 5) **Responses adapter**：`wire = "responses"` 目前是 fatal 拒绝（2026-10-01 修的，此前配置被静默无视），所以拒绝本身是诚实的。落地时一并关闭**开放问题 1**（`encrypted_content`/签名块的统一存储表示）——ChatCompletions 线上没有签名块字段，D3 把「晚到签名」的风险整体移交给了这里；`ReasoningDone { signature }` 与 `SignatureBlock { scheme, data }` 的形状已在协议里，缺的是 Responses wire 到它的翻译与逐字节回放。
- [ ] (M2, Phase 5) **多模态 image 输入**：协议侧 `ContentPart::Image { mime_type, data }` 已在，缺的是本 crate 到 wire 的翻译 **加** CLI 侧的图片输入路径（frontends 开放问题 3，同排 Phase 5/6）。**两半必须一起做**：只做输入路径会让图片进得了库却发不出去，只做翻译则没有任何前端能喂进来。

## 开放问题

见设计文档末尾 3 条（encrypted_content 统一表示、多模态节奏、上游发版）。解决过程记录于此：

- **Retry-After 读不到（2026-09-30 发现）**：`OapiError::ApiError` 只带 status/message/type/code，不携带响应头，所以 429 的 `Retry-After` 无法兑现「尊重服务端指示」；v1 退避纯指数 + 抖动。修法按 ADR-0007「上游缺字段先修上游」：给 openai-interface 的错误体加可选的 `retry_after` 字段（用户自研，发版后切换）。
- **wiremock 连接池竞态（2026-09-30 发现）**：keep-alive 连接被服务端关闭恰逢重试复用时，reqwest 报 `SendError`（分类为可重试，行为正确），但会让「恰好 N 个请求」的断言差一。测试一律用 `pool_max_idle_per_host(0)` 的客户端规避（adapter.rs helper 内注释）；真实部署保留连接池。

## 变更日志

### 2026-10-07 · M2 重新规划对账

M2 全仓库勘察后重排（见 [../roadmap.md](../roadmap.md) 的 M2 节）。本方向的两项 M2 待办**保留在 M2**（用户裁决：worklog 里标了 M2 的全留），落到新的 **Phase 5**——该阶段与 Phase 1–4（检查点/写路径/工具与审批/会话级方法）无依赖，可并行或任意插位。

两处过期文字更正：① 现状句还写着「待真实录制与 live 探测，D3 决策随之定案」，而两件事都已在 2026-09-30/10-01 做完、D3 也已定案；② 录制那条框里还挂着「toolcall 流待重录」，而同日第二轮已用 `tool_choice` 强制重录成功（本文件下面那条记录就是它）。

**与 Phase 0 的交叉影响（记在这里免得两边都以为对方管）**：Phase 0 要把 system prompt 真正注入请求（M1 只交付了 `prompt/render` 的透明性，装配结果从未进过一次请求）。本 crate 侧**无需改动**——`translate.rs:134` 的 `Role::System => WireMessage::system(text)` 早已就位，adapter 只是多翻译一条消息；llm 层的测试各自构造自己的 `messages`，也不受影响。受影响的是 `hatchery-tests` 那条对第二 turn 请求体做整表逐字节比对的 e2e 断言，归 daemon/testing 方向更新。

**多模态的耦合要在排期上体现**：协议侧 `ContentPart::Image` 已在，但「llm 侧翻译」与「CLI 侧图片输入路径」（frontends 开放问题 3）必须同批落地——只做一半的两种失败形态都是静默的（图片存得进库发不出去，或者根本没有前端能喂进来）。

### 2026-10-01 · 评审⑤自查轮（llm）

五处修正，全部带测试：`resolve_key` 修剪粘贴 key 的首尾空白（原测试钉的是原样返回，随行为修正更新）；`resolve_model` 多 provider 同名模型平局改判 None（HashMap 迭代序随机）；第二个 Done 块不再覆盖被延迟的第一个（先冲刷再持有）；`wire = "responses"` 配置 fatal 拒绝（此前字段无人读取，静默按 chat-completions 发）；重试耗尽报错报真实尝试次数而非策略预算（`max_attempts = 0` 不再产生「0 attempt(s)」的谎话）。补测：qwen 空 id 续传终于有真录制钉住（原 worklog 声称的 `real_toolcall_recording_replays_with_complete_fragments` 实际钉的是 deepseek 录制，无该 quirk）、畸形块跳过、流末冲刷 Done、5xx 两退避后放行、ModelSwitch 全臂、温度/上限落体、registry 平局。计数见 worklog/testing.md 本日条目（llm 59 项）。

### 2026-10-01 · capability_table 单点折算（M1 Phase 5）

`ProviderConfig::capability_table()` 成为「built-in 表 + config 覆盖」的唯一折算点：adapter（`with_client`）与 daemon（`provider_for` 的 echo 判定）都从它取表。动机：manager 原来自行 `CapabilityTable::builtin()` 取 `echo_reasoning`，而 provider 用覆盖后的表决定请求是否携带 reasoning——config 的 echo 覆盖只对 wire 生效、对历史回填无效，两边各自为政。现在的约束是结构性成立的：想拿这张表，只有这一个入口。

### 2026-09-30 · 模型世代校准（用户实测驱动的重校准）

用户指出内置目录过时：两家当前都是**混合推理单模型**，默认带推理，请求参数可关——DeepSeek 当前模型 **`deepseek-flash`**（初报为 `deepseek-chat`，随后更正；此前的真实录制早已旁证：请求发 `deepseek-chat`，响应 `model` 字段实际解析为 `deepseek-flash`，见 2026-10-01 条目第 4 条）与 Qwen **`qwen3.8-flash`**。openai-interface 0.14 的 wire 类型对得上：DeepSeek `thinking: {type: enabled|disabled}`（默认 enabled）、Qwen `enable_thinking: bool` + `thinking_budget`。改动：

1. **能力表**：`ReasoningWire` 新增 `ThinkingSwitch` 变体（`{"type": "enabled"/"disabled"}`），deepseek 内置行从 `ModelSwitch`（Off→deepseek-chat / 其余→deepseek-reasoner，模型已过时）改挂 ThinkingSwitch，内置 models 改 `deepseek-flash`——**模型不再切换，effort 只翻开关**；`ModelSwitch` 保留为可配置行（老网关/旧部署）。Qwen 行不变（本就显式传 `enable_thinking`，对默认开的混合模型恰好是确定性的正确写法），内置 models 改 `qwen3.8-flash`。
2. **请求语义**：effort `Off` → 开关显式关；Low..Max → 开关显式开（DeepSeek 无预算档）；**effort None（不请求）→ 不发任何键，服务端默认（=开）生效**。doctor 探测据此重写为**双轮**：default 轮（应见 reasoning）+ off 轮（应无 reasoning），两轮结果并排进报告——这正是对「默认开、参数可关」的直接实测。testkit `MockWire::sse_switched` 按请求体内容路由两条流。
3. **录制器**：`probes_for` 改按 provider 发显式开关字段（deepseek `thinking` 对象、qwen `enable_thinking`），模型统一 `deepseek-flash` / `qwen3.8-flash`；删掉「deepseek 移除 enable_thinking」的特判。**8 个真实 fixture 已于同日用新模型重录**（`record-fixtures --force`，脱敏通过）：新流形状与旧录制的关键结论全部复现——qwen3.8-flash 仍同 chunk 双键（reasoning_content 与 content 同在、其一恒空），reasoning 值仍严格先于 content；唯一的断言适配是 `real_qwen_dual_key_*` 里旧录制的内容词（"Hmm"）改为「非空即verbatim」断言（词属于某次录制，逐字节属性才是 fixture 钉的东西）。usage 细分形状待 doctor 双轮留痕（见 cli.md）：两家现在都报 `reasoning_tokens`。
4. **协议/kernel 的测试样例与 golden fixture 中的模型名不追改**：它们是「一个模型 id 长什么样」的示例载荷，不是供应商目录声明，动了会平白翻动 fixture 确定性门禁。

### 2026-10-01 · 真实录制落地与首轮 live 发现

`cargo xtask record-fixtures --force` 跑通：deepseek + qwen 各四探针，脱敏扫描通过，sidecar 齐。真实字节回放进契约测试（5 项 `real_*`），全部绿。**实测发现四条：**

1. **qwen 同 chunk 双键**（reasoning 流的关键形状）：42 个 chunk 同时携带 `reasoning_content` 与 `content`，其一恒为空串。按「键存在」处理会把空 text delta 插进 reasoning 并提前截断块；按「值非空」过滤的现有实现精确命中。`real_qwen_dual_key_chunks_do_not_split_the_reasoning_block` 以真实字节钉住此行为。
2. **两家 reasoning 值都严格先于 content 值**（deepseek 分 chunk、qwen 同 chunk 换值）→ **D3 定案：ChatCompletions 无需缓冲**（见待办）。
3. **工具调用探针首轮失败**：两家模型都文本作答不调工具（模型有权拒绝）。录制器改为 `tool_choice` 强制 + `must_contain` 字节校验（流里没有 `"tool_calls"` 就报错拒收），**`-toolcall.sse` 现存内容是文本流，待重录**；合成 fixture 继续顶住分片形状的测试。
4. **usage 真实形状**：两家都按 `stream_options.include_usage` 在收尾补 usage-only chunk（choices 空数组），与 openai-interface 的 null-to-empty 兼容层咬合无误。另：deepseek-chat 当前实际解析为 `deepseek-flash`（录到的 model 字段），能力表按族前缀不受影响。

401 错误体两家形状略异（qwen 把 `request_id` 放在 `error` 外层），openai-interface 的嵌套解析都吃下了；provider 原文完整抵达用户（`real_401_*` 测试断言）。

**重录后的 toolcall 分片（2026-10-01 第二轮）**：`tool_choice` 强制生效，两家都流出真实分片。deepseek 把参数 JSON 拆到**每个 token 一个分片**、finish 正确 `tool_calls`；qwen 分三片、**finish 竟是 `stop`**（kernel 按 `calls.is_empty()` 而非 finish_reason 决定下一轮，恰好免疫此怪癖——M0b 的设计决策）；**qwen 续传分片回显 `"id": ""`**——这会穿透到 kernel 的 `merge_call` 把真实 call id 覆盖成空串，工具结果配对失败。已在 adapter 翻译边界过滤空串（与双键 reasoning 同一条纪律：看值不看键），`real_toolcall_recording_replays_with_complete_fragments` 钉住。

### 2026-09-30
- M1 Phase 1 落地。六个模块：`config`（ProviderConfig/WireApi/RetryPolicy/ReasoningConfig，key 只从 env 读）、`capability`（ModelCapabilities + ReasoningWire 三行内置表 + 最长前缀覆盖）、`translate`（请求组装含 effort 映射与回放回填；chunk → 事件，usage 先于 done 出队——kernel 在 Done 处停读，晚到的 usage 块会被吃掉，故 adapter 缓冲一个 Done）、`error`（启动期/流中两套分类，401 提示 env_key）、`provider`（`LlmProvider` 实现：借用窗口内序列化、重试状态机、DeserializationError 单块跳过）、`registry`（register/Handle{dispose,replace}，单 provider 时任意模型名可解析）。
- 测试：单元 29 项 + wiremock 契约 16 项（事件序、byte-exact 回放、三族 effort golden、429 退避时序 `start_paused`、401/402 不重试、无 Done 结尾、取消零请求、header/bearer 到线、能力覆盖教学新族）。
- 基建：testkit `MockWire`/`sse_fixture`/`json_fixture`（sidecar `*.meta.json` 缺失即拒载）；六个合成 fixture（deepseek 文本/推理、qwen 工具调用、401/402/429 错误体）先行顶位，真实录制后替换。
- xtask `record-fixtures`：`--provider/--out/--force`；每家四探针（文本/推理/工具调用/401 故意错 key）；SSE 原始字节直存（openai-interface 解析后拿不到原始帧，故录制器自带 reqwest，与「不自写 HTTP」的产品面约束不冲突）；脱敏扫描命中即拒写不改写。
- TLS：ferritls（纯 Rust）经 `hatchery_llm::install_tls_provider()` 安装，daemon/CLI 启动时调用；测试并行调用安全（先装者胜）。

### 2026-09-28
- 初稿。三家参考项目（qwen-code 映射表、dsh 逐字节回放、atomcode 签名块）的经验合并成 ADR-0007；wire 层唯一依赖 openai-interface，类型不外泄。
