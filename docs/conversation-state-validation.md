# 话题、约定与上下文预算验收

目标仍为：隔离 → 投递历史 → 生成中纠正 → 话题与约定，再扩展睡眠、主动联系和可执行动作。本页只记录当前切片证据，不称完整自然语言协调、40 场景语义达标或完整 M2 / M4 / M5。当前正式基线 `468fcee400601a590e7bc4c8c573d16a7c20bca9` 的结果见 [首批记录](conversation-coordination-validation.md)。

实现契约为 [会话状态](conversation-state.md)、[约定](conversation-commitments.md)与[预算](context-budget.md)。开发在独立工作树 `C:\Users\AIIT\.codex\worktrees\conversation-delivery\DSH` 完成，保留并发关系网提交及原工作目录已有进程。后续睡眠存储在另一独立工作树准备，未混入本切片的程序或验收结论。

## 本地检查

- 冻结源码最终 `cargo test --workspace --lib --locked --quiet --target-dir D:\CodeProject\DSH\target` 退出 0，daemon **549 项通过**，运行 21.73 秒。包括新增“同事实不同备注不冲突”和排队后失效发送门禁，报告 `.local/conversation-state/local-test-final.log`。正式 Linux 制品仍须从固定 SHA 编译并全测。
- 真实 worker 集成含完整末尾输入、超出最近 12 轮后的完整原问题续接、不同 owner 隔离、必需块超预算时零请求、未选中资料零 usage、策略换版不重写缓存 Prompt、约定换版阻断发布、在途物理 lease、匿名群空引用、FTS 故障和授权账本故障、实际投递证据下的保守呈现冷却。
- 发送领取另有 11 项实际 claim 回归：Outbox 已排队后约定 / participants 换版或 TTL 到期，QQ 新 / 兼容领取器与微信领取器重新核验；仅取消该 Run 尚未领取的段，保留 sending / sent / accepted / unknown、原发布决定和所有 lease。原 binding / context / Prompt 三证缺一则拒绝，读取旧结果不补造证明；完全没有新快照的历史路径不虚构新证明。另复验既有 QQ 5、微信 13、worker 16 和 helper 7 项，均实际通过；最终 549 已统一纳入最新强化。
- API / 领域专项覆盖来源审核、主体和对象、真实活动关联、重复与并发版本、未知时间、遗忘 / 撤权、同 scope 精确消费者、跨 TTL、敏感候选不派生和原始消息保留。普通日记版本更新不新增历史水位；来源精准失效不因提高整个 scope epoch 误伤无关在途上下文。
- Rust 格式检查通过；部署 reader / 停止 / 回退门禁 Python 59 项通过。新状态、约定、预算共 17 张表逐表核对：有数据（含删除水位、epoch、幂等记录和仅保存策略）阻旧 reader，空初始化不虚构已用语义。
- 前端 28 文件 / 185 项测试通过，类型检查和生产构建通过。预算位于配置“工作节奏”，独立保存；约定位于日历工具栏弹窗；日志只读提供上下文选择。此处测试使用模拟 API，不能代替后续真实浏览器 / Linux API 验收。
- 严格 Clippy 未通过，退出 101。首批最终报告与本轮冻结源码 `clippy-release.jsonl` 均为 48 条诊断、21 个按文件 / lint / 消息去重的键，新增 0；本轮新增两类已修正，原有 lint 债务未关闭。原始报告、初轮新增诊断及比较保存在 `.local/conversation-state/clippy*.json*`，对照为原工作目录 `.local/conversation-coordination/clippy-final.jsonl`。

本地独立构建命令：

```powershell
cargo rustc -p dshd --bin dshd --locked --target-dir D:\CodeProject\DSH\target -- -o C:\Users\AIIT\.codex\worktrees\conversation-delivery\DSH\.local\conversation-state\dshd-final.exe
```

该开发制品标识仍为 development，SHA256 `eb7c94444eed470ef6d7951e910b551229dbf9c5ed47a787b6348ab9f3b4b8eb`；不能作为正式运行 SHA，也不能将后续仅格式 / 参数组织变化称为已经重新部署。

## 回放与失败保留

原创合成数据为 `tools/fixtures/conversation-regressions-v1.json`，40 组覆盖六类。`conversation-replay-smoke.py` 实际运行临时 Rust 进程、认证管理接口、签名合成 QQ 入站、loopback 模型、持久队列 / 回执、重启和 SQLite 证据。固定原始锚点与实际 host 锚点、时间平移分别记录，不修改主机时钟；模拟历史跨日不称真实等待跨午夜。

