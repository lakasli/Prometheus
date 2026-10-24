# M0 / M1 实施与部署验证

日期：2026-10-24，时区 Asia/Shanghai。M0核心验证与M1基础功能已完成，并已按“本地开发 → Git提交推送 → 树莓派部署测试”交付。真实模型、Harness与QQ私聊闭环已通过，用户确认收到机器人回复。最终Lite系统验证及后续阶段能力仍按下文边界保留。

## 目标环境与授权差异

- 固定目标：树莓派5、8GB、Raspberry Pi OS Lite 64位 / aarch64。
- 实测主机：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1，约7.7 GiB内存，Ubuntu 24.04.4 LTS，kernel 6.8.0-1064-raspi，`graphical.target`。
- 用户明确授权本轮先使用现有Ubuntu验证，保留Lite为最终基线。未重装系统，未将Ubuntu结果记为Lite验收。
- Rust / Cargo 1.88.0安装于用户目录；远端Node 24.22.1 / npm 11.19.1。M1生产服务不需要常驻Node进程或桌面环境。
- 可选真实Harness固定为 `@deepseek-ai/dsh 0.2.1-rc.2`，仅用于隔离的M0实验，未作为M1生产聊天后端启用。

## 验证结果

| 项目 | 状态与证据 |
| --- | --- |
| 真实Cordis生命周期 | Windows与树莓派aarch64均4/4通过，覆盖依赖撤销 / 恢复、重复卸载、配置拒绝和在途任务取消；[M0记录](m0-validation.md) |
| Rust后端 | Windows与树莓派均10/10通过；本地格式、严格Clippy与构建通过 |
| Vue静态前端 | 16项单测、类型检查和生产构建通过 |
| 真实Chromium端到端 | 隔离实例覆盖初始化、登录 / 退出、鉴权、QQ模式、无效配置、保存 / 校验 / 应用、历史、移动导航；无JS异常，无浏览器持久密钥 |
| 真实进程黑盒 | 本地及树莓派19/19通过，含认证CLI、配置冲突、持久去重、会话隔离、重启恢复和中断请求不盲目重放；使用隔离模拟模型，不能代替真实外部联调；[树莓派记录](validation/m1-aarch64.json) |
| 真实Harness无模型协议 | Windows与树莓派7项通过、2项模型检查明确标记not_run；保留初始[树莓派记录](../tools/m0/results/acp-raspi.json) |
| 真实Harness与模型 | 树莓派9/9通过，含真实短回复、HTTP请求在途取消、进程重启后列出 / 恢复 / 关闭会话；[脱敏记录](validation/acp-live-aarch64.json) |
| QQ真实私聊闭环 | WebSocket完成101升级、HELLO及READY；2条C2C事件、2个完成的模型任务、2次发送成功且有平台回执，用户确认收到；[脱敏记录](validation/qq-live-aarch64.json) |
| 常驻与版本 | 用户systemd启动 / 重启后ready，服务enabled、Linger=yes；前端manifest与后端health均匹配部署完整SHA；SSH隧道管理入口通过 |
| Lite系统与资源压测 | 未执行；当前为获准使用的Ubuntu验证，不宣称M6压测完成 |

M1提供Web与CLI原生模型文本对话、QQ官方接入、SQLite持久任务与Inbox / Outbox、配置版本及管理认证。生产对话使用NativeBackend；M0真实Harness实验不代表M3完整权限桥已上线。

## 部署版本与运行状态

- 运行代码：`17b7ee061c4ccb724352c7d4c837975b25f34c4c`，部署前已推送至origin/main。
- 前端压缩包SHA256：`d31f613324c7576ccb0ed0e706d72447184fb78f159733e5201d9b0cf2a78733`。安装器校验摘要和内部commit，页面manifest与后端health返回相同SHA。
- 最终19项黑盒驱动来自随后已推送的 `a4f72b33e90ee0bdca6b1391a3e4ef516fb5d2c4`，仅增加CLI测试，针对上述运行二进制执行；该测试提交与本次报告提交不改变生产程序，不为此重启部署。
- 部署根目录：`/home/campanus/apps/dsh`；`current` 指向对应 `releases/<SHA>`，可执行文件为 `current/bin/dshd`，静态页面为 `current/web`。
- 数据库及凭证位于独立的 `shared/data`，构建、测试与联调日志位于 `shared/logs`。秘密文件不进入Git，报告不含真实消息正文、账号标识或凭证。
- 服务为用户级 `dsh.service`，`ActiveState=active`、`SubState=running`、health=ready，退出SSH后可持续运行。配置内存阈值768M、上限1G；这不是压力测试结论。
- 本轮从 `d5efc8dfba806ef286bcd601ffa001b821620e15` 正常升级到当前版本，保留已有管理账户、模型 / QQ配置及数据库；升级前通过SQLite备份接口在私有 `shared/backups` 保存快照。
- 部署脚本具有失败时切回上一release并检查版本的路径；本轮未注入启动失败执行完整回滚演练，未执行数据库恢复或系统重装。
- 2026-10-24 15:36（+08:00）旧版本未配置模型 / QQ时，空闲后端RSS单次采样为8456 KiB；不能将其作为当前峰值、并发或本地推理容量承诺。

