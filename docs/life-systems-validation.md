# 日程、日记与人物关系验收

本记录对应 2026-10-26 根据 `architecture.md` 第 9、14、18 节新增的三个系统。历史验收报告不回写。本页以实际执行更新；尚未执行的远端步骤不记为通过。

## 实现范围

- 日程：月 / 周 / 日时间块和手机议程、来源隔离、重复系列 / 单次例外 / 此后修改、幂等和 revision、旧任务权威数据兼容投影、系统日记作业只读投影、授权资料与撤销、自主工作目标、有界规则或明确允许的模型规划、真实模型分段笔记与跨时段游标。
- 日记：稳定身份、显式事件、完成对话的授权采集、每日 00:05 兜底、日 / 周 / 月范围、无互动和未观测标记、事实 / 感受、来源版本、模型出域授权、FTS5 trigram 与短词回退、更正 / 遗忘 / 撤权及派生上下文失效。
- 人物：独立稳定身份、字段候选及冲突审核、本人 / 第三方 / 推断、年龄与生日精度、有向边、会话身份核验、同域可逆合并、跨账号同人组且不转授 ACL、来源和人物遗忘、固定点好感与版本化规则 / 停机补算上限、显式日记来源的模型候选。
- 三个后台与旧自动触发共用一个自动模型槽，和交互 / 既有能力共用总请求准入；取消或撤权不能提前释放实际 HTTP 占用。所有管理 API 沿用认证、Origin 和 CSRF，变更校验版本。日记、日程、人物维护在同一持久调度循环内使用各自短事务。

## 当前边界

视频帧 / 音轨学习、真实主动联系、好友软件观察适配、完整睡眠状态机、开放 Python 动作和完整 M4 / M5 未由本轮交付。缺少能力的活动保留阻断原因，不自动外发、不伪造已读 / 已发送结果。

旧记忆仍在带 canonical 日记同步和失效门禁的兼容过渡期，旧查询尚未全部切换为 canonical 读取；普通模型召回及旧命令额外检查日记来源撤权水位。单日仍受 200 条来源上限；新版周 / 月采用固定日记版本与持久分片归并，不以周期来源数超过 200 永久阻断。来源缺失、单日过量或模型预算不足时保留明确状态。当前没有宣称十万条资料的 p95 性能指标通过。

来源采集、模型出域、自主范围和模型规划均需显式授权，升级不自动替用户授予新权限。人物模型整理仅产候选，不自动确认事实、合并身份或批准联系。私人资料与笔记不会因跨账号身份关联而公开给另一个来源。

## 本地验证

Windows / Rust 1.88.0 / Node 24.19.0。检查过程保留了失败并修复：长中文检索超过字节上限导致 QQ 视频回复在请求前失败；取消阅读时挂起 HTTP 未及时结束；日记周期预算错误曾连带回滚日程维护。相关回归已加入测试。

首轮统一本地检查：`cargo test --workspace --locked` 370 项领域测试与内核 4 项全部通过，`cargo build -p dshd --locked` 和 `cargo fmt --all -- --check` 通过；`npm test --prefix web/admin` 21 个文件共 144 项通过，类型检查与生产静态构建通过；部署 Python 回归 44 项通过。

隔离真实进程 / 认证 API / mock provider 验收：日记 9 项、人物 15 项、日程 14 项，共 38 项通过。Playwright 在 1440 像素桌面与 390 像素手机完成日记录入 / 来源 / 更正、人物候选审核、私有资料导入、日程预演 / 创建 / 月周议程切换及待批系列明确批准；无页面脚本错误与手机横向溢出。截图保存在本地 `.local/life-e2e-release/evidence/`。

三后台与兼容读新增真实挂起 HTTP 回归通过：每类任务共用唯一自动槽、交互优先、未首次 poll 丢弃和在途取消均回收物理 lease；旧记忆在撤权 / 重启 / 重新授权后不会通过模型上下文或 `/memory list/show` 复活。扩大并行测试还暴露图片接口在校验控制文本前抢占解码槽的问题，现先校验纯文本及明显非法前缀，完整回归通过。开发制品标识 `development` 不作为正式部署 SHA。

