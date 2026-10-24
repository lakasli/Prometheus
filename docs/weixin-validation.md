# 微信 iLink 实施与验收记录

日期：2026-10-24，时区 Asia/Shanghai。状态：微信切片已于19:19按固定SHA部署，远端原生测试、进程回归、正式页面读取和官方取码 / 取消通过；实际手机扫码和真实私聊闭环未执行。

使用说明、限制与数据语义见 [微信私聊接入](weixin.md)。协议参考固定为腾讯 `openclaw-weixin` 2.4.10、commit `874637a0dbc24050f72d1385add4fc6170a2d726`；本项目采用独立 Rust 实现。

## 已执行的本地验证

工作目录为独立工作树 `C:\Users\AIIT\.codex\worktrees\dsh-memory\DSH`，分支为 `codex/long-run-memory`。测试使用临时 SQLite、合成账号与数值 loopback HTTP 服务，没有真实微信凭据、生产数据、手机扫码或真实模型调用。

| 检查 | 命令与结果 |
| --- | --- |
| 微信相关 Rust 测试 | `cargo test -p dshd weixin_ --locked`，退出 `0`，45 项通过、0 失败；其中含通用凭据删除与微信停用账号引用的交叉测试 |
| 严格静态检查 | `cargo clippy -p dshd --lib --tests --locked -- -D warnings`，退出 `0` |
| 根任务统一后端检查 | 主服务 111 项、Cordis 4 项通过；严格 Clippy 与全工作区格式检查通过 |
| 根任务统一前端检查 | 50 项测试通过，类型检查与生产构建通过 |
| Chromium真实浏览器 | `DSH_E2E_BASE_URL=http://127.0.0.1:18429 npx playwright test weixin.spec.ts`，1项流程通过；真实后端登录 / 401 / CSRF后，扫码状态用明确标注的合成fixture，不发官方登录请求，结束核对后端仍无账号 / 任务 |
| 真实Rust进程回归 | `tools/memory-smoke.py` 38、`summary-smoke.py` 24、`smoke.py` 19、`character-smoke.py` 55 项通过；均为隔离临时实例 / 模拟provider，不是微信真实平台验收 |
| 微信管理进程黑盒 | `python -S tools/weixin-smoke.py --binary .local/qa-bin/weixin-smoke-dshd.exe --web-dir web/admin/dist --report .local/weixin-smoke-local.json`，26项通过；默认不申请官方QR、不启用账号、不发消息 |
| 文档静态检查 | 两份文档可按 UTF-8 读取，`git diff --check` 通过 |

上述结果覆盖：

- 协议：明确的 QR 状态、缺字段 / 非零业务返回码、恶意来源、私网 DNS、自动跳转禁止、正文限额与在途取消；`u64` 消息 ID 无损处理。
- 扫码：单一五分钟任务、验证码一次使用且先清除数据库、缺数字不轮询；取消 / 过期 / 配置修订变化后的迟到确认不能保存或启用账号；重启与停 worker 清 QR / code；终态元数据最多保留 32 条。
- 凭据：已知失效 token 即使文件存在也不能成为 `existing_credentials`；确认只保存为 `connecting`，不会伪造已连接；新凭据元数据、账号替换与扫码结果有事务和版本门禁。
- 入站：本人 / 收件机器人过滤、群与媒体隔离、Inbox / Run / 回复上下文 / 游标一起提交；队列满或后续入队失败时整批回滚，不提前确认游标。
- 真实本地 HTTP 管道：模拟 iLink 接收 → Rust 入队 → 隔离模型或无模型记忆命令 → 固定上下文发送；事件重放只产生一份有效工作与回复。
- 投递：发送结果未知不盲重发、重启中断发送记未知、旧 generation 不可覆盖新配置、过期上下文不可借用后续消息 token；显式平台接受不当成手机可见。
- 管理 API：认证、CSRF、任务 owner、首次 `If-Match: "0"`、版本冲突、核验数字不回显、账号 DTO 不暴露 token / 凭据引用 / 绑定用户 / 游标；单一连接检查绑定账号 generation，由原接收器返回结果。

## 审阅发现与本地修复

1. 原来成功收取只写 `polling`，但页面以 `connected` 识别已连接。现统一为有效 `ret=0` 批次提交后写 `connected`，并增加真实本地 HTTP → 认证管理 API 的连接状态断言；单纯开始轮询或超时不冒充新的成功结果。
2. NUL 文本原来可能进入 `Text`，随后被存储校验拒绝而使整个批次游标永久不前进。现于协议层隔离为格式错误；新增混合批次真实本地 HTTP 回归，证明同批后续好消息正常入队且游标提交。

浏览器 / UI 审阅指出的 `verification_blocked` 终态、`needs_login` 展示优先级和投递计数投影已由根任务修正并增加两项回归，包含在前端 50 项测试中。浏览器验证本地QR编码、遮掩并清除核验数字、超时不自动换码、取消 / 启停确认、检查ID关联，以及1440 / 390 / 320宽度；无外部图像请求、无浏览器错误。截图位于忽略目录`.local/weixin-e2e/screenshots/weixin-fixture-{1440,390,320}.png`，仅含合成二维码。

## 发布验证

