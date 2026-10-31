# Prometheus QQ 入站视频与微信媒体迁移

2026-10-31，Asia/Shanghai，第二十三批。代码固定为 `9b05f043f9a78d4084262e42722473cf4d646fa4`，上一可用 Windows 版本为 `7fe3e2d2c512003baf6780bb2c0f65d7af4ce87e`。schema20 是共享渠道媒体语义的读取门禁；保留原表名和旧记录，不导入旧 Rust 数据。

固定包回归、正式切换、完整实例升级与旧版恢复均已通过。真实平台、客户端播放和外部模型均未执行；不能据此宣称完整迁移。

## 实现与范围

QQ C2C 入站新增 MP4 视频，兼容 `video/mp4` 与有界 octet-stream 的真实容器识别。接收最多 20MiB，沿用既有自包含 H.264/AAC、60 秒、轨道与样本范围检查；不启动解码器，不读取视频帧或音轨。通过校验的原字节绑定原事件、Run、会话、账户与上下文，保留原回复期限。宿主明确回复“尚不能读取画面和声音”，不调用 Native/Vision 来猜内容。

微信继续只接收扫码绑定本人的非群、未删除、已完成用户消息，每条最多一个媒体附件。图片解密后进入既有实际像素规范化和 Vision→Native；视频、已有音频和文件仅保存用于受控回传，宿主如实说明未读。忽略 `voice_item.text`，不将上游文字当作已执行 ASR。文件不解析、不执行；文件名由宿主固定，不接收模型路径、URL 或收件人。

