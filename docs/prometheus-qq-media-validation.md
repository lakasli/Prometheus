# Prometheus QQ 媒体外发与大文件上传迁移

2026-10-31，Asia/Shanghai，第二十二批。代码 **`7fe3e2d2c512003baf6780bb2c0f65d7af4ce87e`**，schema19；上一可用 Windows 制品为 `f5c8314c05147118f8e2a80d932b6de59565db71`，schema18。对照旧固定 Rust `df6429cbceb8f16504efe9464c1abbbdb4a67e29` 的 `qq_media.rs` 和 `media_assets.rs`，迁入受控 C2C 图片/视频/已有音频发送及分片上传，运行链路没有调用 Rust、WSL 或媒体执行程序。[完整迁移](prometheus-completeness-audit.md)仍未完成。

## 发送与权限边界

管理页“渠道与媒体 → QQ 媒体回复”选择实际收到的、原回复已入队且仍在被动回复期限内的私聊消息，展示固定接收方标识。操作者上传并校验文件后，还须单独确认排队。没有任意收件人、URL、系统路径或文件名入口；上传成功不是发送成功。文件/发送请求分别绑定操作者和幂等标识，相同标识更改内容或来源返回冲突，浏览器失败重试保留原标识。

Native 仅能引用本轮实际提供的同 owner/会话/上下文制品目录。当前自动目录仅包含仍有效的已接收规范图片；手工上传的视频/音频不自动授予模型使用权限。媒体回复最多四项，合法图片可与文字混排；每次实际确认后再等待至少 1500ms，最多采用模型声明的 8000ms。没有合成工作器结果时不能以文本 `voice` 指令启动语音合成，已有音频传输不启用 TTS/ASR。

接收方、账号、凭据引用、原消息 ID、截止时间和上下文来自已认证的渠道来源。取得 token 后、每个上传阶段及最终消息 POST 前重新检查；更正、遗忘、配置变化、睡眠暂停或期限到期阻止后续发送。媒体制品与摘要、实际提示提供记录和回复计划持久保存，模型不能添加未提供的制品引用。外发计划及手工追加媒体的相关回复按派生内容处理，不将其通过摘要或候选记忆洗成独立事实；用户原文保留各自用途边界。

## 文件与资源

图片最多 8MiB，沿用既有实际解码和规范 PNG 输出；回传的是规范化来源图片，不是原始附件字节。视频最多 20MiB，迁入旧 MP4 容器检查：最多 60 秒、自包含 H.264 视频轨，可带 AAC；检查 box 边界、时长、编解码配置、样本大小/时间/块映射、mdat 范围和轨道重叠，拒绝外部引用和碎片化容器。没有解码帧、执行 ffmpeg 或声称理解视频画面。

已有音频最多 20MiB：PCM WAV 检查 RIFF 长度、fmt/data 边界、通道、采样率、块对齐和 60 秒上限；MP3/Ogg/SILK 沿用旧实现的有界格式标记检查，仅供传输，不证明完整解码、时长或客户端可播放。真实合成工作器仍禁用。

认证媒体上传接口独享 29MiB JSON 上限，配置等入口保持原限额。读取大正文前即取得单一接收名额，第二个请求 429；严格 base64 及往返检查，单制品 20MiB、全局最多 32 份/128MiB、24 小时暂存。来源撤销或到期清除在线 BLOB，备份不声称同步擦除。渠道外发一次一个，仍受原全局投递队列和回复项数约束，不为历史会话启动常驻进程。这些字节和并发门禁不是操作系统内存隔离证明。

## QQ 协议

不超过 5MiB 使用 `/v2/users/{固定目标}/files` 的 `file_data`。超过 5MiB 先 `upload_prepare`，计算完整 MD5、SHA1 和**前 10002432 字节**的 `md5_10m`；这几个摘要用于平台协议，不替代身份认证。严格复核返回的 block_size、分片数和完整序列，接受从 0 或 1 开始的整数 index，不接受字符串序号、跳号、重复或乱序。全部预签名地址先检查，字节范围按数组位置切分，逐片 PUT 后用平台原 index、实际块长度与 MD5 调用 `upload_part_finish`，最后 `/files` 取得不透明 file_info。

