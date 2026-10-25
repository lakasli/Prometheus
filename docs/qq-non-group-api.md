# QQ 非群接口与富媒体接入

资料核实：2026-10-25（Asia/Shanghai）。当前线上官方文档静态站版本为 `v1.42.0`，见[API 调用指南](https://bot.q.qq.com/wiki/develop/api-v2/dev-prepare/api-call-guide.html)。本文记录接口契约与验收边界；部署 SHA、真实客户端可见结果由本轮独立验收报告记录，不能从“代码已接入”推导为全部平台 API 已实测通过。

## 范围与入口

C2C 单 PNG 入站使用持久短期接收队列，验签 / 接收事务完成后才 ACK 或推进网关游标；媒体下载在独立消费者执行，不阻塞心跳。仅受管 QQ CDN、固定 DNS、无重定向、有界下载和规范 PNG 进入现有 VisionProvider；媒体资料保留来源，原图同时登记当前私聊的私有短期制品，供明确图片回发。失败媒体不会伪装成已看过图片的普通文字。回复期限在接收时冻结，下载不能续期；处理完清除临时原始事件和 CDN URL。

`GET /api/admin/v1/qq/deliveries` 和 `POST /api/admin/v1/qq/deliveries/{id}/resolve` 提供认证管理者核对不确定回执的入口。明确核对后仅放弃旧轮次的未发部分，保留原回执未知证据并恢复后续对话，不重发旧消息，也不把人工放弃记录成客户端送达。

实现 77 个非群 HTTP 操作的固定清单，包括机器人信息、菜单与指令面板、QQ 单聊、频道与频道私信、成员、身份组、权限、禁言、公告、精华、音频、帖子、日程、表情表态、网关及 Markdown 素材。OAuth/token 由已有服务端 AppSecret 凭据链处理，不通过管理 API 回传。群相关 HTTP 操作不在本次清单；小程序 `qq.getGuildInfo` 属于客户端接口，不能冒充 Rust 服务器接口。

管理页位于现有 QQ 配置区域，认证后可查询能力、执行只读探测、选择固定操作、先校验请求或显式执行本次操作。模型聊天无法使用这些管理操作。服务端固定官方基址 `https://api.bot.qq.com`，不接受客户端提供基址、任意 URL、HTTP 方法或认证头。路径参数、查询字段和顶层请求字段依照每项清单验证，未知字段会被拒绝；合法 opaque ID 编码为单个路径段，拒绝 URL、控制字符、预编码和 dot 路径段，不能借 ID 增加路由层级。每次写入须提交本次操作意图；不自动执行删除、禁言或批量管理。

- `GET /api/admin/v1/qq/api/catalog`：官方操作、来源链接及当前账号最近调用结果。
- `POST /api/admin/v1/qq/api/operations/{id}`：固定操作；`dry_run=true` 仅校验请求，`acknowledge_effects=true` 才允许执行写入。
- `POST /api/admin/v1/qq/api/read-probe`：只执行只读操作；缺少资源、废弃项和写入分别标明。
- `GET /api/admin/v1/qq/api/calls`：只含操作标识、HTTP/业务码、TraceID 和结果分类，不保存请求正文。
- `POST /api/admin/v1/qq/api/presigned-put`：仅使用当前管理员、账号及凭据对应的平台响应引用，向经校验和 DNS 固定的公网 COS 上传；不接受任意上传 URL。

平台返回的 `file_info`、预签名地址、上传 ID 与临时下载链接保留在服务端私有存储；前端只获得限时对象引用。后续请求使用 `{"$qq_ref":"object_ref","field":"file_info"}` 形式引用。对象引用会核对管理员、账号、凭据引用及有效期，账号或凭据变更后不可复用。

## 逐句发送与富媒体

普通对话由宿主校验 JSON 回复计划、逐项持久化及留出间隔，QQ 单聊最大 4 项，并使用不同的 `msg_seq`。富媒体仅接收宿主已授权的制品字节，模型不能指定文件路径、下载 URL 或 QQ 发送目标。语音由后端本地 Piper 合成后交给同一 QQ 出站链路。

单聊媒体先调用 `/v2/users/{user_openid}/files`，固定 `srv_send_msg=false` 获取 `file_info`，再发送 `msg_type=7`。不把媒体上传成功当作消息已发送；平台接受也不等于桌面/手机已可见。

- PNG/JPEG 图片、MP4 视频及 WAV/MP3/OGG/SILK 语音按文件签名核验。
- 5MiB 以内使用 `file_name` 与 base64 `file_data` 直接上传；更大文件执行预上传、分片 PUT、分片完成和合并，项目总上限 20MiB。
- 分片包含整文件 MD5、SHA1、前 10002432 字节 MD5 及各分片 MD5；由服务端指定分片配置，按序验证并有界执行。
- 上传并发 1；预签名目标仅 HTTPS、限定腾讯域名、公网 DNS 固定、不附带机器人 Authorization、不跟随重定向。
- 媒体上传失败可由宿主走文字降级。消息提交之后发生网络异常、缺消息 ID 或平台异步审核时，结果为待核对，不自动重发。
- 其他渠道的富媒体能力需要各自契约与验收；本轮发送适配优先 QQ C2C。

## 当前文档与历史资料的差异

### 单聊媒体输入

QQ Gateway 与已验签 Webhook 先把消息写入有上限的私有接收队列，再推进游标或 ACK。单个受管消费者按接收顺序处理，下载与图片解码不会占住 Webhook 确认或 Gateway 心跳循环。事件去重使用账号、事件及消息 ID，相同文本但不同 ID 的两条消息不会互相覆盖；重启后的消费 generation 会使旧持有者失效。

图片理解接入 QQ C2C 单张 PNG，沿用 Web 图片理解的 1MiB、2048×2048 和 2097152 像素上限。下载仅使用平台附件字段中的受管腾讯 CDN HTTPS 地址，固定公网 DNS、不携带机器人凭证、不跟随重定向，校验后去除元数据。普通文本模型只得到带来源标记的视觉描述，不得到下载地址或媒体密钥。

后续还接入 C2C 单个 MP4 的私有接收与原样回发制品路径，限制为 20MiB、60 秒以内的自包含 H.264 MP4。它只提供用户文字说明和受控制品 ID，不读取视频帧或音轨，不代表视频理解或 ASR；详细校验、事务与测试边界见 [QQ 视频制品接收](qq-incoming-video.md)。实际平台接收和客户端播放结果仍须单独记录，不能由离线容器校验通过推导。

规范 PNG、用户消息、Inbox、Run、视觉来源记录及同会话私有可回发制品在一个事务中保存。原图回发只使用当前 owner/conversation 的限时 asset ID，不把图片识别结果变成用户明示记忆。下载或格式不支持时回复明确提示，不能把带附件的输入当成已查看图片的普通文字回答；图片说明文字里的控制命令不会因此执行。

接收队列最多 200 条，总暂存 JSON 最多 32MiB；回复截止时间按首次收到消息及平台消息时间计算，不因下载延长。处理完成、忽略、失败、账号切换或过期后清空队列中的原文、配置引用和 CDN 地址，剩余传输回执最多保留一天。历史 Inbox、会话、记忆和附件采用各自原有策略，不随传输回执清理。部署兼容性检查会阻止不认识持久接收队列的旧版本忽略已 ACK 的待处理消息。

### 资料版本

[消息概述](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/overview.html)和[发送单聊消息](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_messages.post.html)当前说明单聊每条入站最多回复 **4 次**，窗口 **60 分钟**。发送单聊消息页面的 `msg_id` 字段仍出现“5 分钟”的旧描述，执行层采用尽快回复、最多 4 次，实际超时仍以平台返回码为准。

[富媒体概述](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/rich-media.html)列出 WAV/MP3/OGG/SILK 语音条；[单聊上传](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_files.post.html)部分字段表仍写 silk。必须以真实文件上传和客户端播放确认最终账号能力，不能仅依据其中一个表宣称语音格式全部可用。该上传页面于 2026-10-09 更新，明确支持 5M 以内 `file_data`，且二进制/分片上传要求 `file_name`。

腾讯公开 GitHub `bot-docs` 的 `645787a45937e5d9c4f0f61afefdffde0f38696e` 版本仍写单聊最多 5 次、`file_data` 暂未支持、语音仅 SILK，与当前线上文档不同。实现依据当前线上文档，不把旧仓库资料覆盖到新契约。

## 验证命令与结论边界

本地 Rust 单测覆盖 77 项操作实际经过 HTTP mock 的方法、路径、JSON 正文和鉴权头序列化，并检查重定向不跟随、失败业务码、异步审核、204 空响应、无效 JSON 及不自动重试。另测全部操作拒绝额外请求字段、不透明引用跨主体/账号/凭据和过期拒绝。这些是离线协议验证，不等于 77 项真实 QQ 权限通过。

```sh
cargo test -p dshd qq_
python3 tools/qq-api-live.py --binary <fixed-release>/bin/dshd --web-dir <fixed-release>/web --source-data-dir <shared-data> --report <private-log>/qq-api-live.json
```

真实探针读取生产 QQ 配置及其引用的凭据，只在私有临时实例中使用；临时实例启用 Webhook 模式，不建立第二条 Gateway 消费连接，不复制会话、消息或记忆，不向真实用户发送消息，不执行真实频道的删除或禁言。报告按每项区分 `platform_accepted`、权限拒绝、平台拒绝、网络结果未知、缺少可测资源和未执行写入。真实调用结果保留 HTTP 与业务码，不把 401/403、权限缺失、未知结果或缺少频道资源报告为通过。

真实 QQ 文字、图片、视频、语音客户端联调由本轮授权的目标会话单独执行和记录。对缺少平台权限、频道、成员、可撤回消息或操作授权的接口，保留可调用入口和明确待验范围；不为凑齐成功率而修改用户频道、移除成员或撤回他人消息。

真实富媒体探针只接受以 `DSH富媒体验收：` 开头、当前账号凭据、有效 C2C 目标且仅 seq=1 已发送的指定 Run。脚本只发本地 Piper 合成 WAV、自生成几何图案 PNG 和自有不超过 5 秒 MP4，分别占 seq=2/3/4；已存在的报告禁止覆盖，未知投递阻断后续，避免重新执行。探针不会把真实会话复制到临时实例，报告仅保存各文件哈希、大小及数字状态；`client_verified=false` 必须通过客户端验证后另记实际结果。

```sh
python3 tools/qq-media-live.py --binary <fixed-release>/bin/dshd --web-dir <fixed-release>/web --source-data-dir <shared-data> --source-run-id <explicit-test-run> --runtime-root <installed-piper> --video-fixture <owned-short-mp4> --report <new-private-report>
python3 -m unittest discover -s tools/tests -p test_qq_media_live.py -v
```

`tools/qq-c2c-controls-live.py` 使用单独的 `DSH接口验收：` 合成消息，要求该 Run 恰好只有 Native seq=1 已确认发送。输入状态占 seq=2，Markdown 占 seq=3；一个流式消息占 seq=4，按[官方流式契约](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_stream_messages.post.html)在 index=0/1 复用 seq=4 及首片返回的 stream ID，结束片为 input_state=10，replace 内容保持已发送前缀。只有脚本本次新建且已获得 ID 的 Markdown/stream 消息可在短暂显示后撤回，不能撤回 Native seq=1、入站消息或其他既有消息。unknown/异步响应立即阻断，finally 不继续发起网络清理，私有 journal 留下需要人工核对的本次消息引用。

```sh
python3 tools/qq-c2c-controls-live.py --binary <fixed-release>/bin/dshd --web-dir <fixed-release>/web --source-data-dir <shared-data> --source-run-id <explicit-interface-test-run> --report <new-private-report>
python3 -m unittest discover -s tools/tests -p 'test_qq_*.py' -v
```

## 固定操作清单

### 真实接口联调所需条件

`requires_resource` 表示探针尚未提供合适资源，不能直接解释为账号没有权限或没有频道。只读探针先调用机器人和频道列表，再按依赖发现 guild、相应类型的 channel、role、schedule、thread；报告只保存数量，不保存 ID 或正文。成员和帖子接口的“私域机器人”限制来自各自官方说明，不能从“QQ C2C 已可用”推导为这些权限已获批。

| 接口族 | 真实测试必须具备的条件 | 可在现有账号安全执行的范围 |
| --- | --- | --- |
| 鉴权、机器人信息、普通/分片 Gateway 查询 | 已配置 AppID/AppSecret、平台允许服务端来源 IP | 只读查询并分别保存 HTTP/业务码；token 由服务端保管 |
| 菜单 GET/PUT | 账号具备菜单能力；PUT 会覆盖完整菜单 | 可只读；不为验收覆盖用户现有菜单 |
| 指令面板列表 | 必须给 `scope=c2c/group/channel/dm`，每页 `limit≤50` | 默认查询 c2c；40030011 是 scope 错误，不是权限拒绝 |
| 面板创建/详情/修改/删除 | 账号面板额度最多 20；修改/删除须掌握自己的新建 ID | 仅新建 specific 专属测试面板；无目标时尝试空绑定并保留平台拒绝，有独立验收 Run 时只绑定其中明确授权的本人目标，不改为全局配置 |
| 面板关联对象 | specific 面板与明确授权测试用户的真实 C2C openid | 无独立测试用户时不执行关联变更；不编造 openid |
| Markdown 素材准备/完成/查询/删除 | 支持的 PNG/JPEG/GIF/WebP、不超过 10MB、MD5/SHA1、腾讯 COS 预签名上传；总额 100 | 只上传自生成图案，只删除本次创建的 image_id；不清理用户原有素材 |
| C2C 文字/Markdown/输入状态/流式/撤回 | 有效好友关系、用户明确测试消息、相符的 msg_id/seq/窗口；消息类型权限独立确认 | 每条真实测试最多 4 次回复；流式与撤回需另一个新鲜测试上下文，撤回只能指向本次合成消息 |
| C2C 文件、预上传、分片完成 | 明确测试接收者、规范图片/视频/语音、摘要正确；分片遵循服务端 block_size | 直接上传已实测的合成文件；分片必须单独验收，不能由小文件成功推导通过 |
| 频道详情、子频道、角色、API 权限 | 机器人真实加入至少一个频道；相应接口授权、角色要求由平台核验 | 先看实际频道数量和权限；不拿不存在或随意生成的 guild/channel ID 测试 |
| 频道成员/身份组成员/成员详情 | 合法频道与用户/角色 ID；成员列表等官方接口仅私域机器人可申请，开通后可能须重新添加机器人 | 查询自身机器人或明确测试成员；不把 C2C openid 当频道用户 ID |
| 频道/成员禁言、成员移除、角色及权限修改 | 管理员权限、隔离测试频道和测试成员、明确可恢复的权限快照 | 不对当前真实频道成员执行破坏性测试 |
| 公告、精华、日程、帖子 | 相应子频道类型、可操作的本次合成消息/日程/帖子，以及平台 API 权限；帖子列表等为私域限制 | 缺少自有测试对象时不改动现有内容；日程应用 `application_id=1000050`、论坛 `type=10007` 分别识别 |
| 在线人数/频道音频/上麦下麦 | 语音 `type=2` 或直播 `type=10005` 子频道；音频控制还要求平台音频类机器人资格 | QQ C2C 语音条成功不代表频道音频控制或上麦已授权 |
| 频道私信 | 机器人与明确测试收信人有共同频道、正确频道 user ID 和 DMS 会话 | 不能用 C2C 好友 ID 替代；不向任意频道成员建立测试私信 |
| 表情表态与表态成员查询 | 有效文字子频道消息及表情 type/id；读取/修改权限 | 只对本次创建的合成消息测试，不修改他人消息 |
| 互动回调 ACK | 实际订阅并收到 `INTERACTION_CREATE`、有效且未过期的 interaction_id | 缺真实事件时明确未测试，不能拿随机 ID 当联调证据 |
| QQ 小程序 `qq.getGuildInfo` | QQ 小程序工程、机器人服务配置、客户端 extendData 和对应客户端 SDK | 属于客户端能力，不能伪装成已实现的 Rust HTTP 接口 |

`tools/qq-resource-live.py` 提供专属面板与合成素材的有界 CRUD：先完整快照现有 ID，再用随机 marker 创建新对象，只修改、删除本次创建且不在快照中的 ID，使用 0600 ownership journal 留恢复依据。默认面板不绑定用户；显式提供 `--target-source-run-id` 时，必须通过独立接口验收 marker、owner、账号、凭据、回复窗口及 Native seq1 检查，只可临时绑定该 Run 的本人测试目标，并只在这个新面板上测试解除/恢复同一关联。它不写现有菜单，不发消息；unknown 写入不会重发，unknown 删除不会再次自动删除，清理未确认时报告 `cleanup_pending=true`。该脚本是验收工具，实际成功范围须看固定部署 SHA 上生成的报告。

```sh
python3 tools/qq-resource-live.py --binary <fixed-release>/bin/dshd --web-dir <fixed-release>/web --source-data-dir <shared-data> --report <new-private-report>
# 已明确授权本人测试目标时，额外传 --target-source-run-id <explicit-interface-test-run>
```

2026-10-25 的初次只读报告保留了 `/v2/panels` 返回 HTTP400/40030011 的事实。当时请求缺少必填 scope，后续补齐清单、服务端校验、UI 及探针默认查询，不将历史失败改写成平台权限拒绝或成功。角色成员分页采用官方 `start_index`，帖子列表不虚构未公开的分页参数。

同日真实空面板列表返回 `{"is_end":true}` 并省略 records；探针仅在明确结束且无后续 cursor 时将省略/null 数组当作空集。空用户绑定创建返回 HTTP400/40030016，只能判定有必填字段未满足，未凭此猜测缺的是 user_openids 或命令字段；command 项按官方示例带齐 type/name/desc，后续非空本人目标用单独报告验证。

首次合成 Markdown 客户端已显示后，撤回被本地路径参数校验拦截：平台实际消息 ID 含 `!`。修复把合法 opaque ID 的 `!+/=` 作为单个路径 segment 规范转义（例如 `/` 为 `%2F`），并保留 URL、控制字符、预编码 `%` 与 dot 路径段拒绝；安全单 ID `a..b` 不误伤。真实 HTTP mock 验证编码后不能增加路由分段。原报告的保守 unknown 保留，后续工具另外记录本地 4xx/code，区分“未提交平台”与“平台请求结果未知”；旧合成消息不盲重撤回，新版本用新的专属 marker 验收。

2026-10-25 后续真实平台验证按独立报告累计，不能以运行目录中的模板或一次 HTTP mock 代替：

| 固定脚本 SHA / 运行 SHA | 范围与真实结果 |
| --- | --- |
| `ad77bc3014a1695abaf8e91e5915fcde9820c2aa` / `387652c94ac4abc887506abe5ea74112e604ce42` | 七个无资源读取均 HTTP200；guild 首次列表实际返回零项，频道、成员、日程、论坛、频道音频等缺少平台资源，未伪造 ID 联调。[只读报告](validation/qq-api-387652c.json)。 |
| `c86ddef2d837899bd2322bd642d37664ec04eea6` / `387652c94ac4abc887506abe5ea74112e604ce42` | 仅创建本次合成指令面板，绑定本人专属验收 Run 的目标；创建、详情、内容修改、解除/恢复该目标、删除全部 HTTP200。合成 PNG 素材预上传、签名 PUT、完成、列表和删除通过，既有面板/素材最终快照未改变，`cleanup_pending=false`。[专属资源报告](validation/qq-resource-387652c-r3.json)。 |
| `45995830c108a563c3ca6b1d1982a4935d9ae235` / `25d973820568d72c7ace88bd8239b15c8ee29f56` | 新验收上下文的输入状态 seq2、Markdown seq3、该 Markdown 撤回、流式 seq4 的首片及结束片均 HTTP200。首个平台操作时源上下文约 862 秒，支持本账号本样本超过旧文档五分钟仍可被动回复，不能据此声称已测完整 60 分钟边界。[C2C 控制报告](validation/qq-controls-25d9738.json)。 |

本轮累计 **19/77 个不同 HTTP 操作有真实平台接受证据**，逐项来源见[累计平台证据](validation/qq-api-coverage-after-controls.json)；媒体类型和流式分片不重复计算接口数量，COS 签名 PUT 不计入 77 项。其余操作中缺资源、未执行和废弃状态分别保留，不能称为全部非群 API 实机验收通过。

上述四份仓库报告为本地验收原报告的逐字节副本，检查未包含凭据、file_info、预签名 URL、目标 ID 或聊天正文，无字段需脱敏；私有 ownership journal 未提交。机器报告保留 `client_verified=false` 或 `client_verification_included=false`，只表明其中记录的是平台结果；后续桌面观察须另记时间、画面与实际动作，不能改写原始报告或把 HTTP200 当作客户端输入状态、流式内容或撤回动画已被看见。

最后一轮流式响应返回 36 字符 UUID 形式的 `stream_msg_id`，使用它调用普通消息撤回端点返回 HTTP400 / `40061001`（官方含义为请求参数无效）；同轮普通 Markdown 返回的 opaque 消息 ID 撤回已通过。流式首片与终片均被平台接受，流式撤回未通过，脚本没有猜测 ID 转换、改用其他字段或自动重试。该报告 `owned_message_cleanup_pending=true` 仅指本次合成流式消息未撤回，ownership journal 留在私有日志目录；不表示素材/面板清理失败，也不表示结果未知。官方已公开文档未说明 `stream_msg_id` 到普通撤回 `message_id` 的转换，待平台补充契约或提供可验证能力后继续单独验收。

### 清单索引

下表是服务端适配清单，**不是全部真实平台成功清单**。废弃接口保留文档索引，实际执行拒绝并指向替代能力。每项最新状态在管理页或验收 JSON 中查看。

| 方法 | 固定路径 | 功能 | 文档 |
| --- | --- | --- | --- |
| GET | `/users/@me` | 获取机器人详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/users_me.get.html) |
| POST | `/v2/generate_url_link` | 生成分享链接 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_generate_url_link.post.html) |
| GET | `/v2/menu` | 查询全局自定义菜单 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_menu.get.html) |
| PUT | `/v2/menu` | 修改全局自定义菜单 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_menu.put.html) |
| GET | `/v2/panels` | 查询指令面板列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_panels.get.html) |
| POST | `/v2/panels` | 创建指令面板 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_panels.post.html) |
| DELETE | `/v2/panels/{panel_id}` | 删除指令面板 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_panels_panel_id.delete.html) |
| GET | `/v2/panels/{panel_id}` | 查询指令面板详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_panels_panel_id.get.html) |
| PUT | `/v2/panels/{panel_id}` | 修改指令面板 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_panels_panel_id.put.html) |
| PUT | `/v2/panels/{panel_id}/target` | 修改指令面板关联对象 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_panels_panel_id_target.put.html) |
| POST | `/v2/users/{user_id}/upload_part_finish` | 单聊分片上传完成 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_id_upload_part_finish.post.html) |
| POST | `/v2/users/{user_id}/upload_prepare` | 单聊富媒体预上传 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_id_upload_prepare.post.html) |
| POST | `/v2/users/{user_openid}/files` | 单聊富媒体上传 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_files.post.html) |
| POST | `/v2/users/{user_openid}/messages` | 发送单聊消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_messages.post.html) |
| DELETE | `/v2/users/{user_openid}/messages/{message_id}` | 撤回单聊消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_messages_message_id.delete.html) |
| POST | `/v2/users/{user_openid}/stream_messages` | 流式发送单聊消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_stream_messages.post.html) |
| DELETE | `/channels/{channel_id}` | 删除子频道 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/channels_channel_id.delete.html) |
| GET | `/channels/{channel_id}` | 获取子频道详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/channels_channel_id.get.html) |
| PATCH | `/channels/{channel_id}` | 修改子频道 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/channels_channel_id.patch.html) |
| POST | `/channels/{channel_id}/announces` | 创建子频道公告（2022年3月15日后废弃） | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/announces/post_channel_announces.html) |
| DELETE | `/channels/{channel_id}/announces/{message_id}` | 删除子频道公告（2022年3月15日后废弃） | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/announces/delete_channel_announces.html) |
| POST | `/channels/{channel_id}/audio` | 音频控制 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/audio/audio_control.html) |
| GET | `/channels/{channel_id}/members/{user_id}/permissions` | 获取子频道用户权限 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/channel_permissions/get_channel_permissions.html) |
| PUT | `/channels/{channel_id}/members/{user_id}/permissions` | 修改子频道用户权限 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/channel_permissions/put_channel_permissions.html) |
| POST | `/channels/{channel_id}/messages` | 发送子频道消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/message/send.html) |
| DELETE | `/channels/{channel_id}/messages/{message_id}` | 撤回子频道消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/message/recall.html) |
| DELETE | `/channels/{channel_id}/messages/{message_id}/reactions/{type}/{id}` | 表情表态 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/trans/emoji.html) |
| GET | `/channels/{channel_id}/messages/{message_id}/reactions/{type}/{id}` | 表情表态 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/trans/emoji.html) |
| PUT | `/channels/{channel_id}/messages/{message_id}/reactions/{type}/{id}` | 表情表态 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/trans/emoji.html) |
| DELETE | `/channels/{channel_id}/mic` | 机器人下麦 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/audio/delete_mic.html) |
| PUT | `/channels/{channel_id}/mic` | 机器人上麦 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/audio/put_mic.html) |
| GET | `/channels/{channel_id}/online_nums` | 获取子频道在线成员数 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role/get_online_nums.html) |
| GET | `/channels/{channel_id}/pins` | 获取精华消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/pins/get_pins_message.html) |
| DELETE | `/channels/{channel_id}/pins/{message_id}` | 删除精华消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/pins/delete_pins_message.html) |
| PUT | `/channels/{channel_id}/pins/{message_id}` | 添加精华消息 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/pins/put_pins_message.html) |
| GET | `/channels/{channel_id}/roles/{role_id}/permissions` | 获取子频道身份组权限 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/channel_permissions/get_channel_roles_permissions.html) |
| PUT | `/channels/{channel_id}/roles/{role_id}/permissions` | 修改子频道身份组权限 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/channel_permissions/put_channel_roles_permissions.html) |
| GET | `/channels/{channel_id}/schedules` | 获取频道日程列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/schedule/get_schedules.html) |
| POST | `/channels/{channel_id}/schedules` | 创建日程 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/schedule/post_schedule.html) |
| DELETE | `/channels/{channel_id}/schedules/{schedule_id}` | 删除日程 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/schedule/delete_schedule.html) |
| GET | `/channels/{channel_id}/schedules/{schedule_id}` | 获取日程详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/schedule/get_schedule.html) |
| PATCH | `/channels/{channel_id}/schedules/{schedule_id}` | 修改日程 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/schedule/patch_schedule.html) |
| GET | `/channels/{channel_id}/threads` | 获取帖子列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/forum/get_threads_list.html) |
| PUT | `/channels/{channel_id}/threads` | 发表帖子 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/forum/put_thread.html) |
| DELETE | `/channels/{channel_id}/threads/{thread_id}` | 删除帖子 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/forum/delete_thread.html) |
| GET | `/channels/{channel_id}/threads/{thread_id}` | 获取帖子详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/forum/get_thread.html) |
| POST | `/dms/{guild_id}/messages` | 频道私信 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/message/dms.html) |
| DELETE | `/dms/{guild_id}/messages/{message_id}` | 频道私信 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/message/dms.html) |
| GET | `/guilds/{guild_id}` | 获取频道详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/guilds_guild_id.get.html) |
| POST | `/guilds/{guild_id}/announces` | 创建频道公告 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/announces/post_guild_announces.html) |
| DELETE | `/guilds/{guild_id}/announces/{message_id}` | 删除频道公告 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/content/announces/delete_guild_announces.html) |
| GET | `/guilds/{guild_id}/api_permission` | 获取机器人在频道可用权限列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/api_permissions/get_guild_api_permission.html) |
| POST | `/guilds/{guild_id}/api_permission/demand` | 发送机器人在频道接口权限的授权链接 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/api_permissions/post_api_permission_demand.html) |
| GET | `/guilds/{guild_id}/channels` | 获取子频道列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/guilds_guild_id_channels.get.html) |
| POST | `/guilds/{guild_id}/channels` | 创建子频道 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/guilds_guild_id_channels.post.html) |
| GET | `/guilds/{guild_id}/members` | 获取频道成员列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role/member/get_members.html) |
| DELETE | `/guilds/{guild_id}/members/{user_id}` | 删除频道成员 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role/member/delete_member.html) |
| GET | `/guilds/{guild_id}/members/{user_id}` | 获取频道成员详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role/member/get_member.html) |
| PATCH | `/guilds/{guild_id}/members/{user_id}/mute` | 频道指定成员禁言 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/speak/patch_guild_member_mute.html) |
| DELETE | `/guilds/{guild_id}/members/{user_id}/roles/{role_id}` | 删除频道身份组成员 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/delete_guild_member_role.html) |
| PUT | `/guilds/{guild_id}/members/{user_id}/roles/{role_id}` | 创建频道身份组成员 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/put_guild_member_role.html) |
| GET | `/guilds/{guild_id}/message/setting` | 获取频道消息频率的设置详情 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/speak/setting/message_setting.html) |
| PATCH | `/guilds/{guild_id}/mute` | 频道批量成员禁言 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/speak/patch_guild_mute_multi_member.html) |
| GET | `/guilds/{guild_id}/roles` | 获取频道身份组列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/get_guild_roles.html) |
| POST | `/guilds/{guild_id}/roles` | 创建频道身份组 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/post_guild_role.html) |
| DELETE | `/guilds/{guild_id}/roles/{role_id}` | 删除频道身份组 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/delete_guild_role.html) |
| PATCH | `/guilds/{guild_id}/roles/{role_id}` | 修改频道身份组 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role-group/patch_guild_role.html) |
| GET | `/guilds/{guild_id}/roles/{role_id}/members` | 获取频道身份组成员列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/role/member/get_role_members.html) |
| PUT | `/interactions/{interaction_id}` | 互动事件响应 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/interactions_interaction_id.put.html) |
| POST | `/users/@me/dms` | 频道私信 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/channel/message/dms.html) |
| GET | `/users/@me/guilds` | 获取机器人频道列表 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/users_me_guilds.get.html) |
| GET | `/gateway` | 获取通用 WSS 接入点 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/gateway.get.html) |
| GET | `/gateway/bot` | 获取带分片 WSS 接入点 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/openapi/wss/shard_url_get.html) |
| GET | `/v2/media-images` | 素材管理 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/material-management.html) |
| POST | `/v2/media-images/complete` | 素材管理 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/material-management.html) |
| POST | `/v2/media-images/prepare` | 素材管理 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/material-management.html) |
| DELETE | `/v2/media-images/{image_id}/delete` | 素材管理 | [官方说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/material-management.html) |
