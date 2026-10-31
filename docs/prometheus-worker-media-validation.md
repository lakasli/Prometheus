# Prometheus Windows 隔离工作器、语音与视频验收

2026-10-31，Asia/Shanghai；第二十四批。固定版本已部署，启动、健康、功能和完整恢复分别通过下列验收。本批不代表 TypeScript 全部迁移完成。

## 实现范围与授权

业务编排、API、来源许可、持久任务、制品、投递和管理页均位于 `apps/server` / `apps/admin` 的 TypeScript 工作区。原生 C++ 只封装 Windows AppContainer / Job API；固定 Python 媒体脚本只调用锁定的 Piper、sherpa-onnx 和 PyAV 库，不接收用户程序、脚本路径、命令或 URL。未引用、修改或部署用户已有的 `library/`、`source/`、Breeze 和 Rust 未提交改动。

新增独立默认关闭的 ASR、视频采样和 TTS 开关。QQ C2C 与微信本人音频实际下载/解密后，在本机转写，再把有摘要、期限、来源和明确不可信标记的资料交给 Native；微信上游 `voice.text` 不视为识别结果。MP4 本机解码最多60秒，每10秒一帧、最多6帧、每帧512单边；帧进入已配置的视觉模型，描述再进入 Native。只有 ASR 也启用时才处理音轨，未处理时明确记录，采样不冒称完整视频观察。

本地识别没有校准置信度或词时间戳，均保存为 `null`。结果不执行控制命令，不进入事实候选、自动摘要或日记；派生回复的来源失效后也不能从历史绕回模型。转写/描述正文不复制进提示快照。取消、更正、遗忘、账号 generation、来源 epoch、策略版本、期限和当前投递目标在发布前重新检查。

Native 可提出最多四项语音 JSON，每句最多300字；宿主实际合成成功才生成有任务证明的制品。QQ 使用 WAV，微信使用本机编码的 MP3，并继续走既有来源/目标/期限/分片与最终回执门禁。失败保留文字，不发送虚构音频。确认投递历史保留可读朗读文字。管理页在“渠道与媒体”提供显式开关及认证私有试听，试听不发送渠道消息；输入加密暂存，完成/取消/重启清除，制品一小时到期，可提前删除，跨账户和未认证下载被拒绝。

## Windows 隔离与生命周期

每项任务新建空 capabilities 的 AppContainer，低完整性 RID4096；Job 同时限制单进程、进程/Job 内存、CPU硬上限、UI、子进程创建与 kill-on-job-close。实际媒体 Job 为1536MiB、系统总 CPU 的20%、最多两个引擎线程、60秒；固定探针 Job 为64MiB。父进程只传递 Windows 必要目录环境变量和显式句柄，不传模型密钥、平台凭据、用户配置或任意环境变量。

每项任务仅取得自己只读的输入和固定运行库/权重副本，以及低完整性可写输出目录。私有工作目录为`%LOCALAPPDATA%/Prometheus/workers/<绝对数据目录哈希>`，在实例之外；启动验证私有NTFS ACL、路径长度与至少1GiB可用空间，不将数据库/凭据或源码授权给 AppContainer。完整 SHA-256 清单验证先于复制；拒绝链接、额外文件和越界路径。输入最多20MiB；stdout限制16KiB，输出目录监测并在回收时再次限制8MiB和固定文件名。目录额度是监测与事后校验，**不是 Windows 硬磁盘配额**，不能据此启用任意用户代码。

取消和停止通过 broker 终止 Job，等待真实进程消失、Job活动进程归零以及目录/profile清理后才释放物理槽。broker 异常死亡由内核 Job 句柄关闭杀子进程，宿主随后清理独占 UUID。重启只回收本实例且旧 PID 确认退出的工作目录；未知/活动进程阻断恢复，不杀未知 PID。排队/运行媒体试听中断，不在重启后重放。

