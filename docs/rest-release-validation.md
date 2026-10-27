# 睡眠功能集成与发布验收（2026-10-27）

依据架构书 9.6–9.7 与当前接续顺序，将已提交的 `322714af85c7f0809f628cee6bb9fd08f772dfb1` 睡眠分支合入 `335319e` 主线，保留回复四项上限、上下文预算、来源与投递证据。实现契约见 [睡眠协调](rest-coordination.md) 与 [整理执行](rest-consolidation.md)。旧分支的本地测试与失败仍在 [原记录](rest-runtime-validation.md)，不改写成此次通过。

本轮额外修复 QQ 的独立入队路径未登记睡眠控制候选的问题；只在实际身份、原文和 Inbox 同一事务中登记普通直接输入，媒体与专用命令不取得控制能力。自然控制仍须独立来源 grant 和启用的作息策略。入睡确认加入宿主确定的唤醒日期、时间与 IANA 时区，仍受精确载荷、generation、原平台期限和发送前复核约束。

新增 `tools/rest-runtime-smoke.py` 使用真实 Rust 进程、签名合成 QQ 入站与 loopback HTTP。初始化账号前设置持久发送 fence；验证默认关闭、认证 / Origin / CSRF / 版本、控制幂等、接收与健康不中断、在途请求 lease、原始结果检查点、重启与原期限、来源独立授权及撤销。Linux 使用 boot ID 验证继续原期限；Windows 的进程时钟换代按已有契约提前唤醒，不当作 Linux 期限恢复证明。测试从不连接真实 QQ / 微信，不读取生产会话。

日记旧回归在无 provider 时先验证两次相同整理的幂等性，再配置 provider 并验证真实模型作业。这样背景模型合法换版不能抢在两个 API 请求之间造成夹具误判；原恒等断言、九项业务检查及撤销 / 遗忘检查保留。

## 本地与已存在正式版的定向检查

- 合并基线 Rust 库 631 项通过，26.04 秒；修复后专项、完整检查及进程结果见下方独立记录。
- 前端 30 文件 / 204 项通过，类型检查与构建通过；Python tools/tests 135 项通过。未把原分支的检查数量与本轮相加。
- 新进程测试首轮在授权 QQ 自然入睡处失败，前 16 项通过；由此定位并修复上述实际接线遗漏，报告保留 `.local/rest-release/rest-runtime-local-r1.json`。修复后须另存复验，不能改写原失败。
- 修复后 r2 / r3 因 Windows 重启不继承手动时钟确认，新的入睡被正确拒绝；补上本临时实例重启后的显式时钟确认，未改变业务门禁。r4 **22 项通过**，7.87 秒，报告 `rest-runtime-local-r4.json`。其中 Windows 提前唤醒与 Linux 原期限恢复明确分开，不把前者记作后者通过。
- 最终本地 `cargo test --workspace --locked --quiet`：daemon **631 项通过**、26.22 秒，内核生命周期另 **4 项通过**；完整日志 `.local/rest-release/rust-final.log`。新确认时间专项在原七项协调测试内，未重复计数。日记进程原 **9 项通过**，5.55 秒，报告 `diary-local-r1.json`。格式与 diff 检查通过。
- 部署前对当时正式 `ac9b70389507455050aea32862d9a268a7b9c940`，从独立干净 `53a5a89d8069b2846c2eff6eb4db913410cb13a0` 测试工作树运行 `--cases GROUP,S05 --max-real-requests 6`：2 个宿主场景通过，严格 5 次真实 HTTPS 200，峰值并发 1、重试 0。报告 `shared/logs/conversation-state-ac9b703-targeted-real.json`，输出同名 `.log`。状态 `host_complete_review_pending`，表达仍待审；不把前两轮 22 / 23 次失败记录重写或合并成通过统计，也不当作本次新睡眠制品的真实模型验收。

## 发布与远端结果

