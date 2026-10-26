# 刘嘉卉性格与展示文案验收

日期：2026-10-26（Asia/Shanghai）。本轮按用户要求将“助手与模型”改为“性格与模型”，用人类性情的方式重新设计刘嘉卉，并调整管理页面中的固定身份称呼。设计契约见 [性格与展示约定](liu-jiahui-personality.md)，完整六字段见 [刘嘉卉性格 v1](examples/liu-jiahui-personality-v1.json)。历史验收报告保持原样。

## 改动范围

- 导航、页标题、面包屑统一使用“性格与模型”；编辑区使用“她的性格”“姓名”“个人简介”“性情与脾气”“相处情境”“相处约定”和“对话示例”。登录、品牌、运行总览、记忆、插件、日志、状态提示与后端控制回复同步调整固定文案。
- 刘嘉卉取名于《诗经·小雅·四月》“山有嘉卉”。性格温柔、明朗、有韧性，关心具体处境，有主见而不刻薄，亲近而有分寸。包含十二组原创表达示例，不预设年龄、职业、现实身体、恋爱关系或共同生活经历。
- 六字段不使用“助手”“AI”“工具”等机械身份词。事实、记忆、完成状态与送达状态仍以真实可见记录为依据，示例不作为历史事实；内部安全编译、权限检查与来源隔离保留。API 路径、协议字段与原始审计信息保持真实，不改写用户内容和历史正文来消除字串。
- 新安装没有既有性格、默认卡或自定义配置时使用完整的刘嘉卉性格。已保存性格优先，其次沿用既有默认卡，自定义名称与提示原文保留；升级不会自动覆盖这些内容。生产性格须通过认证、CSRF 与 `If-Match` 显式保存，对后续入队请求生效。
- 保留稳定身份、记忆归属、模型配置与原历史 Run。已完成 Run 保留原快照；历史性格恢复产生新 revision。程序回滚、性格恢复与数据恢复分别处理。
- 内置“制作 Skill”和“设置触发任务”的中文说明改用“操作”“能力”等措辞，协议字段及权限限制不变。本轮不以数据库迁移覆盖已保存的 Skill；生产实例是否更新仍须核对当前内容并通过管理 API 保存新版本。

## 本地检查

本地测试使用 Windows 开发环境、独立临时数据库、合成管理账户与回环 HTTP provider，不读取生产凭证，不向 QQ / 微信发送验收消息。以下检查均已完成；模拟 HTTP 请求不作为真实外部模型验收。

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 退出 0 |
| `cargo test --workspace --locked` | 303 daemon + 4 kernel，共 307 项通过 |
| `cargo build -p dshd --locked` | 退出 0 |
| `npm.cmd test --prefix web/admin` | 134 项通过 |
| `npm.cmd run build --prefix web/admin` | 类型检查与生产静态构建通过 |
| `python -S -m unittest discover -s tools/tests` | 91 项通过 |
| `tools/assistant-logs-smoke.py` | 19 项隔离实际进程 / 模拟 HTTP 检查通过 |
| `tools/personality-presentation-smoke.py` | 11 项隔离实际进程 / 模拟 HTTP 检查通过 |
| `tests/e2e/assistant-runtime.spec.ts` | 1 个完整浏览器场景通过，生成 7 张截图；无失败、跳过或 pageerror |

`assistant-logs-smoke.py` 验证匿名访问、CSRF、性格字段校验与版本冲突、旧会话下一轮性格投影、旧 Run 快照、日志过滤与分页、私有 Web owner 隔离、凭证隐藏、历史恢复及重启持久化。

新增 `personality-presentation-smoke.py` 验证六字段完整性与刘嘉卉姓名、字段禁词、认证写入与 409、真实回环 HTTP 请求中的性格和示例回复、已完成快照不变、稳定身份及全部既有记忆保留、历史恢复和重启持久化。本地报告分别位于忽略目录 `.local/liu-jiahui-logs-local.json` 与 `.local/liu-jiahui-presentation-local.json`，均标记 `commit: development`。最终测试二进制 SHA-256 为 `a797c5221625fcb2a52f5b8c47fa14f1ad9bfd2afdb06d47b424972a901ea02e`；这些结果不能代替正式 Linux 发布二进制验收。

