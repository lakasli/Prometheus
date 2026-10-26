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

初版机器报告分别保留为 [模拟 QQ](validation/initial-qq-text-batches-aarch64.json)、[时间与性格](validation/initial-qq-text-batches-presence-aarch64.json)、[逐句回复](validation/initial-qq-text-batches-replies-aarch64.json) 和 [隔离真实模型](validation/initial-qq-text-batches-live-aarch64.json)，不由修正版结果覆盖。

第一次用户手机测试实际发送的是三条相同的完整算式文本，形成一个 Run、一个请求快照和一个已送达回复；用户反馈“已发，收到一次统一结果 59”。机器证据见 [重复正文的真实 QQ 记录](validation/qq-text-batches-real-qq-repeated-aarch64.json)，它只能证明三条平台消息合并，不能替代三段不同内容的验收。

第二次用户按三段发送“帮我算总和”“17+19”“+23”，反馈“已发，出现异常”。接收时间间隔实测为 1,605ms 和 3,135ms，均超出初版 1,500ms 静默窗，产生了三轮回复；接收、生成和投递都完成，但目标行为失败。保留 [初次失败证据](validation/qq-text-batches-real-qq-initial-failure.json)，不将其计为通过。

修正将静默窗改为 3,500ms、从首条起的合并等待上限改为 8,000ms，加入上述实测间隔回归；仍保留数量、字节、冻结快照、回复期限及并发边界。接收积压或处理延迟仍可能使内容在截止后另建批次，实测间隔回归不代表所有网络和负载情况均会合并。

### 修正版本部署

代码与运行 SHA：`1bce688edb4d22573168ba893eede785d4c143b3`。本地提交、推送并核对远端 main 包含此提交后，构建同 SHA 前端归档；远端从该提交读取 `deploy/deploy.sh`，执行：

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-1bce688.sh \
  1bce688edb4d22573168ba893eede785d4c143b3 \
  /home/campanus/.cache/dsh/admin-1bce688edb4d22573168ba893eede785d4c143b3.tar.gz \
  263c1ba69e98d36768ffdccb296eff3ced0fe9a633e4acc9c24b74915951a9b2
```

部署退出 0，于 2026-10-26 11:18:44 +08:00 切换至该 SHA；aarch64 二进制 SHA256 为 `dcf4dfaf64e3a930178ff03000229656ae9f24d4be5d913813ca67962118d031`。源码工作区干净，`current`、`/health/live`、`/health/ready` 与前端 `release.json` 均对应同一完整 SHA，用户级 `dsh.service` 为 active。启动期间健康轮询一次连接失败，随后 ready；没有发生回滚。远端 Rust 302 个 daemon + 4 个 kernel 测试全部通过，Python 91 项及部署 / 回滚脚本语法检查通过。

切换前后核对稳定 assistant identity 与已保存性格的 SHA256 相同，性格 revision 1、配置 v5；正式库仍为 4 个会话、26 个 Run、0 条长期记忆，部署过程未新增或清理聊天。预升级备份为 `shared/backups/pre-1bce688edb4d22573168ba893eede785d4c143b3-20261026T031842902865669.sqlite`。上一可用制品为初版 `401e7ca…`，支持已写入的批次 Schema；未演练生产回滚，不把隔离 Bash 分支测试计为实际回滚。详细环境、命令、版本与数据保持证据见 [部署元数据](validation/qq-text-batches-deployment-aarch64.json)。

### 修正版本自动验收

在固定 release 的独立临时数据库运行以下脚本，均退出 0；模拟实例不写正式库、不向真实 QQ 发送：

| 脚本 | 结果 | 证据 |
| --- | --- | --- |
| `tools/qq-text-batches-smoke.py` | 26 项通过 | [签名入站 / 模拟模型 / Outbox](validation/qq-text-batches-aarch64.json) |
| `tools/conversation-presence-smoke.py` | 19 项通过 | [时间与单性格回归](validation/qq-text-batches-presence-aarch64.json) |
| `tools/replies-smoke.py` | 13 项通过 | [逐句回复与媒体回归](validation/qq-text-batches-replies-aarch64.json) |
| `tools/qq-text-batches-live.py` | 7 项通过，一次合成真实模型请求 | [真实模型独立记录](validation/qq-text-batches-live-aarch64.json) |

上述脚本均使用 `--binary <release>/bin/dshd --web-dir <release>/web --report <shared/logs/...json>`，真实模型脚本另用 `--source-data-dir shared/data` 只读加载生产模型及性格。对应远端日志为 `shared/logs/qq-text-batches[-live/-presence/-replies]-<完整SHA>.json`，Rust 构建与测试日志为 `build-<完整SHA>.log` / `test-<完整SHA>.log`，Python 为 `python-tests-1bce688.log`。

通过既有 SSH 隧道刷新已认证的管理页，实见 QQ 已连接、配置 v5、服务与 Web 均为 `1bce688edb`、待发消息 0。未认证访问实际 `/api/admin/v1/status` 返回 403；此前误用不存在的 `/api/v1/overview` 返回 404，不作为鉴权证据。没有修改管理页面或连接配置。

### 三段不同内容的真实 QQ 与客户端验收

修正版启用后，用户再次分三条发送“帮我算总和”“17+19”“+23”。实测接收间隔为 1,151ms、2,255ms：三个独立平台消息 ID、三条原始片段、三个 Inbox 事件归入同一 Run `341d36e8-26c0-4882-a7e9-f87613d80e54`；聚合原文与模型末条输入均为按序换行的三段内容。单一性格 v1、时间来源与批次来源提示均进入摘要校验正确的一个模型请求快照。

该 Run 完成，只产生一个 Outbox、一条已送达的回复及平台回执；目标绑定最后一条真实消息 ID，结果包含 59，未触发摘要任务。只读机器核对全部通过，见 [真实 QQ 机器记录](validation/qq-text-batches-real-qq-aarch64.json)。正式库 Run 数由 26 增至 27，正好对应这一次新的用户测试；会话与长期记忆数量不变。

用户直接反馈“已发，只收到一次统一结果 59”，客户端观察见 [独立用户反馈记录](validation/qq-text-batches-client-observations.json)。本机没有 QQ 客户端窗口，未声称自动化观察手机界面；机器记录与用户反馈分开保留。本轮目标完成，未扩大到群消息、微信或正在推理中的输入改写。
