# DSH：树莓派上的 Rust AI 助手

部署目标是 **树莓派 5、8GB、Raspberry Pi OS Lite 64 位 / aarch64**。已实现并部署 M0 核心验证和 M1 文本 / Web 管理基础：Rust 服务、真实 Cordis 生命周期、SQLite 持久任务、原生模型 API、QQ 官方接入，以及静态 Vue 管理控制台。

用户已同意先在现有 Ubuntu 24.04 的树莓派上验证，保留 Lite 为最终基线；不能将 Ubuntu 测试记成 Lite 验收。实际执行结果见 [M0 记录](docs/m0-validation.md) 和 [部署 / 开发流程](docs/development-workflow.md)。

M0/M1原始部署与真实Harness / QQ联调已通过，历史证据见 [M0/M1验证记录](docs/m0-m1-validation.md)。后续角色卡M1.1的当前部署版本、回归与真实QQ验证独立记录在 [角色卡验收记录](docs/character-card-validation.md)，不将文档提交SHA当成运行版本。

## 当前功能范围

- 管理员一次性初始化、密码哈希、可撤销会话、Origin / CSRF 检查。
- 模型与 QQ 配置、受限密钥存储、配置草稿 / 校验 / 连接测试 / 版本应用 / 回退。
- 浏览器和 CLI 文本对话、独立会话历史、全局最多两个活跃 Run。
- SQLite 入站去重、持久队列与回复 Outbox；排队任务重启可恢复，在途结果未知时明确标记，不盲目重发。
- QQ 官方 WebSocket（默认）和签名 Webhook（可选），实际平台能力需由已授权账号联调。
- M0 的真实 Cordis 四组生命周期实验与真实 Harness ACP 探针。M1 生产聊天使用 NativeBackend，尚不启用完整 Harness 执行环境。
- 角色卡库、草稿 / 不可变发布版本、编译预览、独立测试、默认角色及会话固定绑定；JSON / 受限AIRI ZIP导入、JSON导出，QQ私聊按显式共享角色切换。
- 助手自身的稳定身份与显式长期记忆：共享 / 来源域私有、中文全文检索、版本纠正、遗忘、来源、JSON导出及Web管理；Web / CLI / QQ复用记忆命令和有界对话召回。实际验收见 [记忆记录](docs/memory-validation.md)。

ACP 通知属于已提交的语义输出，不承诺模型 token 流。M0 在途取消由临时回环 HTTP 代理确认真实上游请求仍未完成，再经私有 IPC 触发 ACP cancel；不以排队成功或已经结束的文本输出作为取消证据。代理不记录凭证、请求或回复正文。

自动记忆抽取、长会话摘要、微信、图像 / ASR / TTS、生成插件和完整 Harness 工具桥属于后续阶段。当前显式记忆不代表M2全部完成。完整设计见 [架构文档](docs/architecture.md)。

角色卡根据AIRI源码研究独立实现，采用CCv3文本内容与DSH模块偏好扩展；服务端版本、会话绑定和入队快照按本项目设计。见 [角色卡契约](docs/character-cards.md)、[Schema](docs/schemas/dsh-character-card-v1.schema.json) 与 [原创示例](docs/examples/campanus.character-card.json)。未引入AIRI依赖。临时源码尚在忽略目录中，删除操作被工具策略拒绝；实际状态见 [研究记录](docs/research/airi-character-card-review.md)。

在Web“角色卡”中新建或导入卡，保存草稿后校验 / 预览，发布版本；“设为新会话默认”与发布分别操作。QQ使用前勾选“允许QQ私聊用户选择此角色”，再私聊发送 `/角色`、`/角色 当前` 或 `/角色 切换 <角色名称或完整ID>`。同名卡须使用完整ID；群聊和频道不开放切换。换角色会开始新会话并保留旧记录，不改变其他用户或实例默认。

CLI新建指定人格会话可使用 `dshd chat --character-card <UUID> --character-revision <N> --message "你好"`；两个角色参数必须同时提供，不能和 `--conversation` 混用。无角色参数时，新会话使用当前默认，已有会话保留原绑定。

在“助手记忆”中管理属于助手的经历与事实；共享条目需明确确认，可跨Web / CLI / QQ使用，私有条目仅在来源账号域内使用，无需把人的跨平台账号绑定。私聊或Web可用 `/memory remember <内容>`、`/memory list`、`/memory show <ID>`、`/memory correct <ID>@<版本> <新内容>`、`/memory forget <ID>@<版本>`；命令不调用模型，群聊和频道只读，角色草稿测试会话不写记忆。遗忘会清空记忆正文 / 旧版 / 索引并停止后续上下文使用相关旧历史，原始聊天消息和离线备份按独立策略保留。详见 [记忆使用与边界](docs/memory.md)。

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
python -S tools/character-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/memory-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/tests/test_deploy_compatibility.py
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
