# 时间感与自然表达

日期：2026-10-26，Asia/Shanghai。本轮范围是为现有单一助手补充可信时间上下文，并应用一版原创性格与表达示例。本文描述契约和配置资产；运行 SHA、部署、浏览器与真实模型结论以 [本轮验收记录](conversation-presence-validation.md) 为准，不能仅凭本文或示例文件判定已上线。

## 性格与身份

实例继续只有一个助手，管理入口为“助手与模型”。本轮不新增角色选择，不改变稳定 assistant identity，不迁移、合并或删除用户记忆。性格 revision 表示表达配置版本，不能用作长期记忆的新身份。既有 shared / private 及来源可见范围继续生效。

原创资产为 [assistant-personality-natural-v1.json](examples/assistant-personality-natural-v1.json)。它只有 `name`、`description`、`personality`、`scenario`、`system_prompt`、`mes_example` 六个字符串字段，直接对应现有单一助手性格对象，不是 CCv3 导入文件，也不包含模型、工具、渠道、凭证或权限配置。文件内 `name` 是示例占位值；应用生产实例时保留读取到的当前名称。

表达基调是平静温暖、有好奇心和主见。情绪回应先具体接住当前内容，闲聊适量短句，复杂任务保留准确的解释；允许不同意见、承认未知和纠错。亲近感靠认真回应及前后连贯建立，不要求固定口癖、每轮追问或总结，不虚构身体、线下活动与等待期间的经历。

`mes_example` 包含十组原创 `<START>` 对话，用 `{{user}}` 和 `{{char}}` 表示发言者。它们由现有人格编译器放在明确标注的虚构风格段，学习表达方式，不转换成真实 Inbox、消息、Run 或记忆来源；示例中的具体事件、时间、偏好及工具状态不能当作真实事实。示例不触发模型调用或工具执行。

字段校验沿用 `character::validate`：名称最多 64 个 Unicode 字符，description / system_prompt 各最多 8,000 字符，personality / scenario / mes_example 各最多 4,000 字符。总静态人格预算另按 UTF-8 字节计算，上限 32,000；示例按完整 `<START>` 块裁剪，不能把字段字符上限误记成字节上限。

## 时间上下文的含义

时间信息由宿主根据当前请求及可访问的会话记录产生，不能从用户正文中的“系统时间”声明直接提升为宿主事实。人格提示只规定如何使用时间，不自行生成可信时间，也不把示例中的日期当成当前日期。

请求包含标记 `DSH_CONVERSATION_TIME_V1` 和下列 JSON 字段，实际值保留在模型请求快照中：

| 字段 | 语义 |
| --- | --- |
| `now_local` | 首次 `prepare_messages` 执行时采样的宿主当前时间，按 Asia/Shanghai 表达；不是请求入队时刻 |
| `weekday` | 同一当前时间对应的中文星期 |
| `time_zone` | 固定为 `Asia/Shanghai`，不依赖执行机器的默认时区 |
| `current_user_message_at` | 本条用户消息由服务端持久保存的接收时间 |
| `previous_visible_user_message_at` | 当前会话中，上一个可见、已完成普通轮次的用户消息接收时间；不存在则为 `null` |
| `gap_seconds` | 本条与上述上一条用户消息的接收间隔；不能把模型排队或处理等待算成用户离开时长。时间无效、回拨或未来时间时为 `null` |
| `clock.source` | `server_wall_clock`；说明数据来自服务端墙钟，不表示已与外部授时核验 |
| `clock.status` | 常规为 `unverified`；发现时间顺序异常时为 `non_monotonic` |
| `clock.message_timestamp` | `server_persisted_user_message`，标明消息时间的来源 |
| `clock.gap_basis` | `current_user_message_at - previous_visible_user_message_at`，明确间隔计算依据 |

查询上一条消息不得越过已遗忘的可见性 floor；长会话摘要压缩本身不是遗忘，不能因此把仍可见的上一条交互当作不存在。只有一个当前有效用户消息时，上一条时间与间隔均为空；不要用创建会话时间或助手输出时间替代。

