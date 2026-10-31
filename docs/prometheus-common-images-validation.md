# Prometheus 常用图片与像素规范化迁移

2026-10-31，Asia/Shanghai，第二十批。继续对照旧固定Rust `image_analysis.rs/canonicalize`。本批迁移受限PNG、JPEG、GIF、WebP和BMP解码、方向和静态首帧、实际像素重编码及原大小边界；图片来源与用途限制沿用[第十九批](prometheus-vision-provenance-validation.md)。渠道附件、视频/语音与通用执行仍未完成，完整目标见[核对清单](prometheus-completeness-audit.md)。

## 输入与输出

原始字节最多8MiB，单边最多12000、总计2400万像素。入口只接受规范base64并复核往返编码，不接收data URL、路径、远程URL、SVG、TIFF或库支持的其他格式。只放大认证`POST /media/assets`的JSON额度至12MiB，配置/凭据等原额度和认证保持不变；生成服务的b64响应同样有界并走同一验证。

PNG逐块验证CRC、ASCII名称/保留位、头/调色板/透明度/连续IDAT/结尾顺序、4096块上限与完整zlib输入，继续拒绝APNG。支持合法灰度/灰度alpha/RGB/RGBA/索引色、1/2/4/8/16位及Adam7；文本、压缩文本、ICC等元数据先剥离，不解析或展开其内部内容。逐行流式核对实际扫描线、滤波器和解压终点；索引色先还原Sub/Up/Average/Paeth滤波，再检查每个真实像素的调色板范围。解码库容忍的越界索引不会变成静默黑色，流式扫描不保留整个16位原始栅格。

JPEG应用全部八种EXIF方向；GIF/WebP仅取第一静态帧。BMP支持OS2 12字节及Windows40/52/56/108/124字节头、1/4/8位调色板、16/24/32位RGB/位域和RLE4/RLE8，明确处理上下行顺序、像素偏移/填充、透明掩码。未知头、无效掩码、越界调色板、行越界与截断压缩流拒绝，不执行嵌入的JPEG/PNG或调用外部程序尝试未知BMP。

实际解码并按宽高比缩小后，保存无附加元数据的8位RGBA PNG，单边不超过2048、像素不超过2097152、文件不超过2MiB。文件仍过大时最多八次按3/4继续缩小；压缩字节的简单转封装不作为像素规范化。视觉请求允许承载完整规范PNG的有界base64，后续Native只收到原来源机制下的描述资料。已受理旧4MP PNG按原摘要读取，不在GET时重写；新上传执行新规范化，旧已消费来源和准确重试仍遵循原生命周期。

## 并发与生命周期

每个运行实例同时只受理一个解码任务，第二个明确429，不建立无限队列。原20份临时BLOB配额在处理前和保存前复核，生成成果为识图新增副本也受同一配额约束。断开连接或关机不发布孤儿成果，实际处理结束才清理占用；退出等待该任务结束后关闭数据库。生图模型槽保持到解码完成，处理中配置/策略撤销再次阻止发布。生成成果的独立下载期限不因识图副本清除而缩短。

