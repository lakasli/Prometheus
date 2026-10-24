# 角色卡 M1.1 实施与验证

日期：2026-10-24，Asia/Shanghai。目标为Pi5 / 8GB / Raspberry Pi OS Lite 64位，用户授权本轮继续在现有Ubuntu 24.04 aarch64主机验证。未重装系统，不将Ubuntu结果记成Lite验收。

## 实现范围

- Rust角色卡模块：CCv3文本子集、DSH扩展、草稿ETag、不可变发布revision、指定版本导出、默认卡及会话固定绑定。
- 有界JSON / AIRI ZIP导入：owner与输入摘要绑定、转换器版本检查、差异与丢弃路径预览、15分钟过期清理；不加载显示模型或抓取外部资源。
- Web卡库、人格编辑、模块偏好、预览、独立测试、发布 / 默认分离、显式QQ共享、导入确认和JSON下载。
- Web / CLI / QQ统一冻结静态人格与当时模型配置；完整历史在前序任务完成后、联网前再持久化。角色、投影与最终请求摘要分开。
- QQ仅私聊支持 `/角色`、`/角色 当前`、`/角色 切换 <名称或ID>`；只选择管理员开放卡，切换新建会话，不更改实例默认。命令也走去重和Outbox，不调用模型或混入模型历史。
- 升级前SQLite一致性备份；部署 / 回滚拒绝角色数据与目标release不兼容的降级。

代码入口为 `crates/dshd/src/character.rs`、`character_import.rs`、`character_api.rs`，在现有入队、QQ事务和worker中接入；Web组件为 `web/admin/src/components/CharacterCards.vue`。均为DSH独立实现，无AIRI运行依赖或复制源码。

## 当前验证进度

| 验证 | 状态 |
| --- | --- |
| Rust后端 | 本地与树莓派aarch64均30项通过，含角色生命周期、导入、QQ私聊切换去重 / 用户隔离 / 显式共享 / 群聊拒绝及失效模型映射下命令恢复 |
| Cordis | 本地与树莓派原生4项生命周期测试通过 |
| 格式 / 静态质量 | fmt、严格Clippy、普通构建通过 |
| M1原有黑盒 | 本地与树莓派19/19通过，真实进程 + 隔离模拟provider；[机器记录](validation/m1-character-aarch64.json) |
| 部署兼容性门禁 | 本地与树莓派4个隔离SQLite用例通过，验证legacy、角色工作与未来schema的降级边界 |
| 角色专用黑盒 | 本地与树莓派55/55通过；真实Rust进程、隔离模拟provider和临时SQLite，明确非真实QQ / 外部模型联调；[机器记录](validation/character-aarch64.json) |
| Web浏览器与单测 | 22项单测、类型检查与构建通过；角色卡和原有管理两个完整Chromium场景通过，含390 / 320像素布局 |
| 树莓派部署 | 154ebf8完整SHA部署成功，health与前端manifest一致，systemd active / enabled，Linger=yes |
| 生产Web / 真实模型 | 已通过登录会话创建、保存、校验、发布“蓝鲸助手”v1并显式开启QQ可选；未改变全局默认。独立草稿会话真实模型回复采用蓝鲸身份，完整请求快照存在；[脱敏记录](validation/character-live-aarch64.json) |
| 真实QQ角色切换 | 网关101 / HELLO / READY已通过；用户实际私聊列表 / 切换 / 回复仍待新消息验证，未把离线fixture记成真实联调 |

本地黑盒脚本为 `tools/character-smoke.py`；模拟provider与合成账号独立于正式数据，不冒称真实模型或QQ已通过。测试中首次ZIP反斜线fixture在Windows被Python自动规范化，已修正fixture，不能把该次结果记成后端安全漏洞。部署门禁测试初次遇到SQLite句柄未关闭导致临时目录清理失败，已显式关闭并复验通过。

独立黑盒实际发现并修复了两项缺陷：ZIP库构建索引时会折叠同名条目，现已在其前预检原始中央目录与本地头；归档后的历史版本建草稿入口现已统一检查归档状态。最终55项回归包含这些真实边界。卡与完整请求快照分别保留规范内容、投影及请求摘要；mock provider收到请求时即核验完整快照已存在。

隔离浏览器截图位于 `.local/character-e2e/screenshots/`，桌面与320像素布局已人工查看；测试服务均已停止，不占用生产18419隧道。当前列表首期限100张卡，尚无翻页；此限制不等于已完成大规模卡库或资源压测。

## 固定版本部署与数据保留

- 运行源码：`154ebf8c5e8b9901b39f4de8fb0a116129c6888a`，部署前已推送origin/main。随后仅更新文档和脱敏证据的提交不需要重启。
- 前端制品：`admin-154ebf8c5e8b9901b39f4de8fb0a116129c6888a.tar.gz`，SHA256为 `bd792300a8af04f55262d14d821a7d5117f8355e66487ca7e0062a78b1b5519a`。
- 2026-10-24 17:23:40 +08:00，从 `4790b5de771e3df3c02e843c338364313df3e557` 正常升级。原管理账户、模型 / QQ凭证及原有会话保留；运行数据仍在 `shared/data`，不混入release。
- 先原生测试和release构建，再停止服务、一致性备份、切换并验健康；整个部署退出码0。备份为owner-only `shared/backups/pre-154ebf8c5e8b9901b39f4de8fb0a116129c6888a-20261024T092340337031644.sqlite`，文件名时间为UTC。
- 日志位于 `shared/logs/test-<完整SHA>.log`、`build-<完整SHA>.log`、`deploy-character-154ebf8.log`、`character-smoke-154ebf8.log` 和 `m1-smoke-character-154ebf8.log`。黑盒均使用私有临时数据，不改正式配置。
- 未执行生产故障注入回滚或数据库恢复演练；回滚能力门禁只做了上述隔离用例，不能等同于灾难恢复实测。

实际执行：

```sh
DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-character.sh \
  154ebf8c5e8b9901b39f4de8fb0a116129c6888a \
  /home/campanus/.cache/dsh/admin-154ebf8c5e8b9901b39f4de8fb0a116129c6888a.tar.gz \
  bd792300a8af04f55262d14d821a7d5117f8355e66487ca7e0062a78b1b5519a
python3 /home/campanus/apps/dsh/current/source/tools/character-smoke.py \
  --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --report /home/campanus/apps/dsh/shared/logs/character-smoke-154ebf8.json
python3 /home/campanus/apps/dsh/current/source/tools/smoke.py \
  --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --report /home/campanus/apps/dsh/shared/logs/m1-smoke-character-154ebf8.json
python3 /home/campanus/apps/dsh/current/source/tools/tests/test_deploy_compatibility.py
```

本机维持SSH隧道后访问 [角色卡](http://127.0.0.1:18419/admin/#characters)。真实QQ待用户私聊发送 `/角色`、`/角色 切换 蓝鲸助手`，再发送普通消息并核验切换确认、冻结revision及平台回执。不能仅凭Web模型成功或网关READY标记这一项通过。

## 边界与源码清理

当前只支持文本；Vision / Speech必须disabled，完整Harness、世界书检索、Live2D / VRM、PNG / CHARX输入和语音不属于本轮交付。post_history_instructions明确仅保存，不执行。生产QQ群聊 / 频道仍使用原有对话能力，但本轮不开放其人格切换。

AIRI研究源码删除曾被自动审批策略拒绝，工具返回blocked by policy；准确目录与手动清理步骤见 [研究记录](research/airi-character-card-review.md)。该检出位于忽略目录，未进入Git或构建依赖；不能报告已经删除。
