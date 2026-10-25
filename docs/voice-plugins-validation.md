# 本地语音、逐句富媒体、QQ 非群 API 与插件工厂验收

日期：2026-10-25，Asia/Shanghai。本文记录本轮实际结果；原架构的完整 M3 / M4 / M5 退出条件维持不变。协议模拟、真实模型、真实 QQ 平台和客户端观察分别列示。

## 发布与环境

目标为 `campanus@192.168.189.191`，实际复核 Raspberry Pi 5 Model B Rev 1.1、Ubuntu 24.04.4 LTS、aarch64、Rust / Cargo 1.88.0；总内存约 7,933 MiB，无 swap。本轮首次采样可用约 7,350 MiB，Wasmtime 发布编译期间约 6,420 MiB，均为采样值。未改变系统、桌面、启动目标或用户数据。

本地修改、检查、分阶段提交推送后，由固定完整 SHA 的 `deploy/deploy.sh` 部署。前端在 Windows 构建为静态资源，SHA-256 与源码 SHA 一起核对；Rust 在目标 Linux 构建。当前服务仍使用既有 loopback 8419，经 SSH 隧道 18419 供认证浏览器访问。真实配置、模型和数据继续位于 `shared/`，无凭据进入 Git。

| 阶段 | 固定运行 SHA | 结果 |
| --- | --- | --- |
| Piper / 逐句计划 / QQ 媒体 | `2c709999f0cc5537be177defd36dc6be1413cbd2` | 部署、健康、远端测试通过；真实 QQ 语音、图片、视频及普通对话语音通过下述独立验收 |
| 初次正式工厂构建 | `6f196e8f8d2513c124e1fbdd59c8ae4eac9af95b` | Linux 编译失败，aarch64 的 C char 符号性与 Windows 不同；尚未切换，旧服务保持健康 |
| 工厂 ABI、QQ scope 与媒体协议修复 | `387652c94ac4abc887506abe5ea74112e604ce42` | 已部署且服务健康；工厂隔离启动竞态与编译诊断失败按实保留，未计通过 |
| QQ 媒体和工厂工具链修复 | `25d973820568d72c7ace88bd8239b15c8ee29f56` | 已部署健康；32 个独立沙箱探针通过，Rust 编译管道操作被拒，后续继续最小修复 |
| 完成 Component 实例化与 clone 参数边界 | `615c7587c1c4b8cee5af69b4d13e572ed5534f49` | 正式部署 ready；工厂 release 18 项、32 个新工作器及单次真实模型 7 项全部通过；普通 QQ 原图回发仍暴露下述模型动作缺失 |
| 明确本条附图回传与最终回归 | `c75b868e33fea4a3c6065d2d593acaeb19847fc7` | 正式部署 ready；Linux 262 + 4、工厂 19、逐句 13、Piper 16 通过；普通 QQ 新消息 text＋image 及桌面实际回图通过 |

初次失败的日志保留在 `shared/logs/test-6f196e8f8d2513c124e1fbdd59c8ae4eac9af95b.log` 与 `plugin-factory-6f196e8.json`，不将失败写成通过。ABI 修复在本地改为 `std::ffi::c_char`，提交 `4e1b76c`，按同一完整流程重新部署。

`2c709999…` 的二进制 SHA-256 为 `f5bfff3186d6f955f79f71ed932cddfe7565de621347cdc6802b47a3c3c338b1`，前端压缩包为 `2e87592e1d2b44e409f407d7de75d545066a89bb87281c6776a1c260f1bf21ad`。`387652c…` 前端包为 `db93c9455287b68835dfa30cf8126c4d3258931a2681a7fac9c711b77f4ca5b6`。部署日志使用 `shared/logs/test-<完整SHA>.log`、`build-<完整SHA>.log`；切换前保存独立 SQLite 备份。前一可用版本最初为 `04fb8277e26dfa81c6e62599535c9e1aa704759c`。

当前正式 `c75b868…` 的二进制 SHA-256 为 `9a148978909efb2d6825faa1b4ff17f2a291b88c9d3ee81dc733a159155ebf0a`，前端包为 `5dbcde267f844a1245a9292999452fba533899fb85475a779d497ca3ac3f3275`。2026-10-25 23:27（+08:00）核对 `current`、源码 HEAD、`/health/ready.commit` 和前端 `release.json` 一致，服务 ready；release 编译 8 分 18 秒。上一版本 `615c758…` 保留，切换前备份为 `shared/backups/pre-c75b868e33fea4a3c6065d2d593acaeb19847fc7-20261025T152319130526256.sqlite`；没有执行生产回滚演练。

