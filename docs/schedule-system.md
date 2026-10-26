# 日程系统实施契约

本页描述依据架构书第 9.11–9.12、18.3 节实现的日程基础与授权文本阅读。实际部署版本、远端、浏览器及真实模型结果以本轮独立验收记录为准；契约和测试源码本身不代表部署验收完成。

## 数据、身份和兼容

`schedule_activities` 保存稳定助手身份、owner、创建者、版本及完整活动定义；`schedule_revisions` 保留修订。所有管理入口沿用现有认证、Origin、CSRF 校验，SQL 先按 owner 查询，再返回标题、目的、资源及笔记。私有资料和结果不会自动成为 shared。其他 owner 的冲突只投影匿名忙闲时间，不返回人物、标题、ID或资料。

创建活动必须携带 `Idempotency-Key`，同一 owner 的同键同请求返回原结果，不同请求返回冲突。修改使用 `If-Match` 或 `expected_revision`；两者同时存在时必须相等。已完成单次机会保存原定义快照，当前系列改期不重写该历史。单次改期使用原 `nominal_at` 作为稳定键；改到别月后仍按新时间出现在查询、冲突检查和执行中。此后修改将原系列截断，并创建保留未来规则的新系列。

旧任务触发保留原表、owner、revision、授权、锚点、游标、机会键、预算和历史 Run；日历通过原 `next_due` 和中国日历版本只读展开，不复制派发。旧事件规则显示为待排规则，不伪造未来发生时间。旧任务缺少持续时间时显示未指定。原“设置触发任务”Skill 及后台 API 继续使用原契约，旧草稿不会自动启用。

日记模型作业与每日兜底以 `origin=diary_system` 的只读系统项投影；它们来自日记权威作业及范围水位，不创建第二个整理任务。系统投影没有可编辑的 `activity_id`。

## 管理接口与输入

所有路径在 `/api/admin/v1/schedule/` 下。

| 路径 | 功能 |
| --- | --- |
| `GET calendar?start=<Unix秒>&end=<Unix秒>&time_zone=Asia/Shanghai` | 最大 93 天，默认最多 500 项；支持 `type/creator/state/offset/limit`；重复展开超过限制时要求缩短范围 |
| `GET/POST activities`、`GET/PATCH activities/{id}` | 查询、新建和修改活动，修改范围为 `single/following/series` |
| `POST activities/preview`、`POST activities/{id}/preview` | 无副作用展开、冲突和能力预演 |
| `POST activities/{id}/pause/resume/cancel` | 修改未来派发，保留实际执行和投递证据 |
| `POST activities/{id}/approve` | 管理员以精确 `If-Match`、`confirmed:true`、`scope:series` 批准当前保存的整个系列及已列例外；仅处理 pending_approval |
| `POST proposals` | 使用已有自主授权提出活动；范围外保存待批准，不授予新权限 |
| `GET/POST autonomy-grants`、`PATCH autonomy-grants/{id}`、`POST .../revoke` | 人工授予、版本化修改或撤销自主范围 |
| `GET/POST resources`、`POST resources/{id}/revoke` | 显式提供私人 UTF-8 文本，最多 512 KiB；不接受 URL、系统路径或任意文件读取 |
| `GET/POST plans`、`PATCH plans/{id}` | 工作目标、资料范围、授权与有界规划模式；修改状态须校验版本 |

活动定义：

```json
{
  "title": "阅读授权资料",
  "type": "reading",
  "purpose": "整理本章的观点与疑问",
  "start_at": 1793055600,
  "end_at": 1793056800,
  "time_zone": "Asia/Shanghai",
  "resource_id": "已授权资料ID",
  "target_ref": null,
  "repeat": null,
  "movable": true,
  "grant_id": null,
  "restart_reading": false
}
```

类型为 `reading/video/consolidation/chat/action`。重复规则为 `{frequency:daily|weekly|monthly,interval:1..31,until:Unix秒或null,count:次数或null}`。使用本地民用时间保持重复时刻；DST 不存在或有歧义的机会跳过，不静默移时。跨日活动须拆分；每个活动时长不超过八小时。新系列首版最多检查未来 90 天的冲突，查询不会无限展开多年机会。

