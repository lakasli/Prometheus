# DSH：树莓派上的 Rust AI 助手

部署目标是 **树莓派 5、8GB、Raspberry Pi OS Lite 64 位 / aarch64**。已实现并部署 M0 核心验证和 M1 文本 / Web 管理基础：Rust 服务、真实 Cordis 生命周期、SQLite 持久任务、原生模型 API、QQ 官方接入，以及静态 Vue 管理控制台。

用户已同意先在现有 Ubuntu 24.04 的树莓派上验证，保留 Lite 为最终基线；不能将 Ubuntu 测试记成 Lite 验收。实际执行结果见 [M0 记录](docs/m0-validation.md) 和 [部署 / 开发流程](docs/development-workflow.md)。

当前运行提交为 `17b7ee0`。树莓派上4项Cordis / 10项后端测试、19项隔离黑盒（含CLI）通过；真实Harness模型回复、HTTP在途取消与重启恢复通过，QQ私聊已完成两轮真实模型回复和平台发送回执，用户已确认收到。前端16项单测及真实Chromium端到端通过。完整范围、限制与脱敏证据见 [M0/M1验证记录](docs/m0-m1-validation.md)。

## 当前功能范围

- 管理员一次性初始化、密码哈希、可撤销会话、Origin / CSRF 检查。
- 模型与 QQ 配置、受限密钥存储、配置草稿 / 校验 / 连接测试 / 版本应用 / 回退。
- 浏览器和 CLI 文本对话、独立会话历史、全局最多两个活跃 Run。
- SQLite 入站去重、持久队列与回复 Outbox；排队任务重启可恢复，在途结果未知时明确标记，不盲目重发。
- QQ 官方 WebSocket（默认）和签名 Webhook（可选），实际平台能力需由已授权账号联调。
- M0 的真实 Cordis 四组生命周期实验与真实 Harness ACP 探针。M1 生产聊天使用 NativeBackend，尚不启用完整 Harness 执行环境。

ACP 通知属于已提交的语义输出，不承诺模型 token 流。M0 在途取消由临时回环 HTTP 代理确认真实上游请求仍未完成，再经私有 IPC 触发 ACP cancel；不以排队成功或已经结束的文本输出作为取消证据。代理不记录凭证、请求或回复正文。

记忆管理、微信、图像 / ASR / TTS、生成插件和完整 Harness 工具桥属于后续阶段，界面不将它们伪装成已可用功能。完整设计见 [架构文档](docs/architecture.md)。

## 本地运行

需要 Rust 1.88、C 编译工具链、Node.js 和 npm。前端只在构建阶段需要 Node，生产静态页面由 Rust 提供。

```powershell
npm.cmd ci --prefix web/admin
npm.cmd run build --prefix web/admin
cargo test --workspace --locked
cargo run -p dshd -- bootstrap-token --data-dir .local/dev
cargo run -p dshd -- serve --data-dir .local/dev --origin http://127.0.0.1:8419
```

打开 `http://127.0.0.1:8419/admin/`，在本地私有的 `.local/dev/bootstrap-token.json` 中读取 15 分钟有效的一次性初始化凭证，创建管理账户。它不使用 Linux SSH 密码，也没有默认管理口令。然后录入自己的模型 API key，保存草稿、校验、连接测试并应用。

凭证文件只供本机用户读取，不要将其上传、粘贴进日志或提交到 Git。模型地址默认要求 HTTPS，拒绝私网地址；本地模拟模型测试显式使用 `--allow-private-model`，生产部署不开启该例外。

## 检查与验收

```powershell
npm.cmd test --prefix web/admin
npm.cmd run build --prefix web/admin
cargo test --workspace --locked
python -S tools/smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
```

`tools/smoke.py` 会启动真实 Rust 进程、临时数据库和隔离的模拟模型，验证认证、配置、对话去重及重启恢复；它不会使用生产数据或凭证，不能代替真实 DeepSeek / QQ 联调。Linux 将 binary 参数改为实际可执行文件路径。

## 树莓派部署和远程访问

遵循 [AGENTS.md](AGENTS.md)：本地编写 → 检查 → Git 提交推送 → 固定 SHA 部署 → 远端测试。

- 部署脚本：[deploy/deploy.sh](deploy/deploy.sh)，使用用户级 systemd，数据与 release 分离。
- 前端打包：[deploy/package-web.ps1](deploy/package-web.ps1)，为已提交 SHA 构建静态制品并输出 SHA256。
- 回滚：[deploy/rollback.sh](deploy/rollback.sh)，只切换已有应用 release，不还原数据库或外部动作。
- 操作步骤及真实部署信息：[开发与部署流程](docs/development-workflow.md)。

远端默认监听 loopback。在用户电脑建立隧道后打开管理页面：

```sh
ssh -N -L 18419:127.0.0.1:8419 campanus@192.168.189.191
```

访问 `http://127.0.0.1:18419/admin/`。若改为直接通过 HTTPS 域名访问，使用受控反向代理，保持前后端同源，配置对应 `--origin https://<管理域名>` 和 `--secure-cookie`；不要将管理端口直接暴露为普通局域网 HTTP。

QQ 按 [最新官方指南](https://bot.q.qq.com/wiki/) 设置连接方式：个人树莓派默认选择 WebSocket，在页面录入 AppID / AppSecret，按实际账号权限订阅和测试；Webhook 才需要额外配置公网 HTTPS 回调。