## 本地 Piper 与语音闭环

固定 Piper 1.8.0、ONNX Runtime 1.31.0、Python 3.12 aarch64 wheel 与 `zh_CN-huayan-medium` 声音的版本及哈希，安装到用户级 `shared/speech/piper-1.8.0`。先核验缓存 wheel 和模型，再运行固定版本的 `deploy/install-piper.sh`；安装命令退出 0，日志 `shared/logs/piper-install-cached-1b0a6a4.log`。无 sudo、云 TTS 或常驻推理进程。

真实中文合成实测 4.03 秒、峰值 RSS 216,040 KiB，生成 3.483 秒、153,644 字节、22,050 Hz / 单声道 / PCM16 WAV。独立认证 API 验收另一次合成耗时 4,825 ms、音频 3,912 ms / 172,588 字节，SHA-256 为 `52ce60b48990ee3aea361d0cf34fa3933ba1622243548c3a6b46a3b70a45c712`。这些是不同请求，不能混合为同一延迟样本或吞吐承诺。

运行 `python3 tools/speech-smoke.py --binary <2c709999 release>/bin/dshd --web-dir <release>/web --runtime-root <shared>/speech/piper-1.8.0 --report <shared>/logs/speech-2c70999.json` 退出 0，16 项检查通过，覆盖鉴权 / CSRF、配置校验、真实 WAV、摘要、跨 owner、无朗读正文日志、worker 退出、清理和 TTL。单独的慢进程夹具验证并发拒绝、超时 kill 后 wait；它不代表真实 Piper 性能。

在实际认证浏览器“本地语音”页保存并应用，配置版本 5，页面明确显示已生效、无需重启、空闲 worker 0/1。QQ 普通对话发送合成验收文字，Run `88b96be4-fcc8-467d-82f5-91157c241236` 由真实模型输出一个 `voice` 对象，后端 Piper 合成、Outbox seq=1 发送并得到平台消息 ID，桌面 QQ 出现 2 秒语音条。另一次 API 媒体验收的 3 秒语音条点击后出现播放进度 / 暂停状态；未以自动化观察声称真人听感或自然度评分通过。

引擎 GPL-3.0-or-later 与声音模型卡中数据集 `Unknown` 分别记录于 [语音契约](speech.md)，没有用仓库首页许可替代模型卡。ASR、实时通话、克隆声音未随本轮交付。

`c75b868…` 再运行同版 `tools/speech-smoke.py`，16 项全部通过，[正式报告](validation/piper-speech-c75b868-aarch64.json)记录真实 Piper 合成 4,487 ms、音频 3,517 ms / 155,180 字节 / 22,050 Hz；这里的受控慢进程仍只是超时回收夹具。`tools/replies-smoke.py` 在该正式二进制上也 [13 项通过](validation/structured-replies-c75b868-aarch64.json)。

## 逐句计划、QQ 图片和视频

`tools/replies-smoke.py` 在 `2c709999…` 上退出 0，13 项检查通过：逐句 JSON、强制间隔、消息可读历史、数量 / 单句 / URL 拒绝、禁用语音降级、私有 PNG、伪 MP4 拒绝、跨 owner / 会话、TTL 和重启持久计划。报告 `shared/logs/replies-2c70999.json`，只用隔离临时数据库和模拟 provider。

真实 QQ 媒体脚本来自已推送 `4a4ed40f2c4f0c46808b92a59cca455fa84ff9ae`，运行程序为 `2c709999…`，两者不能混作运行 SHA。脚本仅接受本次明确测试 marker、匹配账号 / owner / 目标、冻结回复期限、且仅 seq=1 已发送的上下文；不读取或复制其他聊天。运行退出 0，`shared/logs/qq-media-2c70999.json` 中 seq=2 的 Piper WAV、seq=3 的自生成几何 PNG、seq=4 的自生成 2 秒 H.264 MP4 均上传 HTTP 200、发送 HTTP 200 并有消息 ID。未知结果不自动重试。

原始机器报告仍为 `client_verified=false`，保留当时状态；之后独立桌面观察确认三种媒体均出现在“ 大肥鱼 ”测试对话。语音进入播放状态，PNG 显示蓝圆与橙方块，MP4 打开真实视频播放器并从 0 秒推进到 1 秒。客户端观察与平台 HTTP 接受分别成立，不改写原始机器报告。

