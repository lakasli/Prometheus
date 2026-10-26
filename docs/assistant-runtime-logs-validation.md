# 单一助手与运行日志验收

日期：2026-10-26（Asia/Shanghai）。本轮按用户要求将性格设计并入“助手与模型”，停用多角色切换，把对话工作台改为只读日志记录，删除任务与诊断页。旧验收报告不改写。

## 改动范围

- 全实例一个当前性格，六个文本字段、版本冲突检测、最近 20 个旧版本恢复；恢复保存成新版本。新入队 Run 使用当前性格，原排队 / 已完成快照保留。首次沿用原默认卡或助手配置，旧长提示词不截断。
- Web、CLI、QQ、微信、图片与触发任务采用相同冻结路径；旧角色写入 / 选择返回 410，CLI 选择参数移除。
- 日志覆盖输入 / 输出、实际模型请求上下文、Run 状态、工具 / 插件工作、图片、Piper、投递、记忆作业及管理事件。支持筛选 / 逆序游标分页与详情展开，30 天 / 20,000 条事件限额。
- 正文引用原始数据而非复制；记忆水位 / 快照清理后不从日志恢复正文。认证与 Web owner 隔离，实例管理员可读渠道日志，响应 no-store，已配置凭证脱敏。
- 工具与 TTS 记录操作 ID、状态、长度 / 摘要及错误码，不保存原始语音正文或工具正文副本。ASR / subagent 尚未实现，页面显示未接入，不生成虚构成功事件。
- 首次升级从现存 Run、消息、模型快照与语音制品统一按时间导入历史观察；不补造以前没有记录的工具起止、语音失败或其他事件。新事件由同事务触发器和实际调用路径持续记录。
- 旧 WAT 与独立图片诊断不再提供管理页入口，后台原权限边界保留；正式插件工厂、任务触发与记忆管理继续保留。

## 本地检查

本地检查、模拟 provider 与浏览器测试均使用隔离临时数据，不读取生产凭证，不向 QQ / 微信发送验收消息。

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 退出 0 |
| `cargo test --workspace --locked` | 273 daemon + 4 kernel，全部通过 |
| `cargo build -p dshd --locked` | 退出 0 |
| `npm.cmd test --prefix web/admin` | 19 文件 / 134 项通过 |
| `npm.cmd run build --prefix web/admin` | 类型检查与生产构建通过 |
| `python -S -m unittest discover -s tools/tests` | 77 项通过 |
| `tools/assistant-logs-smoke.py` | 19 项隔离实际进程 / 模拟 HTTP 检查通过 |
| `tools/task-triggers-smoke.py` | 27 项通过，无真实外部模型请求 |
| `tools/speech-smoke.py`（未给 runtime-root） | 6 项通过；Windows 未执行实际 Piper 合成 |
| `tests/e2e/assistant-runtime.spec.ts` | 1 个完整浏览器场景通过，实际 Rust 隔离进程与回环 provider |

浏览器覆盖单性格六字段、跨窗口 409 保留、历史恢复产生新版本、匿名鉴权、合成请求完成后的日志筛选 / 展开，以及 320 / 390px 无横向溢出。控制台无 pageerror，localStorage / sessionStorage 为空。六张截图在忽略目录 `.local/assistant-logs-e2e/v2/screenshots/`；已检查桌面与窄屏图，正文及导航未裁切。该轮本地二进制 commit 标识为 development，不作为正式发布 SHA。

新增 `tools/assistant-logs-smoke.py` 覆盖鉴权 / CSRF、性格校验 / 冲突 / 恢复、真实 HTTP 出网请求的性格投影、旧会话更新、历史快照保留、日志过滤 / 分页 / 凭证隐藏、Web 跨 owner 隔离与重启持久化。模拟 HTTP 不作为真实模型验收。

旧 UI E2E 移入 `web/admin/tests/historical/`，保留历史流程并从默认现役用例中移出；本轮不把这些旧页面脚本计作通过。当前页面用 `tests/e2e/assistant-runtime.spec.ts` 验证。

## 部署与远端验证

目标已核验：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1、8GB、Ubuntu 24.04.4 LTS / aarch64，Rust / Cargo 1.88.0。部署前目录归属 campanus，repo origin 匹配且工作区干净；此前可用版本 `ad9c13082f511a309db0124ddfffefad540b8ecc`。

