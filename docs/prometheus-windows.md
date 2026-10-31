# Prometheus Windows 开发与发布

新项目采用 TypeScript / Node.js，与历史 Rust 流程分开。AGENTS.md 的用户修改保护、固定 SHA、Windows 验收、数据分离与禁止降低安全策略等约束继续适用。

## 本地开发

在仓库根目录逐条执行：

```powershell
npm.cmd ci --ignore-scripts
npm.cmd run build
npm.cmd test
npm.cmd run test:e2e --workspace @prometheus/admin
npm.cmd run bootstrap -- --data-dir .local/prometheus/dev
npm.cmd start
```

要求 Node.js 24.7–24.x；正式打包使用 PowerShell7。浏览器测试需要已安装的 Playwright Chromium。缺少浏览器时按其官方流程安装；执行被阻止时保留诊断，不关闭系统安全策略。

管理入口为 `http://127.0.0.1:8419/admin/`。初始化命令只显示私有文件路径，凭证15分钟有效；无默认密码。开发数据为 `.local/prometheus/dev`，不导入旧 dsh.db。ACL 使用原生 icacls / SDDL 检查当前目录及文件，只允许运行账户、SYSTEM和管理员。Windows PowerShell5的Get-Acl模块在本机无法自动加载，因此程序不依赖该模块；未变更安全策略。

初始化页显示当前运行实例的凭证文件、到期时间和带准确`--data-dir`的生成命令，未初始化时仅返回这些必要指引，不返回token。已建立账户后不再返回这些路径。开发dev和正式instance的凭证不能互用；只粘贴JSON的token字段，创建账户后用用户名/密码登录。

## 固定版本发布

先提交、推送并核实完整40位SHA，再执行：

```powershell
pwsh -File deploy/prometheus/package-windows.ps1 -Commit <40位SHA>
# 后续升级记录确已验收的上一版制品
pwsh -File deploy/prometheus/package-windows.ps1 -Commit <新SHA> -PreviousVerifiedCommit <上一可用SHA>
```

脚本验证远端分支tip，从 Git archive 构建，前后端写入同一SHA；输出 `.local/prometheus/releases/<SHA>/source`、`runtime`、ZIP及逐文件SHA-256。已有发布目录不会覆盖。首次 previous_verified_windows_version 为null；后续切换须单独记录当前已验证版本，不能把首次状态沿用为回退证明。

固定版本验收：

```powershell
node deploy/prometheus/verify-release.mjs <runtime绝对路径>
$env:PROMETHEUS_RELEASE_ROOT = '<runtime绝对路径>'
npm.cmd run test:e2e --workspace @prometheus/admin
Remove-Item Env:PROMETHEUS_RELEASE_ROOT
```

验收使用隔离数据与合成loopback模型，验证制品哈希、SHA、启动、live / ready、认证、配置、资料及文字链路，不作为外部模型或QQ / 微信实测。结果写入 `.local/prometheus/release-validation/reports/<SHA>.json`。

## 前台启动与回退

使用包内脚本和制品目录之外的绝对数据路径：

```powershell
pwsh -File <runtime路径>/Start-Prometheus.ps1 -DataDir D:/DSH/.local/prometheus/instance -BootstrapToken
pwsh -File <runtime路径>/Start-Prometheus.ps1 -DataDir D:/DSH/.local/prometheus/instance
```

脚本验证全部制品哈希后前台启动。按Ctrl+C正常退出。不安装服务、自启动或计划任务，不自动连接旧主机，不读取旧token。跨设备访问须有受保护HTTPS反向代理、精确Origin与Secure cookie。

每个数据目录使用原子目录锁。崩溃后的 `recover-lock --data-dir <目录>` 仅在原进程确认退出后清理锁，不杀死未知进程。

旧项目备份：`node deploy/prometheus/backup-legacy.mjs`。数据库使用SQLite backup API并核对integrity_check；Git bundle和差异可在另一个目录恢复，现有文件不删除。第三方运行库和原始素材保留原位。

