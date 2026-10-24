# 记忆候选审核验收

2026-10-24，Asia/Shanghai。已于19:43固定SHA部署并完成真实模型候选 / 审核 / 来源隔离 / 遗忘验收。使用说明见 [候选审核](memory-candidates.md)。

已执行：后端125项+4项Cordis、严格Clippy及格式检查通过；前端62项、类型检查和构建通过；Python兼容 / 视觉fixture13项通过。`tools/memory-candidate-smoke.py`在最终本地开发binary的独立临时真实daemon / 模拟provider中40项通过，未读取正式数据、调用真实模型或发渠道消息。微信管理26、记忆38、摘要24项进程回归通过。

真实Chromium在18430隔离实例1项完整流程通过（6.9s），包含真实登录、来源空GET、未认证401与CSRF403；提取与审核状态明确使用浏览器fixture，无真实模型请求。检查具体接受确认、409锁定刷新、取消 / 到期 / 来源切换清正文、无持久storage / 外部请求 / 浏览器错误；1440 / 390 / 320宽度截图已人工检查，位于忽略目录`.local/memory-candidates-e2e/screenshots/`。

## 部署与远端结果

- 运行SHA：`0a98c63221d13733299b75858a33f4c298328f3c`；分支与main先推送核对，按该SHA提取部署脚本，核对前端归档再在Pi本机构建。
- 主机：`campanus@192.168.189.191`，Pi5 Model B Rev1.1 / aarch64 / 7933MiB / Ubuntu24.04.4，Rust/Cargo1.88；没有重装或作最终Lite验收。
- 部署时间：`2026-10-24T11:43:57.723534+00:00`（19:43:57，+08:00）。前端manifest、Git源码与health完整SHA一致，systemd active/running、NRestarts=0。
- 二进制SHA256：`fdf75ca9c6611c03d784e276d52d4718c15da862a29b6e1f7be7fd1dd1b18559`；前端归档SHA256：`7274153b3a58c8bd6f358a54026ca7bd79f5c947d79a142a800faf738efddf49`。
- 前一运行SHA为`269cf609570a61212ea40d3da89c7408267b07a5`。一致性备份位于私有`shared/backups/pre-0a98c63221d13733299b75858a33f4c298328f3c-20261024T114356367502606.sqlite`；升级成功，未执行回滚或数据恢复。

部署`DSH_CARGO_MIRROR=rsproxy bash <固定SHA脚本> <SHA> <归档> <上述hash>`退出0；`cargo test --workspace --locked -j 2`原生125+4通过，`cargo build --release --locked -p dshd -j 2`退出0、2m00s。日志为`shared/logs/{deploy,test,build}-<SHA>.log`。

在`current/source`先核对Git HEAD，所有脚本使用`--binary current/bin/dshd --web-dir current/web --report shared/logs/<kind>-<SHA>.json`绝对路径，在私有临时实例执行；全部退出0：

| 脚本 | 实测证据 |
| --- | --- |
| `python3 -m unittest discover -s tools/tests -v` | 13项：12兼容门禁与1像素fixture |
| `python3 tools/memory-candidate-smoke.py` | [40项真实进程 / 模拟provider](validation/memory-candidate-aarch64.json) |
| `python3 tools/weixin-smoke.py` | [26项微信管理回归](validation/weixin-candidate-aarch64.json)，未重复官方扫码 |
| `python3 tools/summary-smoke.py` | [24项摘要回归](validation/summary-candidate-aarch64.json) |
| `python3 tools/memory-smoke.py` | [38项记忆回归](validation/memory-candidate-regression-aarch64.json) |
| `python3 tools/smoke.py` | [19项M1](validation/m1-candidate-aarch64.json) |
| `python3 tools/character-smoke.py` | [55项角色](validation/character-candidate-aarch64.json) |
| `python3 tools/memory-candidate-live.py`另加`--source-data-dir shared/data` | [9项真实DeepSeek](validation/memory-candidate-live-aarch64.json) |

真实模型脚本只读现有模型配置 / 该密钥，使用合成来源对话和临时实例：实际4次普通模型请求、1次候选请求、0摘要请求，总预算8。模型产生非空、长度有界且仅引用所选来源的候选；明确接受后生成一条来源private记忆；真实后续模型的持久prompt包含它，其他owner的prompt及memory_run_uses不含它；遗忘后新的真实模型上下文不含该事实或旧回忆echo。没有读取正式聊天 / 记忆、修改正式数据库或接入真实QQ / 微信。

正式Edge页面只读刷新核对`Web 0a98c63221`、候选审核入口、原有稳定助手标识、空记忆库及草稿会话未出现在候选来源；未在正式实例提取或接受。安全截图为`.local/production-evidence/candidates-0a98c63.png`。本轮没有覆盖真实渠道候选、自动历史扫描、M6压力、故障回滚或最终Lite。实际使用候选语义后reader5不可降级；原始消息 / WAL / 备份的独立保留边界继续生效。