浏览器使用独立的 `127.0.0.1:18421` 实例与回环 provider，验证初始刘嘉卉六字段、页面固定文案与输入框值无指定身份词、保存即生效、跨窗口冲突保留草稿、历史恢复、日志筛选 / 展开，以及 320 / 390px 无横向溢出。浏览器存储为空。七张截图位于忽略目录 `.local/liu-jiahui-e2e/screenshots/`，包括初始性格桌面图、保存后性格与日志桌面图，以及两个窄屏宽度下的性格和日志图；已检查初始桌面图与 320px 性格图，页面及控件无裁切。夹具控制进程与子进程已停止，18421 端口已释放。

Rust 针对性检查确认默认构建链没有递归：`AssistantConfig::default` 从捆绑资产构造卡并编译；卡模板和编译器不回调配置默认值。已有性格、默认卡与自定义配置优先级保持，首次读取不偷偷生成已保存性格或历史版本。新资产 JSON 和内置任务 Skill JSON 静态解析通过，任务协议字段保留。

内置 Skill 的最后一轮纯措辞调整发生在本地全量 Rust 测试后；之后的新 debug build 与 19 + 11 项进程检查均使用调整后的源码。正式远端已对完整发布源码重新执行全量测试，**304 daemon + 4 kernel** 全部通过。

本地浏览器结构化结果见 [浏览器证据](validation/liu-jiahui-browser-local.json)。

## 发布标识与远端结果

代码提交为 `e4c955eda1ca7fa01062470f0cad5313e79f6a31`，已推送并通过本地 `git ls-remote` 与远端仓库 `origin/main` 核实。本地打包前端档案 SHA-256 为 `99c285c07bd5b3b2d6ec5940513f2c3dc782d9657315d94fd0c14c92b377b2d0`，与同一提交的 Rust 源码一起交付。

部署前通过 SSH 核验 Raspberry Pi 5 Model B Rev 1.1、7,933 MiB 内存、Ubuntu 24.04.4 LTS、aarch64，Rust / Cargo 1.88.0。目录归属 campanus，repo 工作区干净且 origin 匹配。前端在 Windows 构建后打包，目标主机的当前非交互 PATH 未找到 Node，运行静态前端不依赖它；本轮未改系统环境。

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-e4c955e.sh \
  e4c955eda1ca7fa01062470f0cad5313e79f6a31 \
  /home/campanus/.cache/dsh/admin-e4c955eda1ca7fa01062470f0cad5313e79f6a31.tar.gz \
  99c285c07bd5b3b2d6ec5940513f2c3dc782d9657315d94fd0c14c92b377b2d0