## 发布与远端

首轮 `94ab0ae257904e09902e57c8387f4af689191e7d` 已提交推送并成功部署。远端 Rust 371 + 内核 4 项通过，三模块 mock 9 / 15 / 14 项通过；日记、人物候选真实模型通过。日程真实阅读与重启通过，模型排程未在 120 秒内创建活动，原失败报告保留在 `shared/logs/schedule-94ab0ae-real-1.json`，不计为通过。旧版日志不足以判断该次失败的精确原因。

第二轮处理周期高量整理、普通对话近期日记回退、来源分页，并为规划提供宿主校验的候选时段和不保存模型正文的错误诊断。各次 SHA 与复验结果见下列发布记录；不能用首轮通过替代新版验收。

已重新核验目标为 Raspberry Pi 5 Model B Rev 1.1、MemTotal 8,318,562,304 字节、Ubuntu 24.04.4 LTS / aarch64、Rust 1.88.0。原服务 `dsh.service` 正常，上一可用制品为 `a566f608328d3d6acbdbe08327057eac301e86f8`；原配置 revision 6。正式部署沿用 rsproxy 缓存与单并发，切换前由脚本执行 SQLite 一致性备份；回退须通过新版 reader 门禁，代码切换不代替数据恢复。

## 第二轮发布

代码与前端固定到 `774d89b0e87185fcadb76e537ab08ecd8e1afedd`，已推送并核对 `origin/main`；前端归档 SHA256 为 `429cae52df1b4f1455402b1e4992f2030e2541af1cd7502c69918c1cb3b5640f`。上一运行版本是首轮 `94ab0ae257904e09902e57c8387f4af689191e7d`。部署于 2026-10-26 18:58:04（UTC+08），脚本退出 0，Linux 测试 382 + 内核 4 项通过，正式优化构建 9 分 17 秒。启动时第一次健康探测暂未监听，重试后就绪成功，不把该次探测记为独立健康通过。

正式 aarch64 制品 SHA256：`4a8ab3fb226a5ff1dde8e7194f482ebc93d0c837a714eb882dbc3c70f4a464a7`。测试与构建日志分别为 `shared/logs/test-774d89b0e87185fcadb76e537ab08ecd8e1afedd.log`、`shared/logs/build-774d89b0e87185fcadb76e537ab08ecd8e1afedd.log`。

本地第二轮：`cargo test --workspace --locked --offline` 381 项与内核 4 项全部通过；格式检查、开发制品构建、前端 144 项、类型检查、固定 SHA 生产构建及部署 Python 45 项通过。三模块独立进程 mock 9 / 15 / 14 项通过。新增月记层级 mock 10 项通过：31 个完整日父版本、310 条来源、113,618 字节父正文，实际四次请求（三个持久片段和一次归并），最大完整 provider JSON 为 45,760 字节，分页与遗忘级联 / 重启通过。开发制品标识为 `development-schedule-slots-v2`，不冒充正式 SHA。

本地新版 Playwright 在隔离开发进程通过 1 个完整浏览器流程（20.4 秒）：除已有三系统桌面 / 手机流程，实际建立 102 个来源，详情首次显示 100 条，点击加载后显示完整 102 条且按钮消失。截图 `.local/life-v2-browser/evidence/` 已人工查看周视图与手机议程；没有页面脚本错误和横向溢出。此项与远端固定 SHA 页面复验分开记录。

当前源数据、性格与真实凭证不作为验收输入。三个 smoke 的 `--live-config-from shared/data` 只读取已授权的当前 HTTPS 模型配置，将密钥存入临时实例私有目录，并关闭渠道；只发送合成来源。结果不含密钥、模型正文或人物资料，真实渠道标志保持 false。

本次实际部署及验证命令（路径均为树莓派上路径）：

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy.sh \
  774d89b0e87185fcadb76e537ab08ecd8e1afedd \
  /home/campanus/.cache/dsh/admin-774d89b0e87185fcadb76e537ab08ecd8e1afedd.tar.gz \
  429cae52df1b4f1455402b1e4992f2030e2541af1cd7502c69918c1cb3b5640f