正式运行 SHA 为 `6b9b5dae0eacdf8e448ea4403ba0c8c48eda6337`，2026-10-26 09:43（+08:00）完成切换；09:46 复核 systemd active、health ready，源码 HEAD、前端 release.json 与健康接口 SHA 完全一致。部署命令退出 0，发布构建耗时 8 分 16 秒；[部署证据](validation/assistant-logs-deployment-6b9b5da-aarch64.json)记录版本、摘要、备份与数据计数。

```sh
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy.sh \
  6b9b5dae0eacdf8e448ea4403ba0c8c48eda6337 \
  /home/campanus/.cache/dsh/admin-6b9b5dae0eacdf8e448ea4403ba0c8c48eda6337.tar.gz \
  efb788ce0ada4790f295002f5144c6cd617606e6e94f8b83461b09bc2ba87ff4
```

正式二进制 SHA-256 为 `927dea8423b14affa374839000dd9ad4b8547809c567860b293eaad274a4d45b`。测试 / 构建日志分别为 `shared/logs/test-<完整SHA>.log`、`shared/logs/build-<完整SHA>.log`，Linux 全量 **274 daemon + 4 kernel** 全部通过。私有备份为 `shared/backups/pre-<完整SHA>-20261026T014335396141568.sqlite`。

以下命令都使用当前发布二进制与 web 目录，并输出独立 JSON 到 `shared/logs/`；全部退出 0。每次启动独立临时数据，不在生产数据库批准插件或修改性格。

| 远端脚本 / 主要参数 | 结果 |
| --- | --- |
| `python3 current/source/tools/assistant-logs-smoke.py --binary current/bin/dshd --web-dir current/web --report shared/logs/assistant-logs-6b9b5da.json` | [19 项通过](validation/assistant-logs-6b9b5da-aarch64.json) |
| 同样参数运行 `task-triggers-smoke.py`，report 为 `assistant-logs-triggers-6b9b5da.json` | [27 项通过](validation/assistant-logs-triggers-6b9b5da-aarch64.json) |
| 同样参数运行 `replies-smoke.py`，report 为 `assistant-logs-replies-6b9b5da.json` | [13 项通过](validation/assistant-logs-replies-6b9b5da-aarch64.json) |
| `speech-smoke.py` 另加 `--runtime-root /home/campanus/apps/dsh/shared/speech/piper-1.8.0`，report 为 `assistant-logs-piper-6b9b5da.json` | [18 项通过](validation/assistant-logs-piper-6b9b5da-aarch64.json)，真实本地 Piper 与成功 / 失败日志 |
| 同样参数运行 `plugin-factory-smoke.py`，report 为 `assistant-logs-plugins-6b9b5da.json` | [20 项通过](validation/assistant-logs-plugins-6b9b5da-aarch64.json)，32 个独立隔离 worker、真实编译 / Component 执行与关联 Run 的工具起止日志 |

本次 Piper 合成耗时 4,665 ms，生成 3,413 ms / 150,572 字节 / 22,050 Hz 单声道 WAV；没有把超时进程夹具当成真实语音性能。插件自动批准仅发生在临时合成管理员，生产插件未新增批准；模型返回受控模拟内容，真实 Rust 编译和 Wasmtime 执行与模型模拟分开。

实际认证 Edge 刷新后，页脚显示 `Web 6b9b5dae0e`；“角色卡”“对话工作台”“任务与诊断”均不在导航。“助手与模型”显示单一性格，沿用原 DSH 配置 v0；未改写生产性格。“日志记录”成功载入 50 条现有历史观察并可继续分页，输入 / 输出及上下文来源可见。正式同源未认证日志 API 返回 401。详细保存 / 恢复与窄屏交互来自上述隔离浏览器场景，未在生产尝试保存或恢复。

生产配置仍为 v5，部署前后均为 4 个会话、22 个 Run、43 条消息；数据计数一致，原历史保持。真实配置 / 数据保持独立，未执行生产回滚演练。已保存单性格后回退旧程序须经过 `single_assistant_personality` 兼容检查；数据恢复与代码回退分开，不通过删除用户性格来绕过检查。

本轮没有新增真实外部模型请求、QQ / 微信测试消息、ASR 或 subagent 能力；不将受控 HTTP 模拟、历史真实平台记录或页面重组当作本轮真实平台联调通过。
