# Piper 本地语音合成

> 2026-10-30 起正式目标为 Windows，不再部署到树莓派。下述 Piper 安装脚本、aarch64 lock、默认 `venv/bin/python` 路径和 Unix 资源限制属于现有 Linux 实现。Windows 解释器、锁定依赖、路径、资源限制、取消与真实中文合成须另行适配和验收；未通过时保持语音不可用并保留文字降级。流程见 [Windows 开发与部署](development-workflow.md)。

本文描述本次实现契约；部署 SHA、中文合成实测和 QQ 客户端结果以本轮独立验收记录为准，不由本文代替验收。语音识别与实时通话未随本切片交付。

## 数据与执行路径

普通回复的 `voice` 句段保留可读文字，由 Rust 调用本地 Piper worker，生成 PCM WAV，再交给渠道发送。QQ 采用官方富媒体上传流程，上传成功与客户端可见、可播放分别核验。TTS 不可用时回复保留文字，不伪造语音发送成功。

Piper 按次启动，单进程最多一个活跃合成任务，没有等待队列；超限立即返回忙状态。worker 完成后退出，不为历史会话保留进程。断开 HTTP 请求不会提前归还仍在运行的进程许可；超时会终止并等待子进程退出。Python 使用隔离模式，环境变量不继承模型或平台密钥。ONNX Runtime 固定 CPU provider，推理线程数为 2、会话并行线程数为 1；CPU / 地址空间限制是推理资源保护，不是通用代码安全沙箱。

朗读原文仅通过子进程标准输入传递，不放入命令行参数、制品元数据或审计日志。代码块、独立 URL token、原始音素标记与不适合朗读的 Markdown 标记会清理；空内容和超限内容拒绝。普通对话本身仍遵循已有消息保存规则。

WAV 要求完整 RIFF、单声道、16-bit PCM、允许的采样率与字节率，拒绝截断、重叠字段、非 PCM、异常时长和超限。当前固定中文声音输出 22,050 Hz。每个音频最大 4,000,000 字节、90 秒；默认每段 300 字符、最长可设 400 字符，超时默认 30 秒、可设 5–60 秒。

## 配置与短期制品

`RuntimeConfig.speech` 通过现有配置草稿、字段校验、`If-Match`、应用记录和版本回退管理，成功应用后立即生效，无需重启：

```json
{
  "enabled": false,
  "voice": "zh_CN-huayan-medium",
  "timeout_seconds": 30,
  "max_text_chars": 300
}
```

Web 不提供任意可执行路径、模型路径或 shell 参数。部署级 `DSH_PIPER_EXECUTABLE` 可指定 Python 解释器，`DSH_PIPER_MODEL` 可指定同一锁定声音的 ONNX 文件；它们不是模型可修改参数。默认运行时位于数据目录同级的 `speech/piper-1.8.0/`。

音频作为私有短期 SQLite BLOB 保存，元数据含摘要、时长、合成耗时、声音和过期时间。读取检查 owner、TTL、原会话归属与记忆上下文水位；水位变化后旧音频不能再次读取。普通回复派生制品还绑定 `source_run_id`，保存事务核对运行状态和原水位；源 Run 完成前不可读，取消、失败或重启中断后剪枝，防止未发布结果成为下一轮可引用制品。管理员直接试听不绑定源 Run。默认一小时过期，过期/失效制品由维护清理和访问清理回收；上限为全局 64 个 / 32 MiB、每 owner 20 个。音频访问使用认证、`no-store, private` 和固定 MIME，不能作为匿名公网下载入口。

富媒体与短期制品基础语义要求 reader 9；绑定源 Run 的派生制品要求 reader 10。旧版本回滚必须经过现有 reader 兼容检查，不能让旧二进制静默忽略富媒体或取消源授权。已有制品表通过可空列增量迁移，直接上传或试听制品维持 `source_run_id=NULL`。

