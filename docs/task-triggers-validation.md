# 任务触发运行验收

日期：2026-10-25，Asia/Shanghai（+08:00）。本记录覆盖「任务触发」正式 tab、持久时间 / 事件调度、隔离助手任务及默认「设置触发任务」Skill。用户在详细设计交付后授权实现、检查、提交推送、树莓派部署和远端验收；前一阶段的 [设计静态验证](task-triggers-design-validation.md) 保留原历史事实。

**当前记录状态：本地检查、提交推送、树莓派固定 SHA 部署和远端验收均已完成。正式运行提交为 `04fb8277e26dfa81c6e62599535c9e1aa704759c`。** 上一正式运行版本见 [Skill 与角色卡验收](skills-validation.md)，不把开发中二进制或文档提交当成已部署版本。

## 实现范围

| 功能 | 本轮行为 |
| --- | --- |
| 正式页面 | 分页任务列表与详情、保存停用草稿、校验 / 预演、应用启用、暂停、停止并取消当前、恢复、归档、真实手动执行与结果历史；版本冲突保留待审核内容 |
| 时间规则 | 一次性绝对时刻、首次启用后 N 秒、固定秒间隔；每日、每周、每月指定日 / 月末、每年、显式日期集；IANA 时区、有效窗口、DST 缺失跳过 / 重叠只第一次 |
| 中国日历 | 持久、版本化的 `cn-official` 2026 年法定放假与调休；管理页导入草稿、来源 / 差异审核及摘要核对后应用。未知年份阻断，绝不回退为普通周末 |
| 内部事件 | 本人交互 Run 完成 / 失败、实例服务启动；可信来源、允许字段筛选、启用水位、去重、冷却、有界队列；自动任务结果不继续触发自动链 |
| 执行动作 | 固定提醒，或冻结角色 / Skill 的助手任务；输出支持 text、HTML 绘画与已声明能力的原生 PNG。结果仅存认证 Web 页面，图片按所有权接口读取 |
| 设置 Skill | 独立迁移标记安装第三个默认 Skill，向模型提供当前时间和服务器能力；JSON 草稿经对话审核入口转入任务表单，保存后仍停用，须再次预演并明确启用 |
| 授权与上下文 | 管理认证、Origin / CSRF、owner / source 范围、版本与幂等检查；启用签发持久授权，派发前重新检查；每次模型任务使用隔离上下文，不读取普通聊天 / 长期记忆，不进入摘要 / 候选 |
| 运行边界 | 共享最多 2 个实际模型请求 lease，自动模型最多 1；有界补跑与重叠合并，预算事务预留；停止不提前释放在途请求占用，重启不盲目重复未知外部请求 |
| 时钟与存储 | Linux 周期核对系统 NTP；无法确认时要求管理员核对服务器时间，人工确认仅当前进程有效、最长 1 小时。时钟跳变阻断；数据盘可用空间低于 512 MiB 或检测失败阻断自动任务 |

