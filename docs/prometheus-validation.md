# Prometheus 首轮 Windows 验收

日期：2026-10-30，Asia/Shanghai。本轮是新TypeScript项目基础版本，完整功能迁移尚未完成，见 [迁移表](prometheus-migration.md)。正式SHA和最终验收在本轮固定版本检查后追加，开发测试不计为发布。

## 环境与备份

- Windows11专业版10.0.26200，x64；总内存约31.6GiB，初次可用约14.4GiB；D盘可用约413GiB。
- Node.js24.22.1、npm12.2.1、PowerShell7.6.5。旧Rust1.88.0和WindowsSDK10.0.26100.0仅核验，不参与新链路构建。
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
