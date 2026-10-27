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

本地 tools/tests **140 项通过**，5.309 秒；Linux 五项配置专项通过。配置初版 `688957e616e02ee4d3aa7e8d195ab0bf830ce741` 已推送并从独立干净工作树执行：实际 TLS / 登录 / CSRF / 注销七项通过，但错误 Host 落入 Caddy 的默认空 200，未进入上游，不满足明确拒绝的预期。原失败报告 `shared/logs/lan-https-688957e616e02ee4d3aa7e8d195ab0bf830ce741-preflight.json`、`.process.log` 与 `lan-https-apply-688957e.log` 保留，正式服务未切换。没有放宽到任意 200 或删除测试。

本地加入错误 Host 的显式兜底 403，提交 / 推送为 **`09db6d73f44bc431a49ce3331db84030a51ea995`** 后重做同流程，预演 **9 项全部通过**，两个临时进程实际退出，再应用正式配置。实际命令：

```sh
python3 -u /home/campanus/.cache/dsh/lan-source-09db6d7/deploy/lan_https.py --expected-script-commit 09db6d73f44bc431a49ce3331db84030a51ea995
```

北京时间 **2026-10-27 10:31:15** 完成，退出 0。`dsh.service` 与 `dsh-https.service` 均 active/running，PID 分别 1342776 / 1342786，NRestarts 均 0；代理已启用开机启动。监听实测仅 `127.0.0.1:8419` 与 `192.168.189.191:8086`。应用与静态资源继续为 **`6e8524a21f2813a802a2f99254c56316dac15bd5`**，没有把参数变更冒充新应用构建。实际 profile 为 `shared/config/lan-https.json`；配置校验退出 0，保留了 Caddyfile 空格排版提示，它不是校验失败。

运行库实测 `v2.11.7 h1:yj0Y4fYZGPkSvibBJ1sTWE33xC0fxztVyXEW5iIdUT4=`，可执行文件 SHA256 `0019d864a2cdee41c3675cb2a26185833c5d9540a48f5a6729142c0413461b60`；下载归档按本仓库的固定 SHA512 验证。首次 GitHub API 元数据请求遭公共限额，未依赖其失败结果；改从官方固定 release checksums 文件获取摘要，本机与树莓派两次读取一致，再用该摘要验证实际下载。

09db 的完整预演报告为 `shared/logs/lan-https-09db6d73f44bc431a49ce3331db84030a51ea995-preflight.json`，其 `.process.log` 记录实际临时进程启动 / 退出；切换日志 `lan-https-apply-09db6d7.log`。临时账户、令牌、Cookie 和业务库均为全新合成资料，没有使用或重置生产管理员密码。

10:32 从当前 Windows 电脑**直接经局域网**连接，独立指定专用 CA，严格校验证书链及 IP SAN，不使用 TLS 忽略参数：ready / 静态页面 200、未登录配置 API 401、外域 Origin 403、错误 Host 403，五项通过。记录 `.local/lan-https/windows-lan-check.json`。没有登录生产或提交配置操作。

只读浏览器检查通过，1440 / 390 视口无横溢、JS error 为 0，两张真实登录页截图已目视核对，证据 `.local/lan-https/browser-check.json`、`lan-login-{1440,390}.png`。浏览器临时上下文仅为未安装 CA 的截图跳过证书错误；**不能据此声称客户端系统已经信任证书**。证书链另由 Python 与 Node 两个严格客户端验证，工具未安装 OS 根证书、未输入生产密码或发送写请求，浏览器进程已关闭。

公有根证书通过已认证 SSH 取得，文件 `.local/lan-https/DSH-LAN-Root-CA.cer`（DER）及 `.crt`（PEM）。DER SHA256 / 根证书指纹为：

```text
8D:33:D4:3A:41:76:43:C5:9A:57:2F:30:24:48:AC:BE:5B:29:73:07:35:A5:A5:19:92:81:62:4B:12:E4:A9:AA
```

服务端 `root.key` / `root.crt` 实测 owner campanus、mode 600，私钥未复制。10:35 以 SQLite mode=ro / query_only 核对配置、性格 / 稳定身份、日记来源策略与日程授权四个 SELECT-rowset SHA256，与切换前完全一致。前后报告 `shared/logs/lan-https-before-688957e.json`、`lan-https-after-09db6d7.json`；副本在 `.local/lan-https/remote-evidence/`，profile 和原失败也保留。

正式配置一次成功，未触发生产回滚。后续脚本异常恢复调整为先禁用新代理 unit、再删除 / 恢复文件，避免先移除 unit 后留下启用链接；该顺序修订不改变上述已安装配置。首次恢复入口的三个文件均在 `shared/backups/lan-https-09db6d73f44bc431a49ce3331db84030a51ea995-20261027T023114440846Z/manifest.json` 登记。实际安装数据 / 证书目录未清理，CA 随服务持久续期。

## 恢复

脚本切换前为三个管理文件保存逐字备份与 manifest，失败时恢复原文件、代理启用 / 运行状态并重新启动原后端。业务数据库、性格、模型配置及真实凭据不参与文件替换。备份位置写入 `shared/config/lan-https.json` 的 `backup_manifest`；不得删除证书存储来切换应用版本，避免每次发布要求用户重新信任 CA。

若首次启用后需要恢复原 SSH 入口：停止并禁用 `dsh-https.service`，移除已核实归本脚本管理的 `10-lan-https.conf`，daemon-reload 后重启 `dsh.service`，核对本机 8419 健康及原 18419 SSH 隧道；不删除业务数据或私有 CA。以后已有 HTTPS 配置时，应按备份 manifest 恢复具体上一配置，不盲目套用首次恢复步骤。