Prometheus日常备份需在正常退出后复制完整数据目录，包含数据库、secrets和master.key；仅数据库无法解密凭据。恢复前核验目录与ACL，不覆盖现有实例。切换旧制品须独立验证schema兼容性、配置及数据恢复路径；代码回退不等于数据恢复。

先正常退出运行版本，核实PID、端口和数据锁均已释放，再执行完整实例备份：

```powershell
node deploy/prometheus/backup-instance.mjs --data-dir D:/DSH/.local/prometheus/instance --source-commit <正在替换的完整SHA>
```

脚本取得与服务相同的原子数据锁，复制主密钥等全部文件、生成SQLite一致性快照并检查integrity_check，保存私有清单与逐文件哈希。拒绝符号链接及数据目录内的目标；已有锁时失败，不杀进程。备份位于.local/prometheus/backups/instance-*，不输出秘密。

当前实现schema21不能由schema1/2/3/4/5/6/7/8/9/10/11/12/13/14/15/16/17/18/19/20制品打开，实际部署SHA见对应验收记录。升级前自动创建原schema数据库快照，正式升级另备份完整实例；回退先保留新目录，向另一个私有目录恢复匹配旧制品的完整快照（含主密钥），再用上一版固定SHA启动和复验。不能覆盖运行数据或仅切换代码。

## 隔离工作器前置检查

Harness、用户Python和Component执行仍禁用。固定媒体工作器独立验收并明确启用；不会因此开放用户代码。可在安装MSVC Build Tools/Windows SDK的本机执行`npm.cmd run build:worker`构建空权限探针；打包加`-IncludeWorkerProbe`从同一Git SHA重新编译并记录哈希，`-IncludeMediaRuntime`另装锁定的本地媒体运行库和权重。正常TypeScript构建不下载模型、不自动启用ASR/视频/TTS。

`node deploy/prometheus/verify-worker.mjs <runtime绝对目录>`核对探针哈希与七项权限/资源/生命周期，阻塞退出2，不计为通过。媒体运行库另执行下列十项真实引擎及取消/停止/broker死亡检查；参数只用公开/合成夹具，不上传用户媒体。历史4551/CodeIntegrity及0xC0000142保留，当前结果、许可、长路径修复与完整恢复见[工作器媒体验收](prometheus-worker-media-validation.md)，不改变系统策略。

```powershell
pwsh -File deploy/prometheus/package-windows.ps1 -Commit <已推送完整SHA> -PreviousVerifiedCommit <上一可用SHA> -IncludeMediaRuntime
node deploy/prometheus/verify-worker.mjs <runtime绝对目录>
node deploy/prometheus/verify-media-worker.mjs <runtime绝对目录> <公开WAV> <合成SILK> <合成41秒MP4>
```

每任务新建只读运行库与输入、独占可写输出，AppContainer零capabilities；Job为1536MiB/系统CPU20%/单进程/60秒，本地并发一项。临时目录为`%LOCALAPPDATA%/Prometheus/workers/<数据目录哈希>`，验证私有NTFS ACL、短路径与至少1GiB空闲空间；避免Python和原生codec随数据目录加深而无法启动。只清理已确认退出的本实例UUID，不在制品或备份中保存任务输入。运行库、模型和全部许可随私有包交付；huayan数据集许可Unknown，仅本机个人评估。ASR转写和采样画面将进入已配置的远程模型，需在管理页分别明确开启；音轨仅随ASR开启处理。试听为认证私有制品，不等于平台外发或设备播放验收。

针对本批逻辑的固定制品回归可设置`PROMETHEUS_RELEASE_ROOT`后执行`node --test apps/server/test/migration-logic.test.mjs`；测试数据仍放在工作区忽略目录，不写入制品。

日志筛选和来源即时失效的HTTP回归可同样执行`node --test apps/server/test/journal.test.mjs`。其中schema升级的直接Store夹具检查本地构建，HTTP夹具绑定固定runtime；两者在报告中分别说明。性格历史恢复仍使用当前If-Match另存新版本，不覆盖历史版本或稳定身份。

