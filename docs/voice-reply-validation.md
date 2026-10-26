# 多句语音回复格式修复验收

日期：2026-10-26（Asia/Shanghai）。用户在 QQ 请求语音，运行 `4c8f09a1-0d12-4520-9f0d-7272adb73d88` 返回“每个回复对象只允许一句话”。本轮定位、修复语音正文的句界兼容问题，不改性格、模型配置或既有历史。

## 诊断与修复范围

只读核对该 Run：语音配置已启用，当前请求能力快照也为可用；运行失败，TTS 事件、语音制品和 Outbox 均为零。因此错误发生在回复解析阶段，尚未调用 Piper 或向 QQ 上传。失败前的原始模型输出未持久化，不能声称已经确认该次返回对象的具体类型或正文。

用合成签名 QQ 入站和回环模型返回两句话的 `voice` 对象，在旧二进制重现相同的解析失败；新二进制通过。修复仅将严格 schema 中无 `asset_id/prompt` 的新合成多句语音，按完整句规范化为逐句语音；不额外请求模型、不丢尾句、不复制已有音频引用。原对象长度 / NUL 检查、拆分后总数量上限（配置值且最多四项）、单句合成限额、发送间隔、来源与权限验证保持。文字 / 图片 / 视频多句对象仍拒绝。

原失败记录保留，不重放旧消息、不更换其原始被动回复目标，也不伪造成功。实际 QQ 上传和客户端收听需要新的真实用户请求；本轮自动验证不向生产 QQ 发送测试消息。

## 本地检查

- `cargo fmt --all --check`、`cargo test --workspace --locked`、`cargo build -p dshd --locked` 通过；全量 **306 daemon + 4 kernel**，新增三组解析与边界测试。
- `python -S -m unittest discover -s tools/tests`：91 项通过；新增 Python 脚本语法检查通过。
- `replies-smoke.py`：14 项通过，含禁用语音时多句正文完整保留。
- `voice-replies-smoke.py`：10 项通过，含签名合成 QQ → 回环模型 → 逐句计划 / 持久 Outbox、顺序 / 间隔、超四项整体拒绝、重启不重播。合成账号在启用前设置持久发送阻断，整个测试及重启期间没有真实 QQ 出站。
- Windows 未执行实际 Piper，正式 Linux 二进制、真实 Piper 和真实模型结果须在远端完成后单列。

受控复现于 15:59（+08:00）使用旧本地 `development` 二进制，SHA-256 为 `a797c5221625fcb2a52f5b8c47fa14f1ad9bfd2afdb06d47b424972a901ea02e`。`voice-replies-smoke.py` 已设置合成 QQ 的持久发送阻断，但两句 `voice` 输出未能完成 Run，脚本退出 1，失败阶段为 `signed synthetic QQ request completes after a multi-sentence voice model reply`。失败报告保留于忽略目录 `.local/voice-replies-before.json`。这是合成输入对旧解析行为的复现，不能据此补写生产失败请求的原始模型输出。

修复后于 16:01（+08:00）使用新的本地 `development` 二进制，SHA-256 为 `2b1f78f66077690680b0caa1b74cf4692c25d3ef479a158368049c8ebf3aaa12`，以下两项均退出 0：

| 检查 / 本地报告 | 结果与边界 |
| --- | --- |
| `replies-smoke.py` / `.local/voice-fix-replies-local.json` | 14 项；原 13 项回归及新增多句语音在禁用 TTS 时按句保留完整文字、顺序和 1500ms 间隔 |
| `voice-replies-smoke.py` / `.local/voice-fix-qq-local.json` | 10 项；真实 Rust 进程、签名合成 QQ 入站和回环模型 HTTP，核对两句完整展开、冻结目标、两项原子入队、超过四句时合成 / 发布前整体拒绝、重启后计划和队列不重放 |

两个报告的运行提交标识均为 `development`，不作为正式发布二进制身份。新脚本无 `--runtime-root` 时明确只验证文字降级，不创建音频；传入已安装的固定 Piper 目录后，另外核对每句实际 WAV 的 PCM / 摘要 / 时长、各句制品与 `source_run_id` 的绑定、TTS 完成记录及 worker 回收。QQ 出站保持持久阻断，脚本不使用真实 QQ 凭证；这些检查不包括上传、平台接收或客户端播放。

原始问题、旧二进制受控复现与新版本结果分别记录，不将模拟 HTTP、已入队或已生成 WAV 等同于真实 QQ 上传或客户端播放。

## 发布与远端验证

本轮代码提交为 `a566f608328d3d6acbdbe08327057eac301e86f8`，前端打包档案 SHA-256 为 `1f8484bd96ec7c8a472edc95241b0228cc67a869c030d33e042e34d6bc625fa2`。目标为 `campanus@192.168.189.191`；上一实际运行代码为 `e4c955eda1ca7fa01062470f0cad5313e79f6a31`。本次部署前只读快照中刘嘉卉性格为 v2，运行配置已由上一轮 v5 更新到 v6；本轮保留当前 v6 和原有数据，不恢复旧配置。

本轮已完成提交、推送与固定 SHA 部署，**2026-10-26 16:23:49 +08:00** 切换成功。部署前核验 Raspberry Pi 5 Model B Rev 1.1、7,933 MiB 内存、Ubuntu 24.04.4 LTS / aarch64、Rust / Cargo 1.88.0；目录归属 campanus，origin 匹配且源码工作区干净，磁盘可用约 25GiB。构建与测试使用单并发及 rsproxy，发布构建耗时 8 分 10 秒，部署命令退出 0。固定版本发布命令如下：