日历种子依据北京市政府转载的 [国务院办公厅 2026 年安排](https://www.beijing.gov.cn/zhengce/zhengcefagui/202511/t20251104_4258873.html)，包括 10 月 10 日周六补班和 10 月 1 日周四放假。服务不在后台抓取 URL，不按模型知识填补未来年份；管理员导入和审核新年度版本后才能恢复相应规则。

默认 Skill 用 `trigger_setup_skill_seed_v1` 单独安装，不依赖旧 `skills_seed_v1` 重跑；管理员编辑、停用和删除不被重启覆盖。[Skill 指令](skills/task-trigger-setup.md) 与 [停用导入示例](examples/task-trigger-setup.skill.json) 的正文一致，示例停用与内置种子启用是两种用途。

当前限制固定为任务 256、启用 64、待执行机会 128、自动模型全实例每日 500 次；固定提醒间隔最短 1 秒，固定间隔模型任务最短 60 秒。原始事件资料保留 7 天、结束的自动执行正文保留 30 天，尚无修改期限的管理配置。清理分批进行，实际 lease 未释放则暂缓；保留定义、触发去重键及 cursor，不清除普通聊天、记忆、真实配置或凭据。512 MiB 是剩余空间门槛，不是数据库大小配额。每轮规则求值最多 32 个任务，每个事件任务最多读取 64 条元数据，即最多 2048 条；它是有界批次，不是全局每轮 64 条的实现。这些上限不代表负载吞吐已经验收。

## 已取得的本地证据

| 检查 | 实际结果与范围 |
| --- | --- |
| `cargo test -p dshd trigger_rule --locked` | 8 项通过；示例 / 严格解析、真实日期时区、偏移、DST、远期日期、相对锚点及有界候选计算 |
| `cargo test -p dshd trigger_calendar --locked` | 2 项通过；已知官方日期、unknown 年份和版本持久化 / 导入审核 |
| `cargo build --locked -p dshd` | 最终源码已成功构建本地 Windows 开发二进制；Linux 制品另行核对 |
| `npm test` | 129 项通过；前端规则结构、草稿桥接、错误和状态处理。正式静态制品仍须绑定提交 SHA 打包 |
| `tools/task-triggers-smoke.py` | 27 项通过，退出 0；真实独立 daemon、临时数据库、回环模拟 provider，未读取生产数据或凭据。见 [脱敏报告](validation/task-triggers-local.json) |
| 最终 Rust 格式、全工作区测试及构建 | `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo build -p dshd --locked` 全部退出 0；kernel 4 + daemon 212 项通过，含 DST 预算、授权撤销与上下文隔离回归 |
| 前端类型 / 生产构建与实际浏览器 | 129 项测试及生产构建通过；角色卡、Skill、任务触发 3 项 Playwright 场景全部通过，桌面、320 / 390px 截图已人工检查 |
| 部署兼容性及设计静态检查 | Python 26 项测试通过；设计 11 + 4 合法与 41 拒绝用例通过，Skill 正文仍与 JSON 一致 |
| 现有 Skill 实际进程回归 | 20 项通过，包括制作 Skill、HTML 绘画、模拟原生 PNG、所有权、冻结和重启；修正旧脚本对新增第三个默认 Skill 的总数假设后重跑通过 |

27 项扩展黑盒记录时间为 `2026-10-25T11:00:50.738357+00:00` 至 `2026-10-25T11:01:05.901932+00:00`（UTC），Windows 二进制 SHA-256 为 `21e485581b0cdbaf3503475763bf252f03c4440135e4a3333d83958effcf0420`，报告的 commit 为 `development`；本地开发二进制与 Linux 交付制品分别记录。扩展验收脚本来自已推送测试提交 `47b967753149d90bffde48bf79ac07be5a1a7f21`，未改动运行代码。

黑盒已实际验证：认证 / Origin / CSRF、跨管理员隔离、错误字段和过期版本拒绝、纯预演没有执行、停用保存与幂等、启用后真正触发一次并重启不重复、已生效版本与新草稿分离、秒间隔每日预算及暂停、未知日历和各日历规则、旧 Skill 正文冻结与私有记忆隔离、设置 Skill 只输出草稿、本人交互事件触发一次且不成环、真实在途 HTTP 取消和停止投递、普通 Run 取消接口不能绕过任务状态机、自动任务没有摘要 / 候选，以及删除默认 Skill 后重启不复活。新增用例还覆盖失败交互事件触发固定提醒，以及自动原生 PNG 的真实 HTTP 路由与所有权读取；图片服务仍为模拟服务，不是外部真实生图或平台联调。

## 正式交付与远端验收

应用提交 `04fb8277e26dfa81c6e62599535c9e1aa704759c` 已推送并核对 origin；前端由 `deploy/package-web.ps1 -Commit 04fb8277e26dfa81c6e62599535c9e1aa704759c` 重新 npm ci / 构建，归档 SHA-256 为 `2ee90540dd213f3efb52e3881025e2fb433ec4e35de5312e2fe1a4d0088eeb1a`。正式 Linux 二进制 SHA-256 为 `409446048d1b5e9fe978136c54b9039ad8ffde55c5b6d10b2d017deae5206656`。后续测试专用提交 `47b967753149d90bffde48bf79ac07be5a1a7f21` 只扩展 Python 验收，不改变应用运行行为，不为此重新部署或重启。

目标复核：campanus@192.168.189.191，Raspberry Pi 5 Model B Rev 1.1、Ubuntu 24.04.4 LTS / aarch64，7933 MiB 内存、部署前可用约 6712 MiB，无 swap；Rust / Cargo 1.88.0，NTP 已同步。目录归属 campanus，origin 正确且工作区干净。旧运行 release 为 `1920ac9a17175406958282de844a254be01b8677`。

从已推送应用 SHA 提取部署脚本，执行 `DSH_CARGO_MIRROR=rsproxy bash <脚本> <完整SHA> <前端归档> <归档摘要>`，退出 0。远端限并行 `-j 2`：daemon 测试 89.39 秒；release 构建 3 分 27 秒。`shared/deployment.json` 的切换完成时间为 `2026-10-25T10:58:44.361601+00:00`（UTC）；切换时首次就绪请求尚未连接，随后的完整 SHA 检查成功。源码、`current`、前端 `release.json`、`/health/ready` 均对应应用提交，正式服务 active。

| 远端检查 | 实际结果及证据 |
| --- | --- |
| `cargo test --workspace --locked -j 2` / `cargo build --release --locked -p dshd -j 2` | 216 项 Rust 测试通过，release 构建退出 0 |
| `python3 -S -m unittest discover -s tools/tests` | 26 项通过；含新任务数据拒绝旧读取器的降级门禁 |
| `tools/task-triggers-smoke.py --binary <current>/bin/dshd --web-dir <current>/web --report <日志>` | 27 项通过，退出 0。使用上述测试专用 SHA 的独立 checkout，对固定应用制品执行；见 [任务触发黑盒](validation/task-triggers-aarch64.json) |
| 同参数 `tools/smoke.py` / `tools/character-smoke.py` / `tools/skills-smoke.py` | 19 / 55 / 20 项通过，退出 0。见 [基础管理](validation/task-triggers-m1-aarch64.json)、[角色卡](validation/task-triggers-character-aarch64.json)、[Skill](validation/task-triggers-skills-aarch64.json) |
| `tools/task-triggers-live.py --binary <current>/bin/dshd --web-dir <current>/web --source-data-dir <shared>/data --include-html --report <日志>` | 9 项通过，退出 0；只读生产模型配置与引用凭据，临时独立实例共 3 次真实请求：设置 Skill 生成草稿 → 显式审核保存 / 启用后自动执行 → 已批准的 HTML 绘画。重启不重跑，不产生记忆 / 摘要 / 候选 / 渠道副作用。见 [真实模型](validation/task-triggers-live-aarch64.json) |
| 同 release 的远端隔离实例、SSH 18421、Playwright 三个 spec | 3 项通过，23.6 秒，退出 0；真实远端 API 和合成模型，实际提醒、停用草稿、预演启用、结果、模型草稿审核、日历审核、角色卡与绘画回归，390 / 320px 无溢出。已查看桌面与窄屏截图；见 [浏览器记录](validation/task-triggers-browser-aarch64.json) |

正式数据库配置 revision 仍为 4；管理员 1、会话 4、消息 20、Run 10、角色卡 2、凭据 3 等既有计数保留，默认 Skill 从 2 增至 3；正式任务、执行机会和执行授权均为 0。未把验收任务写入生产库，也未修改生产模型开关。最终服务 RSS 采样 16,712 KiB，只代表当时空闲状态，不是负载上限。完整身份、摘要、前后计数和命令退出状态见 [部署核对](validation/task-triggers-deployment-aarch64.json)。

真实模型 HTML 产出做自包含内容和安全静态检查；浏览器渲染用同契约合成 HTML，二者分别记证据。原生 PNG 路径在普通 Skill 与自动任务均通过实际 HTTP 模拟和鉴权读取，没有调用真实外部生图服务。

日志位于 `/home/campanus/apps/dsh/shared/logs/`：`deploy-<应用SHA>.log`、`test-<应用SHA>.log`、`build-<应用SHA>.log`，以及 `task-triggers-*-aarch64-04fb827.json`。切换前 SQLite 备份为 `/home/campanus/apps/dsh/shared/backups/pre-04fb8277e26dfa81c6e62599535c9e1aa704759c-20261025T105842991497355.sqlite`，权限 0600；上一 release 保留。本轮未执行回滚或数据恢复，不宣称恢复试验通过。`task_triggers:1` 已进入部署兼容性门禁；有任务 / 授权 / 机会或自动 Run 的实例不能静默切回不支持的旧读取器。

浏览器隔离父 / 子进程及专用 18421 隧道已按身份核验关闭；既有 18419 管理隧道保留。最终再次确认正式服务 active / ready。刷新 [管理页面](http://127.0.0.1:18419/admin/#triggers) 可进入任务触发；对话页选择「设置触发任务」Skill 生成草稿，经审核后进入该页面。

## 未包含或未验证的范围

外部 webhook、任意 URL / shell / 文件动作、个人日历、完整 Cron / RRULE、语义事件分类、QQ / 微信主动推送没有开放。普通文字渠道的现有回复不等同于未来主动发送。

原生 PNG 路径要求管理员事先确认服务商能力并启用生图配置，已批准的输出模式不会随配置变化静默升级；模拟接口和真实外部生图分别记录。当前尚无本轮真实外部生图、QQ / 微信真实平台任务推送或高负载 / 长期运行的验收结论。

本地首轮浏览器曾暴露字段辅助文字影响可访问名称、预演未显式展示 IANA 时区及完成后列表状态滞后，均修复并重跑；旧 Skill 验收的默认数量假设、扩展 PNG 用例的缓存头断言也已按实际契约修正后通过。未把这些失败尝试计为通过。
