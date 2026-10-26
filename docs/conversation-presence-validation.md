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

待固定 SHA 推送、树莓派部署及真实模型 / 浏览器验收完成后补入实际证据。当前准备记录不表示新版本已经部署或性格已经应用。
