# 会话协调首批验收

目标顺序：全链路隔离验收 → 按投递结果构建历史 → 生成中纠正 → 话题与约定管理 → 睡眠、主动联系和可执行动作。本文只记录实际证据；后续范围保持待交付，不以本批结果宣称整个目标完成。

## 本地实现与检查

工作目录 `D:\CodeProject\DSH`；保留并发任务提交的关系网动效，未覆盖其文件。当前实现契约见 [会话协调](conversation-coordination.md)。

- 第一轮 daemon 单元测试 416 项通过；补充摘要、日记和对话 API 发布检查后，第二轮 429 项、最终一轮 430 项通过；内核生命周期另有 4 项通过。
- 部署兼容门禁 Python 测试最终 37 项通过。新表及已抑制决策需要相应 reader，不能降级后重新发布；只有初始化计数与空迁移标记时不虚构已使用语义而阻止安全回退。
- `cargo fmt --all -- --check` 修正格式后通过。
- 首轮严格 Clippy 暴露新旧告警；本批告警已修复。对 `31ba12a056c7a1c6993c729a0e9042d34abb5dff` 独立归档和本轮分别执行同一 `cargo clippy --workspace --all-targets --locked --message-format=json -- -D warnings`：基线 22 类独立诊断，本轮 21 类，新增 0 类。两者均退出 101，原有接口参数数目、格式、借用及类型复杂度告警仍在；没有关闭 lint 或将严格检查记为通过。去重对比和原始诊断保留于 `.local/conversation-coordination/clippy-*.jsonl`。

因另一个任务正运行 `target/debug/dshd.exe`，本批通过以下命令构建同源码、独立路径的本地进程制品，未停止或替换该进程：

```powershell
cargo rustc -p dshd --bin dshd --locked -- -o D:\CodeProject\DSH\.local\conversation-coordination\dshd.exe
```

首轮独立制品为 `development`，SHA256 `cfb7136eab35daada2896fc720662e8010c3962a20cd4e0fac19a6c229d81ff0`；这是本地检查制品，不能作为正式运行 SHA。

- 协调进程验收：21 项通过，包含真实 Rust 进程、签名合成 QQ 入站、loopback 模型屏障、物理 lease、旧快照不变、抑制后不创建 Outbox、后继原问题与更正、普通补充、部分投递、unknown、过期与重启。报告 `.local/conversation-coordination/local-coordination-first.json`。
- 隔离进程验收：20 项通过，核查 9 次完整 provider 请求，包含 A/B 并发、排队、交错、同 owner 不同会话、群发言者、冒充与进程重启。报告 `.local/conversation-coordination/local-isolation-r2.json`。
- 隔离首轮 fixture 误用了唯一 `source_key`，导致测试自己创建孤立 Run；原失败报告保留，修正为合法归档会话并启用外键后复验通过，未放宽程序校验。

以上模型和平台回执均为合成测试资料，未执行真实 QQ / 微信发送、外部模型或客户端验收。最终源码变更仍须随固定 SHA 重新构建和远端验证。

最终同源码本地独立制品 SHA256 为 `69d0d7c91b0a34dc2230d5e62bec715214d1660f55c4cc13ab697357891c41ad`；再次执行协调 21 项、隔离 21 项、日记 9 项、运行日志 19 项及摘要 24 项全部通过，对应 `.local/conversation-coordination/local-*-final.json` 和 `local-summary-r2.json`。

摘要旧进程脚本首轮失败也在未修改的基线二进制复现：它要求普通模型生成一条 12,004 字符 / 36,012 字节回复，触发现有每句 1,000 字符门禁，host 错误为“回复类型、单句长度或内容无效”。原失败和单独拒绝诊断保留。改为明确的临时历史超长来源 fixture 后，仍严格检查来源大于 24,000 字节、头尾、摘录 1,600 字节上限、截断标记和覆盖水位；普通回复限制未放宽。

## 远端与发布

本轮只读准备已核验：树莓派 5 Model B Rev 1.1、8,318,562,304 字节内存、Ubuntu 24.04.4 LTS / aarch64、Rust / Cargo 1.88.0。目录归属与 origin 正确，准备时正式服务健康。

提交、推送、最终部署 SHA、时间、远端命令、结果、日志和兼容回退证据待实际执行后填写。尚未执行的步骤不记为通过。
