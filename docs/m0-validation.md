# M0 依赖与协议验证记录

日期：2026-10-24。本文严格区分本地实验、真实上游协议、真实模型调用和目标系统验收。未执行的项目不记为通过。

## 固定依赖与本地环境

| 项目 | 已实测值 |
| --- | --- |
| 本地系统 | Windows x86_64 |
| Rust 测试工具链 | `1.88.0-x86_64-pc-windows-gnu`，rustc `1.88.0 (6b00bc388 2025-06-23)` |
| Cordis Rust facade | crates.io `cordis-rs = 0.11.1`，真实依赖 `cordis-core 0.6.1` |
| Cordis MSRV | 上游声明 Rust 1.88；实际已使用 1.88 编译 |
| Node.js / npm | Node `24.19.0`，npm `11.17.0` |
| DeepSeek Harness | npm `@deepseek-ai/dsh = 0.2.1-rc.2` |

Cordis 从 crates.io 实际下载、编译，未使用自制替代框架冒充。Harness 从 npm 实际安装并启动，未使用模拟 ACP 服务端。Cargo.lock 锁定 Rust 依赖，Harness 验证环境的 npm 依赖锁在 [package-lock.json](../tools/m0/harness-runtime/package-lock.json)。它是可选的 M0 工具环境，不要求 M1 文本 / Web 管理进程加载整套 Harness。

## Cordis 生命周期实验

实现位于 [dsh-kernel](../crates/dsh-kernel/src/lib.rs)，测试位于 [lifecycle.rs](../crates/dsh-kernel/tests/lifecycle.rs)。Kernel 保留真实 Cordis fiber，并在退出时按相反挂载顺序显式卸载；后台任务的取消和 join 是同一个 generation 清理义务。

本地命令：

```powershell
cargo +1.88.0-x86_64-pc-windows-gnu test -p dsh-kernel
```

| 实验 | 本地结果 | 验证内容 |
| --- | --- | --- |
| 必需服务失效 / 恢复 | 通过 | 从 Pending 到 Active；撤销服务后清理旧 generation，再发布服务后重新 apply |
| 重复卸载 | 通过 | 重复 dispose 不重复执行清理，最终为 Disposed |
| 配置更新拒绝 | 通过 | prepare 和 Cordis update policy 分别拒绝无效配置，错误类型 prepared change 也被拒绝；旧插件保持 Active，后续合法更新仍可成功 |
| 在途任务取消 | 通过 | Kernel 卸载触发取消并等待任务结束；重复 shutdown 无重复清理 |

本地测试总计 **4 passed / 0 failed**。这些证明所选接口在 Windows 上可用；Linux aarch64 构建、运行与资源占用由远端验证补充，不能从本地结果推断 Lite 系统已经通过验收。

## Harness ACP 实验

协议探针位于 [acp-probe.mjs](../tools/m0/acp-probe.mjs)。它通过真实子进程的 stdio 发送 ACP v1 JSON-RPC；遇到客户端权限请求一律拒绝，不给探针授予工具执行或文件访问权限。协议 stdout 设置大小上限，stderr 仅计数，不将原始日志或环境凭证写进报告。

准备可选验证环境：

```sh
npm ci --prefix tools/m0/harness-runtime --ignore-scripts --no-audit --no-fund
```

使用绝对路径启动，避免 Windows `.cmd` 和 shell 转义问题；Linux 路径按实际 checkout 替换：

```sh
node tools/m0/acp-probe.mjs \
  --command /absolute/path/to/node \
  --arg /absolute/path/to/DSH/tools/m0/harness-runtime/node_modules/@deepseek-ai/dsh/lib/bin.js \
  --arg --profile --arg acp \
  --home /private/path/to/isolated-harness-home \
  --report /private/path/to/acp-result.json
```

不提供 `--home` 时探针自动创建临时隔离目录。不要指向已有个人 Harness home，以免复用其中的会话、配置或凭证。

