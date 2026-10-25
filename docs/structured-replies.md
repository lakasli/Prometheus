# 逐句回复与富媒体投递

本契约对应本次实现；固定运行 SHA、真实平台结果与未测范围见独立验收记录。模拟测试不表示真实 QQ 发送通过。

## 模型与宿主的职责

普通 Native 对话要求模型返回 JSON 列表，每个对象只有一句话，每轮 1–4 项。显式 Skill 的结构化草稿、HTML 绘画、摘要和记忆抽取保留各自协议。

```json
[
  {"type":"text","text":"我在这里。","delay_ms":0},
  {"type":"voice","text":"今天也一起慢慢来吧。","delay_ms":2000}
]
```

允许 `text`、`voice`、`image`、`video`。`text` 为可读单句；`voice` 由固定本地 Piper 合成；`image` 只能引用当前来源会话的 `asset_id`，或在管理员明确启用原生生图能力后提供 `prompt` 让宿主生成；`video` 只引用已上传的 MP4 制品，不声称具有视频生成模型。模型不能指定 URL、文件路径、接收者、平台令牌、Shell 或权限。

服务端拒绝未知字段、多句对象、超长内容和超过四项的列表。忽略 JSON 协议但返回普通文字的兼容 provider 会被宿主按句界规范化；超过数量限制则任务失败。消息正文保存可读文字，`run_reply_plans` 保存呈现计划，认证消息和 Run API 返回 `reply_plan`。

`reply.min_interval_ms` 默认 1500（可设 1000–10000），`max_interval_ms` 默认 8000（不小于最小值，最多 30000），`max_items` 默认 4（可设 1–4）。第一项不额外等待，后续模型提出的间隔由宿主钳制。配置沿用版本草稿、字段验证、If-Match、应用和回退流程。

## 可靠投递

Run 完成、可读消息、计划、Outbox 父记录与逐项记录在同一 SQLite 事务提交。每条回复有稳定的 `msg_seq`，QQ C2C 遵守当前官方最多四次被动回复规则。

下一条从上一条实际发送完成后开始计算间隔，账号另有限流水位；模型生成耗时不能吞掉发送间隔。同会话尚未处理的回复阻挡较新的回复，其他会话可继续。重启把在途发送标为未知，未知结果不自动重试，也不继续该会话后面的内容；从未开始的发送按持久水位恢复。此策略不承诺平台端 exactly-once。

发送前重查 QQ 账号、凭据引用、目标类型及被动回复期限；上传后再查一次。媒体上传明确 `srv_send_msg=false`，拿到 `file_info` 才发送消息。已明确拒绝的媒体尝试可在同一序号改发文字，并记录降级；发送结果未知时禁止降级重发。WAV 是否在客户端形成可播放语音条须真实验证，不能只根据上传 HTTP 成功认定。

## 私有媒体与回收

`POST /api/admin/v1/conversations/{id}/media` 接受 `{kind,data_base64}`，只允许该会话 owner 上传；PNG 实际解码、像素限额、去元数据，MP4 验证有界盒结构、视频轨道和最长 60 秒，大小最多 20MiB。后端不解码 MP4，不因容器校验宣称已通过客户端编解码验收。

制品在私有 SQLite BLOB 中，绑定 owner / conversation / 遗忘水位，不接受任意本地文件或远程地址。读取须认证，返回 `no-store` / `nosniff`；跨会话、跨 owner、过期或上下文失效返回 404。PNG / MP4 默认保留一天，每 owner 最多 32 项、64MiB，全局 128MiB。Piper 音频单独保留最多一小时、全局 32MiB。分钟级清理与读取时校验共同回收失效制品。

已使用这些持久语义的数据库要求 reader 9。旧版回滚须经过 `deploy/check-compatibility.py`，不能让旧发送器把逐项回复作为整段重新发送；代码回退与数据恢复分别处理。

## 验证命令

```sh
python3 tools/replies-smoke.py --binary <release>/bin/dshd --web-dir <release>/web --report <private-log>/replies-smoke.json
python3 tools/speech-smoke.py --binary <release>/bin/dshd --web-dir <release>/web --runtime-root /home/campanus/apps/dsh/shared/speech/piper-1.8.0 --report <private-log>/speech-smoke.json
```

这些命令创建私有临时数据库和合成账户，不读取用户聊天。QQ 真实客户端、真实外部模型与模拟 provider 的结果分别记录。