## 自主授权与规划

人工授权限定 `allowed_types/resource_ids/time_zone/valid_from/valid_until/horizon_days/window_start/window_end/max_daily_activities/max_daily_minutes/max_daily_changes`。范围内的自主活动直接生效，超范围进入 `pending_approval`，不会因模型称“已获许可”而执行。自主修改仅能操作自己创建且允许移动的活动，不能挪动人工锁定活动；取消活动不会返还此前占用的当日规划预算。

模型预算额外限定 `max_daily_model_calls`（默认 8）、`max_daily_tokens`（默认 24000）、`max_output_tokens`（默认 1200）。每次请求按输入 UTF-8 字节、协议开销和输出上限保守预留；失败、取消与未知消耗不归还预算；后续日期的获授权单次机会可续读此前未完成的资源。该值是保守预留，不伪称 provider 实际计费 token。

工作目标默认为 `planning_mode=bounded_rule`：只在授权窗口查找空闲段，每分钟最多创建一项、同目标一天最多一次、失败至少一小时冷却。显式选 `model` 才调用真实模型规划。宿主按真实忙闲、来源、授权和预算，在最多三天的有界搜索范围内提供最多八个候选 `slot_id`，附精确 Unix 起止、IANA 时区和时长；候选至少提前七分钟，避免模型请求消耗预定开始窗口。其他 owner 的标题和详情不进入模型输入。

模型只收到本目标、可信时间、本人授权书架摘要、授权版本及已校验候选时段，不复用最近聊天或另一 owner 的历史。首选输出为 `{"selection":{"resource_id":"本次提供的资料ID","slot_id":"本次提供的时段ID"},"reason":"简短说明"}`；无合适选择时 `selection` 为 null。模型直接复制 ID，不计算时间戳。宿主使用候选的真实起止和当前目标构造活动，提交前重新校验目标 revision、grant revision、来源、时段未过期、类型、时长、冲突及总预算。候选提出后出现新冲突时拒绝，不能暗自改到另一个时段。旧完整 `activity` JSON 仍经原严格校验兼容，模型始终没有修改授权的接口。

仅允许完整 JSON，或一份完整闭合且没有额外前后文的 `json` 代码围栏；未知字段、重复字段、其他语言围栏和混杂说明均拒绝，不使用字符串搜索截取任意大段回复中的 JSON。

## 规划状态与安全诊断

`schedule_planning_attempts` 保存每次准备、请求、成功、拒绝、失败、中断或无候选的元数据：目标与授权 revision、起止时间、候选数量、保守预留 token、输出上限、请求 SHA-256、原始输出 SHA-256 与字节数，以及稳定机器错误码。它不保存模型正文、输出 reason、目标正文、资源标题、凭证或原始网络错误。模型请求失败时若没有可用文字响应，输出摘要保持空值，不伪造响应证据。

`GET plans` 的每项保留 `state/block_reason/last_planned_at`，并提供 `planning_diagnostics`：`status`、`last_attempt` 和最近十个 `attempts`。状态包括 `not_attempted/prepared/running/completed/declined/rejected/failed/cancelled/interrupted/blocked`。错误示例为 `planning_invalid_json`、`planning_unknown_slot`、`planning_resource_not_offered`、`autonomy_outside_window`、`schedule_conflict`、`planning_model_http_403`、`planning_model_timeout` 和 `planning_model_budget_exhausted`。所有查询仍先按 owner 过滤。

提交失败会回滚活动变更，再独立持久化安全诊断；失败预算和失败记录保留，不自动重新授予权限。无候选时不发起模型请求、不占模型预算；实际请求失败或取消的预留不会归还。终态 attempt 不重放。重启将未结束的 attempt 标为 interrupted，既有失败不会被下一轮空检查覆盖。验收脚本记录 `failed_check` 及安全 attempt 元数据，因此能区分未派发、无候选、预算等待、模型网络失败、协议拒绝和提交冲突。