# 部署成功后运行，不以仅 SSH 连通代替：
cd /home/campanus/apps/dsh
python3 current/source/tools/diary-smoke.py --binary current/bin/dshd --web-dir current/web --report shared/logs/diary-774d89b-mock.json
python3 current/source/tools/relationships-smoke.py --binary current/bin/dshd --web-dir current/web --report shared/logs/relationships-774d89b-mock.json
python3 current/source/tools/schedule-smoke.py --binary current/bin/dshd --web-dir current/web --report shared/logs/schedule-774d89b-mock.json
python3 current/source/tools/diary-hierarchy-smoke.py --binary current/bin/dshd --web-dir current/web --report shared/logs/diary-hierarchy-774d89b-mock.json
# 真实模型独立结果使用 *-real.json，另加 --live-config-from shared/data。
```

管理入口沿用本机 SSH 隧道的 `http://127.0.0.1:18419/admin/`，登录后使用「日程」「日记与资料」「人际关系」。生产仅由 Rust 提供同源静态页面，未启动前端开发服务器。新授权须在正式页面显式保存，升级不自动开启。

## 正式制品功能与安全复验

| 检查 | 结果 / 日志（部署根目录下） |
| --- | --- |
| 日记 mock / 真实外部模型 | 各 9 项通过；`shared/logs/diary-774d89b-{mock,real}.json` |
| 人物 mock / 真实外部模型 | 各 15 项通过；`shared/logs/relationships-774d89b-{mock,real}.json` |
| 日程 mock / 真实外部模型 | 各 14 项通过；`shared/logs/schedule-774d89b-{mock,real}.json` |
| 月记层级首次远端 mock | 9 项后重启断言失败，保留 `shared/logs/diary-hierarchy-774d89b-mock.json`，不覆盖为通过 |
| 月记层级加强脚本 mock / 真实模型 | mock 12 项通过；真实模型在第三项后归并失败，分别保留 `shared/logs/diary-hierarchy-774d89b-rebuild-{mock,real}.json`，真实模型不计通过 |
| Playwright 固定 SHA | 8.4 秒，1 个完整流程通过，含 102 来源分页、人物审核、资料与日程、明确批准、桌面 / 手机；无脚本错误或横向溢出 |
| 生产与备份审计 | 23 项通过；`shared/logs/life-774d89b-audit.json` |

真实日程验收的阅读覆盖确实推进 5,000 字符，跨两个片段并在重启后保持；另建未读资料的工作目标由真实模型在 8 个宿主候选时段中选择并持久化活动，attempt 为 `completed`，错误码为空，输出 201 字节，保守预留 3,895 token。规划通过不代表该未来活动已经读完或已向任何人发送消息。真实日记成功生成分栏内容，人物模型只提交待审候选；三项均使用临时合成账户，生产无来源授权或自主授权变更。

远端浏览器 fixture 的健康 SHA 与二进制摘要均和正式制品一致；仅其数据目录位于 `shared/validation/life-774d89b-browser/`，不访问生产资料。证据图位于本地 `.local/life-remote-774/evidence/`，已人工查看。测试后终止该精确 fixture 进程并回收临时模型与 daemon，不保留历史验收常驻实例。

19:01（UTC+08）只读审计确认：`current/source` HEAD、静态 manifest、运行进程、`/health/live`、`/health/ready` 均为正式 SHA；systemd active/running。配置 revision 6，已保存性格摘要仍为 `0bd6eb0de83707be8bfcf11bfe281268ce1a95cb7968fd8084239d416e586751`；旧 `memory_records/runs/messages/conversations` 数量仍为 `0/35/68/4`，新来源策略、自主授权、人物及日记数据仍为空。未用升级覆盖性格或给任何来源增权。

切换前一致性备份：`shared/backups/pre-774d89b0e87185fcadb76e537ab08ecd8e1afedd-20261026T105802348468244.sqlite`，摘要 `7a86c9a4909f20c2d8241bb0717417cbc556973a7b6d0bf84f4558dae5a058b8`；备份和私有恢复副本的完整性、计数、配置及性格摘要均一致。没有替换生产数据库或执行服务回退。生产已迁移 diary reader2，当前 gate 退出 0；针对旧 reader1 的检查按预期退出 2。因此日常回退不能盲切 `94ab0ae`，需使用兼容 reader2 的制品，或在另行核对新数据与 tombstone 后实施独立恢复。

