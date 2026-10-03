# 工作记录：wire 协议（hatchery-protocol）

- 范围：JSON-RPC 方法/事件/数据模型、client helper、版本化
- 设计文档：[../design/protocol.md](../design/protocol.md)
- 相关 ADR：0001、0003

## 当前状态

**M0b 完成（2026-09-28），评审后的两轮加固已入库（2026-09-29 / 2026-09-30）**：crate 的类型、方法表、事件、帧编解码与版本协商全部落地；golden fixture 在 `tests/fixtures/protocol-v1/`，覆盖率由测试自己机器检查（不靠文档里的数字）。设计文档 `docs/design/protocol.md` 已按实现重写，含一张「草图 vs 实际」的修正表。

## 待办

- [x] (M0) 定 ItemId 方案 → **UUIDv7**（`uuid` crate 的 `v7` + `serde`；理由见设计文档开放问题 3）
- [x] (M0) 命名统一：wire 类型 `Session`（不再用 `Thread`），与 `session/*` 方法名、`sessions` 表名一致
- [x] (M0b) 全部类型定义（`Session`/`Item`/`ItemKind` 9 种/`Content`+`ContentPart`/`ToolStatus`/`ToolOutput`/`CheckpointKind`/`SignatureBlock`/`ServerEvent` 13 种 + `DaemonEvent`/JSON-RPC 帧）
- [x] (M0b) 方法名常量表（daemon 路由表的单一真相）+ `PROTOCOL_VERSION` 常量与支持区间
- [x] (M0b) 错误码枚举定型（14 个：5 个 JSON-RPC 标准 + 9 个应用码），数值 golden 锁定
- [x] (M0b) serde 往返测试 + golden JSON（`tests/fixtures/protocol-v1/`）+ 字段序确定性 + 公共 API 的可运行 doctest —— **golden 用纯 JSON 而非 insta**，理由见设计文档 §6
- [x] (M1) client helper（2026-10-01）：`DaemonClient`（call_raw/call/hello，id 路由 + 超时）+ `EventStream`（`session/event` notification 解包）；attach_or_spawn 的 spawn 半边随 CLI Phase 4，重连补差随 e2e Phase 5
- [x] (M1) `HelloParams.boot_token`（加性字段，fixture 已更新）
- [ ] (M2) 事件 coalescing 策略实测调参（与 daemon hub 联动）；`ServerEvent::is_coalescable` 已就位（2026-10-03 由 M1 改标 M2：M1 定案 hub 不做 coalescing、留接缝，与 worklog/daemon.md 待办对齐）
- [ ] (M2) edit_item/branch/rewind 方法的参数细节随 store 实现定稿（rewind 要带 `purge_untracked`，见 worklog/capabilities.md）

## 开放问题

见设计文档末尾 4 条（coalescing 策略、watch/takeover、大输出引用）。3 已关闭：

- 2026-09-28 ItemId → UUIDv7（用户裁决）。ULID 被否：26 字符可读性的收益不足以抵消「项目里出现第二种 id 格式」的成本；UUIDv7 同样时间有序，且 SQL/JSON/日志工具链天然认。

## 变更日志

### 2026-09-30 · 评审后的两轮加固（2026-09-29 与 2026-09-30）

**读回来的形态要和写出去的形态分开验证。** `SessionPatch { title: Some(None) }`（「清空标题」的记号）写出去是 `"title":null`，读回来却成了外层 `None`（「别动」）——serde 不处理嵌套 Option，双 Option 存在的意义正好被吃掉。自定义 visitor 之后三态分明：缺失 = `None`、`null` = `Some(None)`、有值 = `Some(Some(_))`；写出去的形态没变，所以没有一个 golden 动过。同一套写法现在也用在帧的 `id` 上。

**显式 `"id": null` 一律拒绝（`FrameError::NullId`）。** `Probe.id` 原本是 `Option<Id>` + `#[serde(default)]`，于是 `null` 与「字段缺失」塌成同一个值：`{"id":null,"method":"session/prompt"}` 被分类成通知——一个本该有回复的请求被静默降级，调用方永远等下去。漏写 `skip_serializing_if` 的 `Option<Id>` 序列化出来正好是这种帧，所以它是要报告的客户端 bug，不是要吸收的噪声。`classify` 的文档早就这么写了，这轮是让代码追上文档；规范允许的那种 null-id 错误响应由 M3 的 ACP bridge 归一化。

**`FrameDecoder` 的三处修正（2026-09-29）**：坏行必须在报错**之前**排空，否则此后每次 push 都在同一批字节上失败；`MAX_FRAME_BYTES` 要在线循环**内部**检查，否则带终止符的超长帧能整帧通过；扫描要记住水位（`scanned`）——原本每次 push 都从字节 0 重扫，实测 2 MiB 帧按 32 字节分块喂要 157 s，加水位后 0.02 s。

