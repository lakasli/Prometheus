# 睡眠存储、控制与整理检查点契约

本页对应架构书 9.6–9.7。当前 `codex/rest-runtime` 工作树已接入管理 API / 页面、宿主时钟、统一 worker / scheduler、三种发送领取链路和有界日记整理；尚未提交、固定 SHA 部署或执行生产睡眠。不得把本地集成和 loopback 测试写成真实平台、真实 provider 整理或完整睡眠交付。实际证据与未测范围见 [本地集成记录](rest-runtime-validation.md)，整理执行边界见 [整理契约](rest-consolidation.md)。

## 历史准备与当前集成

独立存储初版仅加入模块声明、存储、权限校验、状态机、准入决定和专项测试；当时没有接入初始化、管理路由、Gateway、worker、接收 / 发送器或真实整理执行，没有部署、调用模型或发送控制消息。该描述是初版准备事实，不能用于否定或替代后续集成证据，也不回写为当时已经完成。

当前工作树已经连接以下路径：

- `AppState` 通过 `rest_runtime::initialize_all` 在一个 savepoint 中初始化 Store、整理元数据、管理幂等和运行表，共 21 张表。完全无 Rest schema 才按首次创建处理；部分表、未知 Rest 表 / 视图、缺失初始状态或不支持的 reader 拒绝，不能修补成默认 Awake 来掩盖旧状态。主程序取得数据目录独占锁后才恢复旧进程的 Rest 尝试和展示检查点。
- `/api/admin/v1/rest`、policy、sleep、wake、cycles、control-grants 路由已经加入正式管理 Router，沿现有认证、Host / Origin、CSRF、版本冲突及幂等边界工作。管理页面组件已接入当前前端，但本轮证据为 mock API / 单元、类型和构建检查，真实浏览器与远端页面待验收。
- 接收入库仍继续。合格纯文本 Run 的封闭控制表达进入宿主控制候选；QQ 控制消息不并入普通文本批。scheduler 在 NTP / 磁盘 / 模型门禁之前推进唤醒及廉价控制；普通 worker 和七类后台领取在事务内检查 Rest，整理请求计入同一物理请求池和一个自动槽。
- QQ 有序句段、QQ 旧文本 Outbox 和微信实际领取函数在领取事务再次核验 Rest、精确确认载荷、原窗口、顺序及幂等边界；不会将已有 sending / sent / unknown 改成未发送。固定控制确认由宿主生成，许可仍须真实目标与原回复凭证，不能授权模型任意外发。
- 已开始的模型请求继续持有物理 lease。睡眠期间返回的结果进入有界待展示检查点，醒来后复用原结果和原期限；新媒体步骤及最终发布仍重新核验权限与来源。来源失效、遗忘或取消的缓存删除在同一事务中进行；凭证删除也核对 Rest 的真实 unfinished attempt。
- 有界整理执行器已真实调用本地 loopback HTTP，并写入现有日记、版本、来源链及索引。未授权或无资料不调用模型；超过单期 16 个引用仍为 partial，人物与好感没有独立睡眠写入授权时保持零写入。

## 权限与默认值

`RestPolicy` 是版本化配置。定时睡眠和自然语言控制均默认关闭，默认睡眠时长和定时唤醒时刻均为空。管理员显式请求睡眠时，可给确定的 Unix 唤醒时刻；否则使用已配置的下次本地唤醒时刻，或默认睡眠时长。没有有效默认值时拒绝入睡。最长睡眠上限 24 小时；DST 不存在或有歧义的唤醒时刻明确拒绝，不静默移时。

初始有界参数为排空 30 秒、整理 300 秒、等待队列 128、每项最多三次尝试、每周期最多八次整理调用和 24,000 保守预留 token。自然控制开启时必须保留正数的确认排空窗口。配置校验限制最大值，保存策略不重写正在进行周期的原策略与截止时间。预留不是 provider 实际计费 token；失败和 unknown 不退回预算。

`VerifiedRequester` 字段私有，不能由 API / 模型 JSON 反序列化。构造入口如下，均在宿主短事务中执行：

