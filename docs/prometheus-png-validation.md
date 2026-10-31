# Prometheus PNG静态边界复核

2026-10-31，Asia/Shanghai，第十八批。对照旧固定Rust `image_analysis.rs/strict_chunks`，本批补回已声明的静态PNG校验边界；不是常用图片完整迁移。

固定30c001f版本的旧PNG路径在有合法CRC的acTL/fcTL存在时仍可返回201，随后把动画控制块剥离。这与原契约明确拒绝APNG不一致；保留首帧的GIF/WebP目标不能替代APNG拒绝规则。专项回归先在此条件失败，修复后通过。

上传和生成结果现在拒绝acTL/fcTL/fdAT、保留位不合法的块名、超过4096块、重复/长度错误/像素之后的PLTE、RGBA上的tRNS及RGB非法透明样本。zlib必须实际消费整个压缩输入，尾随数据不能藏在IDAT内。原有签名、长度、CRC、IHDR/连续IDAT/IEND顺序、像素与解压预算继续检查。合法RGB tRNS和RGBA建议调色板保留，文本等元数据仍剥离。

读取已有制品或用于对话前重新校验PNG与摘要；非法或摘要不符时拒绝使用，数据库记录保留。之前已经剥离的原始控制块无法从规范静态图反推，不声称升级能还原或判断已丢弃的原始格式。

新增3个后端场景：十类非法输入经真实认证API拒绝且不存储；合法透明度/调色板和元数据剥离；旧制品结构/摘要复核且不创建Run。额外复现Node的ASCII解码会掩去块名高位，改为逐字节对应的Latin1后再校验ASCII字母，拒绝伪装成合法名称的非ASCII字节。原视觉→Native来源链、生成结果和其他媒体/上下文场景继续回归。schema仍为15，没有DDL或数据迁移；正式切换仍须完整备份与新旧固定制品恢复。

输入仍限1MiB、2048单边、4194304像素、8位非交错RGB/RGBA PNG。全PNG像素解码/重新编码、其他色型/位深/交错、JPEG方向处理、GIF/WebP首帧、BMP、8MiB原输入与旧版尺寸规范化、渠道附件及真实平台仍待完成；不得把本批静态结构修复称为完整媒体迁移。Windows worker执行安全边界继续适用。

## 固定Windows结果

正式应用 **`9804ef2c42f371c4d2286afb78d25cd4272dda22`** 已提交、推送并核实origin tip，上一可用Windows版本 **`30c001f58c2329f2b2925dd2ccdb2d5d6debf410`**。schema15保持不变，后续证据文档提交不改变运行SHA。

12:54:50+08:00核验Windows11专业版10.0.26200/x64、Core Ultra7 155H/22逻辑处理器、31.61GiB内存/约11.44GiB可用、D盘约400.62GiB可用，Node24.22.1、npm12.2.1、PowerShell7.6.5。没有新的Rust/worker隔离构建与运行证明。

| 检查       | 实际结果                                                                                                                                                                                                                                                                                                        |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 本地       | server构建/类型检查、8项media/context回归、格式和暂存差异检查通过，退出0。APNG和非ASCII名称均先有真实201反例，修复后400；合法透明度/调色板仍通过。                                                                                                                                                              |
| 固定打包   | `pwsh -NoProfile -File deploy/prometheus/package-windows.ps1 -Commit 9804ef2c42f371c4d2286afb78d25cd4272dda22 -PreviousVerifiedCommit 30c001f58c2329f2b2925dd2ccdb2d5d6debf410`，远端已推送Git archive构建，前后端同SHA，退出0。ZIP SHA256 `c9e3c2d25d8cdd6270729442f0a86eb56103b7da2a3bcbf9e5047e172c12e0e1`。 |
| 固定制品   | `node deploy/prometheus/verify-release.mjs <本SHA/runtime>`，18项通过，退出0。                                                                                                                                                                                                                                  |
| 固定后端   | `PROMETHEUS_RELEASE_ROOT`绑定本SHA/runtime，`node --test --test-concurrency=1 apps/server/test/*.test.mjs`：245通过/0失败/0跳过，196.39秒，退出0。服务fixture装载固定包，安全/ACL、严格JSON及部分直接辅助仍来自本地；39份JS/schema中38份字节相同，另1份仅CRLF/LF差异，构建身份单独校验。                        |
| 固定浏览器 | 同环境下`npm run test:e2e --workspace @prometheus/admin`：23通过/0失败，49.3秒，退出0；使用同SHA页面和后端。没有新增页面或恢复Web对话入口；12项关系图纯函数沿用30c001f时实际重跑证据，本批未重跑。                                                                                                              |
| 正式启动   | 包内`Start-Prometheus.ps1 -DataDir D:/DSH/.local/prometheus/instance`逐文件校验后，于12:58:58+08:00前台启动，Node PID28400，`http://127.0.0.1:8419/admin/`。                                                                                                                                                    |
| 正式复核   | 12:59:16+08:00 live/ready/schema15、build.json与数据锁同SHA；25个匿名管理GET、3个正确Origin匿名POST均401；实际登录页正常，无脚本错误。数据库完整性ok，未读取用户密码或代登录。                                                                                                                                  |