普通 QQ 单 PNG 入站、视觉描述、Native 回答和逐句发送在 Run `b9ca8a1c-bf2e-41a7-bcbb-97e4965a4414` 实测：模型正确回答蓝色实心圆与橙黄色实心正方形。三条文本实际发送完成时间为 `1792930781315 / 1792930783850 / 1792930786384` ms，间隔 2,535 / 2,534 ms，超过配置最小 1,500 ms。该轮模型只用文字承诺回图，没有输出 image 对象，因此**该轮原图回发未通过**；随后提交修复分开发送与生成能力、规范白名单 `asset_id` 并强化媒体动作协议，需以后续重测结果单独认定。

媒体范围见 [逐句契约](structured-replies.md) 和 [QQ 接入](qq-non-group-api.md)。QQ C2C 目前按当前官方最多四次回复、60 分钟窗口执行；旧 GitHub 文档的五次限制、暂不支持 file_data 和仅 SILK 说明不作为当前实现依据。没有原生生图 provider 时，不声称已经具备新图片生成能力；发送已有图片与生成图片分别判断。

在 `615c758…` 上用同一合成图和新 caption 再次普通对话验证，Run `a6ce7b51-4912-4630-a1a1-2469a8c34c39` 完成图片识别，但计划只有两项 text；平台发送间隔 2,625 ms，仍没有实际 image 动作。当前图片制品、宿主协议和白名单都已存在，不能将文字承诺算为回图成功。[本次失败报告](validation/qq-image-roundtrip-615c758-aarch64.json)保留失败；`normalized=true` 也可能只是宿主节流修正，不能仅据此推断原模型输出违反 JSON 语法。后续补救必须验证本条用户明确请求和当前图片来源，并走原持久 Outbox，不回填或重发这个已完成 Run。