- `requester_for_admin_in(db, &Admin)` 重新核验真实管理员 ID、用户名和角色。调用方仍须执行现有认证、Origin、CSRF 和版本冲突协议。
- `requester_for_run_in(db, run_id)` 从真实 Run 与持久 `conversation_identity` 绑定恢复来源 owner、会话和实际发言者。草稿、自动 Run、Skill、图片来源不作为自然睡眠控制来源。
- 群、频道及频道私信须有唯一可信发言者。群 owner 代表来源域，不代表群内某个人；缺失或多个发言者不能获得控制作用域。
- `set_source_grant_in(db, admin, source, expected_revision, permissions, at)` 只能由真实管理员授予 / 撤销，精确绑定 owner、会话和发言者，带独立 revision 与有效期。不授予资料读取、模型出域、发送或主动联系许可。

自然控制还须同时满足策略开启、当前 grant 有效和当前真实输入命中封闭的直接控制表达。支持明确的“快去睡觉啦”“去睡觉吧”等入睡，以及“醒醒”“现在醒来”等唤醒。普通“晚安”“先不聊了”、引用、假设、转发、好友观察、日记文本及模型生成文字均不能改变全局状态。不认识的控制表达不执行；当前源控制不允许模型自行补入睡眠时长。

## 状态、时间与重复请求

```text
Awake → SleepPending → Consolidating → Sleeping → Waking → Awake
```

`assistant_rest_states` 是单实例权威状态，保存 generation、revision、唯一 cycle、原计划唤醒、排空 / 整理截止及逻辑时间水位。`rest_cycles` 保存不可变的原策略、来源截止和周期结果；`rest_operations` 保存操作 ID、请求哈希与幂等返回。新周期才增加 generation；阶段变化增加 revision，作业领取另用 fence。

`rest_meta` 当前 schema / reader 为 1，发布能力清单已声明 `rest: 1`。兼容检查只允许完整空的 additive schema，或精确默认 policy revision 1、Awake generation 0 / revision 1 / cycle 空且没有辅助语义行的实例退回无 Rest reader 的版本；纯宿主时钟 / logical 元数据不算使用。策略保存后 revision 大于 1、非默认值、任意控制 / 来源 / 作业 / 确认 / 管理幂等 / 候选 / 延迟 raw cache 行均要求 reader1，撤权、unknown 和清空正文后的行也不能忽略。未来 schema 高于 reader、缺表 / 列或未知列均拒绝，不删除表来降级。正式发布仍须固定 SHA、备份及恢复验证。

`sleep_in(db, requester, expected_generation, key, request, clock)` 和 `wake_in(...)` 使用预期 generation。重复同一请求返回原操作，不延后唤醒；同键不同请求冲突。真实来源 Run 另有唯一操作约束，不能换一个幂等键重放旧消息来开启新周期。已经睡眠时的新普通请求不延长截止。

`ClockSample::new(wall_at, monotonic_ms, continuity_id, trusted_for_new_sleep)` 由宿主采样，不能从外部请求接受。应使用跨进程重启稳定的系统 boot ID 和同一系统单调时钟，例如 Linux boot ID / uptime；不能把每轮的 `Instant::elapsed()` 当作相同时间轴。

`advance_in(db, clock)` 必须在调度器 NTP、磁盘空间及模型准入门禁之前独立调用。相同时间轴中，逻辑时间只前进，墙钟回拨时仍按单调时间到期，频繁亚秒轮询保留不足一秒的余量。前跳到原截止即进入唤醒。进程重启保持同 boot 时恢复原期限；单调连续性丢失则以 `clock_continuity_lost` 提前进入 Waking，避免不可信恢复延长睡眠。它不伪造丢失的停机观测。数据库写入失败仍须由上层显示错误，不能报告状态已经持久改变。

SleepPending 保留原物理请求，在已有请求结束或达到原排空截止后才能进入 Consolidating。截止仍在途时保留 `drain_deadline_with_inflight`，状态可继续，但新整理领取仍受实际请求占用阻挡。达到唤醒时间优先进入 Waking，未完成步骤分别变为 failure / unknown；下一次协调推进到 Awake，不等待模型、索引或其他后台门禁。

正常完成的整理在计划唤醒后仍保持 success，不因为“到了唤醒时间”被改为部分失败。原有 partial / failure / unknown 及其检查点保留，不用一个“睡眠完成”覆盖它们。

