# Prometheus 日程迁移验收

2026-10-31，Asia/Shanghai。第七批对照旧Rust `schedule.rs`、`schedule_model.rs`与[原契约](schedule-system.md)，迁移系列日历、版本授权、工作目标和文本阅读。WSL原版仅用于逻辑参照，不能替代Windows验收；正式发布结果执行后追加。

## 本批行为与边界

- 活动保存稳定身份、owner、创建者与不可变修订。创建要求账户级Idempotency-Key；同键返回原回执，不同内容冲突。修改与预演要求当前If-Match；跨账户仅返回匿名忙闲时间。人工旧日程保存时也复核与新活动的冲突。
- 每日/每周/每月按IANA民用时间展开，间隔1–31，名义次数最多3660；月末无对应日期与DST缺失/歧义机会跳过并消耗名义次数。每次活动不跨当地自然日且不超过8小时，重复活动保持真实时长。单次改期保留原nominal_at；此后拆分剩余次数，定义替换须明确重置未来例外，状态变更保留例外。实际running/partial/completed/unknown历史不可覆盖。
- 月/周/日由服务端返回实际民用日期边界，包括23/25小时及被跳过的日期。手工活动、原人工资料、触发规则与整理任务分别显示；未知工作日年份阻断。拖动只生成单次预演，须核对后保存。过去未物化机会显示已错过，GET不伪造执行记录、不派发任务。范围最多93天/500条，活动与比较次数另有上限；首次冲突检查有界90天，不声称无限期全量检查。
- 阅读资料须明确提交UTF-8正文，最多512KiB/100项，不接收服务器路径或任意URL；浏览器可读本地UTF-8文本后明确保存。资料、进度与笔记私有。撤销清空正文、进度及所有代次笔记，保留必要历史状态；旧响应不能恢复内容。
- 自主grant限定活动类型、本人资源、有效期、时区、每日窗口、规划范围、次数/时长/变更与模型额度。超范围活动保存待审，不因resume或模型自称授权生效。批准精确保存的整个系列与例外并记录actor/哈希；定义或例外变化撤销批准。视频、外发与动作门禁仍独立。
- 工作目标默认停用，明确启用后按规则排程；选择model才请求模型。最多三天/八个候选、至少提前七分钟，模型仅选提供的资源与时段ID，拒绝重复键/混杂JSON/未知字段。提交前重查目标、授权版本、资源与新冲突。每日每目标一次、失败至少一小时冷却，取消不退规划额度。无资料权限显示阻断，不能记为已读完成。
- 阅读每片4096 Unicode码点，成功后才提交笔记与实际覆盖；跨活动/跨重启共享全局游标。显式重读仅开启一次新generation，旧历史保留；每片最多三次失败，成功清零失败数。读完资源不重复请求。窗口过期保留partial，后续授权活动续读。
- 沿用可信时钟、作息与交互优先准入；与其他自动任务共享一个自动槽及全局并发2。保守输入字节/输出预留和全局每日调用额度同时检查。失败、取消及未知消耗不退款；逻辑取消后待HTTP连接关闭才释放lease/凭据引用。活动/资源/grant版本和generation变化中止旧请求；重启仅保留检查点、标中断，不重放原网络请求。诊断只保留hash/字节/版本/预算/模型标识/稳定错误码，不保存请求或回复正文。

本批仍未迁移注册文字活动执行（原抄录等）、渠道模型自主动作桥、视频理解、真实主动联系、通用Python动作、完整日周月覆盖与长期规划优化。正式实例无真实模型与渠道凭据；合成HTTP模型不等于外部模型、QQ/微信客户端或长期运行通过。Windows worker原有策略阻断未改变，执行仍禁用。

## 依赖与本地检查