实际部署命令：

```sh
DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy.sh \
  17b7ee061c4ccb724352c7d4c837975b25f34c4c \
  /home/campanus/.cache/dsh/admin-17b7ee061c4ccb724352c7d4c837975b25f34c4c.tar.gz \
  d31f613324c7576ccb0ed0e706d72447184fb78f159733e5201d9b0cf2a78733
```

源码、脚本和前端均在本地编写、检查、提交与推送，远端按固定SHA检出、原生测试和构建，未在远端改源码。部署命令退出成功，服务健康与版本核对通过。构建、测试日志分别为 `shared/logs/build-17b7ee061c4ccb724352c7d4c837975b25f34c4c.log` 和 `test-17b7ee061c4ccb724352c7d4c837975b25f34c4c.log`，部署日志为 `deploy-17b7ee0.log`。

最终黑盒脚本从已推送测试提交提取到远端私有缓存后运行，使用临时数据库及模拟provider，不改正式数据：

```sh
git -C /home/campanus/apps/dsh/repo show \
  a4f72b33e90ee0bdca6b1391a3e4ef516fb5d2c4:tools/smoke.py \
  > /home/campanus/.cache/dsh/smoke-final.py
python3 /home/campanus/.cache/dsh/smoke-final.py \
  --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --report /home/campanus/apps/dsh/shared/logs/m1-smoke-final.json
```

黑盒退出成功并记录19项通过。Windows Chromium验收使用本地隔离实例和独立测试账户；桌面、390像素移动视口及树莓派真实入口截图保留于本地忽略目录 `.local/browser-e2e/screenshots/`。

## 真实账号联调及诊断结论

管理账户已初始化，模型与QQ凭证由用户通过Web页面录入并应用。真实模型请求先通过，再进行QQ与Harness验证；没有把模拟provider结果替代为真实模型通过。

QQ连接测试取得token和gateway成功，仅证明凭证及HTTP探测可用。针对未观察到入站事件的问题，新增脱敏连接状态 / HTTP / WS / Close码诊断，统一Text与Binary JSON解析、调整HELLO后心跳时序和关闭码恢复，并让Web每5秒刷新实际状态而保留未保存表单。缺乏修复前完整网关日志，不能将历史问题归因于某个唯一原因。

修复版本部署后，日志依次记录token与gateway HTTP 200、WebSocket 101、HELLO和READY。用户在READY后发送新消息，数据库记录两次 `C2C_MESSAGE_CREATE`、两个completed任务和两次带平台回执的sent回复，用户确认收到。因此本次真实QQ私聊 → NativeBackend真实模型 → QQ回复路径通过。

用户提供的 [QQ官方接入指南](https://bot.q.qq.com/wiki/) 明确支持WebSocket与Webhook切换，个人单机可用WebSocket。实现默认WebSocket，并保留签名Webhook；本轮没有部署公网HTTPS业务回调，也未把Webhook验签测试算成公网联调。

Harness通过 `tools/m0/acp-from-dsh.mjs` 以只读方式读取已应用模型配置，在临时home、空工作区、禁用工具的profile中运行真实上游。初次依赖语义输出时点的取消尝试没有得到cancelled，按失败保留诊断，没有误记通过；ACP语义chunk可能在生成结束后才提交。

最终实验使用仅回环的临时代理与私有IPC，确认第二次真实上游请求已建立TLS连接、发送完成且响应尚未结束，再发ACP取消。报告记录 `stop_reason=cancelled`、客户端断开及上游中止；第一轮短回复正常完成，之后进程重启与会话恢复通过，共9项。证据保存在 `shared/logs/acp-live-network-result.json`，初次失败诊断为 `acp-live-semantic-race.json`；入库副本只含脱敏元数据。

## 管理入口与交付边界

当前管理入口为 [http://127.0.0.1:18419/admin/](http://127.0.0.1:18419/admin/)，需要保持本机SSH隧道并登录独立管理账户。隧道断开后可在用户电脑重新连接：

```sh
ssh -N -L 18419:127.0.0.1:8419 campanus@192.168.189.191
```

Rust仅监听树莓派回环地址，未发布公网管理入口。日常直接局域网HTTPS访问可按 [部署流程](development-workflow.md) 配置受保护代理。

未执行项包括最终Lite系统安装后的回归、真实QQ群聊 / 频道、公网Webhook、故障注入回滚及资源压测。M2长期记忆 / 微信、M3完整Harness权限桥、M4图像与语音、M5生成插件仍属于后续开发，不能从本轮文本 / 私聊验证推导为已经可用。