原失败记录保留：历史4551/CodeIntegrity、`0xC0000142`不回写为成功。本次固定探针子程序改为 Windows 子系统并保留无控制台启动、原有所有隔离限制，避免 CRT 控制台初始化失败；修复探针 JSON 尾换行。实际 TS broker 曾因缺少 Windows 必要环境目录而返回203，补齐固定环境白名单后通过，没有继承业务密钥。loopback 返回10060时结合宿主可连接的真实监听端口和隔离端零接入验证，公网明确10013；没有添加 loopback exemption 或修改防火墙。文件复制采用 Windows 扩展路径格式，系统 `LongPathsEnabled=0`保持原状。

Piper 的 espeak-ng 使用 C 路径缓冲；首个固定 `6d502b7` 包在深层验收目录退出1，未部署。改用 broker 固定工作目录内的常量相对数据路径后，本地更深路径通过；固定 `f9dbec1` 的再加深测试又触及 Python 本身启动路径上限。因此临时执行副本最终改为上述短绝对目录，业务数据仍保留原位置；不改变系统开关、权限或隔离。原失败包和日志保留。另在未部署的 `6d502b7` 复现短历史窗口漏记转写派生来源；当前按实际保留的派生轮次继承来源集合，与上下文一起预算，再逐次记录。三次追问、摘要排除和撤销后实际loopback模型请求的回归保留原失败。

通过这些固定工作器检查不授予通用 Harness、ACP、用户 Python 或 Wasmtime Component 执行权限；其权限桥、broker、用户代码与完整威胁边界仍待实现，产品状态继续不可用。