首轮完整回放 40 个 host case、145 条 host 断言通过，原报告 `replay-40-final-first.json` 保留为诊断。加强活动精确 ID / revision / 状态和单槽确定性后，冻结脚本最终全轮 **40 个 host case、147 条 host 断言通过**，90 次 loopback 请求，退出 0；报告 `replay-40-final.json`，runner SHA256 `a864df0d0fdf68d645e2333d7b33b366e6ec23880b5ea4edf14a336186d259ac`。两轮不合并计数。

**40 组表达全部未评估**，17 条原始 criterion 没有对应机器 host 检查，另有 12 条显式未评估 host 检查。case 通过只意味着其记录的 host 断言通过，不代表全部语义通过。QQ API 调用 0、持久 pacing fence 保留，微信账户未启用。原锚点 15:00 平移至实际 23:39:05，偏移约 31,145.7 秒，不改主机时钟。该开发制品早于最后发送领取强化；领取回归由最终 549 项覆盖，固定 Linux 制品仍须重跑全 40 场景。

前期失败均保留：有新 provenance fence 后不能再直接伪造 queued Run；过长合成回复不能绕原每项字数限制；来源不含“明天”时不能自造 time_text；host-only 排除字段不能从 JSON 反序列化注入。修正夹具后保留原功能约束。真实授权账本故障还发现程序会把具体不可核验错误覆盖为普通版本变化，已修复为无生成结果时保留原安全错误，仍然零 provider / Outbox。

E06 / F06 初版仅查字符串可能将 unscheduled 当 scheduled，已加强为精确约定、真实活动 / occurrence 及无执行 / 投递证据检查。管理员审核明确 `source_assent_verified=false`，未冒充来源同意或自动联系许可。各轮报告为 `.local/conversation-state/replay-*.json`，最终结果不能从 loopback 回复推断真实模型表达。

`conversation-state-live.py --self-test` 离线 9 项通过，0 外部模型请求、0 生产数据读取。正式脚本为 12 个代表场景设置最多 24 次新真实请求、单一转发并发、1024 输出和 96,000 字节上限；真实转发须核对实际 Run lease、冻结 Prompt 与预算。输出与 host 状态保留供审读，HTTP 200 不等于程序完成或表达合格；尚未执行前维持 review_pending，也不冒充原 40 场景全部真实模型 / 人工对照。

## 数据恢复与当前发布边界

2026-10-26 23:10:07，使用部署前 `pre-468fcee400601a590e7bc4c8c573d16a7c20bca9-20261026T144449627434068.sqlite` 在私有临时目录恢复独立库：源备份 campanus / 0600，目标目录 0700、独立库 0600；quick_check 均 ok，外键违规 0，7 表数量为 4 / 92 / 0 / 0 / 0 / 0 / 0，配置 revision 7、性格与身份摘要精确一致，实际 468 reader 检查退出 0。报告 `shared/logs/recovery-468fcee.json`。未切服务、生产指针或配置，**仅为独立 SQLite 恢复检查，不是生产灾难恢复验收**。

本切片的提交、推送、正式 Linux 构建、固定运行 SHA、远端回归、真实模型与新浏览器验收在完成后追加。尚未执行的项目不记通过；新状态或策略已有记录后，不能盲降至忽略其语义的 468 版本。睡眠、真实主动联系与可执行动作仍未由本切片交付。

## 2026-10-27 正式发布、失败与安全回退

本节追加此前待执行项目的实际结果，不改写上文当时的开发检查。正式发布提交为 `1a754b29c21171d5d718b983247712405450cdae`，目标仍为 `campanus@192.168.189.191` 的 Ubuntu / aarch64 主机。固定提交已推送后，由发布脚本在 Linux 执行 `cargo test --workspace --locked -j 1` 和 `cargo build --release --locked -p dshd -j 1`；daemon **550 项通过，389.10 秒**，kernel **4 项通过**，release 构建 **9 分 56 秒**，部署主命令退出 0。原测试、构建日志分别为 `shared/logs/test-1a754b29c21171d5d718b983247712405450cdae.log`、`shared/logs/build-1a754b29c21171d5d718b983247712405450cdae.log`。启动窗口内短暂 curl 连接拒绝保留在原部署输出，不能据此覆盖最终健康检查，也不删除该瞬态诊断。

正式制品及发布后只读核对：

