# 工作记录：wire 协议（hatchery-protocol）

- 范围：JSON-RPC 方法/事件/数据模型、client helper、版本化
- 设计文档：[../design/protocol.md](../design/protocol.md)
- 相关 ADR：0001、0003

## 当前状态

设计稿完成，未实现。数据模型（Thread/Turn/Item/ItemKind）与方法表已定；事件表已定初稿。

## 待办

- [ ] (M0) 全部类型定义 + serde 往返测试 + JSON fixture（版本兼容测试的基础）
- [ ] (M0) 定 ItemId 方案（倾向 ULID，见设计文档开放问题 3）
- [ ] (M0) 错误码枚举定型
- [ ] (M1) client helper：attach_or_spawn、重连 + replay_from 补差、generation 过滤
- [ ] (M1) 事件 coalescing 策略实测调参（与 daemon hub 联动）
- [ ] (M2) edit_item/branch/rewind 方法的参数细节随 store 实现定稿

## 开放问题

见设计文档末尾 4 条（coalescing 策略、watch/takeover、ItemId、大输出引用）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿。方法命名对齐 ACP 风格（session/*）以降低 acp crate 的翻译成本；事件模型吸收 codex EventMsg 与 atomcode AgentEvent 的教训（generation 字段是一等公民）。
