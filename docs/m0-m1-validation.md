# M0 / M1 实施与部署验证

日期：2026-10-24，时区 Asia/Shanghai。M1代码、Web管理与树莓派基础部署验收已交付；完整M0的真实模型 / QQ闭环仍需用户在页面初始化并录入凭证。以下严格区分实际通过、模拟验证与未执行项，不能把基础部署成功称为全部外部联调完成。

## 目标环境与授权差异

- 固定目标：树莓派 5、8GB、Raspberry Pi OS Lite 64 位。
- 真实主机：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1，aarch64，约7.7 GiB内存，Ubuntu 24.04.4 LTS，kernel 6.8.0-1064-raspi，`graphical.target`。
- 用户明确授权本轮先使用现有 Ubuntu 验证，保留 Lite 为最终基线。未重装系统，未将 Ubuntu 结果记为 Lite 验收。
- 用户级安装 Rust/Cargo 1.88.0；已有 Node 24.22.1/npm11.19.1；启用用户systemd lingering。
- 可选真实 Harness 固定 `@deepseek-ai/dsh 0.2.1-rc.2`，位于用户目录，不在 M1 生产聊天进程中启用。

## 验证范围

| 项目 | 状态与证据 |
| --- | --- |
| Cordis真实生命周期四项 | Windows与树莓派aarch64均4/4通过；详见 [M0记录](m0-validation.md) |
| 真实 Harness ACP 创建 / 重启恢复 | Windows与树莓派均通过，未使用模拟 ACP；真实模型与在途取消另验 |
| Vue静态前端 | 类型检查、生产构建与12项测试通过；隔离Chromium端到端场景通过（初始化/登录/配置应用/移动导航），无JS异常或浏览器持久密钥 |
| Rust认证、配置、消息一致性 | 本地后端8项测试、严格Clippy通过；真实进程+模拟provider的18项黑盒通过，明确不等于外部模型联调 |
| 树莓派真实Harness ACP | 7项协议检查通过，2项模型相关检查明确not_run；[机器记录](../tools/m0/results/acp-raspi.json) |
| 树莓派原生测试 | 部署提交已原生编译，4项Cordis + 8项后端测试通过；release二进制黑盒18/18通过：[机器记录](validation/m1-aarch64.json) |
| 常驻服务与版本 | 用户systemd启动 / 重启后ready，前端manifest与后端health均匹配完整部署SHA；SSH隧道实际页面无JS错误 |
| 真实模型请求与 QQ 收发 | 用户已有账号，需在Web页面录入凭证后执行；当前未记录通过 |
| Lite系统验收 | 未执行；当前授权使用Ubuntu临时验证 |

## 部署版本与实测运行状态

- 运行代码：`d5efc8dfba806ef286bcd601ffa001b821620e15`，已推送至origin/main。
- 前端压缩包SHA256：`77deae43d52186a33c16729fe019d06b8d6e2fbbc12d3f929b7d97ddb676337c`；安装器校验摘要和内部commit，页面manifest与后端health返回相同SHA。
- 部署根目录：`/home/campanus/apps/dsh`；`current` 指向对应 `releases/<SHA>`；用户数据、凭证、日志位于独立 `shared`。
- 服务：`systemctl --user status dsh.service`，已启用自动启动与用户lingering。重启后 `ActiveState=active`、`SubState=running`，health=ready。
- 2026-10-24 15:36（+08:00）采样，空闲后端RSS为8456 KiB；这是未配置模型 / QQ时的单次采样，不是峰值、并发或本地推理容量承诺。
- 本轮首次安装没有上一可用应用release；回退代码已复核启动失败路径，但不声称完成了跨版本恢复实测，也未执行数据库回滚或系统重装。

实际执行：

```sh
DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy.sh \
  d5efc8dfba806ef286bcd601ffa001b821620e15 \
  /home/campanus/.cache/dsh/admin-d5efc8dfba806ef286bcd601ffa001b821620e15.tar.gz \
  77deae43d52186a33c16729fe019d06b8d6e2fbbc12d3f929b7d97ddb676337c
python3 /home/campanus/apps/dsh/current/source/tools/smoke.py \
  --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --report /home/campanus/apps/dsh/shared/logs/m1-smoke.json
systemctl --user restart dsh.service
```

源码脚本来自已推送提交，未在远端改源码。部署脚本在原生编译前再次执行锁定测试；编译与测试分别写入 `shared/logs/build-<SHA>.log` 和 `test-<SHA>.log`，部署日志为 `deploy-d5efc8d.log`，ACP结果为 `acp-raspi.json`。

Windows本地的真实Chromium验收覆盖首次初始化、登录 / 退出、未认证API、QQ模式切换、无效配置、保存 / 校验 / 应用、历史和移动导航。桌面与移动截图保存在本地忽略目录 `.local/browser-e2e/screenshots/`；另通过SSH隧道检查了树莓派真实部署入口，页面加载正常且无JS异常。

## 尚待用户完成的真实账号步骤

1. 保持SSH隧道后打开 `http://127.0.0.1:18419/admin/`，用短期配对凭证创建独立管理账户。凭证不进入Git或报告。
2. 在“助手与模型”录入真实endpoint、模型名和API key，执行连接测试并应用；可在对话工作台验证真实文字回答。
3. 在“QQ连接”录入AppID / AppSecret，QQ开放平台选择WebSocket并配置所需服务范围 / 体验账号，应用配置。
4. 从已授权QQ账号向机器人发送测试消息，核验事件入站、真实模型调用及回复送达；不能仅凭token获取成功宣称收发闭环通过。
5. 使用已提供的 `tools/m0/acp-from-dsh.mjs` 在隔离、禁用工具的Harness中验证真实短回复和在途取消，保留脱敏结果。

截至本轮基础验收记录，正式实例尚未初始化，未录入真实凭证，真实模型 / QQ收发与Harness模型取消未执行。因此完整M0与外部渠道闭环保持未完成状态，后续配置后继续验收。

## QQ 文档依据修正

用户提供的 [QQ最新官方接入指南](https://bot.q.qq.com/wiki/) 明确支持 WebSocket 与 Webhook 切换，个人单机可用 WebSocket，无需公网回调。因此实现默认 WebSocket，同时保留签名 Webhook；不再沿用旧SDK公告推断WebSocket已不可用。

QQ内部协议测试将涵盖验签、去重、持久事务与消息映射；它们不能替代真实账号登录、事件订阅、平台限流和消息送达验收。

## 交付边界

M1提供基础管理与原生模型文本对话；M2记忆/微信、M3完整Harness权限桥、M4多模态、M5生成插件仍为后续工作。M0仅在隔离空目录运行真实Harness协议探针，不将完整上游默认工具集开放给聊天用户。