准备阶段 SSH 曾超时，恢复后核验目标为 Raspberry Pi 5 Model B Rev 1.1、Ubuntu 24.04.4 LTS、aarch64，Rust / Cargo 1.88.0。可用内存采样约 7,376 MiB、swap 0，磁盘剩余 24 GiB；目录归属 campanus、origin 正确、repo 工作区干净。上一正式版本为 `ac9b70389507455050aea32862d9a268a7b9c940`，`dsh.service` active/running，ready 返回同 SHA。

程序已提交并推送为 **`6e8524a21f2813a802a2f99254c56316dac15bd5`**，远端确认该提交属于 `origin/main` 后部署。北京时间 **2026-10-27 09:50:13** 完成切换，部署退出 0；首次健康轮询短暂连接拒绝，随后 ready，同轮日志保留。实际命令：

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-6e8524a.sh 6e8524a21f2813a802a2f99254c56316dac15bd5 /home/campanus/.cache/dsh/admin-6e8524a21f2813a802a2f99254c56316dac15bd5.tar.gz 4fc95724d2d7e256fa8f166460808d7e03f7dfdb888c7e90397977f96b11edef
```

Linux 全量检查为 daemon **632 项通过，550.75 秒**、内核 **4 项通过**；test profile 构建 5 分 07 秒，release 编译 **10 分 33 秒**。原始日志 `shared/logs/test-<完整SHA>.log` 与 `build-<完整SHA>.log`。Linux binary SHA256 为 `dacab465fcc496f78a298ea2e5a100273dbe4774f499885c64ba89a797ef3f1c`。源码 HEAD / 干净工作区、前端 release.json、live / ready、current 均核对为同一完整 SHA；systemd active/running，PID 1290508、NRestarts=0。

本地严格 Clippy 仍 **退出 101**，48 条诊断，与原睡眠分支基线同集，新增 / 移除均为 0；未关闭 lint 或称严格检查通过。原输出与比较为 `.local/rest-release/clippy-final.jsonl`、`clippy-final-stderr.log`、`clippy-comparison.json`。

### 固定制品的八套隔离进程回归

09:52:50–09:53:46，最多两套并行、各用独立临时数据库与 loopback 模型，所有退出码为 0。命令、时间、退出与输出位置保存在 `shared/logs/rest-release-6e8524a-command-results.json`；各报告和 stdout 为同目录 `rest-release-6e8524a-<套件>.json/.log`。

| 套件 | 本轮结果 |
| --- | --- |
| rest-runtime | 22 项通过，19.60 秒；Linux boot ID 连续，重启保留原截止并自动唤醒；签名合成 QQ 接收、来源控制及撤权、原结果复用、无第二次模型请求 |
| diary | 原 9 项通过，5.58 秒；保留幂等及更正 / 遗忘断言 |
| conversation-coordination | 22 项通过，49.31 秒 |
| conversation-isolation | 21 项通过，7.18 秒 |
| memory | 原 38 项通过，7.93 秒 |
| summary | 24 项通过，14.84 秒 |
| assistant-logs | 19 项通过，2.67 秒；旧报告无统一 status，按真实退出码与 checks 验证 |
| conversation-reply-normalization | 5 场景 / 35 项主断言通过，3.99 秒；保持四项整体拒绝、来源独立偏好与禁用收件人 / URL 字段 |

这些不是实际 QQ / 微信发送、平台回执或客户端展示证明，所有外发均被临时持久 fence 阻断；没有生产来源或历史进入测试。

### 一次真实模型睡眠整理

独立测试脚本为已提交、推送且干净的 **`50f48df50925d99112bd4dd63a0e6a261907102d`**，程序仍为上述 6e 制品。测试脚本的两个后续提交没有应用行为变化，因此未为其重复部署或重启正式服务。09:54 执行：

```sh
python3 -u /home/campanus/.cache/dsh/rest-validation-50f48df/tools/rest-consolidation-live.py --binary /home/campanus/apps/dsh/releases/6e8524a21f2813a802a2f99254c56316dac15bd5/bin/dshd --web-dir /home/campanus/apps/dsh/releases/6e8524a21f2813a802a2f99254c56316dac15bd5/web --source-data-dir /home/campanus/apps/dsh/shared/data --expected-commit 6e8524a21f2813a802a2f99254c56316dac15bd5 --report /home/campanus/apps/dsh/shared/logs/rest-release-6e8524a-real-selection.json
```

退出 0，**5 项通过**，真实 reserved / forwarded / HTTPS 200 均严格为 **1**，重试 0，输入 **845 字节**、输出上限 **1600**，宿主保守预留 **2701**。只读正式模型配置与引用凭据，真实 key 仅在转发器内存；临时实例只存合成 key，使用一条私人合成来源。实际请求摘要与冻结作业 input_digest / 完整 body 证明一致，模型完整返回原文，canonical 日记及来源 DAG 成功提交，保留“计划 / 尚未完成 / 不证明完成或送达”状态，物理 rest lease 回收，独立唤醒成功。原报告及同名 `.log` 不改写；此结果是有界原文选择，不能推广为自然总结质量、完整月或人物写入验收。

本轮此前 5 次 GROUP / S05 调用与此次 1 次整理分开记账。Agent 审读前五条合成回复：两个换题均直接回答 4；S05 过程保留在三句内；GROUP 的 B 另附一行算术解释。仍维持 expression review_pending，不把 Agent 审读当作人工表达确认。

### 固定 Linux 前后端的浏览器与保留核对

09:55，Playwright 经 Windows SSH 隧道访问同一 Linux 制品的全新临时实例，`rest-state.spec.ts` **1 / 1 通过，6.00 秒**，unexpected / flaky / skipped 均 0、page_errors 0。实际走通独立策略、来源许可、入睡 / 唤醒、generation / If-Match、状态与只读整理步骤，视口 1440 / 390 / 320；五张真实截图已目视核对，320 字段与长摘要换行无横溢。证据在 `.local/rest-release/browser-result-r1.json`、`browser-evidence/rest-browser-metadata.json` 和 PNG；这是 Agent 浏览器操作与视觉检查，未冒称用户本人验收。最初读取不存在的 validation.json 仅是证据文件名误认，测试本身第一次即退出 0，真实元数据名如上，没有因此重跑。

临时实例完成 awake / source grant revoked 清理后，按固定 PID 与 cmdline 核对，只停止本轮 fixture 1293615、daemon 1293622 与本地 tunnel 39316。记录 `browser-cleanup.json` 与 `browser-ownership-local.json`，未停止生产服务。

生产库始终使用 `mode=ro` / query_only 核对：配置、稳定身份 / 性格、日记来源策略、自主日程授权的四个精确 SELECT-rowset SHA256 与部署前一致；会话 / 消息 / 记忆 / 日记事件 / 日记条目 / 人物 / 日程活动数量保持 **4 / 92 / 0 / 0 / 1 / 0 / 0**。正式 rest generation 0、awake，自然控制 / 定时作息关闭，周期、操作、控制 grant、控制候选和待发布缓存均 0。前后证据 `shared/logs/rest-release-6e8524a-{before,after}.json`，不是通过删除行保持数量。

部署脚本已在停止服务后执行 SQLite backup，并保留上一 ac9 release。当前 6e 的兼容门禁对旧 ac9 source 实际退出 0，依据是新增 Rest 尚未使用；**本轮未执行代码回退或数据恢复**。后续启用或产生持久 Rest 决策后须重新核验，不能盲降。远端本轮八套回归、真实整理、前后摘要、部署元数据及编译 / 测试日志共 **24 文件**已复制到 `.local/rest-release/remote-evidence/`，逐文件 SHA256 与远端一致；清单 `.local/rest-release/evidence-copy-manifest.json`。

## 保留的功能边界

默认自然控制、定时作息、来源采集与模型出域不自动启用；正式数据和已保存性格不被升级覆盖。单期超过 16 个来源仍 `partial/source_batch_limit`，完整月的多批归并仍待实现。人物 / 好感没有独立睡眠写入授权时保持零写入。真实 QQ / 微信控制确认、真实渠道暂停 / 恢复与客户端展示未测试，不能由合成入站或队列证据替代。完整睡眠整理、主动联系、Python 动作和完整 M2 / M4 / M5 不因此宣称完成。
