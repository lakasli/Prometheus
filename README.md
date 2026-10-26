# DSH：树莓派上的 Rust AI 助手

正式部署目标是 **`192.168.189.191` 现有树莓派 5、8GB、Ubuntu 24.04.4 LTS / aarch64**。已实现并部署 M0 核心验证和 M1 文本 / Web 管理基础：Rust 服务、真实 Cordis 生命周期、SQLite 持久任务、原生模型 API、QQ 官方接入，以及静态 Vue 管理控制台。

用户2026-10-24已确认现有Ubuntu为正式基线，不再要求迁移Raspberry Pi OS Lite，也不重装或改变当前主机环境。历史记录中的Lite目标及Ubuntu例外保留原事实，由本次决定取代。实际执行结果见 [M0 记录](docs/m0-validation.md) 和 [部署 / 开发流程](docs/development-workflow.md)。

M0/M1原始部署与真实Harness / QQ联调已通过，历史证据见 [M0/M1验证记录](docs/m0-m1-validation.md)。历史角色卡M1.1的部署版本、回归与真实QQ验证独立记录在 [角色卡验收记录](docs/character-card-validation.md)，不将文档提交SHA当成运行版本。

## 当前功能范围

- 日程：月 / 周 / 日与议程、重复系列与例外、改期 / 暂停 / 取消、旧触发任务兼容、自主范围与模型预算、授权文本阅读和跨时段续读。视频理解、主动投递与未注册动作保持明确阻断，见 [日程契约](docs/schedule-system.md)。
- 日记与资料：显式事件、来源与模型出域授权、每日兜底、日 / 周 / 月记、事实与感受分栏、来源分页、FTS5 与近期日记检索、更正 / 遗忘和旧记忆兼容迁移。周 / 月按固定日记版本分批归并，保存恢复检查点；超出单日容量或模型预算时显示等待 / 阻断，见 [日记契约](docs/diary-system.md)。
- 人际关系：稳定人物、待审字段与有向边、同名区分、会话身份、跨账号同人确认、同域合并 / 拆分、版本化好感规则及来源失效。联系许可不自动授予发送权，见 [人物契约](docs/relationships-system.md)。本轮实际部署与验收见 [独立记录](docs/life-systems-validation.md)，不等于完整睡眠、好友观察或全部 M2 已交付。
- 管理员一次性初始化、密码哈希、可撤销会话、Origin / CSRF 检查。
- Skill 管理、版本冻结和显式对话调用，默认「制作 Skill」「绘画」与「设置触发任务」；生成草稿经审核保存，绘画按已配置能力走原生 PNG 或隔离 HTML / SVG。契约与验证边界见 [Skill 说明](docs/skills.md)。
- 模型与 QQ 配置、受限密钥存储、配置草稿 / 校验 / 连接测试 / 版本应用 / 回退。
- CLI 与渠道文本对话、独立会话历史、全局最多两个活跃 Run；Web 管理页查看运行日志。
- Rust / WIT / Wasmtime Component 插件工厂：生成函数体、隔离构建、独立测试、摘要审批、版本启用 / 回滚与显式 `/plugin` 调用。当前只开放空权限无状态工具；完整 broker 与模型自动调用循环仍按原架构推进。见 [插件工厂](docs/plugin-factory.md) 与 [实机 / 单次真实模型验收](docs/plugin-factory-validation.md)。
- 普通对话的逐句 JSON 回复计划、1–4条上限、账号与会话持久发送间隔；图片 / 视频私有制品和按需本地 Piper 中文 TTS，富媒体优先适配 QQ C2C。契约见 [逐句与媒体](docs/structured-replies.md) 和 [Piper](docs/speech.md)，部署、真实 QQ 客户端和未测范围见 [本轮验收](docs/voice-plugins-validation.md)。
- QQ 当前官方非群 API 固定白名单管理、参数校验、只读探针及逐项结果状态。见 [范围与文档](docs/qq-non-group-api.md)；协议模拟覆盖不等同于账号获权或真实平台操作通过。
- SQLite 入站去重、持久队列与回复 Outbox；排队任务重启可恢复，在途结果未知时明确标记，不盲目重发。
- 会话身份与来源版本冻结，历史按实际投递句段构建；明确私聊纯文本更正可阻止尚未发布的旧候选，摘要与日记复用投递证据。范围、兼容回退与模拟 / 真实验收见 [会话协调](docs/conversation-coordination.md) 和 [首批记录](docs/conversation-coordination-validation.md)。
- [短期焦点与纠正](docs/conversation-state.md)、[会话约定资料审核](docs/conversation-commitments.md)及[普通文字上下文预算](docs/context-budget.md)：同来源发言者隔离、封闭明确续接 / 收尾、版本与 TTL 失效、完整必需输入、块选择及只读原因。管理审核不冒充来源同意、日程建立或实际履行；模型窗口和 token 明确区分未知、声明与估计。新切片发布与验收见 [记录](docs/conversation-state-validation.md)。睡眠、真实主动联系与 Python 动作继续独立推进。
- QQ 官方 WebSocket（默认）和签名 Webhook（可选），实际平台能力需由已授权账号联调。
- QQ C2C 连续普通短文本可在 3.5 秒静默 / 最长 8 秒窗口内合并为一轮，保留每条来源及去重；命令、媒体与性格 / 配置变更分界。规则与部署证据见 [连续短文本](docs/qq-text-batches.md)。
- M0 的真实 Cordis 四组生命周期实验与真实 Harness ACP 探针。M1 生产聊天使用 NativeBackend，尚不启用完整 Harness 执行环境。
- 唯一助手性格在“助手与模型”编辑，带版本冲突检测，对后续 Run 生效；历史角色卡与快照保留，不再支持多角色切换。
- 助手自身的稳定身份与显式长期记忆：共享 / 来源域私有、中文全文检索、版本纠正、遗忘、来源、JSON导出及Web管理；Web / CLI / QQ复用记忆命令和有界对话召回。实际验收见 [记忆记录](docs/memory-validation.md)。
- 同来源长会话增量摘要：复用现有任务槽、来源与覆盖水位、敏感资料省略、超长轮次明确摘录、取消恢复和遗忘联动；管理页可查看并重建派生摘要。独立验收见 [摘要记录](docs/summary-validation.md)。
- 会话记忆候选：明确勾选来源消息、模型提取、审核后私有入库，拒绝 / 到期 / 遗忘清除候选正文，不自动共享或覆盖事实。当前发布与验证状态见 [候选记录](docs/memory-candidate-validation.md)。
- 微信 iLink 单账号文字私聊适配：管理页扫码、本人来源限制、原子游标 / 入站去重、冻结回复上下文、凭据失效与不确定发送处理。实现与模拟验证见 [微信说明](docs/weixin.md) 和 [验收边界](docs/weixin-validation.md)；真实手机扫码与收发仍待验收。
- PNG 视觉后端保留格式校验、实际解码、去元数据、DeepSeek-flash 与 Native 上下文回答；原 Web 单 PNG 对话界面验收见 [历史图片对话记录](docs/conversation-images-validation.md)。当前 Web 工作台已改为日志，相关运行与视觉事件在日志查看。见 [图片对话契约](docs/image-analysis.md)。