依赖固定`sharp@0.35.5`，以锁文件和`npm ci --ignore-scripts`部署。Windows x64原生库首次用于实际图片时才加载；普通启动无需加载。只使用Buffer和本模块明确的格式白名单，禁用缓存、限制内部并发，单次实际处理设置3秒超时；逻辑取消不当作原生处理已经退出。API依据见官方[安装](https://sharp.pixelplumbing.com/install/)、[输入限制](https://sharp.pixelplumbing.com/api-constructor/)和[处理超时](https://sharp.pixelplumbing.com/api-operation/#timeout)。运行包保留上游README、LICENSE及版本信息。

这是宿主可信、有界的图片输入解析，不提供用户代码执行或通用worker权限。输入/像素/输出、并发和请求完成已实测；采样内存不是操作系统硬限额，也不证明库内每一次分配都受到128MiB硬上限。原通用媒体、语音、Harness和Component的Windows隔离条件继续独立验收。

## 数据版本与检查

schema17标识新增的规范图字节范围，旧16拒绝打开17，避免旧1MiB读取器误用新的2MiB制品。升级前自动保存原schema数据库快照，正式发布另备份含主密钥的完整实例并分别演练新旧恢复。旧有效聊天/来源、配置、凭据、日记和图像摘要不由新默认覆盖。另修复schema16重启会把已完成日记修复游标归零的问题；只有早于15的库需要该次补录回扫，15及以后升级和重启保留真实检查点。

新增22项后端场景，覆盖独立PNG读取器的像素核对、合法PNG色型/位深/交错、滤波后的索引、八种真实JPEG角点方向、GIF/WebP首帧、BMP类别和偏移、大图/编码上限、准确8MiB、非法格式/编码/像素、实际在途断连/关机、生成撤权/独立成果、真实Vision大请求、鉴权与额度隔离、24MP/16位资源样本、并发配额竞争，以及冻结schema16的快照/游标/重启。原媒体与来源31项继续通过。新增浏览器场景核对管理页限制和认证方向JPEG转换，不恢复已撤销的Web对话入口。

本地53项相关后端、26项浏览器与完整构建、格式及暂存差异检查通过。合成provider不作为外部视觉模型或真实平台验收。

## 固定Windows结果

正式应用 **`fd7dd5323428ba86899f7c7d82db761664943c62`** 已提交、推送并核实origin tip，上一可用Windows版本 **`31f407cfbe043c03df6c8c56c6c4be19dc43f7c3`**，数据16→17。后续证据文档提交不改变运行SHA。

14:35:45+08:00核验Windows11专业版10.0.26200/x64、Core Ultra7 155H/22逻辑处理器、31.61GiB内存/约11.91GiB可用、D盘约398.38GiB可用，Node24.22.1、npm12.2.1、PowerShell7.6.5。sharp原生图片转换已在同一Windows主机实际运行；这不是Rust、通用worker或进程隔离通过。

| 检查       | 实际结果                                                                                                                                                                                                                                                                                                      |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 固定打包   | `pwsh -NoProfile -File deploy/prometheus/package-windows.ps1 -Commit fd7dd5323428ba86899f7c7d82db761664943c62 -PreviousVerifiedCommit 31f407cfbe043c03df6c8c56c6c4be19dc43f7c3`，已推送Git archive前后端同SHA构建，退出0；ZIP SHA256 `0caec6f1df80c142188af27699f57335982fa9115d95eb7c2286ea976f1cde2e`。     |
| 固定制品   | `node deploy/prometheus/verify-release.mjs <本SHA/runtime>`：18项通过，退出0，包含依赖文件哈希校验。                                                                                                                                                                                                          |
| 固定后端   | `PROMETHEUS_RELEASE_ROOT`绑定本SHA/runtime，`node --test --test-concurrency=1 apps/server/test/*.test.mjs`：290通过/0失败/0跳过，218.17秒，退出0。服务夹具装载固定包，合成编码器/独立PNG读取器、安全/ACL、严格JSON及部分直接辅助来自本地；43份JS/schema中42份字节相同，另1份仅CRLF/LF差异，构建身份单独校验。 |
| 固定浏览器 | 同环境下`npm run test:e2e --workspace @prometheus/admin`：26通过/0失败，52.3秒，退出0，使用同SHA页面与后端。320px截图在导航收起后查看，限制说明与实际方向JPEG转换通过；12项关系图纯函数沿用30c001f实际重跑证据，本批未重跑。                                                                                  |
| 正式启动   | 包内`Start-Prometheus.ps1 -DataDir D:/DSH/.local/prometheus/instance`逐文件校验后，于14:40:55+08:00前台启动，Node PID28484，入口`http://127.0.0.1:8419/admin/`。                                                                                                                                              |
| 正式复核   | 14:41:11+08:00 live/ready/schema17、build.json和数据锁同SHA；25个匿名管理GET、3个正确Origin匿名POST均401。实际登录页正常且无脚本错误，完整性ok，未读取用户密码或代登录。                                                                                                                                      |

固定包的24MP/16位PNG样本在14:39:06+08:00执行：1034999字节源、192004000字节解压扫描线，输出1773×1182、44430字节规范PNG，整个认证上传/独立像素读取约897.67毫秒。10毫秒间隔采样及终点观测的RSS基线202395648、峰值265916416、增量63520768字节（约60.58MiB）。这是单样本和当前主机的实测，不能推断所有图片的峰值，也不能作为操作系统或库分配器硬限额证明。

14:40:19+08:00核验旧SHA、PID28424完整命令行、锁及8419端口，无在途请求/作业/投递；14:40:31再次检查旧健康版本。Ctrl+C打印Prometheus stopped，Node、锁和监听均释放，外层PowerShell退出1。14:40:44+08:00完整备份18文件至 `.local/prometheus/backups/instance-2026-10-31T06-40-44-089Z-a513fa87-75ae-45a6-a004-e688c14f894f`，含数据库、主密钥、原schema快照和部署元数据，integrity_check为ok，退出0。

逐文件哈希、路径边界、符号链接与私有ACL核验后，向 `recovery-drills/854e7f81-6f7d-4f0d-b32c-d2ec9b2fba7c`恢复这份正式备份。只在副本加入合成管理员并登录，用旧固定31f407c分别提交JPEG/GIF/WebP/BMP/索引PNG，均400；旧版合法4MP PNG保存、读取，另一小图完成真实loopback Vision→Native。保留原管理员行、完整旧消息、有效来源、图像字节/摘要及合成凭据，明确写入日记修复游标777后正常关闭。

新固定包打开同一副本，自动产生完整性ok的schema16快照，游标仍777。原管理员/身份/主密钥、合成凭据解密、旧聊天、4MP未消费图的原字节与摘要、已完成的有效视觉描述均一致；普通追问继续携带旧可信来源，不将已知有效来源误当作旧版无证据资料清理。五类旧版拒绝的输入均通过新的真实认证接口规范化并由独立PNG读取器校验尺寸、颜色与摘要，随后各完成Vision→Native、user角色来源与清图；JPEG方向、GIF/WebP首帧和索引透明度有实际像素结果。旧4MP图也保留原摘要完成一次识图。生成服务实际返回JPEG，经同一规范化后可下载；识图副本结束后原生成成果仍可下载。旧版2次、新版16次合成provider请求分别记账。

旧固定31f407c直接打开17明确拒绝。另将未加入合成内容的原完整备份恢复至 `recovery-drills/7453f0a3-1a39-4800-aeba-43dbb5574baa`，匹配旧版在18469 ready/schema16、管理员/稳定身份/主密钥及完整性通过后关闭。两项正式恢复演练14:40:47+08:00完成，退出0，没有覆盖正式实例。

正式升级也产生唯一schema16快照，快照完整性ok。升级前后管理员1，records/Run/消息/凭据0，配置/性格revision1，稳定身份与主密钥哈希一致；admins/config_revisions/personality_versions/records/secrets/source_grants六表逐行内容哈希相同。模型未配置，QQ/微信关闭，未将任何合成任务或许可写入正式实例。14:41:12+08:00原生icacls核实instance、master.key、完整备份与两个恢复目录仅当前账户、SYSTEM和Administrators；旧WSL `df6429cbceb8f16504efe9464c1abbbdb4a67e29` 在18439仍ready，仅作旧实现对照。

私有证据为 `.local/prometheus/release-validation/reports/fd7dd5323428ba86899f7c7d82db761664943c62.json`、`.local/prometheus/upgrade-evidence/fd7dd53/{drill,production,image-resource}.json`、正式deployment.json，以及开发/固定回归/浏览器/备份日志。运行包包含上游依赖的许可与版本文件，真实配置、秘密和实例数据不提交。