预签名 PUT 仅接受原 HTTPS COS 主机族，实际 socket DNS 拒绝私网或公私混合结果；不使用系统代理，不跟随重定向，不附加 QQ Authorization、App ID 或 Cookie。JSON/PUT 响应最多 512KiB、空闲超时 10 秒、整个媒体投递最多 90 秒。所有上传请求显式 `srv_send_msg=false`，file_info 只在当前请求内使用，不进模型提示、日志、状态页或配置导出。

最终单独发送 `msg_type=7`、file_info、原 msg_id 和序号。记录上传阶段和已确认分片数，不能以上传 HTTP 200 代替消息发送。仅上传失败或中断记“失败”；最终 POST 已开始后遇到断连、异步 201/202/304023/304024 或缺消息 ID 记“结果未知”，不自动重试。明确平台拒绝记失败。重启保留未开始的队列；持久的最终 POST 标记区分上传中断和发送未知。原始接收凭据和工作器安全门禁没有放宽。

## 检查与固定版本

目标 Windows 11 专业版 10.0.26200/x64，Core Ultra 7 155H、22 逻辑处理器、31.61GiB 内存；17:24:44+08:00 可用内存 10.05GiB、D 盘可用 389.21GiB。Node v24.22.1、npm 12.2.1、PowerShell 7.6.5。

本地前后端构建、格式与差异检查通过。新增 20 项后端专项覆盖直接图片/WAV、6.3MB 合成填充 MP4 的 0/1 序号、5MiB/5MiB+1/20MiB/20MiB+1、10002432 字节前缀、全部 PUT 字节与摘要、非法预上传、SSRF/重定向、真实 socket 撤销、最终发送未知、鉴权/作用域、正文接收名额、正常重启和持久中断检查点、过期清图及 Native 实际提供目录。完整后端为 330 项，浏览器 28 项；新增浏览器用例通过实际认证接口完成 6.3MB MP4 四片上传、图片外发、分开的确认与状态展示及 320px 布局。

开发首轮遗漏 `/qq/media` 路由分发导致 503；初次大 base64 分组正则在 6300000 字节输入触发 RangeError。修复路由，改用平坦字符校验、大小和解码往返复核后通过边界验证。中间一次边界场景失败的日志亦保留，后续专项和固定包均重新执行，不将早期失败计为通过。私有原始日志和独立正则反例保留在 `qq-outgoing-*` 文件中。

提交推送后核对 origin 分支 tip 为完整代码 SHA，执行：

```powershell
pwsh -File deploy/prometheus/package-windows.ps1 -Commit 7fe3e2d2c512003baf6780bb2c0f65d7af4ce87e -PreviousVerifiedCommit f5c8314c05147118f8e2a80d932b6de59565db71
node deploy/prometheus/verify-release.mjs .local/prometheus/releases/7fe3e2d2c512003baf6780bb2c0f65d7af4ce87e/runtime
$env:PROMETHEUS_RELEASE_ROOT = 'D:/DSH/.local/prometheus/releases/7fe3e2d2c512003baf6780bb2c0f65d7af4ce87e/runtime'
node --test --test-concurrency=1 apps/server/test/*.test.mjs
npm.cmd run test:e2e --workspace @prometheus/admin
```

从 Git archive 锁定依赖构建，前后端 SHA 一致，ZIP SHA-256 为 `b50e8756d81a992cd7bf5c4316364c99e1e8dcf04078c4a4c4c9b0ece0f59043`。前端构建、后端构建、18 项制品检查、330 项固定后端（410.06 秒）与 28 项固定浏览器分别通过，上列命令均退出 0。服务夹具绑定固定 runtime，签名/安全/严格 JSON/合成构造及独立 DNS wire 辅助仍有显式本地导入，分别记账。工作器本批未重测，沿用上一固定 f5c8314 的 0xC0000142 初始化失败记录，语音生成、Harness/Python 和 Component 执行继续禁用。12 项关系图纯函数沿用原实际记录，本批未重跑。

