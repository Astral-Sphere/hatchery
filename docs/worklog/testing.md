# 工作记录：测试体系（横切）

- 范围：测试纪律与分层、hatchery-testkit、fixture 与录制、CI 门禁、覆盖率、fuzz/属性/基准、手动实测清单
- 设计文档：[../design/testing.md](../design/testing.md)
- 相关 ADR：全部（每条 ADR 的「后果」节都有对应测试面）

## 当前状态

设计稿完成（2026-09-28），未实现。测试分层、testkit 清单、不变量→测试映射表、CI 门禁均已定义；随各里程碑落地。

## 待办

- [ ] (M0) workspace 加 `hatchery-testkit` crate（dev-only）；nextest/insta/proptest/llvm-cov 进工具链；`.config/nextest.toml` 分组（default/invariants/slow/gui/live）
- [ ] (M0) CI 骨架：PR 门禁全套（fmt/clippy -D warnings/nextest 默认组/doctests/fixture 确定性）+ nightly 工作流占位
- [ ] (M0) clippy `disallowed_methods` 配置（不变量 4 编译期强制）
- [ ] (M0) 三个 spike（libSQL/git/i18n）的结论**必须沉淀为回归测试**，不只是文字记录
- [ ] (M0) store：属性测试参考模型 + 崩溃测试框架（真实子进程 kill -9）
- [ ] (M1) testkit v1：ScriptedProvider/MockWire(sse fixture)/Memory 后端三件套/TempWorkspace/TestDaemon/ClientProbe
- [ ] (M1) fixture 录制 xtask + 脱敏（API key 扫描）+ provenance 元数据格式
- [ ] (M1) 不变量套件 `invariants` 分组建立（6 条映射齐 4 条：1/2/3/5；4 编译期已保；6 随 M2）
- [ ] (M1) e2e 场景 1-2 落地（最小对话、双前端扇出）
- [ ] (M2) 影子 Git 安全测试 + 硬门测试（不变量 6/5 补全）；fuzz targets 上线 nightly
- [ ] (M2) e2e 场景 3-6；契约测试套件（LlmProvider/SessionStore/FsBackend/TerminalBackend/ApprovalGate）
- [ ] (M3) fake 宿主集成 + dummy-acp-agent；e2e 场景 7-8；Zed 真机清单执行并记录
- [ ] (M4) gui 组进 CI（GNOME SDK 容器 + xvfb）；10k 性能 fixture；e2e 场景 9
- [ ] (M2+) criterion 基线入库 + nightly 对比；cargo-mutants 试点（store/kernel）

## 开放问题

见设计文档末尾 4 条（testkit 辅助二进制形态、mutants 投入、GUI 截图 diff、e2e 进程形态）。解决过程记录于此：

- （暂无）

## 变更日志

### 2026-09-28
- 初稿（应用户要求补充：「这个项目需要详尽的测试，确保所有代码都能如期运行」）。设计要点：
  - 测试纪律 5 条，其中「实测优先」与「文档示例可运行（doctest 进 CI）」来自用户既有工作偏好；
  - 不变量 2（模型可见=已记录）的锁死方式：MockWire 记录实际请求体字节 vs store 重建逐字节比对——一条测试同时覆盖组装/存储/回放三层；
  - store 是最重投入（属性测试 + 双实现参考模型 + kill -9 崩溃测试），数据层错误不可原谅；
  - GUI 策略 =「逻辑出 GTK」（view-model 纯 Rust 可测）+ xvfb 冒烟 + 人工清单兜底；
  - live 测试不进 CI（密钥问题），以 `hatchery doctor` 与手动 nextest 组承载。
