# Prometheus

面向 Windows 的 TypeScript 日常空间，提供性格与模型配置、记忆、日记、日程、人物关系、渠道与运行日志管理。

当前默认入口是根目录 npm 工作区：`apps/server` 后端和 `apps/admin` 管理页面。后端通过 Node.js 提供 API 和构建后的前端，**启动时只需运行一个服务**，不需要分别启动前后端，也不编译或调用旧 Rust 服务。

管理地址：**[http://127.0.0.1:8419/admin/](http://127.0.0.1:8419/admin/)**。如果该地址已经可以访问，直接登录即可；不要再启动一个占用同端口的实例。

2026-10-30 用户确认以 Prometheus 为全新项目重写，旧数据保留备份。代码仓库为 [GitHub Prometheus](https://github.com/lakasli/Prometheus)，本地目录为 `D:\DSH`。旧 Rust / DSH 说明见 [历史 README](docs/legacy/README-DSH.md)，完整迁移目标及未完成范围见 [迁移范围](docs/prometheus-migration.md)。

## 环境要求

| 项目       | 要求                                                                                   |
| ---------- | -------------------------------------------------------------------------------------- |
| 系统       | Windows；已验收环境为 Windows 11 x64，数据目录需支持 NTFS ACL                          |
| Node.js    | `>=24.7.0 <25`，见根目录 `package.json`                                                |
| npm        | 随 Node.js 安装，按 `package-lock.json` 安装依赖；已验收版本为 12.2.1                  |
| PowerShell | 源码启动可使用 Windows PowerShell；正式打包和发布包启动脚本要求 PowerShell 7（`pwsh`） |
| 可选组件   | 浏览器测试需要 Playwright Chromium；本机媒体包需要 MSVC Build Tools / Windows SDK      |

普通文字服务不需要 Rust、Python、本地模型或媒体工作器。SQLite 与 Argon2 使用 Node.js 内置能力。依赖统一由根目录 npm 工作区管理，不要在旧 `web/admin` 中安装或构建当前管理页。

## 首次从源码启动

在 PowerShell 中逐条执行；某一步失败时先处理错误，再继续下一步：

```powershell
Set-Location D:\DSH
node --version
npm.cmd --version
npm.cmd ci --ignore-scripts
npm.cmd run build
npm.cmd run bootstrap -- --data-dir .local/prometheus/dev
npm.cmd start -- --data-dir .local/prometheus/dev
```

`build` 会编译后端并生成管理页静态资源。`bootstrap` 只生成初始化凭证，随后退出；最后的 `start` 才会在前台运行服务。出现 `"event":"ready"` 后，打开 [管理页](http://127.0.0.1:8419/admin/)。终端保持打开，按 **Ctrl+C** 正常停止。

首次创建账户：

1. 按初始化页面显示的路径，在本机私有编辑器或受保护终端中打开 `bootstrap-token.json`。上述命令对应 `D:\DSH\.local\prometheus\dev\bootstrap-token.json`。
2. 只复制 JSON 中 `token` 字段的值，填入页面的初始化凭证栏；不要复制完整 JSON 或文件路径，也不要把凭证贴入聊天或日志。
3. 设置用户名和密码。用户名为 3–64 位，仅允许字母、数字、下划线、点和连字符；密码为 12–256 位。
4. 创建成功后，服务自动删除凭证文件；以后使用用户名和密码登录。**没有默认用户名或默认密码。**

凭证有效期为 15 分钟。过期或缺失时，在另一个终端按页面显示的命令重新生成；无需重启服务。凭证绑定数据目录，开发目录 `dev` 与正式目录 `instance` 的凭证不能互用。

## 日常启动与配置

已安装依赖、完成构建和初始化后，在仓库根目录执行：

```powershell
Set-Location D:\DSH
npm.cmd start -- --data-dir .local/prometheus/dev
```

日常启动不需要重新生成凭证。源码或依赖更新后，先正常停止服务，按需重新安装依赖并执行 `npm.cmd run build`。`npm.cmd start` 本身不构建；`npm.cmd run dev` 等于先构建再启动，不提供自动热更新。

登录后进入「模型与运行」，填写服务地址和模型名称，点击「加密保存凭据」，确认选中对应模型凭据，再「检查连接」和「保存并生效」。默认配置为 `https://api.deepseek.com` / `deepseek-chat`，但没有预置 API key。**页面能够打开或健康检查通过，不代表外部模型和 QQ / 微信已经配置或联通。**

管理页用于配置与资料管理，不提供旧 Web 对话工作台。配置好模型后，可通过认证 API 或 CLI 发起文字对话。保持服务运行，在另一个终端执行，并替换下列用户名和密码文件路径：

```powershell
node apps/server/dist/cli.js chat --username '你的用户名' --password-file '本机私有密码文件的绝对路径' --message '你好'
```

密码文件只包含登录密码，应置于私有目录且不进入 Git；不要把密码直接写入命令参数。附加 `--conversation <id>` 可延续已有会话；自定义端口时附加 `--url http://127.0.0.1:8420`。

### 健康检查

在另一个 PowerShell 终端中执行：

```powershell
Invoke-RestMethod http://127.0.0.1:8419/health/live
Invoke-RestMethod http://127.0.0.1:8419/health/ready
```

预期 `status` 分别为 `ok` 和 `ready`，并返回版本、commit 与数据 schema。`ready` 只检查服务和数据库可访问性；模型、渠道和媒体需要各自验证。源码普通构建的 commit 为 `development`，固定版本发布包写入完整 Git SHA。

### 自定义目录与端口

`serve` 支持以下配置，优先级为 **CLI 参数 → 环境变量 → 默认值**：

| CLI 参数     | 环境变量              | 默认值                  |
| ------------ | --------------------- | ----------------------- |
| `--data-dir` | `PROMETHEUS_DATA_DIR` | `.local/prometheus/dev` |
| `--listen`   | `PROMETHEUS_LISTEN`   | `127.0.0.1:8419`        |
| `--origin`   | `PROMETHEUS_ORIGIN`   | `http://<listen>`       |
| `--web-dir`  | `PROMETHEUS_WEB_DIR`  | `apps/admin/dist`       |

相对路径按当前工作目录解析。改用独立开发端口时，例如：

```powershell
npm.cmd start -- --data-dir .local/prometheus/dev-8420 --listen 127.0.0.1:8420 --origin http://127.0.0.1:8420
```

随后访问 `http://127.0.0.1:8420/admin/`；新数据目录需要单独初始化。服务只允许 loopback 监听，默认入口请使用 `127.0.0.1`，保持浏览器地址与精确 Origin 一致。跨设备访问需配置受保护的 HTTPS 反向代理、对应 `--origin` 和 `--secure-cookie`，详见 [Windows 流程](docs/prometheus-windows.md)。

## 固定版本发布包启动

正式实例使用已推送的完整 Git SHA 构建，前后端版本一致；数据必须放在制品目录之外。发布包仍需要符合上述版本要求的 Node.js，不安装 Windows 服务或自启动。

以下命令用于构建并验证一个新发布包，运行前须完成相关检查、提交和推送，且当前 SHA 必须是 `origin` 某个分支的最新提交：

```powershell
Set-Location D:\DSH
$taskCommit = (git rev-parse HEAD).Trim()
pwsh -File deploy/prometheus/package-windows.ps1 -Commit $taskCommit
$taskRuntime = Join-Path (Get-Location).Path ".local/prometheus/releases/$taskCommit/runtime"
node deploy/prometheus/verify-release.mjs $taskRuntime
```

包输出至 `.local/prometheus/releases/<完整SHA>/`，包含 `source`、`runtime`、ZIP 和逐文件 SHA-256 清单。打包仅包含该提交的已跟踪文件，不包含工作区未提交内容；已有同 SHA 发布目录不会覆盖。已有可用版本时，打包另传 `-PreviousVerifiedCommit <上一可用完整SHA>`，切换前备份完整实例。

使用包内的 `Start-Prometheus.ps1`。首次使用一个全新正式数据目录时，先生成凭证，再启动：

```powershell
pwsh -File "$taskRuntime/Start-Prometheus.ps1" -DataDir D:/DSH/.local/prometheus/instance -BootstrapToken
pwsh -File "$taskRuntime/Start-Prometheus.ps1" -DataDir D:/DSH/.local/prometheus/instance
```

**`-BootstrapToken` 只生成凭证后退出，不会同时启动服务。** 已初始化的正式实例只执行第二条命令。脚本在启动前验证制品哈希；不直接运行仓库中的 `deploy/prometheus/start-windows.ps1`。服务停止后，可在新终端将 `$taskRuntime` 设置为所选发布包 `runtime` 的绝对路径，再执行第二条命令。

本机 ASR / TTS 与视频采样需要打包时显式加 `-IncludeMediaRuntime`，再单独验证工作器并在管理页启用。普通 `npm.cmd run build` 不下载媒体引擎或权重。固定媒体工作器不开放用户代码执行；许可、资源要求和复验命令见 [工作器媒体验收](docs/prometheus-worker-media-validation.md) 和 [Windows 流程](docs/prometheus-windows.md)。

## 数据、备份与常见问题

开发数据库默认为 `.local/prometheus/dev/prometheus.db`；正式示例数据目录为 `.local/prometheus/instance`。Prometheus 不自动读取或导入旧 `dsh.db`。新目录设置私有 NTFS ACL，已有目录及文件只验证；权限过宽时拒绝启动。凭据仅加密保存在服务端，不回传明文，不随资料导出。

升级前正常停止服务，确认进程退出和数据锁释放，再备份完整实例：

```powershell
node deploy/prometheus/backup-instance.mjs --data-dir D:/DSH/.local/prometheus/instance --source-commit '替换为该实例正在运行的完整40位SHA'
```

完整备份需包含数据库、`secrets` 和 `master.key`；只复制数据库无法恢复凭据。旧项目备份命令为 `node deploy/prometheus/backup-legacy.mjs`。当前数据 schema 为 21，旧制品拒绝打开更新的 schema；回退需要在另一私有目录恢复与旧制品匹配的完整备份，不能仅切回代码。具体恢复规则见 [Windows 流程](docs/prometheus-windows.md)。

| 现象                                              | 处理                                                                             |
| ------------------------------------------------- | -------------------------------------------------------------------------------- |
| 找不到 `apps/server/dist/cli.js` 或管理页资源缺失 | 回到仓库根目录，安装依赖并执行 `npm.cmd run build`，不要只构建后端               |
| `EADDRINUSE` / 8419 端口被占用                    | 先访问管理地址并核对健康信息；如需另一实例，使用独立端口和独立数据目录           |
| 初始化凭证不匹配、过期或缺失                      | 按当前页面显示的数据目录重新生成，只粘贴新文件的 `token` 值；已有账户直接登录    |
| `Data directory is already locked`                | 先确认原服务是否仍在运行；仅在原进程已退出后执行下方恢复命令，不手动删运行中的锁 |
| `Private data ACL...` / 权限检查失败              | 核验 NTFS ACL，只允许运行账户、SYSTEM 和管理员；保留诊断，不跳过权限检查         |
| 媒体能力显示不可用                                | 检查发布包是否包含已验证的媒体运行库及对应开关；源码普通构建不会自动具备这些能力 |

崩溃后的锁恢复命令（目录必须与故障实例一致）：

```powershell
node apps/server/dist/cli.js recover-lock --data-dir .local/prometheus/dev
```

该命令只清理确认已经退出的原进程所属锁，不会终止进程。Windows 应用程序控制策略拦截时记录被阻止程序与错误，不关闭或降低系统安全策略。

## 开发检查

以下是开发验证命令，不是每次启动的前置步骤：

```powershell
npm.cmd run typecheck
npm.cmd test
npm.cmd run test --workspace @prometheus/admin
npx.cmd --no-install playwright install chromium
npm.cmd run test:e2e --workspace @prometheus/admin
```

先完成 `npm.cmd run build`。`npm.cmd test` 会重新构建管理页并运行后端测试；管理页纯函数测试单独执行。Chromium 安装仅首次或浏览器版本更新时需要。浏览器测试启动独立合成服务于 `127.0.0.1:18449`，数据写入忽略的 `.local/prometheus/e2e`，不代表真实模型或 QQ / 微信联调通过。

## 当前范围与代码导航

已实现认证初始化、单一性格与版本化配置、加密凭据、远程文字链路、显式记忆、日记与日程、人物关系与好感、来源授权与模型候选、任务触发与作息、图像处理及受控渠道媒体链路。本机 ASR / TTS 与视频采样按独立开关和固定工作器启用。

**完整旧功能迁移仍未完成。** 真实 QQ / 微信、外部 Vision / Native 与长期性能需按各项验收；Harness、用户 Python / 通用代码与生成插件执行继续禁用。第二十四批 Windows 固定应用版本为 `f10885baf6973d803057c05e564417885aa24f94`，历史构建、运行和测试证据见 [工作器媒体验收](docs/prometheus-worker-media-validation.md)，不将历史结果当作后续版本的验收。

| 目录                                    | 用途                                               |
| --------------------------------------- | -------------------------------------------------- |
| `apps/server`                           | TypeScript 后端、CLI、SQLite、认证、模型与渠道编排 |
| `apps/admin`                            | Vue 管理页；构建后由后端同源提供                   |
| `deploy/prometheus`                     | 当前 Windows 打包、启动、备份和验收脚本            |
| `docs/prometheus-*.md`                  | 当前架构、迁移范围和各批验收记录                   |
| `.local/prometheus`                     | 已忽略的开发数据、发布包、备份及测试产物           |
| `crates`、`web/admin`、其他历史部署脚本 | 旧 DSH 实现与资料，保留供迁移参考                  |

进一步阅读：[Prometheus 架构](docs/prometheus-architecture.md)、[Windows 开发与发布](docs/prometheus-windows.md)、[迁移范围](docs/prometheus-migration.md)、[项目工作规则](AGENTS.md)。