**方法结果与枚举覆盖改成机器强制（2026-09-29）。** 20 个方法里只有 5 个的结果有 fixture，而所谓「typed 校验」接受任意 JSON 对象；现在每个方法都有 fixture + 会拒绝 `{}` 的校验器。枚举 → 注册表的覆盖由宏从同一份来源生成名字表与变体表，编译器兜底。未知枚举值是硬失败、未知字段必须忽略，两条都写进设计文档 §6 并各有测试。

**fixture 生成要能并发跑。** nextest 并行跑多个测试二进制，而 `version_compat` 从磁盘读 golden、`golden_fixtures` 往磁盘写：非原子的重写会让读者看到半截文件，报一个从没发生过的兼容性破坏。改成 tempfile + rename，读者在 `UPDATE_FIXTURES=1` 期间跳过。

**UUIDv7 的 id 在同一进程内严格递增（实测，uuid 1.26.1）。** `Uuid::now_v7()` 走进程级共享的 `ContextV7`：同毫秒内计数器递增而不是重新随机，计数器溢出就把时间戳往前推一毫秒。`ids_minted_back_to_back_increase` 连铸 1000 个 id 把这条钉住（上游改换上下文会主动失败，性质同 store 的 `SPIKE_*` tripwire）。依赖它的地方：`list_sessions` 的 `id ASC` tiebreak、`all_items`/`branch_tree` 的 `ORDER BY created_at ASC, id ASC`。**跨进程不成立**：时钟回拨后重启的进程会铸出比库里已有 id 更小的 id。（此前多处注释把同毫秒的 id 说成「随机尾巴、不保证顺序」，与实现相反，已一并改正。）

**`ItemIdRange` 不提供成员判定。** 原来的 `contains` 比 id 大小，前提是「id 序 == 链序」，而按上一条，这个前提只在同进程、时钟未回拨时成立。compaction（M5）用它判「这条 item 是否已被摘要覆盖」，错一次就是静默的历史损坏（该重放的不再重放，或反之）。span 因此是**位置**语义：成员是沿链落在两端点之间的那些 item，只有持有链的一方能解析——M5 与它的第一个消费者一起在 store 的树遍历里实现，现在不加没有调用方的 API。类型上只剩 `first`/`last` 与「两端含」的约定。

### 2026-09-28

**M0b 落地**（93 测试 + 4 doctest）。以下结论全部来自实测或落地时的结构性矛盾，不来自推断：

- **`#[serde(flatten)]` + 相邻标签枚举可用（serde 1.0.229）**：`ItemKind` 用 `tag="kind"`/`content="payload"` 并在 `Item` 上 flatten，得到 `{"id":…,"kind":"user_message","payload":{…}}`——与 `items` 表的 `kind`/`payload` 两列一一对应。九个变体的 `to_payload`/`from_parts` 往返有测试逐条断言。
- **内部标签枚举写不出「持有原始值的 newtype 变体」**：`StreamEvent::TextDelta(String)` 序列化直接失败（serde 的限制），改成命名字段变体 `TextDelta { text }`。
- **信封与事件字段重名会让帧写得出、读不回**：`SessionUpdated { session }` 与信封的 `session`、`GenerationBumped { generation }` 与信封的 `generation` 各撞一次。前者改成 `state`，后者改成无载荷变体（新代际就是信封的 `generation`）。新增测试 `no_event_field_collides_with_the_envelope` 防复发——它抓出的正是这两个。
- **内部标签枚举的 tag 写在载荷字段之前**：`{"session":…,"generation":…,"type":"text_delta","item":…,"text":…}`。`typed_serialization_keeps_declaration_order` 锁定这一顺序。
- **flatten 会把可变长字段放到最后**：`Item` 的 `created_at` 在 `kind`/`payload` 之后输出，与声明序一致（`value` 是 BTreeMap，golden 里是字母序）。
- **insta 不适合数据驱动 fixture**：快照名由断言表达式推导且必须字面量，62 个 fixture 要么手写 62 条断言、要么放弃注册表。最终用纯 JSON + `UPDATE_FIXTURES=1` 生成器；`scripts/ci.sh` 的 `INSTA_UPDATE=no` 让门禁里的重写被拒绝。详见设计文档 §6。
- **协议层不需要 `deny_unknown_fields`**：forward-compat 要求旧客户端忽略未知字段，测试 `a_missing_optional_field_deserializes_to_none` 断言未知字段不致命。
- `PROTOCOL_VERSION = "1.0.0"`，兼容判定只看 major（`is_compatible`）；无法解析的版本一律拒绝。

### 2026-09-28（M0a）

- 初稿。方法命名对齐 ACP 风格（session/*）以降低 acp crate 的翻译成本；事件模型吸收 codex EventMsg 与 atomcode AgentEvent 的教训（generation 字段是一等公民）。
- M0a：crate 骨架建立（GPL-3.0-only 元数据按可发布标准写全，依赖 serde/serde_json/thiserror/uuid）；ItemId 定为 UUIDv7；`Thread` 更名 `Session`（architecture.md 术语表、design/protocol.md §2 已同步）。存储侧的连带影响：`sessions.id`/`items.id` 以 TEXT 存 UUID（turso 的 `Value` 没有 UUID 类型，ADR-0010）。
