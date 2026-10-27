# 局域网 HTTPS 8086 部署与验收（2026-10-27）

用户要求局域网用户访问 Web，端口 8086，并明确选择 HTTPS / 局域网证书方案。正式入口为 `https://192.168.189.191:8086/admin/`，Rust 后端继续仅监听 `127.0.0.1:8419`。沿用认证、精确 Host / Origin、CSRF 和服务端授权，启用 Secure Cookie；不以局域网为由开放匿名管理。

## 实现与发布方式

`deploy/lan_https.py` 从已推送且干净的固定 Git 提交执行，使用 DSH 既有部署锁。Caddy 独立用户服务 `dsh-https.service` 绑定 `192.168.189.191:8086`，只接收 `192.168.189.0/24` 对端；关闭自动 HTTP 重定向、Caddy 管理接口与自动安装信任，不占用其他项目的 80 / 443，也不监听设备的全球 IPv6 地址。它转发到本机 Rust，保留真实 Host，让应用执行原有同源安全校验。

运行库固定为官方 Caddy 2.11.7 / linux_arm64，以 [官方发布](https://github.com/caddyserver/caddy/releases/tag/v2.11.7) 的 SHA512 校验，版本、下载地址和摘要见 `deploy/caddy-runtime.json`。独立进程内存上限 192 MiB、TasksMax 64，按需证书续期，不为用户或历史会话增加进程。

应用采用 systemd drop-in `dsh.service.d/10-lan-https.conf`，指定精确 HTTPS Origin 与 `--secure-cookie`。代码、静态资源和数据目录沿用已部署 release，不为运行参数变更重建 Rust。后续应用部署仍使用现有本机健康端口；drop-in 与证书位于 release 外，保留统一局域网入口。

变更前在实际 LAN 端口运行临时 Rust / Caddy，仅操作新建临时库，验证完整 TLS 链、IP SAN、静态页、匿名拒绝、临时账户初始化 / 登录态、Secure / HttpOnly / SameSite Cookie、Origin / CSRF 和注销。确认这两个自有进程退出后，再启用正式代理与后端设置。私有 CA key 留在 `shared/https/storage/`，仅通过 SSH 复制公有根证书，不复制私钥。

## 证书与访问

内部地址采用专用私有 CA，浏览器需要信任其根证书，证书续期由 Caddy 处理；依据 [Caddy 本地 HTTPS 文档](https://caddyserver.com/docs/automatic-https#local-https)。本次脚本不修改开发机或树莓派的系统信任库。

Windows 可双击通过本次 SSH 安全取得的 `DSH-LAN-Root-CA.cer`，选择“安装证书 → 当前用户 → 受信任的根证书颁发机构”。其他局域网设备分别导入同一公有根证书。只信任本次提供并核对摘要的证书，保留私钥在树莓派。原管理账户继续使用，仍须登录；不创建或重置生产密码。

变更后原 `http://127.0.0.1:18419/admin/` 不再是生产管理 Origin；开发模式的原 loopback 默认值保留。HTTPS / Secure Cookie / Origin 必须作为一组恢复，不能单独改端口后关闭鉴权。

## 检查与实际结果

准备时确认树莓派 5 / 8GB / Ubuntu 24.04.4 LTS / aarch64，8086 空闲，现有 80 / 443 属于其他服务，不修改。正式应用是 `6e8524a21f2813a802a2f99254c56316dac15bd5`，服务 active、工作区干净。本地五项配置边界检查通过；首轮 Windows 渲染把 PosixPath 显示为反斜杠，三项静态检查失败，已用统一 POSIX 表示修复，未放宽网络或安全条件。记录 `.local/lan-https/local-unit-final.log`。

配置提交、远端 Caddy 校验、实际 TLS / 认证预演、正式启动、客户端访问与证书摘要在执行后追加，不提前计作通过。

## 恢复

脚本切换前为三个管理文件保存逐字备份与 manifest，失败时恢复原文件、代理启用 / 运行状态并重新启动原后端。业务数据库、性格、模型配置及真实凭据不参与文件替换。备份位置写入 `shared/config/lan-https.json` 的 `backup_manifest`；不得删除证书存储来切换应用版本，避免每次发布要求用户重新信任 CA。

若首次启用后需要恢复原 SSH 入口：停止并禁用 `dsh-https.service`，移除已核实归本脚本管理的 `10-lan-https.conf`，daemon-reload 后重启 `dsh.service`，核对本机 8419 健康及原 18419 SSH 隧道；不删除业务数据或私有 CA。以后已有 HTTPS 配置时，应按备份 manifest 恢复具体上一配置，不盲目套用首次恢复步骤。