超出自主范围的待批活动可经独立管理批准入口生效。批准必须认证、CSRF、可信时钟和精确 If-Match，并显式确认整个已保存系列；不支持用普通 resume 代替批准。宿主重新校验所有资料的 owner / 撤权状态、实际未来机会和冲突，将原 grant 约束清除，以 `schedule_admin_approvals` 持久记录批准 ID、actor、revision、定义与例外的精确哈希和审批时间，保留原 AI 创建者。修改获批正文、时间、重复规则或例外会撤销审批并重新待批；暂停 / 恢复同一份未变定义仍保留来源证明。该接口不出现在模型规划输出协议中，不改变模型 provider / 网络准入或收件人许可；没有附加 grant 的人工批准活动沿用每日最多八请求、24000 token 保守预留及每请求1200输出上限。视频、消息外发和未知动作仍受原能力阻断，批准系列不等于收到真实处理或投递结果。

## 真实文本阅读与共享准入

日程 tick 挂在原持久调度器中，不另开计时器、无限模型循环或按人常驻 worker。交互请求优先。日记、人物整理、日程规划 / 阅读及旧自动触发合计最多一个自动模型任务；所有实际请求与既有原生请求共用总并发准入，取消逻辑状态不提前释放真实网络 lease。

`prepare_read_chunk` 只取一个最多 4096 字符的授权片段并保存 fence。只有真实模型请求成功并由 `commit_read_note` 校验来源哈希、owner、活动 revision、fence、资源及授权后，才推进字符水位并保存笔记、provider 和实际覆盖范围。仅查询字符或排好计划不构成阅读完成。多片段累积所有笔记，并在 `schedule_reading_progress` 按 owner、资源 ID 和内容哈希保存全局游标。新日期、新单次机会及不同工作目标默认共享该资源的真实覆盖，续读最后已提交片段，不每天重读开头。每段证据保留原 occurrence_id。显式 `restart_reading=true` 才开启新 generation 重读，旧单次历史仍保留；模型规划不能自行要求重读。已读完资源的新普通活动标为 skipped / resource_already_read，不冒充本次又完成阅读。hash 改变不继承旧游标。每片段最多三次失败，活动执行窗口过期时保留部分进度，后续获授权活动从全局游标继续。资料撤销会删除正文和全局游标、清除所有既有派生笔记并阻断后续读取；在途旧结果不能重新提交。

生产模型凭证只由宿主读取，活动内容与模型输出不能指定 provider URL、环境变量、文件路径、收件人或 shell。在途活动、目标、来源、授权或阅读 generation 改变时每 200ms 复核并中止旧请求，先销毁 HTTP future 再释放真实 lease。Rust 事务和宿主 broker 是本切片的可信实现例外；通用 Python worker 隔离与 broker 未验收，未注册可执行 Python 动作，也不把虚拟环境视为沙箱。

## 能力和验收边界

文本资料读取、真实模型笔记、规则排程及可选真实模型规划各自验证。视频活动当前阻断为 `video_provider_unavailable`，不会以字幕或现有图片能力宣称视频理解。聊天活动阻断为 `contact_consent_and_verified_delivery_required`，不会因为新建日程自动向任何人外发。通用动作要求已验证注册 broker；用户手建整理活动当前只展示阻断原因，真实日记作业独立投影。

本切片不宣称视频帧 / 音轨、真实主动聊天、完整 Python 动作系统或完整 M4 / M5 已交付。联系人接收许可、渠道能力及真实平台 / 客户端证据仍须独立验收。新数据库和删除水位要求部署兼容门禁；回滚不能通过删除新表、恢复旧数据库或重建用户数据绕过门禁。

验证命令：`cargo test -p dshd --lib schedule --offline`。覆盖 owner 隔离、幂等、冲突、授权越界 / 预算 / 撤销、跨日程 / 跨目标续读、显式重读 generation、精确管理员批准及变更撤销、日历范围、民用时间与 DST、单次及此后改期、版本冲突、真实模型提交边界、重启检查点、资源撤销与缺能力阻断。部署和真实模型结果另记，不将单元测试当成真实模型或客户端验收。