- Linux binary SHA256：`49be9658d661f5e5432141779f1af4d45513fc2e96edfa9dff2dcb25ba0c4534`。
- 正式前端包 `admin-1a754b29c21171d5d718b983247712405450cdae.tar.gz` SHA256：`ac65b7b0f42a41ccd02e128110aedfb1718af8a0f64ec7c5b7b327d79d1d9cfb`，远端保留于 `/home/campanus/.cache/dsh/`。其 release 元数据与源码 HEAD 均为完整 1a SHA。
- `current`、source HEAD、前端 release、`/health/live`、`/health/ready` 和实际进程 exe 一致，源码干净，systemd active / running，服务开始于北京时间 **00:49:31**，当时 PID 1044574。记录 `shared/logs/pre-acceptance-readonly-1a754b2.json`。
- 正式库配置 revision **7**，conversations **4**、messages **92**、memory_records **0**、diary_events **0**、diary_entries **1**、relationship_people **0**、schedule_activities **0**。日记来源策略、日程自主授权、联系许可均为 **0**，Rest 授权表尚未存在；验收后这些数量、授权和配置 revision 均未变化，记录 `shared/logs/post-acceptance-readonly-1a754b2.json`。

日记条目 1 与更早的部署前数量 0 有差异，因此另行核验了元数据：该 private day 条目 revision 1、updated_at 为北京时间 **00:05:05**，早于 1a 服务启动，属于此前 468 运行时段已有行；source_count 0、observation_complete 0、generator `evidence_excerpt_v1`、status `incomplete`，diary_events 0、entry_sources 0、model_jobs 空。没有读取或记录其正文，也不把这条无来源记录称为 1a 模型采集。证据 `.local/conversation-state/diary-midnight-metadata.json`。

### 固定 1a 制品的远端隔离回归

每套脚本均使用该 release 的 source / binary、独立临时数据与端口，资源并行不超过两套。QQ 入站、账号、签名和回执全为合成，启用账号前先写持久 pacing fence；无真实 QQ / 微信外发。命令、时间和退出状态记录于 `shared/logs/conversation-acceptance-1a754b2-command-results.json`，汇总 `shared/logs/conversation-acceptance-1a754b2-assessed.json`。

| 套件 | 实际结果 | 报告（均在 `shared/logs/`） |
| --- | --- | --- |
| conversation-isolation | 21 / 21，退出 0 | `conversation-isolation-1a754b2-aarch64.json` |
| conversation-coordination | 22 / 22，退出 0 | `conversation-coordination-1a754b2-aarch64.json` |
| diary | 9 / 9，退出 0 | `diary-1a754b2-aarch64.json` |
| summary | 24 / 24，退出 0 | `summary-1a754b2-aarch64.json` |
| assistant-logs | 19 / 19，退出 0；旧报告无 status 字段，按退出码和检查清单核对 | `assistant-logs-1a754b2-aarch64.json` |
| memory | **前 10 项通过后失败，退出 1；余下 28 项未执行** | `memory-1a754b2-aarch64.json` |
| conversation-commitments | 11 / 11，退出 0 | `conversation-commitments-1a754b2-aarch64.json` |
| conversation-replay | 40 / 40 host case、147 / 147 host 断言，退出 0 | `conversation-replay-1a754b2-aarch64.json` |

因此，六套既有回归中只有五套通过。memory 失败原因为 `memory envelope was not observed as user-role JSON data`：既有 provenance 断言要求 user-role 数据仍使用 JSON 数组，新 worker 单资料块发出了对象。原 `memory-smoke.py` 及断言未改，失败 stdout 保存在 `memory-1a754b2-stdout.log`，不能把前 10 项通过写成 38 项通过。

远端 40 场景使用了 **91 次 scripted loopback 请求**；这是独立一轮，不与上文本地 90 次合并。**40 项表达全部未评估**，仍有 12 项显式未评估 host 细项、17 条 criterion 没有机器 host 检查。QQ API 调用 0、pacing fence 保留、微信未启用；这些结果只证明记录的宿主断言，未证明 40 场景语义全部合格。

### 真实模型首轮：22 次调用后保留失败

从固定 1a release 执行：

```bash
python3 /home/campanus/apps/dsh/releases/1a754b29c21171d5d718b983247712405450cdae/source/tools/conversation-state-live.py \
  --binary /home/campanus/apps/dsh/releases/1a754b29c21171d5d718b983247712405450cdae/bin/dshd \
  --web-dir /home/campanus/apps/dsh/releases/1a754b29c21171d5d718b983247712405450cdae/web \
  --source-data-dir /home/campanus/apps/dsh/shared/data \
  --expected-commit 1a754b29c21171d5d718b983247712405450cdae \
  --max-real-requests 24 \
  --report /home/campanus/apps/dsh/shared/logs/conversation-state-1a754b2-real.json
```