配置草稿与异步操作回归：`node --test apps/server/test/configuration.test.mjs`，同样支持PROMETHEUS_RELEASE_ROOT绑定服务夹具。草稿保存、字段校验、连接检查与明确应用互相独立；管理页“模型与运行”中展开“配置草稿与操作记录”。QQ检查仅获取凭据，不证明真实平台消息可发送。直接Store的schema3夹具使用本地构建，固定制品升级另做完整实例恢复演练。

人物账本回归：`node --test apps/server/test/affinity.test.mjs`。HTTP和进程内服务夹具可绑定固定runtime；测试中的独立schema4数据库定义和密码/ACL辅助函数为本地夹具，固定包真实升级仍单独演练。升级不以新默认覆盖已有规则和分值；新合并保留原档案及独立账本，早期删除式合并不猜测恢复。

日程系列/授权阅读回归：`node --test apps/server/test/schedule.test.mjs`。HTTP和服务夹具绑定PROMETHEUS_RELEASE_ROOT；Temporal/JSON纯函数与安全辅助函数来自本地dist，分别记账。管理页在日程中提供系列与活动、授权文本与工作目标；拖动只预演，保存与批准分开。新授予模型权限并不启用尚未验证的执行或渠道外发。详见[日程迁移验收](prometheus-schedule-validation.md)。

日记周期与覆盖回归：`node --test apps/server/test/diary.test.mjs`。HTTP/进程内服务夹具可绑定固定runtime；Temporal/严格JSON纯函数、安全辅助和少量直接方法来自本地构建。新管理页的周期账本与旧选源模型草稿分别显示；仅获模型许可的正文进入普通Native检索。schema6升级前完整备份，旧制品不能直接打开7。详见[日记迁移验收](prometheus-diary-validation.md)。

人物来源与身份回归：`node --test apps/server/test/profiles.test.mjs`，HTTP和进程内服务夹具支持绑定固定runtime，安全/ACL/密码散列辅助来自本地构建；独立schema7 SQL夹具升级仍通过所绑定的实际服务。字段/有向边审核、来源失效、私聊身份与跨账号关联不授予工具或主动发送权限。schema7升级前完整备份，旧7制品拒绝打开8；详见[人物来源迁移记录](prometheus-profiles-validation.md)。

显式日记人物候选回归：`node --test apps/server/test/profile-model.test.mjs`，同样支持固定runtime服务夹具，安全/ACL/密码散列及构造场景辅助为本地。schema8 SQL夹具通过实际服务升级，正式发布另演练完整备份恢复。排队引用、模型完成、保存待审与人工批准分别验证，取消不提前归还物理请求槽或凭据引用。schema8升级前完整备份，旧8制品拒绝打开9；见[人物候选迁移记录](prometheus-profile-model-validation.md)。

互动来源与账本重放回归：`node --test apps/server/test/affinity-sources.test.mjs`，服务夹具支持固定runtime，安全/ACL辅助和合成SQL场景构造来自本地。schema9旧分数作为已知检查点，新增事件按实际来源、原策略和停机覆盖重算；旧9制品拒绝打开10，完整实例升级/恢复独立演练。见[互动来源与重放记录](prometheus-affinity-sources-validation.md)。

人物映射与关系图：`node --test apps/server/test/profile-mappings.test.mjs` 支持固定runtime服务夹具，源域/身份的合成SQL及安全辅助为本地；`npm run test --workspace @prometheus/admin`执行12项本地图布局/物理纯函数，浏览器仍用固定包执行test:e2e，分别记账。本批保持schema10，无数据库迁移；正式切换仍备份完整实例并在独立目录验证新旧制品恢复。见[映射与关系图记录](prometheus-profile-network-validation.md)。

原生文字安排回归：`node --test apps/server/test/daily-life.test.mjs apps/server/test/channels.test.mjs`。HTTP及进程内服务夹具支持固定runtime；严格JSON纯函数、安全/ACL和合成场景构造来自本地dist。未来执行使用明确的夹具时钟，实际短窗口等待与生产至少7分钟缓冲分开验证，不提供HTTP时钟入口。schema10升级至11前完整备份，旧10制品拒绝打开11；完整实例恢复须另起私有目录。来源授权、已准备、实际保存和渠道投递分别记账，见[文字安排迁移记录](prometheus-daily-text-validation.md)。