## 正式切换与恢复证据

17:33:07+08:00 核对旧固定 f5c8314、PID2656 的完整命令行、数据锁和健康版本，确认无在途 Run、作业或投递。向其前台控制台发送 Ctrl+C，17:33:10+08:00 原 Node、数据锁与 8419 监听均释放；未强杀或删除锁。完整备份取得同一原子锁，20 个文件（含数据库、主密钥和历史快照）保存至 `.local/prometheus/backups/instance-2026-10-31T09-33-10-714Z-c56c7c46-39cb-438e-bbf1-ce21149bdd14`，完整性 ok，退出 0。

核对逐文件哈希、路径、符号链接与私有 ACL 后恢复至 `recovery-drills/4439b589-02f9-4900-ae80-4e3303ca1f80`，仅在副本加入合成账户和凭据。先由真实旧18制品接收图片并完成 Vision→Native/文字投递，旧媒体外发入口确为 503。新19打开同一副本后，旧管理员、稳定身份、主密钥、原消息、规范来源图片原字节/摘要及原回复期限一致，自动保存 schema18 快照。

随后在同一旧被动回复上依次以序号 2/3/4 完成图片、6300000 字节填充 MP4 和 PCM WAV 的实际 loopback 上传/发送。MP4 四片 PUT 拼接后与输入逐字节一致，确认序号为 1/2/3/4，PUT 没有 QQ Authorization、App ID 或 Cookie；相同发送标识回取同一项，没有重复发送。再由一个新入站普通文字 Run 实际接收旧来源图目录，Native 返回受控图片计划并完成文字和图片投递，没有将 file_info 放入提示。旧模型两次、新模型一次均为合成 provider 请求，不计为外部模型。

旧固定18直接打开19明确拒绝。将原完整备份独立恢复至 `recovery-drills/f9bcfd91-35e0-43f1-8b85-1a593efc68ef`，匹配旧制品启动后 ready/schema18、管理员、稳定身份、主密钥及完整性均通过，再正常关闭。两项恢复演练于 17:33:30+08:00 完成，退出 0，未覆盖正式实例。

17:33:52+08:00，包内 `Start-Prometheus.ps1 -DataDir D:/DSH/.local/prometheus/instance` 前台启动新固定版本，Node PID34388。17:34:00+08:00，live/ready/schema19、前端 build.json 和数据锁同 SHA，21 个匿名管理 GET 与 2 个携带正确 Origin 的匿名媒体 POST 均 401；真实登录页无脚本错误，数据库完整性 ok。未读取用户密码或代登录，没有安装服务或自启动。

正式升级生成唯一且完整性 ok 的 schema18 快照。前后管理员 1，records/Run/消息/凭据均 0，配置与性格 revision1，稳定身份及主密钥一致；admins/config_revisions/personality_versions/records/secrets/source_grants 六表逐行哈希相同。六个新媒体表和旧接收表均为空，没有合成任务或授权写入正式实例。原 Rust/Breeze/旧管理页未提交差异与切换前逐字节一致。

私有证据包括 `.local/prometheus/release-validation/reports/7fe3e2d2c512003baf6780bb2c0f65d7af4ce87e.json`、`upgrade-evidence/7fe3e2d/{drill,production}.json`、正式 deployment.json、`qq-outgoing-{host,base64-counterexample}.json`、备份清单及本地/固定构建、回归、浏览器、恢复与生产复核日志。配置、凭据、实例数据及合成运行产物不提交 Git。

## 未验收范围

真实 QQ 平台上传/发送、客户端图片/视频/音频播放、真实模型效果未执行；合成 HTTP 回执不能当作真实平台通过。群聊/频道媒体、视频理解、TTS/ASR、任意外链媒体、自动图片生成发送、微信媒体与其他审计缺项仍保留；当前传输不是通用执行工作器或完整 M4/M5。正式实例原 QQ/微信关闭、模型未配置的状态保持不变，不移用旧系统凭据。
