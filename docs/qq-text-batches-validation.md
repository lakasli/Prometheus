# QQ 连续短文本合并验收

日期：2026-10-26，Asia/Shanghai。契约见 [QQ 连续短文本合并](qq-text-batches.md)。本轮沿用生产单一助手性格 v1，不改模型配置。

## 本地验证

本地 `cargo fmt --all --check`、`cargo test --workspace --locked` 和 `cargo build --locked -p dshd` 通过；全量为 300 个 daemon 与 4 个 kernel 测试，包含 12 项批次、5 项实际入站 / 调度和 1 项容量重排检查。`python -S -B -m unittest discover -s tools/tests` 为 91 项，其中兼容性专项 33 项、停止服务后再检查的隔离 Bash 分支 10 项；部署 / 回滚脚本 Bash 语法检查、新验收脚本语法和差异检查通过。

`tools/qq-text-batches-smoke.py` 26 项通过：真实 Rust 进程、Ed25519 签名 Webhook、持久接收、回环模拟模型及 Outbox，包含三条入站只形成一轮、重复 / 换 event_id 重投、pending 重启、inflight 新批、命令 / 媒体 / Skill / 长文本边界、配置和性格冻结、8 条上限及 owner 隔离。始终以持久发送屏障阻止合成账号出站，不访问真实 QQ。

既有 `tools/conversation-presence-smoke.py` 19 项回归通过，时间感、单性格与快照语义保持。本地二进制为 `development` 标识，不能当作正式发布 SHA；真实模型、真实 QQ 出站及客户端结果分别见下文。

## 发布与远端验收

部署目标已重新核验 Raspberry Pi 5 Model B Rev 1.1、7,933 MiB、Ubuntu 24.04.4 LTS / aarch64、Rust / Cargo 1.88.0，远端仓库干净且 origin 匹配。原运行版本为 `60d559fa583611768ee9e58ef9270ac1eeb7b09c`。

本节在固定 SHA 推送、部署及实际验收后补充，不把准备状态记作部署通过。签名模拟 QQ Webhook、真实模型请求、Outbox 入队、真实 QQ 出站和客户端观察必须分别记录。
