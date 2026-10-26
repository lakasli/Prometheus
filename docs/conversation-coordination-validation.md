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

修复在本地完成：已抑制候选先作为原始审计保存，跳过呈现解析与媒体准备；最终保存事务再次解决“校验期间被更正”的竞态。新回归刻意返回一个含两句的无效文本对象，修复前二进制可复现失败，修复后仍须无回复计划、无 Outbox、无旧候选历史及无提前归还 lease。回滚脚本同时补上连接拒绝重试。修复后部署与复验见下一节，不以修测试替代程序修复。

修复本地：daemon 430 项、部署兼容 37 项、停止/回退门禁 10 项以及协调进程 22 项均通过；新进程制品 SHA256 为 `5f83a941a6666a5760996e5f7890b213b0aae5a0971fa25fca3df005a271b649`，仍标记为 `development`。本次真实模型脚本保留三次请求上限，并用 `--prior-model-requests 1` 显式记录首轮已发生的一次请求，重测成功时累计为四次，不抹去失败消耗。

## 修复发布与最终复验

修复已提交、推送并部署为 **`468fcee400601a590e7bc4c8c573d16a7c20bca9`**，部署前已核实属于远端 `origin/main`。实际命令：

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-468fcee.sh 468fcee400601a590e7bc4c8c573d16a7c20bca9 /home/campanus/.cache/dsh/admin-468fcee400601a590e7bc4c8c573d16a7c20bca9.tar.gz b1822c51717f5ac977924a38917bdb4b654d3d994d89daf56de5b3db5c0fa54e
```

部署退出 0；Linux daemon 431 项、内核 4 项通过，release 构建 9 分 22 秒。2026-10-26 22:56（Asia/Shanghai）复核源码 HEAD、静态 release.json、live / ready、current 及实际进程 exe 均为完整 SHA；systemd active/running。Linux 二进制 SHA256 为 `041c5b4fd9a8d39d6abbe0c21e3bfbf8ae6846f33cd7305cbecec44e915dabc5`。首次健康轮询的短暂连接拒绝保留，随后同次部署就绪。构建与测试日志为 `shared/logs/{build,test}-<完整SHA>.log`。

从固定 release 顺序执行 6 套临时实例进程回归，退出码均为 0：隔离 21、协调 22、日记 9、摘要 24、日志 19、记忆 38，共 **133 项**。均使用合成模型、来源与回执。命令、时间、退出码和报告路径见 `shared/logs/conversation-acceptance-468fcee-command-results.json`，最终核对为 `conversation-acceptance-468fcee-assessed.json`。旧日志报告没有统一 `status` 字段，外层初次误标失败；按真实退出码与 19 项结果另存 assessed，原汇总保留，未因此重复测试。

真实模型实际命令：

```sh
python3 -u /home/campanus/apps/dsh/releases/468fcee400601a590e7bc4c8c573d16a7c20bca9/source/tools/conversation-coordination-live.py --binary /home/campanus/apps/dsh/releases/468fcee400601a590e7bc4c8c573d16a7c20bca9/bin/dshd --web-dir /home/campanus/apps/dsh/releases/468fcee400601a590e7bc4c8c573d16a7c20bca9/web --source-data-dir /home/campanus/apps/dsh/shared/data --expected-commit 468fcee400601a590e7bc4c8c573d16a7c20bca9 --prior-model-requests 1 --report /home/campanus/apps/dsh/shared/logs/conversation-coordination-468fcee-real.json
```

22:53:51–22:54:01 执行，退出 0，**11 项通过**。严格发生 3 次新真实 HTTP 200，均限制 `max_tokens=1024`；加首轮失败已发生的一次，共 4 次。完整请求体摘要与冻结 Prompt 匹配。原轮次 completed + suppressed revision 2，保存 `output_suppressed_by_correction` 且无 Outbox；更正后 A 与独立 B 完成，仅有被临时 pacing fence 阻断的 queued Outbox，三者物理 lease 最终回收。屏障位于真实转发之前，**不证明真实 provider 已在途请求的取消**。未测试真实 QQ / 微信上传、客户端播放或用户已读。

Playwright 经 Windows SSH 隧道访问同一 Linux 制品的全新临时实例，23:01 完成 `assistant-runtime.spec.ts`，1 个完整场景通过，覆盖认证、性格版本应用 / 回退、真实进程合成模型对话、只读日志及 320/390 像素布局。桌面与窄屏截图目视核对，未运行前端开发服务器。首轮缺 fixture-info 路径失败，第二轮复用已被首轮改动性格的夹具导致初始性格断言失败；两份报告保留，第三轮以新数据目录和完整参数通过，未放宽断言。自身 fixture PID 与 tunnel 已停止。报告和七张截图位于修复工作树 `.local/conversation-coordination/browser-468fcee-r{1,2,3}.json`、`browser-evidence-r3/`；未登录生产或使用真实渠道。

验收后只读核对：生产配置 revision 7、4 个会话、92 条消息，记忆、日记事件 / 条目、人物和日程活动仍为 0。配置、已保存性格、稳定身份及三个来源 / 模型 / 规划授权策略摘要与部署前精确一致。首次摘要编码不同的诊断保留为 r1；采用相同 SELECT-rowset JSON 后全部相等，见 `production-readonly-468fcee-r2.json` 和 `post-acceptance-readonly-468fcee.json`。没有自动授予采集、模型出域、联系或动作权限，没有覆盖生产性格或数据库。

本批完成的是上述隔离、投递历史与保守明确更正范围。话题、约定和预算的新实现仍在独立工作树开发；睡眠、真实主动联系、可执行动作与完整 M2 / M4 / M5 尚未据此完成。