| 管理接口 | 行为 |
| --- | --- |
| `GET /api/admin/v1/speech` | 配置与运行时文件存在状态、worker 数、限制、许可信息；实际可合成性由试听验证 |
| `POST /api/admin/v1/speech/syntheses` | `{"text":"普通文字"}`；成功 201，返回私有制品引用，不发送到聊天平台 |
| `GET /api/admin/v1/speech/assets/{id}` | 仅制品 owner 读取 WAV；跨 owner、过期或失效为 404 |
| `DELETE /api/admin/v1/speech/assets/{id}` | owner 清理音频，需既有 Origin / CSRF 校验 |

## 历史 Linux 依赖与部署

`deploy/install-piper.sh` 仅支持历史 Linux aarch64 / Python 3.12，安装到 `/home/campanus/apps/dsh/shared/speech`，不修改应用配置、源代码或既有数据，不要求 sudo。它不适用 Windows，不再作为当前默认安装步骤；以下命令仅记录当时从已推送固定 SHA release 安装的方式：

```sh
bash /home/campanus/apps/dsh/releases/<SHA>/source/deploy/install-piper.sh
```

`deploy/piper-aarch64-py312.lock` 固定 Piper 1.8.0、ONNX Runtime 1.31.0 和完整运行依赖 wheel 版本与 SHA-256，使用 `--require-hashes --only-binary=:all:`。可通过 `PIP_NO_INDEX=1` 与 `PIP_FIND_LINKS=<已核验的私有wheel缓存>` 离线复用同一制品；不能绕过哈希校验。

声音固定为 `rhasspy/piper-voices` 的 commit `5041a7a49ae5b3a0f30e6837769eb03fdbb231bd` 下 `zh/zh_CN/huayan/medium`。ONNX SHA-256 为 `9929917bf8cabb26fd528ea44d3a6699c11e87317a14765312420be230be0f3d`，配置 SHA-256 为 `d521dc45504a8ccc99e325822b35946dd701840bfb07e3dbb31a40929ed6a82b`；安装和合成前均验证。

Piper 引擎标记为 GPL-3.0-or-later；中文 huayan 模型卡将训练数据集许可标为 `Unknown`。本轮用于用户指定的个人本地评估，不能据此宣称声音权重已获商业分发许可。引擎、运行库与声音权重分别核查，不能用模型仓库首页的 MIT 标签覆盖具体模型卡。

## 验证命令

无模型时可验证禁用、鉴权、CSRF、字段约束、配置冲突和缺失运行时：

```powershell
python -S tools/speech-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist --report .local/speech-smoke-windows.json
```

历史 Linux 中文合成检查增加 `--runtime-root /home/campanus/apps/dsh/shared/speech/piper-1.8.0`，不能直接照搬到 Windows。完成 Windows 运行时适配后再更新并执行真实合成命令。脚本使用独立临时数据库和合成验收文字，不读取生产会话或平台凭据；覆盖实际 WAV、摘要与时长、跨 owner 拒绝、无原文日志、worker 退出、显式清理与过期剪枝。它不代替 QQ 真实上传与客户端播放验收。

参考： [Piper 官方 CLI](https://github.com/OHF-Voice/piper1-gpl/blob/v1.8.0/docs/CLI.md)、[官方 Python API](https://github.com/OHF-Voice/piper1-gpl/blob/v1.8.0/docs/API_PYTHON.md)、[引擎许可元数据](https://github.com/OHF-Voice/piper1-gpl/blob/v1.8.0/setup.py)、[固定声音模型卡](https://huggingface.co/rhasspy/piper-voices/blob/5041a7a49ae5b3a0f30e6837769eb03fdbb231bd/zh/zh_CN/huayan/medium/MODEL_CARD)、[QQ 当前富媒体消息说明](https://bot.q.qq.com/wiki/develop/api-v2/server-inter/message/rich-media.html)。