本轮真实只读模型配置与凭据引用，密钥只在转发器内存中使用，不落临时库或报告；未读取生产性格、记忆、对话或真实用户正文，生产库未写入。临时实例使用合成性格与来源，经 Rust 实际 worker、冻结 Prompt 和预算后转发 HTTPS；每次实际 body / messages 哈希、Run 与 lease 证据均保留。每次输出上限 1024、输入上限 96,000 字节、硬超时 60 秒、实际并发峰值 1，不自动重试。

预算上限 24，实际 **reserved / forwarded / HTTP 200 均为 22**，转发失败 0；**11 / 12 host 场景通过，GROUP 失败，主命令退出 1**。群场景的首条输出是合理的两句澄清：

```json
[{"type":"text","text":"你说的“这题”是指哪一题？我这边没看到具体题目内容。","delay_ms":0}]
```

它把问句与陈述放在一个 text 对象内，1a 解析器报“每个回复对象只允许一句话”，实际 Run.failed、无 Outbox / reply parts，物理模型 lease 已结束。未执行该群场景余下两次计划调用，没有补满 24 或再次真实运行来覆盖失败。HTTP 200 不是 Run 完成，更不是表达合格。

全部 **12 场景表达仍为 review_pending**，脚本没有模型自评分或语义通过结论。Agent 对合成样本的审读还指出：E05 将“未关联活动”扩大成“没有提醒或日历功能”；E06 将时间未结构化表述成“明天具体是哪一天未确定”。这些是待修的表达依据边界，不替代真人验收，也不将管理员审核当作来源同意、接收人许可或已调度证据。原报告、命令 / stdout 分别为 `shared/logs/conversation-state-1a754b2-real.json`、`conversation-state-1a754b2-real-command.json`、`conversation-state-1a754b2-real-stdout.log`；本地合成输出审读副本为 `.local/conversation-state/acceptance-1a754b2/review-samples.json`。真实 QQ / 微信发送及客户端到达均未验收，调用数为 0，pacing fence 保留。

### 00:59 安全回退，未恢复数据库

正式功能验收失败后，先只读核对新增 17 张状态 / 约定 / 预算表，**全部记录数为 0**，包括 source tombstones、epoch、幂等、独立策略版本及快照。使用当前 1a 的 `deploy/check-compatibility.py` 对正式数据与目标 468 source 实际检查，退出 0；没有删除新表或数据来使检查通过。预检记录 `shared/logs/rollback-preflight-1a754b2-to-468.json`。

北京时间 **2026-10-27 00:59:27.599 至 00:59:28.816**，执行当前 1a 的回退脚本：

```bash
bash /home/campanus/apps/dsh/releases/1a754b29c21171d5d718b983247712405450cdae/source/deploy/rollback.sh \
  468fcee400601a590e7bc4c8c573d16a7c20bca9
```

脚本在停止服务后再次正常核验兼容性，再切换并启动，退出 0；没有绕过 gate，也没有恢复、删除或重建生产数据库。回退后 current、干净 source HEAD、前端、live / ready、实际 process exe 全部为 `468fcee400601a590e7bc4c8c573d16a7c20bca9`，systemd active / running，PID 1050602，binary SHA256 `041c5b4fd9a8d39d6abbe0c21e3bfbf8ae6846f33cd7305cbecec44e915dabc5`。命令、时间、退出与复验见 `shared/logs/rollback-1a754b2-to-468.json`、`rollback-1a754b2-to-468-stdout.log`。这次允许代码回退的依据是新语义数据为空，不意味着以后已有状态 / 策略记录时也可盲降。

## 回退后的本地修复与尚待发布项目

以下均为正式 1a 失败后的本地修改或开发静态资源检查，**尚无新固定 SHA 提交、部署或真实模型复测**，不能覆盖上述原失败：