```

目标为 `campanus@192.168.189.191`，上一实际运行提交为 `1bce688edb4d22573168ba893eede785d4c143b3`。**2026-10-26 15:36:18 +08:00** 完成正式切换，部署命令退出 0。发布构建耗时 8 分 17 秒，二进制 SHA-256 为 `ab36210a893998944d2e38fd3b6fbbb02be9720d4439fcbf9dc63c4ab0f3a388`。源码 HEAD、前端 `release.json` 与 `/health/ready` 均为完整代码 SHA，release 源码工作区干净，用户级 `dsh.service` active。构建期间原版本保持服务，切换后的首次就绪探测短暂连接失败，随后在脚本重试窗口内 ready；未把首次探测失败当作最终部署失败或通过证据。

源码目录为 `/home/campanus/apps/dsh/releases/e4c955eda1ca7fa01062470f0cad5313e79f6a31/source`。部署脚本执行 `cargo test --workspace --locked -j 1` 与 `cargo build --release --locked -p dshd -j 1`，均退出 0；日志分别在 `shared/logs/test-<完整SHA>.log` 与 `build-<完整SHA>.log`。详细版本、制品、备份及数据核对见 [部署证据](validation/liu-jiahui-deployment-aarch64.json)。

## 远端功能与真实模型验证

以下脚本从本次 release 的 `current/source/tools/` 执行，使用 `--binary /home/campanus/apps/dsh/current/bin/dshd --web-dir /home/campanus/apps/dsh/current/web`，均退出 0。

| 脚本 | 结果及证据 |
| --- | --- |
| `assistant-logs-smoke.py --report shared/logs/liu-jiahui-logs-e4c955e.json` | [19 项通过](validation/liu-jiahui-logs-aarch64.json)，临时数据库与回环模型 |
| `personality-presentation-smoke.py --report shared/logs/liu-jiahui-presentation-e4c955e.json` | [11 项通过](validation/liu-jiahui-presentation-aarch64.json)，临时数据库与回环模型 |
| `conversation-presence-live.py --source-data-dir /home/campanus/apps/dsh/shared/data --personality current/source/docs/examples/liu-jiahui-personality-v1.json --report shared/logs/liu-jiahui-live-e4c955e.json` | [6 次真实模型请求、21 项检查通过](validation/liu-jiahui-live-aarch64.json) |

表中 report 与 personality 的短路径以 `/home/campanus/apps/dsh/` 为根。真实模型脚本只读取生产模型配置及对应凭证，把刘嘉卉性格和六组合成输入放在独立临时实例；关闭渠道、语音与生图，不读取真实聊天 / 记忆，不写生产数据库。模型在 15:37:29–15:37:36 +08:00 返回真实日期、区分引用的伪造时间、按来源记录理解“昨天”、计算 17×19=323，并在未知出生日期时明确不知道。疲惫且不想继续的输入收到简短收尾，没有追问或列表。已审阅六条合成回答；这些检查不等于所有场景自然度或未来架构目标均通过。

正式访问来源下，性格、性格历史、日志三个未认证 API 均返回 401；不在允许列表的 Host 返回 403。浏览器固定文案与本轮六字段不含指定身份词，未隐藏原始用户输入、历史审计正文或协议标识。

## 生产配置与数据保留

通过现有已认证 Edge 管理会话填写并保存六字段，服务端沿用 CSRF 与 `If-Match`；**性格 v2 已保存并生效**。刷新后六字段与发布 JSON 逐字一致，包含全部十二组示例。页面导航、面包屑与标题为“性格与模型”，品牌为“日常空间”，记忆入口为“记忆”。正式截图位于本机忽略目录 `.local/liu-jiahui-production/01-liu-jiahui-applied.png`，截图摘要保存在部署证据。

“制作 Skill”和“设置触发任务”在保存前分别与旧发布版本的完整指令逐字比较，确认没有自定义修改；随后仅替换中文称谓，通过管理页保存为 v2。说明内容与新发布资产一致，启用状态、执行方式、协议字段与原任务快照保持。未新增执行权限、未运行这两个 Skill、未修改自定义内容；页面结果截图为同目录 `02-skill-copy-applied.png`。

保存前后稳定身份摘要相同，模型配置仍为 v5，均为 **4 个会话、30 个 Run、59 条消息、0 条记忆**。未向生产会话添加测试消息，原性格 v1 / v0 仍可从历史恢复；新性格用于后续入队请求，既有排队 / 完成快照保留。检查结束只有一个生产 dshd 进程，可用内存采样为 7,310 MiB。

上一版本仍保留在 `releases/1bce688edb4d22573168ba893eede785d4c143b3`，切换前私有 SQLite 备份为 `/home/campanus/apps/dsh/shared/backups/pre-e4c955eda1ca7fa01062470f0cad5313e79f6a31-20261026T073616536430129.sqlite`。本轮没有执行生产代码回滚或数据恢复。性格恢复应读取历史字段并用当前 etag 保存成新版本；程序回滚先执行兼容检查，不能通过删除用户数据绕过门禁。

本轮未发送真实 QQ / 微信验收消息，未验证 ASR、新媒体能力、完整 M4 / M5 或未来日记 / 日程系统；既有历史验收保持原记录。最后的验收文档与证据提交只含文档，不为它重新启动应用；实际运行代码仍为上述 e4c955e 完整 SHA。
