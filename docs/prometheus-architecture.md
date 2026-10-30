# Prometheus TypeScript 架构

2026-10-30 用户授权：使用 TypeScript 重写并改名为 Prometheus，作为全新项目运行，旧数据保留备份。本文件描述新实现；[原架构](architecture.md) 的完整功能目标继续保留，不回写历史验收。

| 层       | 当前实现                                                         |
| -------- | ---------------------------------------------------------------- |
| 运行时   | Windows 原生 Node.js 24.7–24.x，ES modules                       |
| 后端     | TypeScript strict，构建为 JavaScript                             |
| 生命周期 | 官方 TypeScript Cordis 3.18.1；当前仅可信内置队列插件            |
| HTTP     | Node HTTP / HTTPS，认证 API 与静态管理页同源                     |
| 页面     | Vue 3、TypeScript、Vite，新 `apps/admin` 工作区                  |
| 数据     | Node SQLite，WAL、FULL synchronous、secure_delete、版本化 schema |
| 认证     | Argon2id PHC、随机会话、服务端 CSRF、8小时到期                   |
| 凭据     | AES-256-GCM 私有文件、独立主密钥、NTFS ACL                       |
| 模型     | OpenAI 兼容公共 HTTPS 文字协议，连接时校验真实 DNS 地址          |
| 执行门禁 | 未验证的渠道、媒体和外部执行返回503，不加载不可信 npm 插件       |

新运行链路不编译或调用 Rust。WIT / WASM、Python 动作和 Harness 目标仍保留；后续由 TypeScript 宿主调用经 Windows 验证的独立 worker，不退化为宿主进程内执行。

## 数据与权限

`metadata` 保存 schema 和稳定性格身份；`admins` / `sessions` 保存管理身份；配置与性格各有不可变版本历史。`conversations`、`runs`、`messages` 保存对话与运行；持久队列与进程内物理请求 lease 分开记账。

`records` 按 kind 分别校验记忆、日记、日程、人物、关系和 Skill，不接受任意执行字段。资料默认私有、按账户隔离；共享记忆须逐次确认发布。其他域当前仅私有。配置权限不代替渠道接收方同意或外发授权。

更正与遗忘增加上下文 epoch，取消受影响的排队及在途请求。遗忘清空当前内容、全文索引和递归依赖的派生资料，原始对话与离线备份保留。中断模型请求不盲重放，旧渠道发送状态不导入。

## 网络与秘密

后端只监听 loopback。管理写操作同时验证 Host、精确 Origin、会话与 CSRF；Cookie 为 HttpOnly / SameSite=Strict，HTTPS 必须使用 Secure。静态文件解析后检查实际路径，不提供源码、数据或任意文件入口。

模型不跟随重定向；凭据只放授权请求头。请求字节、历史数量、响应大小、超时与并发均有上限；连接时拒绝受保护网络。loopback 模型例外仅能显式在开发 / 合成测试启用。上游错误正文不进入日志，已知凭据在输出中隐藏。

完整功能仍待继续迁移，见 [迁移表](prometheus-migration.md)。基础版不能记为完整 M2 / M4 / M5。