- 新 text / voice 对象在未携带 asset_id / prompt 时，可把完整多句规范化为逐句计划；分句前仍限原始对象 1000 字符，分句后整轮仍最多 4 项，超出则整体失败，不截掉后半段；首项 0、后项仍受配置间隔约束。已有媒体 / 制品和绘画提案不能借分句绕过验证，recipient / URL 等未授权字段仍拒绝。
- memory 每资料块恢复原有 **单元素 JSON 数组 envelope**，保留 user-role、版本与来源证明；没有修改原 `tools/memory-smoke.py` 的 38 项断言来适配回归。
- 约定上下文增加只由宿主生成的能力说明：实例已有日程管理核心、本对话不会自动建活动、真实主动提醒外发仍不可用；未关联活动不等于没有日历功能。unresolved 只表示时间尚未结构化，不否定原话“明天”及已有可信锚点；未独立验证 source assent 不代表用户否认意向。管理员 review 不代替来源同意或发送许可。这些说明未扩大能力、来源范围、确认或调度权限。
- 管理弹窗异步确认后移除焦点按钮，曾使 body 获得焦点、Esc 无法到达弹窗处理器；原浏览器失败保留 `.local/conversation-state/browser-1a-first.json`。修复后前端 **28 文件 / 187 项**、定向约定 11 项、类型检查和构建通过；开发静态包 SHA256 `818ba2008192e8df41870232202a183832b26dc0e85b327332bc89302f6354ce` 搭配固定 1a 后端，在独立临时数据执行 **1 项浏览器检查通过**，含 1440 / 390 / 320 视口，报告 `.local/conversation-state/browser-focus-dev/validation.json`。这是开发静态资源验证，**不是修复后正式固定 SHA 的前端验收**，也未访问生产数据或真实渠道。

本地 `dshd-text-fix.exe` 仍标识 development，SHA256 `6ec498ce753fe1ec1cdf36f2cbf5a8a23b3ad0fe1f578e33156c374841bc589b`。新 `tools/conversation-reply-normalization-smoke.py` 运行实际 Rust 进程、签名合成 GROUP 与 scripted loopback：A 先通过真实入站建立题目，再发送原问题“这题详细点。”，原失败 text 规范化为两个完整 text，配置间隔为 `[0, 2200]`；五完整句整体失败、无 plan / Outbox，recipient / url 字段仍拒绝；A 的实际 detailed 偏好生效，同群 B 不继承，两个可信 speaker 不同。实际 HTTP 与冻结 Prompt 一致，**5 / 5 场景、35 / 35 主断言通过**，另有 4 项 A 题目准备检查，退出 0；QQ API 0、sending 0、fence 保留、微信未启用。报告 `.local/conversation-state/local-reply-normalization-text-fix-r3.json`。

同一脚本用旧 development 制品 `dshd-final.exe`（SHA256 `eb7c94444eed470ef6d7951e910b551229dbf9c5ed47a787b6348ab9f3b4b8eb`）单场景复现了原 parser 错误，退出 1，报告 `local-reply-normalization-old-1a.json`；它是本地旧制品对照，不冒称重新运行正式 1a Linux binary。新脚本开发 r1 误用了不存在的 lease 表，r2 没有先准备当前题目便要求 A 的 current-topic preference 激活；修正夹具后才获得 r3 结果，原 r1 / r2 失败报告均保留，未改业务条件或降低四项 / 授权断言。

使用同一个新 development binary，原 memory **38 / 38** 与 coordination **22 / 22** 均退出 0，报告 `.local/conversation-state/local-memory-text-fix.json`、`local-coordination-text-fix.json`。全部均为独立临时实例，最多两个并行，无生产数据读取、真实模型或平台请求；各轮时间、退出码、报告和 binary hash 汇总 `.local/conversation-state/local-reply-text-fix-command-results.json`。脚本 AST、`--help`、空白及 Git diff 检查通过。

本地修复源码随后统一运行 `cargo test --workspace --lib --locked --quiet --target-dir D:\CodeProject\DSH\target`，daemon **555 项通过，21.43 秒，退出 0**，包括新增能力说明与分句回归；envelope 兼容性另由上述原 38 项进程验收覆盖。原始日志 `.local/conversation-state/local-text-fix-tests.log`。该结果不代替 Linux 重建、正式部署、旧套件与 40 场景重跑、新浏览器及新一轮独立真实模型预算；这些仍待新固定 SHA 后追加，当前不预记通过。原 22 次真实调用与任何后续批次分别记账，不自动重试或覆盖原报告。

修复后的严格 Clippy 仍退出 **101**，48 条诊断、21 个不同的文件 / code / message 组合与既有基线一致，新增 0；未关闭 lint 或绕过检查。比较记录 `.local/conversation-state/clippy-text-fix-comparison.json`，原输出 `clippy-text-fix.jsonl` / `clippy-text-fix-stderr.log`。

## 2026-10-27 固定 648 修复版发布与验收

