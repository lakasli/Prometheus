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

schema2不能由schema1制品打开。回退先保留schema2目录，向新私有目录恢复完整schema1快照，再用上一版固定SHA启动和复验；不能覆盖运行数据或遗漏主密钥。

## 隔离工作器前置检查

语音、Harness、Python和Component执行仍禁用。可在安装MSVC Build Tools/Windows SDK的本机执行`npm.cmd run build:worker`构建固定空权限探针；打包加`-IncludeWorkerProbe`时会从同一Git SHA重新编译并记录制品哈希。正常TypeScript构建不自动启动该探针。

`node deploy/prometheus/verify-worker.mjs <runtime绝对目录>`核对探针哈希并写入独立私有报告；阻塞退出2，不计为应用或隔离通过。当前主机已记录签名策略4551/CodeIntegrity事件及子进程0xC0000142初始化失败，权限和完整生命周期尚未验收，不调整安全策略。详见[迁移审查](prometheus-migration-audit.md)。

针对本批逻辑的固定制品回归可设置`PROMETHEUS_RELEASE_ROOT`后执行`node --test apps/server/test/migration-logic.test.mjs`；测试数据仍放在工作区忽略目录，不写入制品。
