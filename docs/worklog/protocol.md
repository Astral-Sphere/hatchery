# 工作记录：wire 协议（hatchery-protocol）

- 范围：JSON-RPC 方法/事件/数据模型、client helper、版本化
- 设计文档：[../design/protocol.md](../design/protocol.md)
- 相关 ADR：0001、0003

## 当前状态

**M0b 完成（2026-09-28）**：crate 的类型、方法表、事件、帧编解码与版本协商全部落地，93 个测试 + 4 个可运行 doctest 全绿，62 个 golden fixture 入库（`tests/fixtures/protocol-v1/`）。设计文档 `docs/design/protocol.md` 已按实现重写，含一张「草图 vs 实际」的修正表。

## 待办

- [x] (M0) 定 ItemId 方案 → **UUIDv7**（`uuid` crate 的 `v7` + `serde`；理由见设计文档开放问题 3）
- [x] (M0) 命名统一：wire 类型 `Session`（不再用 `Thread`），与 `session/*` 方法名、`sessions` 表名一致
- [x] (M0b) 全部类型定义（`Session`/`Item`/`ItemKind` 9 种/`Content`+`ContentPart`/`ToolStatus`/`ToolOutput`/`CheckpointKind`/`SignatureBlock`/`ServerEvent` 13 种 + `DaemonEvent`/JSON-RPC 帧）
- [x] (M0b) 方法名常量表（daemon 路由表的单一真相）+ `PROTOCOL_VERSION` 常量与支持区间
- [x] (M0b) 错误码枚举定型（14 个：5 个 JSON-RPC 标准 + 9 个应用码），数值 golden 锁定
- [x] (M0b) serde 往返测试 + golden JSON（`tests/fixtures/protocol-v1/`）+ 字段序确定性 + 公共 API 的可运行 doctest —— **golden 用纯 JSON 而非 insta**，理由见设计文档 §6
- [ ] (M1) client helper：attach_or_spawn、重连 + replay_from 补差、generation 过滤
- [ ] (M1) 事件 coalescing 策略实测调参（与 daemon hub 联动）；`ServerEvent::is_coalescable` 已就位
- [ ] (M2) edit_item/branch/rewind 方法的参数细节随 store 实现定稿（rewind 要带 `purge_untracked`，见 worklog/capabilities.md）

## 开放问题

见设计文档末尾 4 条（coalescing 策略、watch/takeover、大输出引用）。3 已关闭：

- 2026-09-28 ItemId → UUIDv7（用户裁决）。ULID 被否：26 字符可读性的收益不足以抵消「项目里出现第二种 id 格式」的成本；UUIDv7 同样时间有序，且 SQL/JSON/日志工具链天然认。

## 变更日志

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