| 项目 | 当前结果 |
| --- | --- |
| 全工作区测试、格式 | 本地通过；Pi原生111后端+4Cordis通过，退出0 |
| 前端组件、生产构建 | 50项通过；同SHA静态制品与manifest核对一致 |
| Rust 发布构建 | Pi `cargo build --release --locked -p dshd -j 2`，退出0，1m54s |
| 真实浏览器登录 / 扫码状态 / 移动布局 | 本地Chromium通过，扫码状态为明确fixture；真实扫码未执行 |
| 现有 M1、角色、记忆、摘要进程脚本回归 | 本地与Pi均19 / 55 / 38 / 24通过；微信管理26、部署兼容10通过 |
| 微信固定完整 SHA 提交推送与部署 | `269cf609570a61212ea40d3da89c7408267b07a5`，分支与main均推送并核对 |
| 远端 Ubuntu / aarch64 构建与服务健康 | ready完整SHA一致，用户级systemd active/running、NRestarts=0；正式浏览器显示同版本及无账号 / 全零投递 |
| 官方服务受控 QR 获取 / 取消 | 显式`--official-qr-probe`真实HTTPS获取，立即取消；31项通过，无绑定、无验证码提交、无消息发送 |
| 真实手机扫码与凭据保存 | 未执行；需要实际本人账号授权 |
| 真实本人私聊接收 / 模型 / 手机可见回复 | 未执行，不能称真实微信闭环已通过 |
| 群聊、媒体、主动发送 | 不在本切片能力范围，不列为已实现 |
| Raspberry Pi OS Lite 最终验收 | 未执行；现有 Ubuntu 例外不替代最终基线 |

## 固定版本证据栏

```text
微信提交完整 SHA：269cf609570a61212ea40d3da89c7408267b07a5
目标主机：campanus@192.168.189.191
部署时间：2026-10-24T11:19:18.119438+00:00（19:19:18，+08:00）
实际系统：Pi5 Model B Rev1.1 / aarch64 / 7933MiB / Ubuntu24.04.4；Rust/Cargo1.88
桌面状态：graphical.target，display-manager inactive；未重装或作Lite验收
前一运行 SHA：f0203969a9ad6bb6a8d34d42d44dff66b42b44bc；原配置继续使用
SQLite备份：shared/backups/pre-269cf609570a61212ea40d3da89c7408267b07a5-20261024T111916668829259.sqlite（campanus/0600）
构建 / 部署 / 测试退出码：全部0
二进制SHA256：2ddc1b7187f8d1958f95307f8389bc4409598afd8ac994311d08039d27f1e8ae
前端归档SHA256：3c43aa6953c694bb2fa27c76ded4805691812b9ff54bb52caa96701c829bf924
前端manifest / health：完整SHA均与上述运行SHA一致
官方QR：获取、鉴权隔离、状态脱敏、立即取消、私有字段清除通过
真实手机授权及私聊闭环：未执行
回滚：升级成功，未执行回滚或数据库恢复；空微信表允许原版本，实际使用后需reader4
```

部署使用该已推送SHA提取的`deploy/deploy.sh`，`DSH_CARGO_MIRROR=rsproxy`、归档与上述固定hash；归档核对、原生测试、构建、兼容检查、停服备份、切换与ready核对按脚本完成。日志位于远端`shared/logs/{deploy,build,test}-<SHA>.log`。启动瞬间第一次curl连接失败后重试ready成功，不记为健康失败。

在`current/source`核对Git HEAD后，使用`current/bin/dshd`、`current/web`依次执行以下脚本，均退出0，全部数据为临时私有实例；没有改正式配置、角色、记忆或连接正式微信：

| 脚本 | 脱敏证据 |
| --- | --- |
| `python3 tools/weixin-smoke.py --binary <bin> --web-dir <web> --report <log>` | [管理26项](validation/weixin-aarch64.json) |
| 同上加`--official-qr-probe` | [真实官方取码31项](validation/weixin-qr-aarch64.json)，不是手机私聊验收 |
| `python3 tools/summary-smoke.py` | [摘要24项](validation/summary-weixin-aarch64.json) |
| `python3 tools/memory-smoke.py` | [记忆38项](validation/memory-weixin-aarch64.json) |
| `python3 tools/smoke.py` | [M1 19项](validation/m1-weixin-aarch64.json) |
| `python3 tools/character-smoke.py` | [角色55项](validation/character-weixin-aarch64.json) |

各脚本均携带同一`--binary`、`--web-dir`及私有`--report shared/logs/<kind>-<SHA>.json`参数；原件与复制证据一致。M1旧脚本报告本身不含制品SHA，来源由本轮明确命令和切换前后健康核对限定。其余报告核对build SHA及binary hash。

正式Edge入口刷新读取后，页面显示`Web 269cf60957`、尚未绑定、所有投递计数0；未点击正式扫码。安全截图为忽略目录`.local/production-evidence/weixin-269cf60.png`，没有QR或凭据。原有稳定assistant identity、管理员和角色数据继续保留。

一旦产生微信账号或持久收发语义，数据库保留 reader 4 要求；停用不降级该要求。只切换旧二进制不能恢复数据库，不能通过删除用户历史或迁移标记伪造可回滚结果。微信登录任务、凭据引用与私有上下文不进入公开报告或配置导出。
