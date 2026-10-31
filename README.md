# Prometheus

面向 Windows 的 TypeScript 日常空间。2026-10-30用户确认：**使用 TypeScript 重写，命名为 Prometheus，作为全新项目运行，旧数据保留备份**。

新入口为根目录 npm 工作区：`apps/server` 后端、`apps/admin` 管理页面；运行链路不编译或调用 Rust。原代码、素材和未提交的 Breeze 改动保留，旧说明见 [DSH历史README](docs/legacy/README-DSH.md)。用户于2026-10-31指定新仓库[GitHub Prometheus](https://github.com/lakasli/Prometheus)，旧远端保留为dsh-legacy，本地仍为D:\DSH。

**当前已推进到第二十二批，新增QQ受控媒体外发与大文件分片上传；完整旧功能的迁移仍未完成。** 本批固定Windows交付、330项后端/28项浏览器和完整升级恢复结果见[QQ媒体验收](docs/prometheus-qq-media-validation.md)。 真实QQ / 微信、正式外部模型与媒体平台验收单独记录；语音、Harness和生成插件执行在Windows隔离验证前保持禁用。原目标继续保留，见 [逐项迁移范围](docs/prometheus-migration.md)。

## 开发与启动

需要 Windows、Node.js24.7–24.x、npm；正式打包使用PowerShell7。当前环境为Windows11 x64、Node.js24.22.1、npm12.2.1。SQLite、Argon2使用Node原生模块；官方TypeScript Cordis固定3.18.1，根目录package-lock.json锁定依赖。

在仓库根目录逐条执行：

```powershell
npm.cmd ci --ignore-scripts
npm.cmd run build
npm.cmd test
npm.cmd run test:e2e --workspace @prometheus/admin
npm.cmd run bootstrap -- --data-dir .local/prometheus/dev
npm.cmd start
```

浏览器打开 **http://127.0.0.1:8419/admin/**。初始化命令只显示私有文件路径；在本机受保护终端读取凭证，在页面创建账户。凭证15分钟有效，用后删除；没有默认密码。保持前台运行，Ctrl+C正常退出。

凭证绑定数据目录。正式实例使用`.local/prometheus/instance`时，应按初始化页面显示的路径和命令生成，不能使用dev目录的凭证。只复制JSON中的`token`字段，不复制路径或完整JSON。首次创建账户后改用用户名和密码登录，token不作为日常密码。

默认开发数据为 `.local/prometheus/dev/prometheus.db`，不导入旧dsh.db。NTFS ACL只允许当前运行账户、SYSTEM和管理员；新目录设置私有权限，已有目录仅验证，权限过宽时拒绝启动。

生产前端由TypeScript服务同源提供构建后的静态文件，不常驻Vite。外部设备使用受保护的HTTPS反向代理、精确Origin和Secure cookie。

## 当前能力

- 一次性初始化、Argon2id登录、服务端会话、CSRF、Host / Origin与按账户授权。
- 单一性格、稳定身份、配置版本冲突、实际连接检查、生效状态和回退历史。
- AES-256-GCM凭据库、只写不回显、引用检查、导出排除秘密。
- 远程模型文字对话、私有历史、持久队列、幂等、两路并发、取消和退出恢复。
- 显式记忆、共享确认、更正与遗忘，派生资料和在途上下文失效。
- 人工日记、日程与进度、人物、有向关系、显式文字Skill、认证运行日志。
- QQ签名Webhook / WebSocket与iLink扫码本人文字适配、去重、逐段Outbox和实际投递历史；真实平台另验。
- 明确来源的模型记忆候选、摘要、多批日记与人物整理；来源授权、审核、遗忘水位及后台预算独立。
- 停用触发草稿、预演后启用、隔离文字任务与有界阅读、上海日历、作息与原期限恢复。
- PNG来源绑定、视觉描述进入Native、显式生图、HTML / SVG隔离预览、好感事件与衰减。

管理页不恢复旧Web对话工作台。配置模型后，可通过认证API或CLI使用文字链路：

```powershell
node apps/server/dist/cli.js chat --username admin --password-file <本机私有密码文件> --message "你好"
```

`--conversation <id>`延续已有会话。密码文件不进入Git；密码不通过参数传入。生产模型使用公共HTTPS地址；`--allow-loopback-model`仅供显式开发和合成测试，不开放任意私网。

## 备份与固定版本发布

```powershell
node deploy/prometheus/backup-legacy.mjs
pwsh -File deploy/prometheus/package-windows.ps1 -Commit <已推送的完整40位SHA>
node deploy/prometheus/verify-release.mjs <runtime绝对路径>
```

发布从已推送SHA构建，前后端版本一致，生成逐文件哈希和ZIP。包内Start-Prometheus.ps1验证后前台启动；数据在制品之外，不安装服务或自启动。

旧备份包含Git bundle、工作区差异、原有未跟踪源码和SQLite一致性快照。第三方运行库及原始音频素材保留原位。详见 [架构](docs/prometheus-architecture.md)、[Windows流程](docs/prometheus-windows.md)、[首轮验收](docs/prometheus-validation.md) 与 [第二批验收](docs/prometheus-phase2-validation.md)。
