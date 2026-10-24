# M0 / M1 实施与部署验证

日期：2026-10-24，时区 Asia/Shanghai。此文件记录实际检查；真实外部账号验证与隔离模拟测试分别列出。部署和远端验收正在执行，以下未完成项不记为通过。

## 目标环境与授权差异

- 固定目标：树莓派 5、8GB、Raspberry Pi OS Lite 64 位。
- 真实主机：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1，aarch64，约7.7 GiB内存，Ubuntu 24.04.4 LTS，kernel 6.8.0-1064-raspi，`graphical.target`。
- 用户明确授权本轮先使用现有 Ubuntu 验证，保留 Lite 为最终基线。未重装系统，未将 Ubuntu 结果记为 Lite 验收。
- 用户级安装 Rust/Cargo 1.88.0；已有 Node 24.22.1/npm11.19.1；启用用户systemd lingering。
- 可选真实 Harness 固定 `@deepseek-ai/dsh 0.2.1-rc.2`，位于用户目录，不在 M1 生产聊天进程中启用。

## 验证范围

| 项目 | 状态与证据 |
| --- | --- |
| Cordis真实生命周期四项 | 本地通过；详见 [M0记录](m0-validation.md) |
| 真实 Harness ACP 创建 / 重启恢复 | 本地通过，未使用模拟 ACP；真实模型与在途取消另验 |
| Vue静态前端 | 类型检查、生产构建与12项测试通过；隔离Chromium端到端场景通过（初始化/登录/配置应用/移动导航），无JS异常或浏览器持久密钥 |
| Rust认证、配置、消息一致性 | 本地后端8项测试、严格Clippy通过；真实进程+模拟provider的17项黑盒通过，明确不等于外部模型联调 |
| 树莓派真实Harness ACP | 初始化、创建、关闭、重启、列出和恢复通过；真实模型和在途取消尚待凭证 |
| 树莓派原生测试 | 代码提交 `6d9a79a9e786ff92ea63775a833b6d21f662aa1d` 已原生编译并通过4项Cordis + 8项后端测试；release部署与进程级验收继续执行 |
| 真实模型请求与 QQ 收发 | 用户已有账号，需在Web页面录入凭证后执行；当前未记录通过 |
| Lite系统验收 | 未执行；当前授权使用Ubuntu临时验证 |

## QQ 文档依据修正

用户提供的 [QQ最新官方接入指南](https://bot.q.qq.com/wiki/) 明确支持 WebSocket 与 Webhook 切换，个人单机可用 WebSocket，无需公网回调。因此实现默认 WebSocket，同时保留签名 Webhook；不再沿用旧SDK公告推断WebSocket已不可用。

QQ内部协议测试将涵盖验签、去重、持久事务与消息映射；它们不能替代真实账号登录、事件订阅、平台限流和消息送达验收。

## 交付边界

M1提供基础管理与原生模型文本对话；M2记忆/微信、M3完整Harness权限桥、M4多模态、M5生成插件仍为后续工作。M0仅在隔离空目录运行真实Harness协议探针，不将完整上游默认工具集开放给聊天用户。
