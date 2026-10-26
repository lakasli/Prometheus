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

首轮程序提交为 `2d3ea9886317a28bccdcc5c3b7e67c4306a97e38`，保留并行关系网验收后，以合并 SHA `1789494f31293b1c5f27f39440d262e207a0e1cb` 推送并部署。静态前端在独立工作树构建，25 文件 / 167 项前端测试通过；归档 SHA256 为 `978572dc7736b192df910443b44070f259362d3ab410b276b50bca0b3869f82e`。

远端实际执行：

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-1789494.sh 1789494f31293b1c5f27f39440d262e207a0e1cb /home/campanus/.cache/dsh/admin-1789494f31293b1c5f27f39440d262e207a0e1cb.tar.gz 978572dc7736b192df910443b44070f259362d3ab410b276b50bca0b3869f82e
```

部署退出 0，Linux daemon 431 项（含 Linux 专用项）与内核 4 项通过；release 编译 9 分 21 秒。源码、静态 release.json、live / ready 接口均为完整 `1789494...`；二进制 SHA256 为 `aa02409e1ffb876fd0a36a78912e14a105c891729c3c48ab9c7b5d92ca063080`。启动轮询曾短暂连接拒绝，随后同轮核对成功，原记录未删。

从该固定 release 执行临时实例验收：隔离 21、协调 21、日记 9、摘要 24、日志 19、记忆 38 项均通过；均为模拟模型 / 合成来源与回执，不计真实平台。报告在 `shared/logs/<suite>-1789494-aarch64.json`，构建及测试日志为 `test-<完整SHA>.log` 与 `build-<完整SHA>.log`。

## 真实模型首轮失败与回退

限额真实模型检查仅只读生产当前模型配置及引用凭证，真实配置为 v7；所有对话、QQ 身份、签名凭证及投递队列属于临时合成实例，真实 QQ / 微信外发始终未执行。首轮报告 `shared/logs/conversation-coordination-1789494-real.json` 保存 4 项已通过检查、1 次真实模型 HTTP 200 与实际请求摘要，整体退出 1，不能记为真实功能通过。

失败发生于已抑制的原轮次结算：模型返回的第二个文本对象含两句，程序仍先执行逐句呈现校验，导致旧候选未按审计完成。另发现测试原断言把原始用户问题中的标记也当成应从后继全部排除的旧答案，已改为按 assistant 角色检查，继续保留原问题及全部事实、来源、lease 与无 Outbox 断言。补充诊断在清理临时实例前记录各角色 Run 的状态、host 错误及有界合成回复；模型凭证一律替换。

按当前 reader 门禁检查通过后执行 `rollback.sh 31ba12a056c7a1c6993c729a0e9042d34abb5dff`。脚本已切换并启动上一版本，但其第一次健康请求遇到监听尚未就绪而退出 1；单独带连接拒绝重试的复核退出 0，current、live、ready 和 systemd active/running 均确认回到 `31ba12a...`，NRestarts=0。没有删除或恢复用户数据库，也未回退配置；原失败与复核分开记录。

修复在本地完成：已抑制候选先作为原始审计保存，跳过呈现解析与媒体准备；最终保存事务再次解决“校验期间被更正”的竞态。新回归刻意返回一个含两句的无效文本对象，修复前二进制可复现失败，修复后仍须无回复计划、无 Outbox、无旧候选历史及无提前归还 lease。回滚脚本同时补上连接拒绝重试。修复再次提交、推送、固定 SHA 部署与实际复验结果待补充，不以修测试替代程序修复。

修复本地：daemon 430 项、部署兼容 37 项、停止/回退门禁 10 项以及协调进程 22 项均通过；新进程制品 SHA256 为 `5f83a941a6666a5760996e5f7890b213b0aae5a0971fa25fca3df005a271b649`，仍标记为 `development`。本次真实模型脚本保留三次请求上限，并用 `--prior-model-requests 1` 显式记录首轮已发生的一次请求，重测成功时累计为四次，不抹去失败消耗。