提前一分钟选择回归：`node --test apps/server/test/schedule-notices.test.mjs`。HTTP/进程内服务夹具支持固定runtime，选择JSON纯函数、安全辅助、合成SQL及少量内部构造来自本地。真实民用分钟唤醒及真实越过原定时间的第二次延期独立计时，多日与未来成果夹具不算长期运行证明。schema11升级至12保留原wake_at上限，旧制品拒绝打开12；正式升级前备份完整实例并另起目录验证恢复。日程页明确开启选项，不自动启用模型许可，见[提前选择迁移记录](prometheus-schedule-notices-validation.md)。

明确记录入账回归：`node --test apps/server/test/diary-records.test.mjs`，17个HTTP/进程内场景支持固定runtime，安全/ACL和合成SQL构造为本地。schema14冻结SQL保留升级前结构；旧14实际成功模型检查点另用新旧固定制品兼容演练，不以当前结构重建旧库代替。schema15保留原字段上限、旧快照和源数据，详见[明确记录迁移记录](prometheus-record-import-validation.md)。

PNG静态边界回归：`node --test apps/server/test/media-context.test.mjs`，8项HTTP/进程内场景支持固定runtime，校验构造/合成字节为本地。十类非法PNG拒绝、合法透明度及旧制品复核分别测试，保持schema15；正式切换仍完整备份并分别恢复新旧制品。仅结构与现有PNG链通过，其他格式、像素规范化及识图来源/清理边界见[本批记录](prometheus-png-validation.md)。

识图来源与临时输入回归：`node --test apps/server/test/vision-provenance.test.mjs`，23个场景的服务夹具支持固定runtime；安全/ACL及合成数据构造为本地。冻结schema15 SQL检查迁移，实际旧制品产生图像/日记后再升级另行验收。schema16的描述资料角色、实际引用、用途限制和临时字节生命周期见[来源记录](prometheus-vision-provenance-validation.md)。普通上传首次用于识图后不再提供下载，相同客户端标识仍可幂等取得原Run；明确生成成果保留独立期限。

常用图片回归：`node --test apps/server/test/image-codec.test.mjs`，22项HTTP/实际解码场景支持固定runtime服务，合成编码器/独立PNG读取器、安全和资源采样为本地辅助。schema16冻结SQL与真实旧制品升级分开验证；17标识2MiB规范图范围，旧16拒绝打开。输入解码不赋予通用worker权限，见[常用图片迁移记录](prometheus-common-images-validation.md)。

QQ单聊收图回归：`node --test apps/server/test/qq-incoming-media.test.mjs`，20项实际HTTP/解码/恢复场景支持固定runtime；安全/ACL、签名、合成图片和旧schema SQL为本地夹具。schema17冻结SQL与真实旧17制品升级分别验证，18保留旧附件原文并排除未核实来源及其递归派生记录。正式QQ接收仍需已有渠道配置及明确启用视觉模型，默认状态不改变；来源图片仅为有界规范化副本，不等于原附件或外发broker。固定Windows探针本批仍初始化失败，未启用语音或执行功能，见[QQ收图迁移记录](prometheus-qq-images-validation.md)。

QQ媒体外发回归：`node --test apps/server/test/qq-outgoing-media.test.mjs`，20项场景支持固定runtime服务夹具；安全/签名、直接DNS wire和合成编码/旧MP4样本为本地辅助。schema18冻结SQL保留；正式发布另由真实旧18制品生成来源图，在完整副本升级后验证图片/6.3MB填充MP4/WAV发送、新Native实际目录及旧制品恢复。管理页在“渠道与媒体”中先暂存文件，再核对固定接收方并确认排队；只追加到有效C2C被动回复，最多四项。不会启用语音合成或通用工作器，真实QQ/客户端仍须另验，见[QQ媒体验收](prometheus-qq-media-validation.md)。