Node24没有原生Temporal；固定使用`@js-temporal/polyfill@0.5.1`（ISC，纯JS，锁文件精确版本，间接jsbi），不修改全局Temporal。依据[项目README](https://github.com/js-temporal/temporal-polyfill)使用Instant/PlainDateTime/ZonedDateTime。上游仍提示生产成熟度边界，本次通过实际Windows的纽约春秋DST、Lord Howe半小时跳变、月末与民用日长检查，不推广为所有时区历史数据均已验证。npm安装使用ignore-scripts，审计0漏洞。

执行`npm run typecheck`、`npm test`、`npm run test:e2e --workspace @prometheus/admin`与格式/差异检查。日程专项`node --test apps/server/test/schedule.test.mjs`包含18项，覆盖实际loopback模型请求、失败/取消/共享物理槽、跨重启Unicode覆盖、重读代次、规划重复JSON与提交冲突、所有者隔离、系列历史和schema5保留。服务HTTP/进程内夹具支持`PROMETHEUS_RELEASE_ROOT`；Temporal/JSON纯函数与密码/ACL辅助函数引用本地dist，报告分开说明。冻结schema5.sql来自上一已推送版本；完整实例正式恢复另验。

浏览器新增日程场景，既有11项继续回归：月末三次系列、单次改到无自然机会月份、拖动仅预演、此后剩余次数、资料/grant/目标表单、实际阅读笔记与撤销；截图检查360px布局无横向溢出。较早试跑暴露了datetime-local测试格式和既有测试人工日程占用当前时段的问题，修正夹具使用明确未来日期，保留服务冲突检查。新增schema5夹具曾使用错误账本列名而失败，改为真实schema字段后通过。

代码复核还修复：过期虚拟机会误显计划中；grant版本改变未立即使人工绑定的在途阅读失效；目标资源被移出授权时误显完成。新增回归分别覆盖，失败结果不计通过。

04:05–04:06+08:00最终本地检查：类型检查退出0，前后端构建成功，服务端85项全部通过（约41.4秒），浏览器12项全部通过（约16.2秒），Prettier与git diff --check通过。此前84项回归属于新增授权版本场景前的结果，最终以85项为准。

## 固定SHA Windows验收与正式运行

应用SHA **`d7e6fb5d0b1c3f82eb14df44b141351237b1ae8b`** 已提交并推送origin，`git ls-remote`核实远端分支tip相同。打包命令为`pwsh -NoProfile -File deploy/prometheus/package-windows.ps1 -Commit d7e6fb5d0b1c3f82eb14df44b141351237b1ae8b -PreviousVerifiedCommit 782a9828e15985b4d0df4437085ccd8a5b40d263 -IncludeWorkerProbe`，退出0。仅Git archive源码进入包，未夹带用户未提交的Rust/Breeze文件。前后端同SHA，ZIP SHA-256为`ed5de5a52af22f6e4e67c3107834ef68d11bfc0dfc4d5ec32b6ca4bbfd50ffbb`。

目标主机：Microsoft Windows 11专业版10.0.26200 x64，Ultra7 155H/22逻辑处理器；RAM31.6GiB，验收时空闲约12.2GiB，D盘空闲约410.5GiB。Node24.22.1/npm12.2.1/PowerShell7.6.5，MSVC14.44.35207与SDK10.0.26100.0。Get-ComputerInfo的ProductName仍报Windows10，CIM实际Caption为Windows11；保留这一查询差异。探针同SHA编译成功，本批未重新执行，不产生Windows隔离通过结论。

| 检查           | 实际结果                                                                                                                                                                                                                  |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Git及固定构建  | 提交、推送、远端tip、archive构建、生产依赖安装、前端静态资源与后端构建均通过，退出0；依赖审计0漏洞。                                                                                                                      |
| 固定包基础验收 | `node deploy/prometheus/verify-release.mjs <runtime>` 18项通过，退出0；核对逐文件hash、SHA、启动、live/ready、认证与合成文字流程。                                                                                        |
| 固定包专项     | 设置PROMETHEUS_RELEASE_ROOT后执行schedule.test.mjs，18项通过，约4.5秒，退出0。其中17项绑定固定服务HTTP/进程内夹具（包括schema5升级），1项Temporal/JSON纯函数检查使用本地dist；密码、ACL及部分时间辅助函数也来自本地构建。 |
| 固定包浏览器   | 同环境运行管理页12项全部通过，约17.3秒，退出0；实际静态资源与服务来自固定包，不重建源码。360px截图已检查，无横向溢出。                                                                                                    |
| 正式健康与权限 | 前端build.json和live/ready均完整SHA、schema6；15个匿名管理API返回401，包含日程/资料/grant/目标。既有登录页无JavaScript错误，未使用用户密码或代登录。                                                                      |

04:07:49+08:00复核上一版782a982的Node PID25844、命令行、数据锁和健康SHA；无Run/整理/投递在途，渠道关闭。Ctrl+C输出`Prometheus stopped`，外层PowerShell实际退出1；PID、8419监听和锁均消失。空闲退出不当作正式在途取消验证。

04:07:56+08:00执行完整实例备份，7个文件、integrity_check=ok：`.local/prometheus/backups/instance-2026-10-30T20-07-56-908Z-fbf4996d-ec17-4272-a0dd-e44f43690550`。主密钥、数据库及旧快照/部署元数据一起备份。复制前核验文件hash、路径和NTFS ACL，不覆盖现有备份。

04:07:58+08:00在独立恢复目录`recovery-drills/516d1858-5ebf-466d-affa-abe2c2f9bcae`加入合成schema5草稿、人工日程和已有固定点账本，仅修改演练副本。实际新制品18469 ready并升级6；草稿内容/基准/创建时间、人工日程及只读投影、分值48.25/余数123/直接互动时间均保留，主密钥hash一致。新日程表无伪造活动/请求，自动schema5快照仍可读且integrity_check=ok。实际旧782a982 Store拒绝schema6；另一私有目录`recovery-drills/d2a1b706-c023-4e51-a9a5-610f490e12eb`恢复未经合成修改的完整schema5备份，旧制品18469 ready并关闭。恢复与新库目录分开。

04:08:03+08:00包内Start-Prometheus.ps1核对制品hash后前台启动正式实例，PID2224，入口`http://127.0.0.1:8419/admin/`。04:08:32复验完成：管理员1，资料/Run/消息/凭据0，配置/性格revision1，稳定身份及主密钥hash与升级前一致；人物/草稿/新日程资源等表无合成数据进入正式实例。当前库和自动schema5快照integrity_check=ok，实例、secrets和master.key原生NTFS ACL仅运行账户、SYSTEM与Administrators。

私有证据：`.local/prometheus/release-validation/reports/d7e6fb5d0b1c3f82eb14df44b141351237b1ae8b.json`、`.local/prometheus/upgrade-evidence/d7e6fb5/drill.json`与`production.json`；正式deployment.json记录当前SHA、上一可用SHA、备份、PID和验证范围。WSL原Rust `df6429cbceb8f16504efe9464c1abbbdb4a67e29` 在18439仍ready，仅供对照。应用运行、固定制品合成场景、真实平台及原始完整功能迁移是不同结论，本批不宣称全部功能迁移完成。