## 准入与受理确认

`admission_in(db, RestWork, at)` 返回 `RestAdmission::Allow / Hold / Reject / PreserveInFlight`。这是睡眠准入决定，不代替身份、资料、工具、接收方或渠道凭证校验。

| 工作 | 睡眠阶段的决定 |
| --- | --- |
| 接收器、管理、唤醒控制 | 保持可处理；不停止协议 ACK、去重和持久入库 |
| 普通 Run、普通自动工作、普通 queued 投递 | Hold，醒来后重新校验原窗口与权限 |
| 超过原回复截止 | Reject，保留过期事实，不套用新回复凭证 |
| 超出有界普通待处理队列 | Reject 执行，原始入站仍持久保存，由上层显示需处理状态 |
| 已领取 / sending / unknown 尝试 | PreserveInFlight，保持原事实与幂等边界 |
| 本周期整理 | 仅 Consolidating 且匹配当前 cycle 才可准入 |

受理入睡必须避免把自身确认回执挡住。源码已提供 `rest_ack_permits` 元数据：绑定原始控制 Run、真实来源作用域、grant revision、generation、固定载荷哈希、状态与短期限。`bind_ack_payload_in` 仅由宿主为固定确认文字绑定精确哈希；普通模型候选不能借同一 Run 绕过暂停。未领取的受理确认在限定窗口内保持 SleepPending；`mark_ack_attempt_started_in` 在发送领取事务中一次性消费，sending / unknown 不重新授权。

入睡确认只允许 SleepPending；“已醒来”确认只允许实际 Awake。过期、已领取、撤权、载荷不匹配或阶段不匹配的确认明确拒绝；不能在醒来后退化为普通 queued 文字补发“已入睡”。当前宿主控制路径已经生成固定确认文字并绑定精确许可，实际领取函数已经消费许可；真实 QQ / 微信控制确认仍未通过平台验收。实际发送须原目标、原回复凭证、原平台截止和 Outbox 检查，没有给模型提供许可操作接口。

## 来源快照与作业

受理周期的同一事务中冻结允许的 `diary_source_policies` revision / 模型出域许可、撤权 epoch / 水位、删除 epoch、Run / 事件 / 条目 / 人物 rowid 截止，以及真实事件、条目、人物与已审核断言的 ID、revision、摘要；同时冻结实际模型 profile revision / hash、provider、model 与 credential 引用。Store 来源表和整理元数据不复制来源正文或密钥。运行层的 `rest_deferred_presentations` 则明确保存本 Run 的待展示 raw 模型结果，单项最多 128 KiB，不能把它当作已发布回复或长期事实；其来源失效与删除契约见下节。

当前每周期最多冻结 64 个来源域和 512 个来源引用，每页返回最多 64 个引用。超出范围明确标记 `source_snapshot_page_limit` 和 partial，不阻止入睡 / 唤醒，也不声称完整覆盖十万规模资料。分批追赶及跨周期选择由后续执行器接入；不能动态读取新资料来冒充本批已冻结证据。

`frozen_source_in / frozen_sources_page_in` 只返回初始冻结版本。`queue_job_in` 只接受初始冻结来源或本 cycle 已成功提交的宿主 ProducedRef，且当前仍有效；不会把任意新消息、稍后资料或更正版本混入旧批。ProducedRef 在 canonical 写入、`finish_job_in` 的同一事务登记，绑定生产作业 / attempt / fence、owner、父引用完整摘要、目标版本和阶段；消费者逐层检查初始来源或更早阶段派生来源，不能自引或跨 owner。条目同时检查事件来源版本和 rowid，人物断言同时检查父人物、审核状态和有效期。

`consolidation_jobs` 的唯一键为实例、来源 owner、步骤 / 周期类型、周期起止和目标 revision。同键同快照复用原作业；同键不同快照冲突，不覆盖旧完成结果。真实日 / 周 / 月周期划分与最终日记 / 人物 / 索引提交仍由原领域服务负责，本模块没有第二套事实库或自行批准字段的接口。

执行器须按以下顺序使用 API：

