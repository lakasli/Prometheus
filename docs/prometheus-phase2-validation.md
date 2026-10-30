# Prometheus 第二批迁移验收

本轮从首轮Windows版本49c3ed46b9f6737c8913ad2eff78ee7bc20a8049继续迁移。完整功能与未完成范围见[逐项清单](prometheus-migration.md)。合成模型、协议服务与数据库断言不计为真实QQ / 微信、外部模型或客户端验收。

## 本地Windows检查

2026-10-31 00:10（Asia/Shanghai），Windows11 Pro 10.0.26200 x64、Node24.22.1、npm12.2.1。主机Intel Core Ultra7 155H、22逻辑CPU、31.6GiB内存；本轮检查可用内存约15.9GiB、D盘可用413GiB。运行链路使用Node，不调用Rust或更改系统应用控制策略。

- `npm.cmd run typecheck`：TypeScript与Vue检查通过。
- `npm.cmd test`：前端构建通过；后端首轮36项通过，补测睡眠重置不得恢复已抑制回复后，最终37项通过、0失败、0跳过，约20.7秒。
- `npm.cmd run test:e2e --workspace @prometheus/admin`：7项通过，约9.2秒。
- `npm.cmd run format:check`、`git diff --check`（本轮文件）：通过。
- `npm.cmd audit --omit=dev`：0漏洞。

后端覆盖QQ签名、批次、去重、逐段回执，WS identify与撤销，iLink本人/游标/上下文；77项目录构造及字段拒绝（无平台派发）；候选审核、撤销、遗忘水位、多批日记；触发预演与唯一firing、睡眠原期限重启恢复；PNG校验、视觉进入Native、显式生图拒绝URL；预算provider绑定、本地记忆；鉴权、秘密、取消；schema1快照及schema2升级、完整实例主密钥恢复；更正抑制与同域合并。

浏览器覆盖首建登录、桌面和移动宽度、记忆更正遗忘、性格/配置冲突、各类资料、来源审核/触发启用/日历/预算、好感/停用建议/HTML预览。隔离iframe中的合成恶意脚本未改动父页面。截图在`.local/prometheus/screenshots/`，已检查排版；账户与数据均为独立合成夹具。

## 原失败与修复

首轮全量回归1项旧断言把新作息入口当作503门禁，随后按实际路由更正并通过。浏览器旧断言把QQ当作禁用，新实现应标注真实平台待验收；新增好感卡片后关系断言需限定目标记录；建议轮询最初命中静态按钮文字，改为等待审核按钮。失败未计入通过数。

审查修复：自动任务完成不得再次触发自身；授权起点绑定消息序号；睡醒停止周期整理并保留检查点；更正抑制未发候选；触发提议审核后保存停用规则；人物合并不更新无关关系版本。未移除执行门禁。

## 正式发布

用户于2026-10-31提供新的Git地址`https://github.com/lakasli/Prometheus.git`。原Git服务器多次HTTP无响应（curl超时、Git低速15秒超时），没有将未确认的推送计为成功。新origin已推送并用ls-remote核实；旧远端保留为dsh-legacy，不强推、不改写历史，原有Rust/Breeze工作区变更未提交。

- 部署代码：`5cc647cd8ce304408563bc9a154b3ccebda06f61`，分支`codex/prometheus-typescript`。
- 上一可用Windows版本：`49c3ed46b9f6737c8913ad2eff78ee7bc20a8049`。
- 构建：从已推送SHA的Git archive执行锁文件安装，前后端均写入同一SHA；Windows后端与前端构建分别通过，退出0。
- 命令：`pwsh -NoProfile -File deploy/prometheus/package-windows.ps1 -Commit 5cc647cd8ce304408563bc9a154b3ccebda06f61 -PreviousVerifiedCommit 49c3ed46b9f6737c8913ad2eff78ee7bc20a8049`。
- 制品：`D:\DSH\.local\prometheus\releases\5cc647cd8ce304408563bc9a154b3ccebda06f61\runtime`。
- ZIP SHA256：`68a4e8778d75179ef2680a58f1282f6ccb582ad1d2c76e8d77b53b4c803c70cb`。

### 固定制品测试

`node deploy/prometheus/verify-release.mjs <runtime>`退出0，18项检查通过，包括制品逐文件哈希、双健康SHA、认证、初始化、凭据与配置、资料、合成模型、明确来源审核、触发实际结果、睡醒、77项目录、预算/媒体策略和执行门禁。报告：`.local/prometheus/release-validation/reports/5cc647cd8ce304408563bc9a154b3ccebda06f61.json`。

设置`PROMETHEUS_RELEASE_ROOT`指向上述runtime后，7项浏览器测试通过（约8.7秒），退出0。测试数据、模型和渠道均为独立loopback合成服务。没有访问生产凭据，也没有向真实QQ / 微信派发。

### 停机、备份和恢复演练

2026-10-31 00:25:19+08:00，确认旧PID3776、锁文件、健康SHA一致，0在途任务、0管理员、渠道均未启用。自动审批拒绝向上一轮终端会话发送Ctrl+C（要求审批但当前环境禁止请求），因此改用核实身份后的Windows Stop-Process只停止该Node进程；命令退出0。**这不是优雅退出通过的证据。** 随后旧版recover-lock确认原PID已退出再清理锁；没有删除数据库。

完整备份：`.local/prometheus/backups/instance-2026-10-30T16-25-30-999Z-2ed547be-d7e3-4def-bec6-b5c446317031/`。备份脚本退出0，4个文件含数据库、主密钥、原初始化文件与部署记录，SQLite integrity_check为ok，NTFS ACL检查通过。快照在无服务持锁时生成，原WAL中的数据由SQLite backup API纳入。

00:25:59+08:00，备份逐文件哈希通过；完整恢复到独立目录`.local/prometheus/recovery-drills/82482bcd-e6b4-4903-80e7-8d7decb456f6/`，以旧49c3ed4制品在18469端口启动。ready、身份、schema1均通过，随后关闭。没有把备份目录直接当运行数据，也没有覆盖原实例。

### 实际实例切换

00:26:01+08:00使用包内Start-Prometheus.ps1启动新版本，前台PID2604，数据仍为`D:\DSH\.local\prometheus\instance`，监听`127.0.0.1:8419`。新服务产生ready日志；live / ready返回相同5cc647c完整SHA。

schema升级为2，自动schema1快照已产生；数据库integrity_check为ok。升级前后稳定身份、管理员数0、资料数0、配置revision1、性格revision1及主密钥SHA256一致。8个新管理入口匿名请求均401，管理HTML与新静态资源对应固定制品。实际启动日志位于前台会话，私有切换证据位于`.local/prometheus/upgrade-evidence/`，部署元数据位于实例deployment.json。未安装Windows服务或自启动。

实际8419端口的只读浏览器检查通过：初始化标题与字段、页面宽度、无JavaScript异常，未创建生产账户。补充检查最初误把遮蔽的配对凭证计作普通密码而断言输入数量2，实际为3；改为分别按密码、确认密码和配对凭证的语义验证，随后通过。这不改变上述固定制品7项测试的统计。

当前正式实例尚未建立管理员或配置真实模型、QQ、微信。初始化凭证已在本机私有文件重新生成，15分钟有效，未输出其内容。功能验收为固定制品隔离数据测试；正式实例只验证启动、健康、数据连续性、鉴权和未初始化管理入口。真实外部模型、手机扫码、平台收发、语音/Harness/插件隔离执行仍未通过，不称全量重写完成。
