# QQ 连续短文本合并验收

日期：2026-10-26，Asia/Shanghai。契约见 [QQ 连续短文本合并](qq-text-batches.md)。本轮沿用生产单一助手性格 v1，不改模型配置。

## 本地验证

本地 `cargo fmt --all --check`、`cargo test --workspace --locked` 和 `cargo build --locked -p dshd` 通过；修正窗口后全量为 301 个 daemon 与 4 个 kernel 测试，包含 13 项批次、5 项实际入站 / 调度和 1 项容量重排检查。新增回归精确覆盖真实输入间隔 1,605ms、3,135ms，断言三条按序合并且仍受 8 秒上限限制。`python -S -B -m unittest discover -s tools/tests` 为 91 项，其中兼容性专项 33 项、停止服务后再检查的隔离 Bash 分支 10 项；部署 / 回滚脚本 Bash 语法检查、新验收脚本语法和差异检查通过。

`tools/qq-text-batches-smoke.py` 26 项通过：真实 Rust 进程、Ed25519 签名 Webhook、持久接收、回环模拟模型及 Outbox，包含三条入站只形成一轮、重复 / 换 event_id 重投、pending 重启、inflight 新批、命令 / 媒体 / Skill / 长文本边界、配置和性格冻结、8 条上限及 owner 隔离。始终以持久发送屏障阻止合成账号出站，不访问真实 QQ。

既有 `tools/conversation-presence-smoke.py` 19 项回归通过，时间感、单性格与快照语义保持。本地二进制为 `development` 标识，不能当作正式发布 SHA；真实模型、真实 QQ 出站及客户端结果分别见下文。

## 发布与远端验收

部署目标已重新核验 Raspberry Pi 5 Model B Rev 1.1、7,933 MiB、Ubuntu 24.04.4 LTS / aarch64、Rust / Cargo 1.88.0，远端仓库干净且 origin 匹配。原运行版本为 `60d559fa583611768ee9e58ef9270ac1eeb7b09c`。

### 初次发布与真实输入发现的问题

初次运行版本为 `401e7ca5a1f2b9162dcb8906cdaba08f57419503`，2026-10-26 10:50:09 +08:00 切换；前端归档 SHA256 为 `d0c11543f126a5441b043992098dca67de00e2ed92fcb283601982a4ce1082b0`，aarch64 二进制 SHA256 为 `eec89819bec78bfa516609838fc4b3b607a0279d1a119fcc8d0d6f02b6a59b5f`。远端 Rust 301 个 daemon / 4 个 kernel 测试通过；签名 Webhook 模拟 26 项、时间与性格回归 19 项、逐句回复回归 13 项和隔离真实模型 7 项通过。真实模型测试仅消耗一个合成请求，QQ 出站始终由持久屏障阻止。

第一次用户手机测试实际发送的是三条相同的完整算式文本，形成一个 Run、一个请求快照和一个已送达回复；用户反馈“已发，收到一次统一结果 59”。机器证据见 [重复正文的真实 QQ 记录](validation/qq-text-batches-real-qq-repeated-aarch64.json)，它只能证明三条平台消息合并，不能替代三段不同内容的验收。

第二次用户按三段发送“帮我算总和”“17+19”“+23”，反馈“已发，出现异常”。接收时间间隔实测为 1,605ms 和 3,135ms，均超出初版 1,500ms 静默窗，产生了三轮回复；接收、生成和投递都完成，但目标行为失败。保留 [初次失败证据](validation/qq-text-batches-real-qq-initial-failure.json)，不将其计为通过。

修正将静默窗改为 3,500ms、从首条起的合并等待上限改为 8,000ms，加入上述实测间隔回归；仍保留数量、字节、冻结快照、回复期限及并发边界。本节后续记录修正版本的固定 SHA 部署与重新验收；未执行部分不计通过。