1. `advance_in`；`start_step_in` 按冻结来源、当日日记、已结束周期、人物关系、好感候选、版本 / 索引及检查点顺序推进。
2. `queue_job_in` 保存真实受证明 refs；`claim_job_in` 领取唯一 fence，计入每项 / 每周期预算，并与现有日记、日程、人物、普通原生及插件 / WASM 请求占用互斥。当前 `worker::running_count` 已包含 Rest，其他领取器也已接入睡眠门禁，不能为睡眠另开自动槽；物理计数不能重复加 Rest。
3. `sources_for_claim_in / validate_claim_in` 校验 cycle、generation、fence、授权、epoch、当前版本和期限，再由真实领域 reader 读取正文。
4. 在同一个提交事务中再次 `validate_claim_in`，提交原领域结果，并 `finish_job_in` 保存 success / partial / failure / unknown、引用游标、计数和结果摘要。任何校验变化丢弃旧结果，不换用最新来源补交。成功结果必须提供摘要。
5. 真实 HTTP future / 子进程结束或销毁后，原 owner 调用 `request_ended_in`。逻辑取消、来源失效、醒来或超时不会提前归还物理槽。

`retry_job_in` 只允许已结束传输、仍有效来源的 failure / partial，且受次数和预算限制；unknown 不自动重试。步骤不能把尚未结束或不成功的作业汇总为 success。`progress_in` 分别返回每一步和每个作业的真实状态与检查点。

初始化不清除运行租约。仅在主程序已取得独占数据目录锁、确认旧服务退出且旧传输已消失的启动恢复中，才可调用 `recover_after_confirmed_process_exit_in`：将本模块旧请求记为 unknown 并结束其自有请求记录，不重放结果、不退预算，恢复原期限。它不删除其他 native / model 租约，不修改 Outbox 回执，不能作为运行中的超时回收方法。

## 待展示 raw、媒体检查点与失效

模型结果写入 `rest_deferred_presentations` 前，在同一事务确认 Run 仍在执行且原上下文 / 来源可用；遗忘与写入并发不能回填已经失效的 raw。睡眠时将执行暂缓而非宣称回复已完成；醒来复用该冻结结果，不再调用 provider 重做同一回答。计划正文和媒体制品仍按原 Run、来源、预算及平台窗口检查。

开始新的媒体 HTTP / 子进程步骤前先提交 `media_inflight=1`，完成的 plan 检查点置回 0。独占锁启动恢复时，`interrupted + media_inflight=1` 记为媒体步骤 unknown / failed，不自动重做该步骤；`media_inflight=0` 可将本 Run 排队复用已完成的原模型结果，不能替换原回复截止。实际原文 / 来源授权在新 dispatch、展示和最终发送 claim 继续复验。

已有遗忘 / 来源水位事务调用精确 cache erasure，并取消实际受影响且已暂缓的 queued Run；人物 / 日记等普通派生更版仍不盲推历史水位。管理取消 Run 在变更成功的同一事务删除它的 raw / plan cache，其他 owner 的缓存、native / model 物理 lease 和 sending / unknown 回执保留；cache 删除失败时 Run 取消也回滚。凭证删除在真实 Rest attempt 未结束时拒绝，即使活动 profile 已换走凭证或周期已逻辑唤醒；profiles / attempts 部分 schema 不能当作无引用。

## 尚待固定版本与验收

初始化、reader 门禁、管理路由 / 页面、控制入口、统一调度 / 物理计数、worker、发送 claim 和有界日记执行已在工作树接入。单个日 / 周 / 月期超过 16 个来源引用的多批归并仍未实现，当前保留 partial；人物和好感独立授权未接入时为零写入，不能称全套人物睡眠整理完成。固定 SHA 提交 / 推送、远端构建部署、真实 provider 整理、真实 QQ 控制确认、生产睡眠、真实浏览器、跨日兜底及正式资源占用仍待分别验收。

专项仅在临时 SQLite / AppState、合成账户与本机 loopback 模型服务运行。Store 元数据测试不代表真实模型整理；loopback HTTP 不代表外部 provider 通过；真实领取函数不代表平台已接受或用户已读。阶段检查、保留的失败日志和待完成全量结果统一记录在 [本地集成记录](rest-runtime-validation.md)，不把不同源码快照的测试数相加作为最终总数。