修复使用 [明确回传本条图片的宿主规则](structured-replies.md#明确回传本条图片)：闭合肯定短句、当前 Run 的图片来源、owner / 会话 / 水位及实际字节摘要独立核验。2026-10-25 23:30 在 QQ 发送新的同图消息，文字为普通自然句“请用一句话描述附图，再把这张原图发给我。”，没有验收 marker。新 Run `9e3c22e8-ce41-4ac3-826b-ea76988e1866` 在 `c75b868…` 上形成 text＋image，两项均 sent、有平台消息 ID、error 为空，实际间隔 3,350 ms ≥ 1,500 ms。图像为隐私规范化后的 224×128 PNG、1,518 字节，SHA-256 `a2ad032ccebd2f6203f257a000a532e15f9256b37885071a3ddea96ad671cb68`，不把原附件 712 字节的摘要混作同一制品。

[只读平台证据](validation/qq-image-roundtrip-c75b868-aarch64.json)的 31 项来源、归属、摘要、计划及无文字降级检查全真，退出 0；脚本来自已推送 `8b2642511fd252b8a2ac1bb10b614d76809910c6`，运行程序仍为 `c75b868…`，两种 SHA 分开记录。机器报告维持 `client_verified=false`；随后 [独立 QQ 桌面观察](validation/qq-image-roundtrip-client-c75b868.json)确认左侧助手消息确实显示蓝色圆形与橙黄色方块及图片 caption。旧失败不改写为成功；这不代表任意自然语言意图解析或新图片生成 provider 已可用。

## QQ 非群 API 的证据边界

当前线上官方文档 v1.42.0 的 77 个非群 HTTP 操作已接入固定目录，方法 / 路径 / 参数 / 查询 / 正文和实际 HTTP mock 序列化验证覆盖全部 77 项；模型无管理 API 权限。平台敏感响应使用服务端短期引用，前端不接收 file_info、预签名 URL、上传凭据或 token。管理操作保持认证、Origin / CSRF、意图确认及账号 / 凭据 / TTL 绑定。

首次真实只读探针在 `2c709999…` 上对全部 77 项分类：6 项平台接受、1 项平台拒绝、19 项缺少测试资源、51 项写入未执行。唯一拒绝为面板列表缺少必填 scope（业务码 40030011），不表示账号权限拒绝。修复明确默认 `scope=c2c`、limit ≤ 50，并按依赖先发现实际 guild、typed channel、role 等资源再测试；后续报告须单列，不能把首次 19 个 `requires_resource` 推断为“账号没有频道”。

不得为了凑齐成功率修改用户已有菜单、移除成员、禁言真实频道或撤回别人消息。支持调用、离线协议通过、真实账号权限通过与桌面实际收到均是不同结论；最终缺少资源 / 权限 / 平台能力的清单按真实探针原样保留。

后续真实只读探针、本人面板与合成素材 CRUD、输入状态、Markdown 发送及撤回、流式首尾片累计使 **19/77 项** 获平台接受；剩余 18 项缺资源、40 项写操作未执行。机器人当前频道列表首页实际为空，不能据此伪造频道、成员或音频等实测。流式 UUID 撤回为 HTTP 400 / 业务码 40061001，未重试；这次失败保留，不推断永久不支持。原始报告与每项来源 / 运行 SHA 见 [QQ 累计证据](qq-non-group-api.md#验证命令与结论边界)，机器报告的 `client_verified=false` 不改写为后续人工观察。

`c75b868…` 上另用 6,302,964 字节的合成填充 MP4 验证大文件路径，[首次分片测试失败](validation/qq-multipart-c75b868-aarch64.json)：Rust 文件校验通过，但实际发送降级成文字，无法定位内部失败阶段，预上传和分片完成没有独立 HTTP 证据。该次 `sent` 不能算视频成功；未改变上面的 19 项直接回执统计，原测试源不重试。小 MP4 成功不扩展为大文件已可用。

## 正式插件工厂

本轮实现为 Rust 模板 → `dsh:plugin@1.0.0` WIT Component → Wasmtime 36.0.19，使用真正的 Component `health / invoke`；原纯整数 WAT / Wasmi 仍仅为折叠开发诊断。生成、构建、测试、摘要审批、激活、回滚 / 隔离分别操作，模型不能批准自身代码。当前只开放空权限、无状态 JSON 工具，完整 HTTP / KV / 记忆 / 发送 broker 和模型自主工具循环尚未交付，保留为原架构目标。

Linux 真实 Rust 编译、WIT Component 执行、摘要审批 / 激活 / 重启 / 回滚 / 隔离下架及恶意输入拒绝在正式 release 验收中完成 18 项；另有单次真实模型 7 项全通过，包含显式 Web `/plugin` 未见输入得到 77。精确 SHA、二进制摘要、32 个新沙箱和审批边界见 [工厂实机验收](plugin-factory-validation.md)。本地单测和“systemd 能启动”不替代 Landlock / seccomp / cgroup 及精确 unit 退出实测。契约与命令见 [插件工厂](plugin-factory.md)。

后续 `c75b868…` 正式版再次 [19 项全部通过](validation/plugin-factory-c75b868-aarch64.json)，新增受控模型夹具下的普通聊天显式 `/plugin` 调用回归。单次真实生成预算仍只使用此前的 1 次，没有再请求模型生成或批准生产版本。

## 本地检查与限制

工厂与媒体协议阶段 Rust workspace 246 个 daemon 单测及 4 个 kernel 生命周期测试通过；`615c758…` 在 Linux 执行 252 个 daemon、4 个 kernel 测试全通过，其中插件专项 7 项。回图修复阶段本地 Windows 261 个 daemon、4 个 kernel；正式 `c75b868…` 在 Linux 262 个 daemon、4 个 kernel 全通过（额外包含 Linux clone 策略测试）。附件回传专项 10 项覆盖原始模型呈现无效、max_items=1、拒绝未知 / 否定 / 引用指令、当前图源、跨主体、过期、遗忘、取消及已完成 / unknown 不重发。前端媒体附件区阶段全量为 18 文件 / 136 项；工厂专项 3 项、typecheck 与最终同 SHA 生产构建另行通过。Python 部署兼容性、QQ 探针边界、合成夹具及只读回图检查在 `c81c628…` 后共 71 项通过，预上传单次诊断工具新增后共 76 项通过；这些本地 / 离线检查不替代真实平台验收。

本轮不将 ASR、微信媒体、完整 Harness 权限桥、通用联网插件、视频帧 / 音轨理解、未知图片生成 provider 或全部 QQ 平台权限写成可用。代码回滚必须经过 reader 兼容性检查，数据恢复单独处理；失败版本尚未切换时无需恢复数据库。没有执行过的生产回滚演练不会写为通过。