| 实验 | 本地真实 Harness 结果 |
| --- | --- |
| `initialize` 协商 ACP v1 | 通过 |
| `session/new` 创建持久会话 | 通过 |
| `session/close` 关闭会话 | 通过 |
| Harness 进程退出后重新启动、列出会话 | 通过 |
| `session/resume` 恢复并再次关闭 | 通过 |
| 真实模型回复 | 初始本地未执行；后续树莓派真实验证通过，见补充记录 |
| 模型请求在途取消 | 初始本地未执行；后续通过真实HTTP在途见证与ACP cancelled验证 |

本地机器可读记录见 [acp-windows.json](../tools/m0/results/acp-windows.json)。本轮 `initialize` 返回 `audio=false`、`image=false`；这反映该未配置模型路由的实例能力，不能推导整个 Harness 不支持图像。

真实模型与在途取消的补充实验可通过 `--prompt` 启用，使用无外部动作的简单提示；凭证通过受限环境或隔离 Harness 凭证存储注入，不作为命令行参数、不入库。第一轮必须从当前会话的 `agent_message_chunk` 收到预期标记，默认 `ACP_M0_OK`，可通过 `--expect-text` 指定其他短标记；仅 `session/prompt` 返回、用户消息回显或其他会话输出都不算模型回复通过。报告只保存命中布尔值、字符数和停止原因，不保存回复正文或思考内容。

ACP 通知是已提交的语义更新，不承诺逐 token 输出；收到 `agent_message_chunk` 时，上游生成可能已经完成。因此直接运行探针的语义输出取消路径保留严格失败检查，但推荐使用下面辅助脚本的真实 HTTP 在途见证模式。

第二轮使用有上限的无害长列表请求。临时代理确认真实上游 TCP / TLS 连接已建立、请求数据已发送且响应尚未结束，通过私有父子进程 IPC 通知探针；探针再请求一次实时确认，只有该请求仍在途才向 ACP 发送 cancel，并要求最终 `stopReason=cancelled`。若请求已经完成、只收到排队 / 用户事件或取消未能胜出，记录失败并要求重试，不将其计作在途取消通过。只有这些检查实际成功，报告才会记为 `passed_with_model`。

M1 配置已经通过 Web 页面录入后，可在树莓派上运行 [acp-from-dsh.mjs](../tools/m0/acp-from-dsh.mjs)。该脚本以只读方式读取当前 SQLite 配置和其 `model_api_key` 引用，校验凭证文件权限，然后仅将这一把模型密钥注入探针子进程的 `DSH_M0_MODEL_KEY` 环境；不会输出密钥，不修改生产数据库和 secret store。脚本启动仅监听 `127.0.0.1` 随机端口的临时 [HTTP 代理](../tools/m0/inflight-proxy.mjs)，仅转发带随机私有路径和匹配鉴权的模型 POST 请求到既有 endpoint，不记录请求 / 响应正文或标头，也不提供任意目标代理功能：

```sh
node tools/m0/acp-from-dsh.mjs \
  --data-dir /home/campanus/apps/dsh/shared/data \
  --harness-bin /home/campanus/.local/share/dsh-harness/node_modules/@deepseek-ai/dsh/lib/bin.js \
  --report /home/campanus/apps/dsh/shared/logs/acp-live-result.json
```

这些目录要以实际部署为准。该辅助脚本使用 Node 24 的 `node:sqlite`，在临时 home 和空工作区启动 Harness，将 M1 已配置的 OpenAI 兼容 endpoint / 模型经临时回环代理映射到独立 `dsh-m0-provider` 路由。探针先请求并核验一句固定短回复，再等待第二次长列表请求被真实上游网络见证后取消，最后验证重启恢复。模型回复与取消的结果仍以实测报告为准。