修复提交 `6486c48deb47b16b2bb25b7c09d47eff9bbc6569` 已提交并推送，再由同一 Ubuntu / aarch64 主机单并发构建。Linux 原始日志精确为 daemon **556 项通过，392.60 秒**，kernel **4 项通过**；release 构建 **9 分 56 秒**。分别保留 `shared/logs/test-6486c48deb47b16b2bb25b7c09d47eff9bbc6569.log`、`build-6486c48deb47b16b2bb25b7c09d47eff9bbc6569.log`，不是把本地 555 项或上轮 Linux 550 项改写为新轮结果。

首个部署调用因 PowerShell CRLF 使 archive hash 参数未通过完整 SHA256 校验，报 `A complete archive SHA256 is required`，在构建和服务切换前非零退出，原记录 `.local/conversation-state/deployment-6486c48.log` 保留。修正调用参数后的 r2 使用同一已推送 SHA 与原包重新执行，archive 校验通过，部署退出 0；启动瞬态 curl 拒绝与最终 ready 均保留于 `deployment-6486c48-r2.log`，没有删去第一次错误或称其已部署成功。

正式制品与运行时对应关系已核验：

- Linux binary SHA256：`9c76004450cb855919ed6d013ed0093906d86e12970194fb248942fc2dcb0ab0`。
- 正式 archive `admin-6486c48deb47b16b2bb25b7c09d47eff9bbc6569.tar.gz` SHA256：`ef8f969d35fad0a249355159c55894f6e8545fc6b2f23c40f0d1e942936d4f04`。
- source HEAD、干净工作区、前端 release、current、live / ready 与实际进程 exe 全部匹配完整 648 SHA，PID 1080040，systemd active / running。发布核验 `shared/logs/post-deploy-6486c48.json`，验收前再次核验 `conversation-acceptance-6486c48-preflight.json`；10 个脚本 `--help` 均退出 0。预检首次误将另一个静态服务端口 8765 当成生产 health，得到 404；核对实际 ExecStart 的 8419 后完成检查，原端口假设错误记录于 preflight，不当作程序故障或隐去。

### 九套隔离回归全部通过，表达边界保留

全部使用固定 648 release 的 source 脚本、bin 和 web，每套独立临时库 / 端口；最多两套 mock 并行，真实模型批次开始前所有本轮 mock 均已结束。报告中的 source commit 和 binary hash 均逐套核对一致。命令、开始 / 结束时间、退出、stdout 路径汇总 `shared/logs/conversation-acceptance-6486c48-command-results.json`，结果核对 `conversation-acceptance-6486c48-assessed.json`。

| 套件 | 本轮结果 | 报告（均在 `shared/logs/`） |
| --- | --- | --- |
| conversation-isolation | 21 / 21，退出 0 | `conversation-isolation-6486c48-aarch64.json` |
| conversation-coordination | 22 / 22，退出 0 | `conversation-coordination-6486c48-aarch64.json` |
| diary | 9 / 9，退出 0 | `diary-6486c48-aarch64.json` |
| summary | 24 / 24，退出 0 | `summary-6486c48-aarch64.json` |
| assistant-logs | 19 / 19，退出 0；按旧报告清单和退出码核验 | `assistant-logs-6486c48-aarch64.json` |
| memory | 原断言 38 / 38，退出 0；未改旧 envelope 验收条件 | `memory-6486c48-aarch64.json` |
| conversation-commitments | 11 / 11，退出 0 | `conversation-commitments-6486c48-aarch64.json` |
| conversation-replay | 40 / 40 host case、147 / 147 host 断言，退出 0 | `conversation-replay-6486c48-aarch64.json` |
| conversation-reply-normalization | 5 / 5 场景、35 / 35 主断言，另 4 项题目准备检查，退出 0 | `conversation-reply-normalization-6486c48-aarch64.json` |

40 场景这轮为 **91 次 scripted loopback 请求**；40 项表达仍全部未评估，12 项 host 细项与 17 条无机器检查的 criterion 仍未评估，不因换版而补记通过。normalization 场景通过真实签名合成入站先建立 A 题目，再激活 A 的 detailed 偏好，核验同群 B 不继承；这项 active 偏好机器证据不能偷换为真实模型已经稳定遵守详略范围。QQ API 调用 0、pacing fence 保留、微信未启用，均不证明真实平台发送或客户端到达。

### 真实模型第二轮：23 / 24，S05 失败仍保留

