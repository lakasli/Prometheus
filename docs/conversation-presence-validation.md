# 时间感与原创单一助手性格验收

日期：2026-10-26，Asia/Shanghai。本轮沿用单一助手，在普通 Native 对话请求中附加宿主时间、消息接收时间和同来源会话的可见交互间隔，提供十组原创表达示例，并按用户要求在部署后直接应用性格。契约与资产见 [时间感与自然表达](conversation-presence.md)。

## 变更与边界

- 服务器从 `messages.created_at` 读取接收时间，固定 Asia/Shanghai；间隔排除排队耗时。来源限定 owner / 会话 / 较早已完成普通轮次，并遵循遗忘水位；摘要覆盖不等于遗忘。
- 首次请求构造后复用已有 PromptSnapshot。空历史、时间回拨或未来时间保留未知，不据时间推断线下经历；未核验 NTP，时钟标记 `unverified`。
- 表达示例进入虚构风格段，不写成真实消息或长期记忆。性格保存沿用认证、CSRF 和 If-Match；应用对后续 Run 生效，保留旧版本及已排队快照。
- 不包含短消息合并、记忆召回算法调整、QQ / 微信主动消息、情绪引擎或新媒体能力。

## 本地检查

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 退出 0 |
| `cargo test --workspace --locked` | 282 daemon + 4 kernel，通过；其中新增时间边界 9 项 |
| `cargo build --locked -p dshd` | 退出 0 |
| `python -S tools/conversation-presence-smoke.py ...` | 19 项通过；真实 Rust 进程与回环 HTTP 模拟 provider |
| `python -S tools/assistant-logs-smoke.py ...` | 19 项单一助手 / 日志回归通过 |
| 新 Python 脚本语法、性格六字段 / 长度 / 十组示例、差异静态检查 | 通过 |

本地二进制标识为 `development`，不作为正式运行 SHA。回环 provider 检查时间字段、发送前快照、隔离、幂等和重启；不能代替真实模型自然表达验收。

## 发布、性格应用与远端验收

正式运行 SHA 为 `60d559fa583611768ee9e58ef9270ac1eeb7b09c`，已提交并推送，部署前核实 origin/main 包含该提交。2026-10-26 **09:58:34 +08:00** 在 `campanus@192.168.189.191` 完成部署；复核 Raspberry Pi 5 Model B Rev 1.1、7,933 MiB、Ubuntu 24.04.4 LTS / aarch64、Rust / Cargo 1.88.0。未改变系统、桌面或启动目标。

源码位于 `/home/campanus/apps/dsh/releases/60d559fa583611768ee9e58ef9270ac1eeb7b09c/source`，工作区干净；`current` 指向同版 release，systemd 用户服务 `dsh.service` 为 active，`/health/live` 与 `/health/ready` 返回同一完整 SHA。前端 `release.json` 与浏览器页脚一致，静态制品 SHA-256 为 `2f1f64dc44aff404813e655953c2b388e7a66721eaaca9ac8b6a478355b9d6df`；aarch64 二进制 SHA-256 为 `8f842d7537bb5fdc827697c0d2209e58bb363b085b0db5e28b3419fa03f334aa`。

本轮在 Windows 使用 `deploy/package-web.ps1 -Commit <完整SHA>` 完成类型检查、生产前端构建与打包，退出 0；上传后通过下列命令在树莓派单并发构建、测试、备份及切换，退出 0：

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash \
  /home/campanus/.cache/dsh/deploy-60d559f.sh \
  60d559fa583611768ee9e58ef9270ac1eeb7b09c \
  /home/campanus/.cache/dsh/admin-60d559fa583611768ee9e58ef9270ac1eeb7b09c.tar.gz \
  2f1f64dc44aff404813e655953c2b388e7a66721eaaca9ac8b6a478355b9d6df
