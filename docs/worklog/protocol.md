# 工作记录：wire 协议（hatchery-protocol）

- 范围：JSON-RPC 方法/事件/数据模型、client helper、版本化
- 设计文档：[../design/protocol.md](../design/protocol.md)
- 相关 ADR：0001、0003

## 当前状态

crate 骨架已在（M0a），**类型尚未定义**（M0b 的第一件事）。数据模型、方法表、事件表已在设计文档定稿；M0a 又定了两个悬案：ItemId 方案与 `Thread`→`Session` 命名统一。

## 待办

- [x] (M0) 定 ItemId 方案 → **UUIDv7**（`uuid` crate 的 `v7` + `serde`；理由见设计文档开放问题 3）
- [x] (M0) 命名统一：wire 类型 `Session`（不再用 `Thread`），与 `session/*` 方法名、`sessions` 表名一致
- [ ] (M0b) 全部类型定义（`Session`/`Item`/`ItemKind` 9 种/`Content`+`ContentPart`/`ToolStatus`/`ToolOutput`/`CheckpointScope`/`SignatureBlock`/`ServerEvent` 14 种/JSON-RPC 帧）
- [ ] (M0b) 方法名常量表（daemon 路由表的单一真相）+ `PROTOCOL_VERSION` 常量与支持区间
- [ ] (M0b) 错误码枚举定型（`SessionNotFound`/`GenerationMismatch`/`TurnInProgress`/`ApprovalDenied`/`StoreError`/`LlmError{retryable}`…）
- [ ] (M0b) serde 往返测试 + insta golden JSON（`tests/fixtures/protocol-v1/`）+ 字段序确定性 + 公共 API 的可运行 doctest
- [ ] (M1) client helper：attach_or_spawn、重连 + replay_from 补差、generation 过滤
- [ ] (M1) 事件 coalescing 策略实测调参（与 daemon hub 联动）
- [ ] (M2) edit_item/branch/rewind 方法的参数细节随 store 实现定稿（rewind 要带 `purge_untracked`，见 worklog/capabilities.md）

## 开放问题

见设计文档末尾 4 条（coalescing 策略、watch/takeover、大输出引用）。3 已关闭：

- 2026-09-28 ItemId → UUIDv7（用户裁决）。ULID 被否：26 字符可读性的收益不足以抵消「项目里出现第二种 id 格式」的成本；UUIDv7 同样时间有序，且 SQL/JSON/日志工具链天然认。

## 变更日志

### 2026-09-28
- 初稿。方法命名对齐 ACP 风格（session/*）以降低 acp crate 的翻译成本；事件模型吸收 codex EventMsg 与 atomcode AgentEvent 的教训（generation 字段是一等公民）。
- M0a：crate 骨架建立（GPL-3.0-only 元数据按可发布标准写全，依赖 serde/serde_json/thiserror/uuid）；ItemId 定为 UUIDv7；`Thread` 更名 `Session`（architecture.md 术语表、design/protocol.md §2 已同步）。存储侧的连带影响：`sessions.id`/`items.id` 以 TEXT 存 UUID（turso 的 `Value` 没有 UUID 类型，ADR-0010）。