使用与上轮相同 live 脚本入口，但所有 source / bin / web / `--expected-commit` 固定为完整 648 SHA，报告使用新文件 `shared/logs/conversation-state-6486c48-real.json`。本轮北京时间 **02:07:41.348 至 02:08:19.081**，命令退出 **1**；hard cap 24，实际 reserved / forwarded / HTTP 200 均为 **23**，转发失败 0、并发峰值 1、自动重试 0。仍限每次输出 1024、输入 96,000 字节，独立 HTTPS 转发 body 与 Rust 冻结 Prompt / budget 对齐；只读生产模型配置及引用凭据，临时库不存真实 key，未读生产性格、记忆、聊天或用户正文，未写生产库。命令与 stdout 分别为 `conversation-state-6486c48-real-command.json`、`conversation-state-6486c48-real-stdout.log`。

结果是 **11 / 12 host 场景通过**。此前 GROUP 的 3 个 Run 本轮均 completed，原本问句与陈述共存的 text 已可规范化为两条完整句子；此处 completed 只代表宿主生成成功，队列受临时 fence 阻断，不代表 QQ 已发。新的失败是 S05 首轮 `132f79f5-4653-402c-94a1-b3deb1266e51`：真实输出为 **5 个 text 对象，第二个对象还含两句，合计 6 句**，finish_reason 为 stop，并非转发超时。它违反原每轮最多 4 项合同，宿主整体报“模型回复超过逐句消息数量限制，请缩短回答”，Run.failed、reply plan / Outbox / parts 均为 0，物理模型 lease 已回收。对应实际请求为 request_evidence ordinal 15，raw generation SHA256 `a5aed39df3cda5f2d78743eaf980afef399dd6fda086f087bdda5f5aa53347a5`，完整有界合成输出保留在原报告中。

这是**正确拒绝超限输出**，不是把功能场景标为通过的理由；S05 第二题未执行，因此只发生 23 次而非 24 次。没有截断、丢句、增加每轮上限或补发一次请求来使结果变绿。本阶段两轮实际真实模型请求独立为 **22 + 23 = 45 次**，原 1a 失败报告不变、没有 retry；[首批协调阶段](conversation-coordination-validation.md) 的 **4 次**另列，不混入这 45 次，不把两阶段或未用完预算合并成通过统计。

23 条实际输出已分三批做 **Agent 审读，非人工确认**，全部维持 review_pending，没有模型自评分或整组语义通过结论：

- 算术 76、长原文尾标 LX-734、旅行 / 采购双焦点必要澄清、预算 200→300 后继回答、A / B 私有编号与 FTS 故障后依据当前历史回答 FS-381，样本中均与合成来源相符。
- E05 改为“没法自动到点主动提醒”，不再把未建活动扩大为实例无日程 / 日历功能；宿主仍核验没有新约定、日程或许可。E06 明确未排期、提醒未设、此对话不自动建活动和不能主动外发，不再把 unresolved 解释成“明天究竟哪天未定”。“已确认”只能作为管理资料审核状态，不能扩大成来源或接收人同意。
- GROUP 的 B 回答 15 简洁；但 A 换题“下一题，2加2”后仍答“详细说：2加2等于4”并补解释，表现出旧详略语气残留。宿主已无 response_preference，不是跨 speaker 权限泄漏，却仍不能称整个 GROUP 表达通过。live GROUP 未预先建立 A 题目，其 active 偏好覆盖局限仍保留。
- F01 正常短句收尾、无新问题或订阅；F03 没编猫名且限定为本次资料未查到。但“记下了”“我就能记住称呼”等措辞存在持久性歧义，不作为长期记忆已经写入、后台采集或将来必可跨会话回忆的证据。

本轮 29 个脱敏远端 JSON / log 已复制到 `.local/conversation-state/acceptance-6486c48/`，逐文件 SHA256 与远端一致，清单 `evidence-copy-manifest.json`；完整 live 副本、23 条有界 `review-samples.json` 与逐条 `agent-expression-review.json` 均在该目录，后者显式 `human_review_performed=false`、`semantic_pass_claimed=false`。没有修改原 1a / 648 报告或用 Agent 审读替代真人验收。

### 固定 648 正式浏览器与生产保留核对

固定 **648 正式前端与后端**，使用独立临时数据和低负载 loopback，浏览器 **1 / 1 通过，7.05 秒**，9 张截图、1440 / 390 / 320 视口，unexpected / flaky / skipped 均为 0。桌面预算与 320 约定布局经 Agent 视觉检查无横溢；这是 Agent 浏览器操作与视觉审查，不是用户本人已确认。证据 `.local/conversation-state/browser-6486c48/validation.json`、截图及 `ownership.json`。只按原 ownership 核实并停止 fixture PID 1081070、daemon PID 1081077 和本地 tunnel PID 22680，未触及生产服务；无真实模型、QQ 或生产数据访问。这次可称固定 648 前端检查，不能回写为旧 1a 正式前端已通过。