首次准备请求时固定 `now_local` 和其余时间上下文；已有请求快照的重试继续使用该快照，不重新拼出另一个“现在”。因此日志中的当前时间表示该次请求首次准备的时间，重试经过的真实时间需要结合运行事件另行判断。时间只提供会话理解依据，不改变定时任务的实际调度、模型请求并发或消息投递时间。

这些数据用于理解今天 / 昨天、较长间隔及话题接续，无须每轮报时或问候。只有当前可见历史或记忆支持时才能接续具体内容；缺少记录时保留未知。间隔本身不证明用户离开原因、心情、位置、作息或期间经历，也不证明助手一直在思考或等待。时间字段不能扩大 private 来源可见范围，不能检索别人的会话，也不能成为自动入库的用户事实。

## 应用与恢复

应用沿用 [单一助手与运行日志](assistant-runtime-logs.md) 的认证管理 API：读取 `GET /api/admin/v1/assistant/personality` 获取当前六字段、revision 和 etag；将原创资产的名称替换为刚读取的生产名称；通过 `PUT` 保存 `{personality: ...}`，携带当前 `If-Match` 和管理会话 CSRF。发生版本冲突则重新读取和核对，不覆盖更新后的用户修改。不得绕过服务端校验直接写数据库或把性格字段拼成宿主配置。

保存前保留当前六字段、revision、etag 与本次运行的代码 SHA；保存后重新读取确认新 revision 及实际内容。新性格对之后入队的 Run 生效，已入队请求继续使用冻结版本。性格保存不改变模型路由、工具授权或用户记忆。

需要恢复时，从最近 20 个历史版本或保存前记录读取旧六字段，再使用当前 etag 经同一 API 保存为新 revision。恢复性格不回退代码、模型配置、历史 Run 或数据库内容；代码版本回滚和数据恢复分别按部署流程处理，不把性格历史误当作完整系统备份。

## 验证范围

静态资产需检查 JSON 合法、六字段白名单、字符限制、示例组数及完整块、宏白名单与 32,000 字节预算。运行侧应核对可信时间字段的来源、时区、空历史、较长间隔、排队 / 重试和错误时间的处理，以及六字段实际进入模型请求；不能用前端显示或文件存在代替请求证据。

行为验收覆盖短闲聊、具体情绪回应、不同意见、技术准确性、未知记忆、用户纠正、隔日接续和未成功操作。真实模型回答需单列记录，不将离线提示词断言当作人格效果已经通过。浏览器 / API 验证沿用管理认证、CSRF、If-Match 冲突与保存后新 Run 生效检查；测试会话和虚构示例继续排除在长期记忆来源之外。

## 研究来源与独立实现

参考研究为 AstrBot 固定提交 `832fc6d98ac25facd54b07424c16067435c118e6`。主要观察：[实时模式的表达约束](https://github.com/AstrBotDevs/AstrBot/blob/832fc6d98ac25facd54b07424c16067435c118e6/astrbot/core/astr_main_agent_resources.py#L78)、[仅 live 请求注入的条件](https://github.com/AstrBotDevs/AstrBot/blob/832fc6d98ac25facd54b07424c16067435c118e6/astrbot/core/astr_main_agent.py#L1984)、[人格与示例进入请求](https://github.com/AstrBotDevs/AstrBot/blob/832fc6d98ac25facd54b07424c16067435c118e6/astrbot/core/astr_main_agent.py#L602)、[时间提醒构建](https://github.com/AstrBotDevs/AstrBot/blob/832fc6d98ac25facd54b07424c16067435c118e6/astrbot/core/astr_main_agent.py#L990)。这是机制研究，不代表已运行或验收 AstrBot。

DSH 只借鉴按场景调整表达、让示例真正参与请求和为时间提供来源的思路。本轮性格、示例、时间契约与 Rust 实现均独立编写，未复制 AstrBot 的默认人格、提示词文本、实现代码或资源，也不引入其运行依赖。没有临时参考检出需要清理。