```sh
git -C /home/campanus/apps/dsh/repo fetch origin
git -C /home/campanus/apps/dsh/repo show a566f608328d3d6acbdbe08327057eac301e86f8:deploy/deploy.sh > /home/campanus/.cache/dsh/deploy-a566f60.sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-a566f60.sh \
  a566f608328d3d6acbdbe08327057eac301e86f8 \
  /home/campanus/.cache/dsh/admin-a566f608328d3d6acbdbe08327057eac301e86f8.tar.gz \
  1f8484bd96ec7c8a472edc95241b0228cc67a869c030d33e042e34d6bc625fa2
```

远端全量 **307 daemon + 4 kernel** 全部通过。源码 HEAD、前端 `release.json` 与 live / ready 健康接口都为上述完整 SHA，`dsh.service` active，正式二进制 SHA-256 为 `670fa23b1d07bf27f2c295748e7d038c690d6a862e7f466c594ae4575198d34b`。切换后首次就绪探测短暂连接失败，随后在重试窗口内 ready，最终部署退出 0；未将首次探测当作成功证据。

以下脚本从 `/home/campanus/apps/dsh/current/source/tools/` 执行，均使用 `--binary /home/campanus/apps/dsh/current/bin/dshd --web-dir /home/campanus/apps/dsh/current/web`，均退出 0。Piper 参数为 `--runtime-root /home/campanus/apps/dsh/shared/speech/piper-1.8.0`，报告输出到 `/home/campanus/apps/dsh/shared/logs/`。

| 脚本 / 报告文件 | 结果与证据 |
| --- | --- |
| `replies-smoke.py --report …/voice-fix-replies-a566f60.json` | [14 项通过](validation/voice-reply-replies-aarch64.json) |
| `voice-replies-smoke.py` 加 Piper 参数，report 为 `voice-fix-qq-a566f60.json` | [15 项通过](validation/voice-reply-qq-aarch64.json)，合成签名 QQ 入站、回环模型、实际两句 Piper 与持久队列；真实 QQ 出站被阻断 |
| `speech-smoke.py` 加 Piper 参数，report 为 `voice-fix-speech-a566f60.json` | [18 项通过](validation/voice-reply-speech-aarch64.json)，实际音频、认证 / 私有归属、过期回收、单 worker 与超时精确进程回收 |
| `voice-reply-live.py` 加 Piper、`--source-data-dir /home/campanus/apps/dsh/shared/data`、`--personality /home/campanus/apps/dsh/current/source/docs/examples/liu-jiahui-personality-v1.json`，report 为 `voice-fix-live-a566f60.json` | [2 次真实模型请求、11 项检查通过](validation/voice-reply-live-aarch64.json)，临时无渠道实例和实际 Piper |

受控的多句对象由宿主规范化，两个 WAV 分别为 2,472 / 2,821 ms、109,100 / 124,460 字节，均为 22,050Hz 单声道 16 位 PCM；各自绑定同一来源 Run 的独立制品，持久发送顺序为 1、2，间隔为 0 / 1,500ms。两句合成后 worker 均退出，超四句提案在任何音频创建或部分发布前整体失败，重启未重放模型或发送。

真实模型检查在 16:24:27–16:24:49 +08:00 完成：短问候产生一段 2,554ms / 112,684 字节音频；两句话请求产生 1,858ms / 81,964 字节和 2,043ms / 90,156 字节两段音频。两次模型输出本身已符合逐句协议（`normalized=false`），因此真实模型检查证明模型到实际音频的链路，多句对象规范化由上面的受控 HTTP + 真实 Piper 用例证明，不推断原生产失败的原始输出。

真实模型脚本只读生产模型配置及对应凭证，不读聊天 / 记忆，所有会话、消息、音频均在临时数据库；未启用 QQ / 微信、未创建生产测试消息。测试完成后没有语音 worker 遗留，只有一个生产 dshd 进程，可用内存采样 7,314 MiB。正式未认证语音 API 返回 401。

## 数据保留与恢复

[部署证据](validation/voice-reply-deployment-aarch64.json)核对部署前后稳定身份摘要相同，运行配置均为 **v6**，刘嘉卉性格均为 **v2**，六字段与发布资产一致；均为 **4 个会话、34 个 Run、66 条消息、0 条记忆**。原失败 Run 的状态、错误、时间、零 TTS / 零音频 / 零 Outbox 均保持，不改写历史、不重放过期请求。

上一版本仍在 `releases/e4c955eda1ca7fa01062470f0cad5313e79f6a31`；停服后私有 SQLite 备份为 `/home/campanus/apps/dsh/shared/backups/pre-a566f608328d3d6acbdbe08327057eac301e86f8-20261026T082348301208867.sqlite`。测试 / 构建日志为 `shared/logs/test-<完整SHA>.log` 和 `build-<完整SHA>.log`。本轮未执行生产回滚或数据恢复，代码回滚仍先经兼容门禁，不能通过删除数据绕过检查。

本轮未执行真实 QQ 上传、平台接收或客户端播放，也没有新增微信媒体能力。新真实用户请求的收听结果须单独核实，不将签名模拟入站、已入队或生成 WAV 扩称为真实 QQ 播放通过。最终验收记录提交只含文档和脱敏证据，不为此重启应用；正式运行仍为 a566f60 的完整代码 SHA。