生产库在验收前后以 `mode=ro`、`PRAGMA query_only` 和短只读事务核对。配置 revision **7**，数量仍为 conversations / messages / memory_records / diary_events / diary_entries / relationship_people / schedule_activities = **4 / 92 / 0 / 0 / 1 / 0 / 0**；日记来源、日程自主、联系与触发执行 grant 数量均为 0。已有 diary 条目仍是前述 **468 运行时段 00:05:05 的无来源 incomplete 行**，本轮未读取正文或产生新采集。

配置、性格、稳定身份及五张策略表的 **8 个 SHA256 全部与原 pre-deploy 基准和本轮验收前值一致**。哈希口径为精确 SELECT 的 tuple rowsets，以 `json.dumps(rows, ensure_ascii=False, separators=(',', ':'))` 序列化再 SHA256，不输出配置、性格或身份值，也不重新解析内嵌 JSON。精确 SQL、哈希及比较分别保留 `shared/logs/pre-acceptance-readonly-6486c48.json`、`post-acceptance-readonly-6486c48.json`。新增 17 表仍全空；当前 648 的 compatibility gate 对 468 目标实际退出 0，但**本轮没有执行回退**。

当前维持 648 healthy / active：本轮没有发现数据、来源隔离或投递边界回归，S05 超限拒绝符合原四项安全合同，因此不为这一正确拒绝进行无收益的代码回退。**功能场景 S05 及表达仍未通过完整验收**。下一步仅准备强化最终宿主回复协议的优先位置，协调“详细”偏好与四句上限；必须另行本地检查、提交推送、固定新 SHA 部署后，按独立核算的小预算运行 S05 两轮及必要回归，不能以扩大上限、自动重试或重跑 24 次覆盖本轮失败。此处只记录待办，不预记新修复、新 SHA 或后续测试已完成。


### 648 后的最终回复协议本地检查

普通对话在历史与可选性格资料之后、完整当前输入之前追加一个 required 的宿主 system block。它计入原预算，明确整轮对象和完整句总数均不超过已配置的 1–4 项，详细解题仍须在这些句子内给出完整核心过程；不截断、不扩大发送额度、不新增模型重试。临时偏好只从宿主已核验的 brief / detailed 枚举产生，缺省时不沿用另一题的详细要求；不将原始来源文字提升为 system。Skill 草稿保持原专用 Schema，不注入这份普通回复协议。

实际 loopback worker 检查确认该规则以 system role 紧邻完整当前输入，且预算证据中 required / selected、字节均计入；不是仅检查静态 prompt 字符串。本地库 **556 / 556，22.18 秒，退出 0**；`cargo fmt --all --check` 与 diff 检查通过。严格 Clippy 仍退出 **101**、既有 48 条诊断，新增 0；记录 `.local/conversation-state/local-final-contract-library.log`、`clippy-final-contract-comparison.json`。这只证明协议位置、预算与原宿主回归，不能保证真实模型一定服从；新固定 SHA 和真实定向复测仍待后续记录。


### 定向验收脚本的独立版本

原 live 工具没有场景选择入口；仅把额度改为 6 会运行清单开头的其它场景，不能用于定向证明 S05 / GROUP。后续工具增加闭集 `--cases`：空项、未知或重复 ID、显式选择超过 hard cap 均在读取生产配置或凭据前拒绝。`--cases GROUP,S05 --max-real-requests 6` 按原清单顺序执行两个场景、**仅计划 5 次**，不会补满第六次；默认不选择场景时仍保留原 12 场景 / 24 次及 budget_limited 行为。

新增 `--expected-script-commit` 可核验独立干净 Git validation 工作树的精确 SHA，报告单列测试脚本 SHA、runner / helper hash 和实际程序 expected commit；不改写已发布 source，也不把测试脚本提交冒充程序重新部署。本地 **15 项离线测试、自测 13 项（含原 9 项 gate）通过**，日志 `.local/conversation-state/live-selection-unit.log`、`live-selection-self-test.json`。此改动没有 Rust / 前端运行行为变化，不为它另行重启服务。真实定向调用仍须等 ac9 程序部署 ready 后，从新固定脚本工作树执行并另存报告；本段不预记调用或功能通过。