代理以 Node `ClientRequest.finish` 与 TCP `connect` / TLS `secureConnect` 事件共同见证请求已交给已建立的连接，以响应 `end` 排除已完成请求；这证明 HTTP 请求在途，不冒称已经观测到服务端具体 token 或模型内部状态。[Node HTTP 文档](https://nodejs.org/api/http.html#event-finish)、[TLS 文档](https://nodejs.org/api/tls.html#event-secureconnect)。报告只保存请求序号、连接 / 发送 / 完成布尔值以及取消后连接是否断开。脚本退出会关闭代理；清理前先记录状态，避免把脚本清理造成的中止假记为 ACP 取消。

探针自身的反误判回归测试使用明确标注的协议 / 本地 HTTP fixture：`node --test tools/m0/tests/acp-probe.test.mjs tools/m0/tests/inflight-proxy.test.mjs`。8 项测试覆盖标记跨 chunk、错误会话、用户回显、仅排队事件、生成已完成、无 `--prompt` 路径、无语义 chunk 但真实本地 HTTP 在途的取消、未授权请求不转发，以及见证过期后的拒绝确认。它们只验证探针逻辑，不作为真实 Harness / 模型验收记录。

[no-tools.patch.json](../tools/m0/no-tools.patch.json) 针对锁定的 `0.2.1-rc.2` 禁用默认 profile 的工具、子 Agent 和插件管理入口，且探针对权限请求一律拒绝。它是这次 M0 模型实验的限制措施；升级 Harness 后需重新核对最终 profile，不能将此 patch 当作完整的 M3 工具安全验收。

上述 no-tools patch 与自定义 OpenAI 兼容路由已组合运行真实 ACP 的无模型协议实验，初始化、创建、重启列出和恢复均通过，记录见 [acp-windows-no-tools.json](../tools/m0/results/acp-windows-no-tools.json)。这证明配置组合可以启动，不等于已经验证模型联网回复。

默认上游 ACP profile 含较多内置插件，因此本轮仅在空工作区做 M0 协议验证，未将其作为普通聊天的生产执行环境。M3 必须另行落实最小 profile、工具 guard、受限网络和宿主 MCP 工具权限，不能以此次协议连通代替这些安全边界。

## 树莓派实测补充

已在用户授权的Ubuntu 24.04.4 aarch64树莓派上原生运行四项Cordis生命周期测试并通过。固定部署SHA和命令详见 [M0/M1部署记录](m0-m1-validation.md)。真实Harness `0.2.1-rc.2` 在该主机的无工具空工作区通过7项协议检查，2项模型检查标记not_run，记录见 [acp-raspi.json](../tools/m0/results/acp-raspi.json)。未将Ubuntu结果推断为Lite系统验收。

树莓派已使用用户在Web页面录入的模型配置，在隔离、禁用工具的真实Harness中通过9项检查，包括短回复、确认HTTP请求真实在途后的取消及进程重启恢复。代理记录证实第二次请求已建立TLS连接并完成请求发送，响应尚未完成；取消后客户端断开、上游中止。元数据见 [acp-live-aarch64.json](validation/acp-live-aarch64.json)，不含密钥、回复或思考正文。

QQ WebSocket已完成101升级 / HELLO / READY，两条真实C2C消息完成模型处理、平台发送并取得回执，用户确认收到。记录见 [qq-live-aarch64.json](validation/qq-live-aarch64.json)。这不推导为群聊、频道或微信均已验证。

## 最终系统与后续范围

- 本轮核心原生、真实模型、ACP协议 / 取消 / 恢复和QQ私聊路径已通过。
- 保留 Raspberry Pi OS Lite 64 位最终基线，系统切换后的回归仍待执行，不与当前Ubuntu结果混为一谈。
- QQ群聊 / 频道、微信iLink及后续阶段能力未做真实账号验收；Webhook验签回放也不能代替真实公网回调联调。

本轮结论限定于已验证的QQ私聊与模型路径，不将所有渠道、最终Lite系统或后续阶段标为全部完成。
