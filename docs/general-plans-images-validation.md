# 自然安排与常用图片发布验收

2026-10-27，本轮按用户最新要求移除抄诗专用开关、次数配置、固定偏好和诗目录提示。既有写作授权迁移为普通日常自主安排，保留原限额与期限；新安排实际登记并在可执行范围内保存文字成果。PNG、JPEG/JPG、WebP、GIF、BMP 走真实解码和普通对话，失败使用自然短句。契约见 [自然安排与常用图片](general-plans-images.md)，性格资产为 [v3](examples/liu-jiahui-personality-v3.json)。

## 版本与环境

- 正式应用提交：`25ff608fba7e7a137f86ce0f31fa84382e4ec815`，分支 `codex/qq-plans-common-images`，已推送该分支及 `origin/main`。首轮代码为 `c7fb2d6a518ed29e31bebc6b72c6b12d4ea8cf2b`，实际失败和后续修复分别记录。
- 部署工具提交：`8efb4b10f8e3593361ad28ddaa0a82ee0f0b645e`，仅修复列门禁与已知新增列投影，未将工具 SHA 冒称应用二进制 SHA，见 [门禁工具修复](compatibility-gate-repair.md)。
- 切换前应用为 `c7fb2d6`（带本轮发现的格式问题）；本轮开始前可用应用为 `3c5b7f6e2a53f3a0c4bc5cfc99cd8b5b0c4fc723`。旧验收报告保留，不补造截图中那次历史日程。
- 主机：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1，Ubuntu 24.04.4 LTS，aarch64，7933 MiB，Rust / Cargo 1.88.0。部署前采样可用内存 7249 MiB，无 swap，NTP 同步。
- 正式前端包 SHA-256：`a36ebbd60b328dbc95cbfa00b4984b9a1c8415758bf06306fbbe867e99e6b7b8`；首轮包为 `98131440521e2a69ac1fb1dec859e9250113206eb838f77954fc5913b01aab5c`。
- 正式入口：[局域网 HTTPS](https://192.168.189.191:8086/admin/)，Rust 继续监听本机 8419。
- 2026-10-27 19:16:03 +08:00 完成正式切换；源码 / Web / ready 均为完整 `25ff608`，工作区干净，`dsh.service` active，唯一常驻 dshd。二进制 SHA-256 为 `f33e47b41937608a001b779d37b0339448af9c66c4d124a4e96d1c897e9f7a81`，与隔离性格测试记录相同。
- 切换前 SQLite 备份：`/home/campanus/apps/dsh/shared/backups/pre-25ff608fba7e7a137f86ce0f31fa84382e4ec815-20261027T111602319752180.sqlite`。未执行回滚或数据恢复。
- 脱敏结构化证据见 [JSON](validation/general-plans-images-25ff608.json)。

## 已完成的本地检查

| 检查 | 命令 / 证据 | 结果 |
| --- | --- | --- |
| Rust 格式、完整测试 | `cargo fmt --all -- --check`；`cargo test --workspace --locked --quiet` | 最终 649 daemon + 4 kernel 通过，daemon 31.06 秒；初轮 647 + 4、42.57 秒另存 |
| 本地二进制 | `cargo build -p dshd --locked` | 通过，development 标识，不冒称正式 SHA |
| 前端 | `npm.cmd test -- --run`；`npm.cmd run build`（含类型检查） | 30 文件 / 204 测试通过，构建通过 |
| Python 回归 | `python -S -m unittest discover -s tools/tests` | 最终 149 通过，含 reader2、真实旧门禁与停服务分支；初轮 144 另存 |
| 隔离进程 | `python -S tools/daily-life-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist --report .local/qq-generic-process-local-r2.json` | 27 通过，合成模型、全新临时数据 |
| 隔离浏览器 | `npx.cmd playwright test daily-life.spec.ts`，18423 本地临时实例 | 1 通过，桌面 1440 与移动 390；07/23 保存刷新，专用配置不存在，无页面异常 / 横向溢出 |
| 发布前端 | `./deploy/package-web.ps1 -Commit 25ff608fba7e7a137f86ce0f31fa84382e4ec815` | 两轮均通过，完整 SHA 与各自包摘要校验 |

图片回归覆盖五种真实编码、JPEG 方向纠正、1200 万像素缩小、首帧、元数据清除、损坏图和资源限制。QQ 回归通过实际入站准备、解码、Vision、Native、语义补录至日程和持久 Outbox；没有运行真实 QQ 发送器。语义缓存回归包含一次检查、重用原回复、重启中断、跨来源与遗忘边界；没有降低原四句发送上限。

本地日志保留在 `.local/qq-generic-rust-full-final.log`、`qq-images-web-r2.log`、`qq-images-web-build-r2.log`、`qq-generic-python-tests.log`、`qq-generic-process-local-r2.{log,json}`、`qq-generic-browser-r1/`。图片均为合成杯子，用户附件未加入 Git。
格式修复的最终日志另为 `.local/qq-generic-duplicate-{focused,full}.log`，门禁修复为 `.local/qq-generic-gates-full-r2.log`。

## Linux 与模型验证

部署脚本从已推送完整 SHA 检出，单并发、锁定依赖、rsproxy 缓存来源，在新私有 `/dev/shm/dsh-qq-media-c7fb2d6-*` 临时根下运行测试，避免 SD 卡临时数据库写入造成无意义延迟。正式配置、附件和数据库仍在原 shared 目录，不搬迁至 tmpfs。

```sh
DSH_COMPATIBILITY_SOURCE_SHA=8efb4b10f8e3593361ad28ddaa0a82ee0f0b645e \
TMPDIR=<本轮私有临时目录> DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy \
  bash /home/campanus/.cache/dsh/deploy-qq-gate-repair.sh \
  25ff608fba7e7a137f86ce0f31fa84382e4ec815 \
  /home/campanus/.cache/dsh/admin-25ff608fba7e7a137f86ce0f31fa84382e4ec815.tar.gz \
  a36ebbd60b328dbc95cbfa00b4984b9a1c8415758bf06306fbbe867e99e6b7b8
```

Linux 最终完整测试通过：650 daemon + 4 kernel，最后部署执行 daemon 34.30 秒。修复版首次执行 35.30 秒、release 10 分 55 秒；门禁工具修复后复用同一应用 SHA 的构建缓存，仍重跑完整测试，release 检查 0.31 秒。同 SHA 的脚本重跑会覆盖其 test / build 日志，最终日志为最后这次结果，前次观察时间单独记录，不合并重复通过数量。首轮 648 + 4、36.94 秒另存。日志位于 `shared/logs/test-<完整SHA>.log`、`build-<完整SHA>.log`。

三次真实语义组件请求已通过（均 HTTP 200）：截图原句作为合成重放能得到新活动，引用与条件句不登记。此检查直接使用已提交系统指令，不读取生产聊天，不代表完整普通对话流程。记录位于 `shared/logs/qq-generic-live-probe-c7fb2d6.json`。

| 最终 Linux 检查 | 结果与边界 |
| --- | --- |
| `tools/daily-life-smoke.py --binary <25ff608>/bin/dshd --web-dir <25ff608>/web --report <logs>/qq-generic-mock-25ff608.json` | 27 通过，合成模型；五种格式实际解码，通用授权、登记、成果与私有日记 |
| 同脚本追加 `--live-config-from <shared/data> --max-real-requests 12` | 19 通过，实际 8 次 HTTP 200，测试串行峰值 1；普通 Native 新安排、真实语义重放、家人称呼、真实 JPG Vision → Native、日程通知 |
| `tools/daily-life-wake-smoke.py --expected-commit <25ff608> ...` | 11 通过；没有注入时钟 / 日程事件，真实计时从 sleeping 提前一分钟通知，再固定起床，旧原始上下文不入新请求，原日志保留，日记事实可回忆；模型为合成 provider |
| `tools/personality-presentation-smoke.py --personality docs/examples/liu-jiahui-personality-v3.json ...` | 11 通过；认证 / CSRF / 陈旧版本拒绝、已有会话后续版本、记忆保持、恢复与重启 |
| `python3 -m unittest discover -s <8efb4b1>/tools/tests` | 149 通过，30.601 秒；没有实际操作其他服务 |

四项隔离脚本退出状态均为 0；报告和日志位于 `shared/logs/qq-generic-{mock,live,wake,persona}-25ff608.{json,log}`。使用真实模型时只读正式模型 profile / key，测试会话、账号、日记、时间压缩和 event 均属临时实例。已知 QQ 原句的 Native 输出由测试注入，随后语义判断确实请求真实模型；不将重放称为真实 QQ 入站。8 次最终请求之外，三次组件请求和三次失败轮各一次也分别保留，合计 14 次 HTTP 收据，不把失败轮算为通过。

最终 HTTPS 使用现有公有 CA 进行完整链与主机名校验，`/admin/` 及 JS 资源与发布目录字节摘要相同；未登录的性格、活动、配置、选择记录 API 均返回 401。原证书、Origin、Secure Cookie、权限和 CA 私钥没有改动。

正式浏览器在首轮 `c7fb2d6` 已确认专用抄诗控制不存在，并保存性格 v4；[保存时截图](validation/general-plans-personality-save.png) 的页脚属于当时版本。最后切换后的自动化浏览器重载没有完成：原 tab ID 失效，重新连接出现 CDP 超时，新标签报 `ERR_CERT_AUTHORITY_INVALID`，没有跳过证书警告、安装新根证书或降低验证。此项如实保留为最终浏览器限制，不能冒称最新浏览器重载通过；最终服务 / API / 资源另经上述有证书校验的 HTTPS 和只读数据核对。

## 失败保留与边界

- 首次本地 Rust 检查暴露小图被 thumbnail 放大、旧 fixture 使用虚构 Run、旧 V1 契约断言；修正为仅超限才缩小、真实排队 Run 与 V2 断言。后续出现测试模块路径 / ApiError 转换、QQ 合成 target 非法字符、日记 fixture 使用未来完成时间、未取消的合成异源 Run 抢占检查，均在本地修复并完整复验。失败日志 `qq-generic-rust-first.log`、`rust-r2.log`、`rust-r3.log`、`focused-r4.log`、`focused-r5.log` 保留，不合并重复运行数量。
- 首次前端测试沿用 PNG 专用期望，更新为五种格式后 204 通过。`qq-images-web-first.log` 保留。
- 首次隔离脚本对 revision=0 错误期待 409，实际字段验证返回 400。改为有效 revision=1 修改至 2 后再验证陈旧 1 返回 409；`qq-generic-process-local-r1.{json,log}` 保留，最终 r2 通过。
- 部署前 PATH 中没有 Node；本轮前端在 Windows 构建，远端服务无需 Node。一次 PowerShell 管道 CRLF 导致健康 URL 末尾字符非法，改为直接 SSH 命令后确认旧服务 ready。另一次本地启动命令引号解析失败，未发起部署；修正为 LF 文件后启动实际部署。未把这些失败称为部署成功。
- 不把组件重放、模拟模型、持久发送队列或浏览器保存当成真实 QQ / 微信客户端收发。本轮未向真实联系人发送测试消息。
- 当前自动动作仅保存文字成果。没有物理抄写、外出、任意代码、外发、动态视频理解证据；不改变完整睡眠多批整理、微信媒体及完整 M2 / M4 / M5 的未交付边界。
- `daily_life:2` 数据与检查缓存要求 reader2，原版本不支持盲降；切回旧二进制不等于恢复数据库，不删除记录绕过门禁。实际备份和回滚是否执行在最终结果中记录。

首轮 `c7fb2d6` 已成功部署并 ready，Linux 合成流程 27、实际计时醒后日记 11、性格呈现 11 通过；真实模型完整流程未通过，不能将其他结果覆盖这一失败。连续三个有界隔离检查均在首轮 Native 回复失败，前两次诊断不足，最后一次保留了合成响应：模型先输出逐句数组，随后又输出一个包含相同回复及日程的 JSON 围栏，`finish_reason=stop`，并非截断或识图失败。诊断工具提交 `8c3447df585ea8d74a0ef7aa7b3dc9a983b18e4c` 和 `1a98c624e9fa9881d3c2788ad7d19699ff11625f` 仅补充合成测试日志，未重启应用。实际报告 `qq-generic-live-c7fb2d6{,-r2,-r3}.{json,log}` 均保留。

修复 `25ff608` 明确要求一个最外层对象，并只合并内容完全一致、严格类型的数组与后续日程对象；禁止差异、额外字段、重复键或多份对象，仍执行原呈现限制。新增解析及真实 worker 回归，14 项针对检查通过；最终本地完整测试 649 + 4 通过。该修复需要重新部署并单独复验，不能以修改提示后某一次组件结果冒称全部通过。

首轮部署后，正式浏览器通过认证版本接口保存性格资产 v3 为 revision4；最终只读复核字段仍完全相同，不含固定抄诗爱好或机械身份标签。原配置 revision7 及摘要、稳定身份摘要、所核对的七类原数据数量、作息 policy revision2 和来源授权保持不变：4 会话、67 Run、130 消息、0 显式记忆、5 日记事件、1 日记正文、3 secret 记录。旧写作授权仅名称 / revision 迁移，其他策略摘要相同。

`25ff608` 首次切换前被旧列门禁拒绝（退出 2），服务尚未停止；诊断保留 `.local/qq-generic-deploy-r2.log`。`8efb4b1` 把三个新增列纳入精确清单，并让旧门禁在私有只读备份的明确列投影上继续检查全部旧策略。新旧门禁均通过后才停服务、备份和切换；不是删除或改写生产列。首次新门禁全套本地检查有 8 项提取分支 fixture 未设置 `compat_source`，补足新参数、保留未知旧目标 fallback 后最终 149 全部通过；失败 `qq-generic-gates-full.log` 保留。最终部署日志 `.local/qq-generic-deploy-gate-repair.log` 退出 0。启动重试中第一次连接拒绝后即 ready，未触发失败回滚。

收尾只读门禁对保留的 reader2 `c7fb2d6` 返回 0，对 reader1 `3c5b7f6` 返回 2；未实际切换或恢复数据库。所有隔离实例已退出，私有 tmpfs 临时根确认位于 `/dev/shm/dsh-qq-media-c7fb2d6-*` 且为空后以非递归 rmdir 回收，正式服务和私有备份保留。