## 分层验收发现与后续修复

第一次层级 mock 的失败是测试假设错误：Linux 的 NTP 状态使服务重启后立即维护，日记可从剩余有效来源安全重建；原脚本却要求永久返回 404。验收脚本在 `06b861bea111a79550f4b3daeb984cfe25836e0f` 单独修正，运行源码无变化，未因此重启应用。远端固定该测试源码后，mock 12 项通过，强制核对被遗忘事件的 tombstone / 版本正文、旧日 / 周 / 月版本及 fragment 均清空；安全重建的新月记 revision 必须提高、来源精确为其余 309 条，不能包含遗忘事件。旧失败报告保留。

加强脚本的真实模型运行发现独立运行问题：首个 fragment 已完成，下一批连续 `invalid_model_output`，最终失败退出 1。使用同样 45,749 字节合成输入、原 system 和 2,400 输出 token 的三次独立结构诊断中，两次 stop 且 JSON 完整，一次明确 `finish_reason=length`、4,502 字节输出、无代码围栏且 JSON 未闭合，证实存在输出截断概率。诊断只保留结构和哈希，不记录输出正文或密钥。后续版本收紧单批摘要长度，并拒绝截断结果，不能把截断文本补成有效 JSON。

## 输出预算修复发布

修复提交 `df2539f56cfcbca7b17d29d343b3f104148c3d2a` 已推送并核对远端。该版本保留共同基线 `2fbf100ba40f0593aa8f4baa9a108d119ef11df6` 已提交的新版关系网及兼容管理入口移除，专门页面验收见 [独立记录](relationship-network-validation.md)。等待该部署完成后才取得部署锁，记录其就绪状态，再开始本版本完整流程，未并行启动第二个构建或抢先切换。

修复仅收紧日记单批输出并增加安全诊断；共享请求抽取保留原 DNS 固定解析、无代理、禁重定向、超时、1 MiB 响应和 64 KiB 文本上限，其他原调用仍只读取 content。模型出域、来源 ACL、日记 reader2 和取消时实际 HTTP lease 顺序均不变。原三次与新一次合成结构诊断见 [安全证据](validation/diary-fragment-output-structure.json)；新提示同样 2,400 token 的单次响应为 stop / 434 字节完整 JSON，不能以这次单片诊断代替整个月记验收。

本地最终代码：Rust 386 + 内核 4 项通过，格式检查通过；干净工作树固定 SHA 构建和 22 文件 / 155 项前端测试通过。前端归档 SHA256 为 `1724b1c532673c64280d4e1dc6ae054926896090049c7f74289bdf3f24242f0e`。只打包提交内容，未混入原工作区待提交文档。远端最终结果见下节。

## 最终固定 SHA 结果

`df2539f56cfcbca7b17d29d343b3f104148c3d2a` 于 2026-10-26 19:46:01（UTC+08）部署就绪，部署命令退出 0。Linux Rust 387 + 内核 4 项全部通过（235.44 秒），优化构建 9 分 21 秒。aarch64 制品 SHA256 为 `8e99c1afec9336bbeffdf34920c24cccec9f1bdd2e83d7bb5dd281dca69a612e`。切换时第一次监听探测暂拒绝，随后重试成功；没有因此宣称首次探测通过，也没有执行应用回滚。