12:58:27+08:00核验旧SHA、PID26748完整命令行、锁及无在途请求/作业/投递。Ctrl+C打印Prometheus stopped，Node、锁和8419监听均消失；外层PowerShell退出1。12:58:49+08:00完整备份17文件至 `.local/prometheus/backups/instance-2026-10-31T04-58-49-085Z-4696f8c9-7af7-4b15-a013-7cdfe547c392`，包含数据库、主密钥、原快照和部署元数据，完整性ok，退出0。本批无DDL，不制造新的schema迁移快照。

逐文件哈希、路径边界、符号链接与ACL核验后，恢复至 `recovery-drills/d2ac0251-af9f-431e-8eee-cf5aa5ca0969`，只在此副本增加合成管理员并真实登录。用旧固定30c001f上传合法带文本元数据PNG及像素之后带PLTE的非法PNG，均返回201；旧版GET也返回200。合法图经真实loopback Vision→Native两步完成。关闭旧版，再用新固定包打开同库：合法图字节和摘要一致，非法旧图GET及chat均400且BLOB/摘要原样保留，没有发起这条非法图的模型请求；新APNG也400。合法旧图重新完成Vision→Native两步，两个版本共4次合成provider请求，原文本元数据未保留；原管理员行、稳定身份、主密钥与合成凭据解密一致。仅验证协议和来源链，不计为真实视觉模型或平台外发。

另将未加入合成内容的完整备份恢复至 `recovery-drills/822c0d82-d8d8-499b-8eb9-428398d0e702`，旧固定30c001f在18469 ready/schema15、身份/主密钥/原管理员及完整性均通过后关闭。两项正式备份恢复演练12:58:51+08:00完成，退出0；没有覆盖正式实例，也不把同schema回退冒称版本降级阻断。

正式前后管理员1，records/Run/消息/凭据0，配置/性格revision1，稳定身份和主密钥哈希一致；六表admins/config_revisions/personality_versions/records/secrets/source_grants逐行内容哈希相同。模型未配置、QQ/微信关闭，未写入合成任务或自动开启许可。instance、master.key、备份和两个恢复目录原生icacls只有当前账户、SYSTEM、Administrators；旧WSL `df6429cbceb8f16504efe9464c1abbbdb4a67e29`在18439仍ready，仅作旧逻辑对照。

私有证据：`.local/prometheus/release-validation/reports/9804ef2c42f371c4d2286afb78d25cd4272dda22.json`、`.local/prometheus/upgrade-evidence/9804ef2/{drill,production}.json`、正式deployment.json，以及分开的反例/开发/固定/浏览器/备份日志。秘密与用户数据不提交。

## 独立识图来源审查

12:57:13+08:00在固定9804ef2的独立合成副本审查既有完整图片对话，真实认证API把一条识图回答作为summary和memory_candidates来源均返回202。这只证明队列受理，提交时工作器已停用；不冒称模型已经完成或候选已获批准入库。保存的Native请求仍把视觉描述放在system消息，两个已完成图片Run仍有非空临时BLOB。旧图片契约要求不把派生识图回答用于摘要/记忆候选、描述作为不可信user角色资料、完成后清图；这些仍未完整迁移，不由本批结构校验修复代替。原始图片路径的严格边界和这些来源/生命周期问题分开验收。

证据为 `.local/prometheus/visual-provenance-audit.json` 与 `audit-visual-provenance.mjs`，仅合成数据；正式实例未参与。还需补齐后续追问的可信来源记录、遗忘/更正/取消/重启下的描述与图像清理，再继续常用格式与渠道媒体。