协议对照腾讯官方仓库固定 `874637a0dbc24050f72d1385add4fc6170a2d726` 的[媒体字段](https://github.com/Tencent/openclaw-weixin/blob/874637a0dbc24050f72d1385add4fc6170a2d726/src/api/types.ts)、[上传流程](https://github.com/Tencent/openclaw-weixin/blob/874637a0dbc24050f72d1385add4fc6170a2d726/src/cdn/upload.ts)和[API 封装](https://github.com/Tencent/openclaw-weixin/blob/874637a0dbc24050f72d1385add4fc6170a2d726/src/api/api.ts)：AES-128-ECB/PKCS#7、明文摘要与填充后大小、上传地址/参数、下载加密参数及单项消息。独立实现于本项目，未保留参考检出或引入上游运行依赖。

入站图片接受外层十六进制密钥、base64 原始 16 字节或 base64 十六进制文本；图片允许无密钥的明文输入，其他类型要求密钥。解密后再次核对长度、可选 MD5 与实际格式。图片 8MiB，其余 20MiB；密文额外只容许一个 16 字节填充块。媒体引用、密钥和上下文仅进入加密的持久事件载荷，完成/失败/撤销后清空，不进入管理 DTO、导出、审计或模型提示。

CDN 使用限定的 HTTPS 微信主机族，实际 socket DNS 拒绝私网与公私混合结果；不跟随重定向，不发送平台 Authorization/Cookie，不使用任意远程地址。下载、API 和上传有独立响应上限、空闲超时及整个任务期限，退出和撤销待实际连接关闭。loopback 仅由测试构造显式注入，不是生产配置选项。

微信外发复用已认证的媒体管理入口、同会话实际制品目录与 Outbox。每次申请新随机 AES 密钥，取得上传地址后发送密文，从 CDN 回执取得下载参数，再按固定本人、原 context_token 和稳定 delivery part ID 发送单媒体项。微信说明另占一个文字项，同一回复最多四项；语音只传已有 SILK/MP3，其他音频可明确作为文件发送。图片是规范副本，视频/音频/文件保持通过校验的来源字节；格式标记检查不证明完整压缩音频解码或客户端播放。

全局单一媒体接收任务、单一外发任务、持久接收数量/字节配额及 32 份/128MiB、24 小时暂存继续生效。不为历史会话启动常驻 worker。去重先于配额，微信游标和接收事件在同一事务提交；原 create_time_ms 决定回复期限，重复投递不续期。入站与发送阶段复核当前绑定 generation、凭据、来源 epoch、配置、期限和暂停状态。旧19微信文字目标没有 generation，聊天和原投递历史保留，但不会被补造为媒体授权；新收到消息才冻结当前绑定。

普通媒体传输不是视觉事实。视频、音频和文件 Run 明确标为 `channel_media_transport`，其原文与回复不进入候选、摘要或日记事实链。后续 Native 只能引用当轮实际提供的同会话制品 ID/摘要，不能凭空指定文件。没有自动推送、语音合成、视频理解或新执行权限。

最终消息 HTTP200 且 `ret=0` 才计确认；允许没有 message_id 的明确成功回执，存在 uint64 ID 时无损保存。HTTP201/202/5xx、断连或缺业务回执记未知，禁止自动重试。明确业务拒绝记失败；申请上传或 CDN 上传失败不冒称已发。申请地址响应允许省略 `ret`，仍拒绝任何明确错误并强制校验返回地址。持久最终 POST 标记区分上传中断与发送未知。

## 检查与失败保留

Windows 11 专业版 10.0.26200/x64，Core Ultra 7 155H、22 逻辑处理器、31.61GiB 内存；19:32:47+08:00 可用内存 9.07GiB，D 盘可用 387.45GiB。Node v24.22.1、npm12.2.1、PowerShell7.6.5。未重跑无关原生 worker，沿用先前 `0xC0000142` 失败，TTS、Harness/Python 与 Component 执行继续禁用。

本地前后端构建、格式和差异检查通过。原有330项后端通过；新增31项媒体专项先通过，兼容响应修复后4项相关场景重跑通过，包含新增加的 uint64/缺业务回执场景。新增专项最终共32项。29项浏览器通过，覆盖实际扫码绑定、加密文件入站、文件暂存/人工确认、独立文字说明、发送状态与320px布局。

首轮合成服务把无正文 GET 当 JSON 解析，以及测试误用运行查询方法和图像状态名，均在夹具/断言中修正。首轮浏览器第29项因测试期间更新静态资源、旧标签与新资源不一致而超时；资源固定后29项重新通过。初始提交 `8337c8c5b72d7f1356d2528a31a27606158b59a8` 的预上传要求 `ret`，对照官方仅含上传字段的定义后修复为当前 SHA；保留初始包，未正式部署，新增缺省字段与最终回执反例，不把模拟器原先总返回 `ret=0` 当兼容证明。

固定版本打包命令：

```powershell
pwsh -File deploy/prometheus/package-windows.ps1 -Commit 9b05f043f9a78d4084262e42722473cf4d646fa4 -PreviousVerifiedCommit 7fe3e2d2c512003baf6780bb2c0f65d7af4ce87e
node deploy/prometheus/verify-release.mjs .local/prometheus/releases/9b05f043f9a78d4084262e42722473cf4d646fa4/runtime
$env:PROMETHEUS_RELEASE_ROOT = 'D:/DSH/.local/prometheus/releases/9b05f043f9a78d4084262e42722473cf4d646fa4/runtime'
node --test --test-concurrency=1 apps/server/test/*.test.mjs
npm.cmd run test:e2e --workspace @prometheus/admin
```

Git archive 锁定源码与依赖，前后端同 SHA，ZIP SHA-256 为 `9c1b226415228545637dfc3d253141a5586b6feb734ec8ec502ff460d252b084`。前端构建、后端构建、18项制品检查、362项固定后端（383.51秒）与29项固定浏览器分别通过，上列命令均退出0。服务夹具指向固定 runtime，独立安全/签名/严格 JSON/DNS wire 辅助与合成构造仍有显式本地导入，分别记账。未重新运行无关关系图纯函数专项或原生隔离探针，不重复沿用旧次数冒称本批执行。

## 正式切换与恢复

19:43:49+08:00 核对旧固定7fe3e2d、PID34388完整命令行、数据锁和ready/schema19；无在途Run、作业或投递。向已确认的前台控制台发送Ctrl+C，19:43:52+08:00 原Node、8419监听与数据锁均释放，未强杀或删除锁。完整备份取得相同原子锁，21个文件（含数据库、主密钥和历史快照）保存至 `.local/prometheus/backups/instance-2026-10-31T11-43-52-266Z-7387564a-f8c5-4c56-9c8a-f01118f274eb`，integrity_check为ok，退出0。

逐文件哈希、路径、符号链接及私有ACL检查后恢复至 `recovery-drills/387d2c9d-c6a8-441a-8577-f2971e39138d`，只在副本建立合成管理员和凭据。实际旧19接收QQ图片并完成Vision→Native，以及扫码绑定本人后的微信文字回复；保存原消息、规范图字节/摘要、绑定generation、全部目标和原期限。

真实新20打开同一副本后，上述记录、稳定身份、旧管理员和主密钥保持一致，自动保存完整性ok的schema19数据库快照。旧微信文字目标没有generation，仍可读但媒体授权明确拒绝。随后新QQ接收6300000字节填充MP4；新微信分别实际加密接收并上传/发送图片、视频、SILK标记音频和文件。CDN上传的密文独立解密后摘要与对应来源字节一致，没有平台Authorization或Cookie，最终目标和上下文固定。后续新Native实际引用旧规范图片目录完成QQ回传。旧模型3次、新模型3次均为loopback provider，不计真实外部模型。

旧19直接打开20明确拒绝。将原完整备份独立恢复到 `recovery-drills/549e8494-fbd5-491d-bd94-0eac2cd09271`，匹配旧固定制品启动后ready/schema19、管理员、稳定身份、主密钥和数据库完整性通过，再正常关闭。19:44:07+08:00 两项恢复演练完成，退出0，未覆盖正式实例。

19:44:16+08:00 包内 `Start-Prometheus.ps1 -DataDir D:/DSH/.local/prometheus/instance` 前台启动新固定版本，Node PID35212。19:44:29+08:00 live/ready/schema20、前端build.json及数据锁同SHA；21个匿名管理GET与2个带正确Origin的匿名媒体POST均401，真实登录页无脚本错误。未读取密码或代登录，没有安装服务、自启动或反向代理。

正式升级创建唯一且完整性ok的schema19快照。前后管理员1，records/Run/消息/凭据均0，配置与性格revision1，稳定身份及主密钥一致；admins/config_revisions/personality_versions/records/secrets/source_grants六表逐行哈希相同。九个接收/来源/制品/计划/传输表均为空，正式实例没有合成账号、媒体任务或新授权。原Rust/Breeze/旧管理页未提交差异与切换前逐字节一致，模型未配置、QQ与微信关闭的状态保持不变。

私有证据包括 `.local/prometheus/release-validation/reports/9b05f043f9a78d4084262e42722473cf4d646fa4.json`、`upgrade-evidence/9b05f04/{drill,production}.json`、正式deployment.json、备份清单及 `wx-media-*` 本地/固定构建、回归、浏览器、恢复、主机和切换日志。失败日志与初始8337c8c制品保留。配置、凭据、实例数据及合成运行产物不提交Git。

## 未验收范围

真实手机扫码、QQ/微信收发、客户端视频/音频播放、真实模型表达与长期规模未执行；本页合成 HTTP 与字节验证不能替代平台验收。视频帧/音轨理解、ASR、TTS、群聊媒体、旧封闭回图补救、自动生图发送及其余[完整性缺项](prometheus-completeness-audit.md)继续保留。当前不是完整 M4/M5，也不称 TypeScript 全部迁移完成。正式实例没有真实模型或渠道凭据，不移用旧系统秘密，不扩大原授权。