依据：[Microsoft AppContainer](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)、[网络隔离诊断](https://techcommunity.microsoft.com/blog/coreinfrastructureandsecurityblog/diagnosing-network-isolation-issues/2511562)、[Job CPU限制](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_cpu_rate_control_information)。实测只证明本机和本批探针覆盖范围，不证明所有主机、Windows服务或长期运行。

## 依赖、模型与制品

`deploy/prometheus/media-runtime-lock.json` 固定下载 URL、字节数和 SHA-256；安装器只处理可信固定依赖，保留 Python/wheel/模型原许可与额外 SILK SDK 许可。常规 npm 构建不自动下载或启用本地模型；正式打包显式使用 `-IncludeMediaRuntime`，从同一 Git archive 重新构建原生 broker 并安装运行库。生产目录没有常驻媒体 worker。

本次为 Python3.13.17 Windows embeddable、Piper1.8.0、sherpa-onnx2.0.1、PyAV19.0.2、pysilk-mod1.6.4，以及原已选的 huayan medium 和 SenseVoice int8 2024-07-17。3.13.17是所选官方 Windows 二进制版本；3.13.18为源码安全发布，不把3.13.17称为最新安全源码。huayan 数据集许可仍是 Unknown，仅本机个人评估，不推断商业/再分发权利。Piper GPL、SenseVoice FunASR 模型条款、FFmpeg与各运行库许可分别保留，不统一冒称 MIT。

来源：[Python3.13.17](https://www.python.org/downloads/release/python-31317/)、[Python3.13.18](https://www.python.org/downloads/release/python-31318/)、[Piper Python API](https://github.com/OHF-Voice/piper1-gpl/blob/v1.8.0/docs/API_PYTHON.md)、[SenseVoice模型](https://k2-fsa.github.io/sherpa/onnx/sense-voice/pretrained.html)、[FunASR模型许可](https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE)、[SILK SDK许可来源](https://github.com/DCZYewen/Python-Silk-Module/blob/27a0eecafaf880329b9242668c6310d59b74cbbc/src/silk/interface/SKP_Silk_SDK_API.h)。没有保留上游参考检出或 `.git` 对象。

## 验收结果

最终应用代码为已推送的 **`f10885baf6973d803057c05e564417885aa24f94`**（schema21）。`6d502b7`、`ef17749`、`f9dbec1` 的未发布尝试保留在忽略的发布目录，未作为正式可用版本。上一可用 Windows 版本为 **`9b05f043f9a78d4084262e42722473cf4d646fa4`**（schema20）。

本机 Windows11 Pro 10.0.26200/x64，Core Ultra7 155H、22逻辑CPU、31.61GiB内存；21:27:36+08:00预检可用内存约8.1GiB，D盘约377GiB，工作缓存C盘约360GiB。Node24.22.1、npm12.2.1、MSVC14.44.35207、SDK10.0.26100.0；旧Rust/Cargo1.88.0只查询版本，不参与运行构建。前后端从同一 Git archive 构建，PowerShell原生打包和依赖安装退出0。ZIP SHA-256：`05cee4acb4a244353e9c5b852db6352fd7ed51c7bdca3929bd661505623a73a4`。

| 检查                   | 实际结果与范围                                                                                                                                                                                                                                                         |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 固定前后端与逐文件哈希 | 同一完整应用SHA；构建、打包及18项制品检查通过，退出0。                                                                                                                                                                                                                 |
| 后端                   | 固定runtime服务夹具383/383，失败0，462.35秒。19项本批媒体专项及2项微信本地理解/MP3协议场景包含在383内，不另加总。安全/签名/严格JSON及部分直接构造辅助仍来自本地相同源码，不冒称全部辅助均加载固定包。                                                                  |
| 浏览器                 | 固定静态包30/30，覆盖ASR/视频开关、TTS明确保存、私有WAV加载/删除和320px布局；90秒以内，退出0。另在实际升级副本通过Chromium加载真实Piper WAV元数据。                                                                                                                    |
| 固定基础探针           | 7/7：私有配置/真实编译源码/父进程/环境/子进程/网络拒绝，超时、输出洪泛、非零退出、取消、饱和CPU和broker死亡清理。所有正常finished回执活动进程0，broker死亡后独立核验PID消失。                                                                                          |
| 实际媒体工作器         | 10/10：1536MiB真实Job的小分配成功/2GiB分配失败、模型/输入只读和他任务不可读；Piper WAV/MP3；SenseVoice WAV/SILK；41秒MP4有/无音轨转写；实际取消/broker死亡/停止及目录回收。两种视频路径均独立读PNG像素，核对0/10/20/30/40秒红绿交替，非重复首帧。                      |
| 完整实例恢复           | 22文件完整备份，逐文件哈希/ACL、旧20实际生成图像/消息/目标后升级21，身份、原消息、图像字节/摘要、管理员、主密钥、目标和微信generation保留；旧图可用于新Native媒体回复，export21、integrity_check=ok。旧20拒绝打开21，另一完整备份副本用旧20实际ready，配置与身份保留。 |
| 副本真实本机引擎与协议 | QQ/微信各音频与视频，共4条实际本机理解→loopback Vision/Native→实际TTS→Outbox；QQ WAV，微信MP3实际AES/CDN上传/解密比对，固定上下文且CDN无QQ/微信凭据。4条链共8项真实引擎任务，另1项私有试听。均不是外部平台证明。                                                       |
| 正式实例               | 21:49:22.700+08:00前台ready，PID28640；live/ready/admin build为同一SHA。21:50:06后核对23项匿名读取与3项匿名写入拒绝、登录页无重新初始化入口、浏览器错误0、integrity_check=ok；六张核心表哈希、稳定身份、配置/性格版本及主密钥不变。                                    |

固定媒体样本中，Piper WAV/MP3的总处理时间约11.25/11.59秒，ASR WAV/SILK约8.91/9.87秒，视频不转写/转写音轨约5.95/9.18秒，均含私有副本和清理；实际Job峰值约1030.52MiB，低于1536MiB限额。早期冷启动ASR曾约30秒，不能把暖缓存样本当作长期性能或所有格式保证。基础CPU探针实测累计CPU并检查短窗口容差，配置与内核查询均为2000/10000，不能把简单逻辑核折算说成精确20.00%全机占用。

21:43:54+08:00核对旧SHA、PID和命令行后发送Ctrl+C，输出`Prometheus stopped.`，PID及数据锁释放；中断助手退出0，旧前台PTY会话因Ctrl+C退出1，未将其记成正常命令退出0。随后完整备份为`.local/prometheus/backups/instance-2026-10-31T13-43-54-276Z-79e5aa27-1925-488c-a27e-e70d064fbbd2`，22文件、schema20、integrity_check=ok。第二次完整恢复演练21:48:15.811+08:00完成，使用独立目录，没有覆盖正式数据。

首次恢复演练已跑通实际媒体与试听，删除试听的验证脚本漏带JSON Content-Type被正确拒绝415；补齐后从同一完整备份重新恢复两个新目录并完整重跑，原失败保留。首轮本地380项中2项旧断言仍把SILK/旧语音URL当作未实现，另一次通用制品脚本也使用旧URL；更新有效负例及关闭状态的新接口请求后分别复测，最终固定383和18项均通过，不将初轮失败计为通过。

正式实例没有配置真实模型/渠道凭据，ASR/视频/TTS开关保持原默认关闭；新增理解、媒体任务与私有音频表均空，没有注入合成会话或更换身份。新进程仍在PowerShell前台运行，未安装服务或自启动。生产目录与工作缓存再次验证私有ACL；缓存任务目录/请求目录无残留活动任务。Git仅提交本批TypeScript、固定工作器/部署验证及文档；原Rust/Breeze等差异与初始二进制patch完全一致。

私有证据（均在忽略目录，报告不输出秘密）：

- `worker-media-host-inventory.json`、`worker-media-package-short-final.log`、`worker-media-artifact-f10885b.log`。
- `worker-media-backend-f10885b.log`、`worker-media-browser-f10885b.log`；构建/测试/打包/两类worker验证/成功恢复演练/生产验证退出0，运行中的新前台进程不伪造退出状态。
- `worker-isolation-f10885b.log`及`worker-validation/<SHA>/.../report.json`；`media-worker-f10885b.log`及`media-worker-validation/<SHA>/Windows-long-path-compatibility-proof/.../report.json`。
- `worker-media-lineage-counterexample.log`保留未发布旧版来源断链，`worker-media-lineage-regression.log`为修复后19项；`worker-media-artifact-fixed.log`、`media-worker-fixed.log`、`media-worker-f9dbec1.log`和初次恢复日志保留失败。
- `backup-worker-media-release.json`、`worker-media-upgrade-drill-2.log`、`upgrade-evidence/f10885b/drill.json`与`production.json`；`pre/post-worker-media-release.json`和`worker-media-core-before/after.json`。

## 复验命令与证据

```powershell
pwsh -File deploy/prometheus/package-windows.ps1 -Commit <完整应用SHA> -PreviousVerifiedCommit 9b05f043f9a78d4084262e42722473cf4d646fa4 -IncludeMediaRuntime
node deploy/prometheus/verify-release.mjs <runtime绝对目录>
node deploy/prometheus/verify-worker.mjs <runtime绝对目录>
node deploy/prometheus/verify-media-worker.mjs <runtime绝对目录> <公开WAV夹具> <合成SILK夹具> <合成41秒MP4夹具>
$env:PROMETHEUS_RELEASE_ROOT = '<runtime绝对目录>'
node --test --test-concurrency=1 apps/server/test/*.test.mjs
npm.cmd run test:e2e --workspace @prometheus/admin
Remove-Item Env:PROMETHEUS_RELEASE_ROOT
```

媒体验收参数仅使用公开/合成文件，不指向用户媒体。样本识别文字有误差，不作为准确率评测；loopback Vision/Native 与真实本机 ASR/TTS/解码分开记账。真实 QQ / 微信账号、手机/客户端、外部 Vision / Native、麦克风与设备播放、通宵和多主机压力测试未执行。完整迁移仍缺的项目继续见[完整性核对](prometheus-completeness-audit.md)。
