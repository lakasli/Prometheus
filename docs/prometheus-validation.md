# Prometheus 首轮 Windows 验收

日期：2026-10-30，Asia/Shanghai。本轮是新TypeScript项目基础版本，完整功能迁移尚未完成，见 [迁移表](prometheus-migration.md)。正式SHA和最终验收在本轮固定版本检查后追加，开发测试不计为发布。

## 固定版本发布结果

运行与制品固定为 **`49c3ed46b9f6737c8913ad2eff78ee7bc20a8049`**，已推送至 `origin/codex/prometheus-typescript` 并由打包脚本使用 `git ls-remote --heads origin` 核实。后续本验收文档的提交不是应用运行版本。

| 结果         | Windows 实际证据                                                                                                                                     |
| ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| 干净源码     | 从上述 Git SHA 的 archive 独立解出，不含工作区未提交改动                                                                                             |
| 后端构建     | `npm.cmd ci --ignore-scripts`、`npm.cmd run build` 的服务端构建成功，退出0                                                                           |
| 前端构建     | 同次构建成功，Vite静态资源与后端写入同一完整SHA，退出0                                                                                               |
| 运行依赖     | 独立runtime中 `npm.cmd ci --omit=dev --ignore-scripts` 成功，退出0                                                                                   |
| 制品哈希     | release.json逐文件SHA-256校验通过；ZIP SHA-256为 `da08beb09c148daae86f4aed03b0aa1c12937306fd31798673b2541563141988`                                  |
| 发布包运行   | `node deploy/prometheus/verify-release.mjs <runtime>` 于2026-10-30T22:42:14+08:00完成13项检查，退出0                                                 |
| 发布包浏览器 | 设置PROMETHEUS_RELEASE_ROOT指向实际runtime后，5项Chromium流程全部通过，退出0；不是工作区开发服务                                                     |
| 正式前台启动 | 包内Start-Prometheus.ps1验证哈希后启动Windows Node进程，2026-10-30T22:44:16+08:00重启成功，PID3776                                                   |
| 健康         | 2026-10-30T22:44:54+08:00再次检查live=ok、ready=ready，两者SHA一致                                                                                   |
| 退出与重启   | 实际Ctrl+C输出Prometheus stopped，Node进程退出、8419端口与目录锁释放；随后同一实例重启成功。PowerShell受Ctrl+C中断的外层终端退出码为1，不冒记为退出0 |
| 实例状态     | 新目录 `.local/prometheus/instance`，管理账户尚未初始化，默认模型未配置，未导入旧密钥、数据和渠道授权                                                |
| 上一可用版本 | 首个经过本轮验证的Windows Prometheus版本；此前没有可作为回退依据的Windows Prometheus版本                                                             |

运行目录：`.local/prometheus/releases/49c3ed46b9f6737c8913ad2eff78ee7bc20a8049/runtime`。入口 `http://127.0.0.1:8419/admin/`。初始化凭证通过包内脚本生成，只显示私有文件路径，没有输出凭证值；15分钟有效，过期可在本机重新生成。

本轮运行检查覆盖认证、一次性初始化、凭据、配置生效、记忆、人工日记 / 日程 / 人物 / Skill、合成模型文字链路及执行门禁。**未执行外部真实模型、QQ / 微信真实收发、媒体或隔离worker功能验收**；这些结果不能称为整个项目重写完成。

原始结构化证据：`.local/prometheus/release-validation/reports/49c3ed46b9f6737c8913ad2eff78ee7bc20a8049.json` 与同目录 `49c3ed46b9f6737c8913ad2eff78ee7bc20a8049-foreground.json`；桌面与手机截图在 `.local/prometheus/screenshots/`。这些是本地私有测试证据，不含真实凭据，不上传资源中心。

## 环境与备份

- Windows11专业版10.0.26200，x64；总内存约31.6GiB，初次可用约14.4GiB；D盘可用约413GiB。
- Node.js24.22.1、npm12.2.1、PowerShell7.6.5。旧Rust1.88.0和WindowsSDK10.0.26100.0仅核验，不参与新链路构建。
- CPU为Intel Core Ultra7 155H，64位，22个逻辑处理器；vswhere核实MSVC BuildTools安装于 `C:/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools`。新TypeScript构建无需调用这些原生编译工具。
- 旧源码 `cdec2ed1d3337dffd0f9e73ddbbb8d5b3de56379`；分支 `codex/prometheus-typescript`。
- 2026-10-30T22:23:43+08:00生成备份：`.local/prometheus/backups/legacy-2026-10-30T14-23-43-557Z/`，含Git bundle、差异、既有未跟踪业务代码和1个旧数据库一致快照，integrity_check为ok。
- Bundle SHA-256：`b3ab31365bc55b69041941d3778f3ded1d52e1abd8a178af50ba480526744bc5`。大型运行库和素材保留原位。

## 开发结果与失败保留

| 检查         | 结果                                                                     |
| ------------ | ------------------------------------------------------------------------ |
| 后端首次构建 | CLI重复name字段，修复后通过                                              |
| 前端首次构建 | `/admin/flame.svg`误作为模块导入；改为BASE_URL路径后通过                 |
| 首轮服务测试 | 测试工作区相对路径错误，13项无法启动，改为文件定位仓库根目录             |
| ACL首轮      | Windows PowerShell5无法加载Get-Acl，改为原生icacls检查，无安全策略更改   |
| 退出与Host   | 修复停止等待物理请求；Host测试使用真正发送自定义Host的HTTP客户端         |
| 后端回归     | 16项通过、0失败、0跳过；包含鉴权、配置、凭据、隔离、遗忘、队列和退出恢复 |
| 浏览器定位   | 修正初始化字段及aria-hidden装饰图标的可访问名称定位                      |
| 浏览器保存   | 实际发现Vue proxy不能structuredClone，改为JSON资料复制后修复             |

开发结果不代表真实外部模型、QQ / 微信收发或Windows worker隔离通过。页面标出未完成能力，API拒绝启用；没有自动批准、不可信插件执行或门禁放宽。

浏览器最终开发回归：5项通过、0失败。桌面1440px与手机390px截图已检查，无横向溢出、控制台错误或CSP错误。后端另覆盖重定向拒绝与异常套接字关闭；遗忘覆盖二级派生日记。类型检查与格式检查通过。