| 最终验收 | 结果 |
| --- | --- |
| 三模块 mock | 日记 9、人物 15、日程 14，全部通过 |
| 分层月记 mock | 12 项通过；4 请求 / 3 fragment，最大完整请求 45,916 字节 |
| 三模块真实外部模型 | 日记 9、人物 15、日程 14，全部通过 |
| 分层月记真实外部模型 | 11 项通过；4 请求 / 3 fragment，最大完整请求 45,905 字节；31 日父版本覆盖 310 来源 |
| 遗忘与重启 | 模型月记遗忘后旧正文 / 片段清除，tombstone 保留；实际观察安全重建，精确保留其余 309 来源 |
| 生产 / 备份 / 恢复副本 | 25 项通过，当前及上一 reader2 允许，reader1 按预期拒绝 |
| 干净工作树浏览器 | 新关系网 3.5 秒、三系统流程 10.0 秒，均退出 0；含 102 来源分页、认证 / CSRF、候选审核、日程批准、桌面与手机 |

八份独立进程报告、生产审计与原失败日志的 SHA256、范围、真实模型标志及检查名称已汇总为 [最终安全证据](validation/life-systems-df2539f.json)。mock 合计 50 项，真实模型合计 49 项，二者分别统计；不计为 QQ / 微信真实平台接入或主动发送验收。

浏览器第一次在已含 12 条审核关系的合成实例运行综合流程时，旧脚本在全页查找「已审核」产生多匹配断言失败；不是接口或审核失败。失败文本保留于 `.local/life-remote-df2539f/life-flow-selector-failure.md`。测试修复提交 `a1d4e3f4628d7eb42e2c9d04a2dce462eb4eb9f8` 只把断言限定到目标人物字段，没有修改程序或重启生产服务。为排除原工作区并行编辑影响，最后在该提交的干净工作树、全新隔离实例重新执行两个流程并通过，健康及前端 manifest 均核对正式 `df2539f`；证据图 `.local/life-remote-df2539f-r3/evidence/` 已人工查看。

最终日志位于 `shared/logs/{diary,relationships,schedule,diary-hierarchy}-df2539f-{mock,real}.json`，审计为 `shared/logs/life-df2539f-audit.json`，构建 / 测试日志分别是 `build-df2539f56cfcbca7b17d29d343b3f104148c3d2a.log` 与 `test-df2539f56cfcbca7b17d29d343b3f104148c3d2a.log`。实际部署命令：

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-df2539f56cfcbca7b17d29d343b3f104148c3d2a.sh \
  df2539f56cfcbca7b17d29d343b3f104148c3d2a \
  /home/campanus/.cache/dsh/admin-df2539f56cfcbca7b17d29d343b3f104148c3d2a.tar.gz \
  1724b1c532673c64280d4e1dc6ae054926896090049c7f74289bdf3f24242f0e
cd /home/campanus/apps/dsh
python3 -S current/source/tools/diary-hierarchy-smoke.py --binary current/bin/dshd --web-dir current/web --live-config-from shared/data --report shared/logs/diary-hierarchy-df2539f-real.json
# 其余三个系统相同参数各自独立执行；mock 不带 --live-config-from。
```

最终审计确认配置 6、性格摘要和旧 `0/35/68/4` 数据计数不变，新来源 / 模型出域 / 自主授权仍未授予。切换前备份 `shared/backups/pre-df2539f56cfcbca7b17d29d343b3f104148c3d2a-20261026T114600239375481.sqlite`，SHA256 为 `b2f90cbd45d8074f231a03d47573df1ca3fe4e3cb85d6f22a0b66364e872542e`；备份及私有恢复副本通过完整性、计数及性格 / 配置摘要核对。上一健康版本为 `2fbf100ba40f0593aa8f4baa9a108d119ef11df6`，与当前同为 reader2；旧 `94ab0ae` reader1 仍禁止直接降级。未恢复或重建生产用户数据库。

所有本轮临时 fixture / daemon 与测试 SSH 隧道在验证后回收，保留生产管理隧道。后续仅提交验收与契约文档，不为文档变更重启应用。原工作区后续并行页面修改保持原样，未打入本轮正式包。

干净发布 / 浏览器工作树位于 `C:\Users\AIIT\.codex\worktrees\life-release-df2539f\DSH`，最终保持已推送的测试提交且无未提交文件。应用将它标为本聊天主工作树，归档工具拒绝移除；原仓库不属于可重新附加的受管工作树，因此保留检出，不使用文件删除绕过保护。前端正式归档已另外保存在原仓库 Git 制品目录，未留验收常驻进程。