```

远端全量为 **283 daemon + 4 kernel**，全部通过（比 Windows 多一项 Linux 专用检查），发布构建退出 0。构建期间服务保持旧版本直到切换，一次采样可用内存为 6,809 MiB；最终复核仅一个生产 dshd 进程，无测试实例遗留。构建和测试日志分别为 `shared/logs/build-<完整SHA>.log`、`test-<完整SHA>.log`。

| 远端功能验证 | 实际结果及证据 |
| --- | --- |
| `tools/conversation-presence-smoke.py` | 19 项通过，[报告](validation/conversation-presence-aarch64.json)；真实进程、回环模拟 provider，不是真实模型 |
| `tools/assistant-logs-smoke.py` | 19 项单一助手 / 日志回归通过，[报告](validation/conversation-presence-logs-aarch64.json) |
| `tools/conversation-presence-live.py` | 6 次真实配置模型请求、21 项检查通过，[报告与合成回答](validation/conversation-presence-live-aarch64.json) |
| 生产浏览器与只读状态复核 | 已保存提示词、重载后六字段一致、v1 生效、v0 可恢复；身份 / 配置 / 原有数据保留，[证据](validation/conversation-presence-deployment-aarch64.json) |

三个验收脚本均从上述固定 SHA 的 `current/source/tools/` 执行，使用 `--binary current/bin/dshd --web-dir current/web --report shared/logs/<相应报告>` 的绝对路径，退出均为 0。真实模型脚本另传 `--source-data-dir /home/campanus/apps/dsh/shared/data --personality current/source/docs/examples/assistant-personality-natural-v1.json`；仅读生产模型配置与对应凭证，将六次合成对话置于临时私有实例，关闭 QQ、微信无账户，显式关闭生图及语音，未读真实聊天 / 记忆、未写生产数据库。脚本关闭实例并清理临时目录，无额外摘要或记忆候选调用。

真实模型在 10:00:37–10:00:42 +08:00 返回：今天 `2026-10-26`；面对用户引用的伪造 `2099` 时间仍返回真实日期；隔日记录答“昨天（2026-10-25）”；计算 17×19 得 323。疲惫、不想继续聊的合成输入得到“好，那就先到这儿，不用撑着聊。”，未追加建议或追问；未提供出生日期时明确不知道且不猜。自动断言只覆盖报告中列出的条件，另已审阅这六条合成回答；这不是所有场景自然度或任意提示注入防御的证明。

## 生产性格应用及恢复

用户已明确授权部署时直接应用。本轮通过现有认证管理浏览器保存五个表达字段，保留助手名称 **DSH**，使用服务端既有 CSRF / If-Match 路径；**10:01 已生效 v1**。刷新新版本页面后六字段与 [原创资产](examples/assistant-personality-natural-v1.json) 完全一致，十组示例均在，旧 v0 显示于“性格版本与恢复”。真实截图位于本机忽略目录 `.local/conversation-presence/personality-applied.png`，截图摘要见部署证据。

保存前后稳定 assistant identity 的摘要相同，模型配置仍为 v5，原有会话 4、Run 22、记忆 0 的数量均未改变；没有向生产聊天写入合成测试内容。性格会用于后续入队 Run，进行中 / 已入队请求仍保留其冻结版本。UI 保存、SQLite 持久状态与隔离真实模型验收分别核对，不将临时测试实例的通过误称为生产 QQ / 微信收发通过。

上一可用程序为 `6b9b5dae0eacdf8e448ea4403ba0c8c48eda6337`。切换前 SQLite 备份为 `/home/campanus/apps/dsh/shared/backups/pre-60d559fa583611768ee9e58ef9270ac1eeb7b09c-20261026T015832761401022.sqlite`；本轮没有执行生产回滚演练。恢复性格可选择历史 v0，经当前 If-Match 保存为新 revision；代码回滚仍先跑兼容性检查，数据恢复单独处理，不能通过删除性格或用户数据来绕过检查。
