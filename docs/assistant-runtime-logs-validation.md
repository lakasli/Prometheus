# 单一助手与运行日志验收

日期：2026-10-26（Asia/Shanghai）。本轮按用户要求将性格设计并入“助手与模型”，停用多角色切换，把对话工作台改为只读日志记录，删除任务与诊断页。旧验收报告不改写。

## 改动范围

- 全实例一个当前性格，六个文本字段、版本冲突检测、最近 20 个旧版本恢复；恢复保存成新版本。新入队 Run 使用当前性格，原排队 / 已完成快照保留。首次沿用原默认卡或助手配置，旧长提示词不截断。
- Web、CLI、QQ、微信、图片与触发任务采用相同冻结路径；旧角色写入 / 选择返回 410，CLI 选择参数移除。
- 日志覆盖输入 / 输出、实际模型请求上下文、Run 状态、工具 / 插件工作、图片、Piper、投递、记忆作业及管理事件。支持筛选 / 逆序游标分页与详情展开，30 天 / 20,000 条事件限额。
- 正文引用原始数据而非复制；记忆水位 / 快照清理后不从日志恢复正文。认证与 Web owner 隔离，实例管理员可读渠道日志，响应 no-store，已配置凭证脱敏。
- 工具与 TTS 记录操作 ID、状态、长度 / 摘要及错误码，不保存原始语音正文或工具正文副本。ASR / subagent 尚未实现，页面显示未接入，不生成虚构成功事件。
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

正式发布证据待固定 SHA 部署及远端测试完成后补入，不能把本节准备状态当成部署通过。真实配置 / 数据保持独立，部署脚本在切换前备份 SQLite。已保存单性格后回退旧程序须经过 `single_assistant_personality` 兼容检查；数据恢复与代码回退分开，不通过删除用户性格来绕过检查。
