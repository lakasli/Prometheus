# Prometheus TypeScript 架构

2026-10-30 用户授权：使用 TypeScript 重写并改名为 Prometheus，作为全新项目运行，旧数据保留备份。本文件描述新实现；[原架构](architecture.md) 的完整功能目标继续保留，不回写历史验收。

| 层       | 当前实现                                                              |
| -------- | --------------------------------------------------------------------- |
| 运行时   | Windows 原生 Node.js 24.7–24.x，ES modules                            |
| 后端     | TypeScript strict，构建为 JavaScript                                  |
| 生命周期 | 官方 TypeScript Cordis 3.18.1；当前仅可信内置队列插件                 |
| HTTP     | Node HTTP / HTTPS，认证 API 与静态管理页同源                          |
| 页面     | Vue 3、TypeScript、Vite，新 `apps/admin` 工作区                       |
| 数据     | Node SQLite，WAL、FULL synchronous、secure_delete、版本化 schema      |
| 认证     | Argon2id PHC、随机会话、服务端 CSRF、8小时到期                        |
| 凭据     | AES-256-GCM 私有文件、独立主密钥、NTFS ACL                            |
| 模型     | OpenAI 兼容公共 HTTPS 文字协议，连接时校验真实 DNS 地址               |
| 执行门禁 | 语音、Harness和插件执行返回503；媒体模型显式配置，不加载不可信npm插件 |

新运行链路不编译或调用 Rust。WIT / WASM、Python 动作和 Harness 目标仍保留；后续由 TypeScript 宿主调用经 Windows 验证的独立 worker，不退化为宿主进程内执行。

## 数据与权限

`metadata` 保存 schema 和稳定性格身份；`admins` / `sessions` 保存管理身份；配置与性格各有不可变版本历史。`conversations`、`runs`、`messages` 保存对话与运行；持久队列与进程内物理请求 lease 分开记账。

`records` 按 kind 分别校验记忆、日记、日程、人物、关系和 Skill，不接受任意执行字段。资料默认私有、按账户隔离；共享记忆须逐次确认发布。其他域当前仅私有。配置权限不代替渠道接收方同意或外发授权。

更正与遗忘增加上下文 epoch，取消受影响的排队及在途请求。遗忘清空当前内容、全文索引和递归依赖的派生资料，原始对话与离线备份保留。中断模型请求不盲重放，旧渠道发送状态不导入。

## 网络与秘密

后端只监听 loopback。管理写操作同时验证 Host、精确 Origin、会话与 CSRF；Cookie 为 HttpOnly / SameSite=Strict，HTTPS 必须使用 Secure。静态文件解析后检查实际路径，不提供源码、数据或任意文件入口。

模型不跟随重定向；凭据只放授权请求头。请求字节、历史数量、响应大小、超时与并发均有上限；连接时拒绝受保护网络。loopback 模型例外仅能显式在开发 / 合成测试启用。上游错误正文不进入日志，已知凭据在输出中隐藏。

完整功能仍待继续迁移，见 [迁移表](prometheus-migration.md)。基础版不能记为完整 M2 / M4 / M5。

## schema3日志与性格恢复（2026-10-31）

性格历史管理入口通过带当前If-Match的PUT另存版本。日志事件只存引用；输入/输出读取原消息，Native请求保存已隐藏已知凭据的消息快照。纠正、遗忘、来源过期和睡眠上下文重置会清理快照；日志分页按来源授权解析，不能由日志恢复失效上下文。30天/20,000事件上限同步清理相应快照，不删除原聊天。后台整理等任务仍使用已有审计，不伪造尚未接入的ASR/子任务/worker结果。

schema1/2升级前生成原数据库快照，事务内建立schema3与消息引用回填标记；旧制品拒绝打开schema3。正式回退恢复完整旧实例到新目录，不能仅切换代码。来源许可在认证操作、Native排队/模型请求/发布和渠道投递边界即时复核；过期后续期重置采集起点，不补采授权中断期间的消息。

## 第二批运行模块（2026-10-31）

features将channels、knowledge、automation、media、relationships与context绑定到可信宿主。管理入口沿用认证、CSRF和版本边界；QQ webhook只接受Ed25519签名原始字节，不能绕过管理API认证。QQ / iLink生产端点固定，loopback注入仅限合成测试构造器。

schema2增加来源授权、整理检查点、发送分段、作息周期、触发firing、附件、上下文选取与好感事件。schema1升级前VACUUM一致性快照，DDL和版本号在同一事务提交。数据库和主密钥须一同独立备份。启动将含糊发送标记unknown、在途模型标记interrupted，不猜测成功或自动重发。

自动整理与文字任务共享一个自动槽、每日调用预算，并计入全局两路物理网络lease。日记每批16来源保存累计JSON检查点，最多128来源、8次调用。阅读请求成功后才推进游标。审核资料不会授予发送权；触发提议先保存停用规则，再由操作者预演启用。

来源采集、模型出域、睡眠控制与唤醒控制分开。更正/遗忘后旧原文跨越水位而失去提取资格，原聊天仍可核对。撤销或过期使摘要、在途结果与依赖资料失效。睡眠原始期限持久化，先排空或取消在途再重置上下文，保留待处理输入和未发送回复，唤醒后复用。

PNG不下载任意外部URL，验证CRC、解压大小和尺寸并剥离元数据；视觉结果带ID和SHA进入Native。HTML / SVG只在认证的无脚本/网络iframe预览。Component / Python仅能登记空权限制品，没有Windows隔离证明就不编译、批准、激活或执行。
