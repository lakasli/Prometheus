# 长会话摘要验收

状态：2026-10-24 18:27—18:29（+08:00）已按固定SHA在获准使用的Ubuntu树莓派部署并通过真实模型验收。前一显式记忆切片的历史结果见 [记忆记录](memory-validation.md)。本轮仍不代表微信、自动事实抽取或完整M2已经交付。

本地范围：66后端+4Cordis、strict Clippy / fmt、38前端 / 类型检查 / 构建、8项部署门禁已通过；真实Rust进程摘要黑盒24项通过，观察到native模型请求最多2个并发、摘要最多1个。真实Chromium验证空摘要、具体重建确认、opaque版本、并发冲突、来源切换、原始消息保留与320 / 390 / 桌面布局；未用浏览器伪造摘要正文或发模型消息。

新增路径：持久有界摘要job、来源覆盖水位 / epoch CAS、同来源私有资料、重启恢复、敏感模式过滤和超大轮次明确摘录。黑盒模拟provider不代表真实DeepSeek；`tools/summary-live.py`将在固定SHA部署后只读现有模型配置，在临时私有实例验证合成长会话、重启和遗忘，不读取真实聊天或发QQ。

## 部署与一致性

- 运行SHA：`f0203969a9ad6bb6a8d34d42d44dff66b42b44bc`，先推送功能分支及main并核对origin/main，再由该SHA提取部署脚本。
- 目标：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1 / 7933 MiB / aarch64 / Ubuntu 24.04.4 LTS；Rust与Cargo 1.88.0。graphical.target且display-manager inactive，未重装、未作最终Lite验收。
- 部署时间：`2026-10-24T10:27:29.577295+00:00`（18:27:29，+08:00）。release根为`/home/campanus/apps/dsh/releases/<SHA>`，源码`source/`、程序`bin/dshd`、静态页`web/`。
- 前端归档SHA256：`425474b5128706b03777b9c355924ce2fcad7364c6cd06ac652011cc6ac08c75`；二进制SHA256：`3622b65b8524e61d05dc07da488b3070112999aa9ce703a45e1cf6c0b2324404`。
- 前后端manifest / health完整SHA一致，systemd用户服务active/running、NRestarts=0。原有管理员、配置、角色和稳定助手身份继续使用；远端现有Edge登录会话刷新后核对新页面，未向正式记忆库或摘要写入测试资料。
- 上一可用SHA为`60ef557388203150d56ab41348649601e735630c`。升级前以SQLite备份接口保存到私有`shared/backups/pre-f0203969a9ad6bb6a8d34d42d44dff66b42b44bc-20261024T102728216443462.sqlite`（campanus / 0600）。升级成功，未执行回滚或数据库恢复。

```sh
DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-f0203969a9ad6bb6a8d34d42d44dff66b42b44bc.sh \
  f0203969a9ad6bb6a8d34d42d44dff66b42b44bc \
  /home/campanus/.cache/dsh/admin-f0203969a9ad6bb6a8d34d42d44dff66b42b44bc.tar.gz \
  425474b5128706b03777b9c355924ce2fcad7364c6cd06ac652011cc6ac08c75
```

部署退出0；远端`cargo test --workspace --locked -j 2`与`cargo build --release --locked -p dshd -j 2`退出0，原生66后端+4Cordis通过。构建 / 测试 / 部署日志为`shared/logs/build-<SHA>.log`、`test-<SHA>.log`、`deploy-<SHA>.log`。

## 远端测试

进入`current/source`核对Git SHA，以下命令均使用同版本`current/bin/dshd`和`current/web`，报告保存于私有`shared/logs`，全部退出0：

| 命令 | 结果 |
| --- | --- |
| `python3 -m unittest discover -s tools/tests -v` | 8项兼容门禁，包含摘要清除后仍禁止旧reader忽略失效语义 |
| `python3 tools/summary-smoke.py --binary <binary> --web-dir <web> --report <private-log>` | 24项真实进程 / 模拟模型；[脱敏记录](validation/summary-aarch64.json) |
| `python3 tools/memory-smoke.py --binary <binary> --web-dir <web> --report <private-log>` | 38项记忆回归；[记录](validation/memory-summary-aarch64.json) |
| `python3 tools/smoke.py --binary <binary> --web-dir <web> --report <private-log>` | 19项原M1回归；[记录](validation/m1-summary-aarch64.json) |
| `python3 tools/character-smoke.py --binary <binary> --web-dir <web> --report <private-log>` | 55项角色卡回归；[记录](validation/character-summary-aarch64.json) |
| `python3 tools/summary-live.py --binary <binary> --web-dir <web> --source-data-dir <shared-data> --report <private-log>` | 9项真实DeepSeek验收；[记录](validation/summary-live-aarch64.json) |

真实模型脚本只读取既有模型字段与该凭证，在临时私有实例测试合成早期代号及短填充对话；观察到18个前台请求、2个摘要job（总请求上限20）。确实生成有界摘要，在超出原24条消息窗口后仍可回忆早期事实；重启后保留；遗忘其来源关联的显式记忆后，摘要失效，后续请求和重启后请求均不再含旧代号 / echo，真实模型返回UNKNOWN。没有读取正式聊天、记忆或角色，没有改生产数据库、连接真实QQ或发送消息。

模拟黑盒验证来源私有、来源去重、覆盖与快照同事务、敏感输入 / 输出阻断、大来源明确摘录、503降级、真实HTTP在途取消、一次尝试 / Run。观察到模型请求并发最多2、摘要最多1，这是本验收的并发断言，不是M6资源压力测试。

原件为`shared/logs/summary-<SHA>.json`、`summary-live-<SHA>.json`及各回归JSON。本地真实浏览器截图在忽略的`.local/summary-e2e/screenshots/summary-{1440,390,320}.png`，远端入口只读截图在`.local/production-evidence/summary-f020396.png`。UI真实重建仅在隔离实例执行，正式实例只读取。

## 当前边界

摘要是来源私有、有损派生资料，不自动变成shared事实。源消息与离线备份保留；重建摘要不等于遗忘原始经历。秘密模式检查只覆盖明确可识别格式，不能承诺识别所有个人敏感信息。首次持久摘要要求reader3，后续清除正文仍保留无正文版本元数据并阻止语义降级；空表 / job初始化可回旧版。尚未进行故障注入回滚、数据恢复、真实QQ长对话、最终Lite与资源压力验收。微信、自动事实抽取、Harness及后续阶段继续实施。