管理页面按 [架构的信息架构](docs/architecture.md#143-页面与用户操作) 组织。“角色卡”并入“助手与模型”，“对话工作台”改为只读“日志记录”，删除“任务与诊断”入口。日志展示输入 / 输出、Run、工具、视觉、语音合成与投递等实际运行事件；ASR / subagent 尚未接入。旧 WAT 与独立图片诊断 API 保留原权限边界，不再挂页面入口。契约与本轮验收见 [单助手与运行日志](docs/assistant-runtime-logs.md)。

ACP 通知属于已提交的语义输出，不承诺模型 token 流。M0 在途取消由临时回环 HTTP 代理确认真实上游请求仍未完成，再经私有 IPC 触发 ACP cancel；不以排队成功或已经结束的文本输出作为取消证据。代理不记录凭证、请求或回复正文。

后台自动事实抽取、微信真实平台收发验收、微信渠道媒体、ASR、完整插件工厂和Harness工具桥仍待交付；Piper与QQ富媒体本轮实现及验收单独记录；历史Web单PNG图片对话曾部署并通过独立验收，当前管理页已改为日志；相关后端保留，不代表完整M4。WAT开发诊断原型不代表M5，现有记忆与微信切片也不代表M2全部完成。后续按 [架构文档](docs/architecture.md) 的功能与信息架构推进，重大偏离先说明并由用户确认，不改写目标来迁就原型。

「任务触发」已实现正式管理 tab、持久调度与默认 [设置触发任务 Skill](docs/skills/task-trigger-setup.md)：一次性时刻 / 启用后秒延迟、固定秒间隔、日历日期、中国工作日 / 休息日，以及本人交互 Run 完成 / 失败与服务启动事件。模型仅生成待审核草稿，保存停用后须预演并明确启用；固定提醒或隔离助手任务的结果保存在认证 Web 页面。当前内置已核验的 2026 年中国日历，未知年份阻断，不发送 QQ / 微信主动消息或接受外部 webhook。契约见 [详细设计](docs/task-triggers.md) 和 [Schema](docs/schemas/dsh-task-trigger-v1.schema.json)，本地检查及正式部署状态见 [运行验收](docs/task-triggers-validation.md)；先前 [静态验证](docs/task-triggers-design-validation.md) 仅为历史设计证据。

角色卡根据AIRI源码研究独立实现，采用CCv3文本内容与DSH模块偏好扩展；服务端版本、会话绑定和入队快照按本项目设计。见 [角色卡契约](docs/character-cards.md)、[Schema](docs/schemas/dsh-character-card-v1.schema.json) 与 [原创示例](docs/examples/campanus.character-card.json)。未引入AIRI依赖。临时源码尚在忽略目录中，删除操作被工具策略拒绝；实际状态见 [研究记录](docs/research/airi-character-card-review.md)。

在 Web“助手与模型”中编辑唯一性格，保存后下一个入队的 Run 使用新版本，已有会话也遵循该规则。首次沿用原默认角色内容；没有默认卡时使用原助手设置。旧角色卡写入 / 选择 API 已停用，QQ `/角色` 仅提示使用单一助手。

CLI 使用 `dshd chat --message "你好"`，或用 `--conversation <ID>` 续接历史，均使用当前助手性格，不再提供角色选择参数。CLI 与渠道对话后端保留；Web 管理页专注配置与日志。

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
python -S tools/assistant-logs-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/memory-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/summary-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/task-triggers-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
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
