# 图片对话与导航调整验收

2026-10-24用户明确要求：图片理解接入普通对话；计算原型移入“任务与诊断 → 开发诊断”，取消图片分析和计算工坊两个顶级导航入口；后续按原架构推进，阶段验证页不能当作正式功能。正式部署基线改为`campanus@192.168.189.191`现有树莓派5 / 8GB / Ubuntu24.04.4 LTS / aarch64，取代原Lite最终目标。

本文区分需求落文档与实际运行交付。本轮已完成本地检查、Git提交推送、固定SHA部署、远端和真实模型验收。原有独立PNG、WAT以及此前Ubuntu验证记录保留为当时的范围；本轮仍不代表完整M4 / M5已交付。

本轮计划验证普通会话的PNG上传、VisionProvider描述与Native回答、历史来源标注、后续追问、身份隔离、幂等、取消、重启和遗忘；浏览器验证主导航、默认折叠开发诊断、旧链接迁移和移动布局。真实模型仅使用无用户数据的合成图形及临时会话，既有生产会话和记忆不作为测试资料。

需求文档提交为`1e4db813afb9087c53726624805994a9ccbed48f`，该提交本身未部署或重启。运行交付与后续验收证据分别记录，文档状态更新不再次切换应用版本。

## 本地检查

Windows：`cargo test --workspace --locked`后端171项与生命周期4项通过，格式和严格Clippy通过。前端110项测试、类型检查和构建通过。新真实Rust进程配合loopback模型24项通过，覆盖图片到普通Run、视觉描述到Native、追问basis、owner、元数据剥离、跨类型幂等、来源遗忘、Vision / Native两阶段取消与重启、1MiB边界；摘要24、候选40、记忆38回归通过。Python兼容与夹具19项通过。

浏览器新图片对话、旧图片诊断链接和旧计算诊断链接3项通过；修复小屏角色选择宽度后再次运行新对话用例通过。已检查1440 / 390 / 320像素截图，开发诊断默认关闭、图片预览和历史来源可读。浏览器合法模型响应使用合成数据，真实后端只验证认证、空历史、能力状态和CSRF，不能据此认定真实模型识图通过。截图在忽略的`.local/conversation-image-e2e/screenshots`中。

## 正式Ubuntu部署

- 运行SHA：`bd914f8ea73ffad5697248ddf3df716f7208299a`，已推送main及开发分支，前端manifest与后端健康接口一致。
- 部署时间：2026-10-24T14:01:29.600567Z（22:01:29 +08:00）。正式目标`campanus@192.168.189.191`，Raspberry Pi5 ModelB Rev1.1、7933MiB标称8GB、Ubuntu24.04.4 LTS / aarch64。
- 本轮部署前实际架构 / OS / 内存 / 目录 / Git状态已核实，Rust/Cargo1.88.0在远端原生构建；构建并行2。仍为graphical.target、display-manager inactive，没有因新基线决定而重装或改变启动目标。
- 前端归档SHA256：`60936970484cbcc15f6c15f8c5d6156562c0cb7b599553e411bca28c570c9d8d`。
- 运行二进制SHA256：`07f0e089fc5a10dd6c66cbb0f7a59274f720aaf31b0eb01ef6657e6fb7b3e947`。
- 前一release：`07175c16f590be0689925a61673da282bb37bf21`；切换前SQLite备份：`/home/campanus/apps/dsh/shared/backups/pre-bd914f8ea73ffad5697248ddf3df716f7208299a-20261024T140128171116663.sqlite`。
- `dsh.service`为active/running、NRestarts=0，ready为上述运行SHA。启动轮询首次连接失败后成功ready，整体部署命令退出0。元数据见 [部署记录](validation/conversation-deployment-aarch64.json)。未执行回滚或备份恢复，不将版本切换测试当作数据恢复验收。

中间功能版本`07175c1`也已完成远端171+4测试、部署及 [24项真实进程验收](validation/conversation-images-initial-aarch64.json)。随后发现总览仍有“图片分析单独使用”的旧说明，在本地仅改该文案后提交`bd914f8`，重新打包、推送和固定SHA部署。两个版本间只差这一行前端说明，保留中间记录，不将其冒充最终版本证据。

## 远端执行与结果

从已推送SHA取`deploy/deploy.sh`，使用`DSH_CARGO_MIRROR=rsproxy bash <固定SHA脚本> <完整SHA> <前端归档> <本地核实的SHA256>`部署。实际执行`cargo test --workspace --locked -j 2`（171+4通过）和`cargo build --release --locked -p dshd -j 2`，全部成功后切换服务。日志位于`/home/campanus/apps/dsh/shared/logs/{test,build,deploy}-bd914f8ea73ffad5697248ddf3df716f7208299a.log`。

在`current/source`断言HEAD等于完整运行SHA后，以下命令以当前bin/web为参数依次执行；组合脚本及各项退出0，报告在同一私有logs目录。真实模型只复制现有provider配置与凭据到临时数据库，不读取生产会话、记忆、图片、角色或渠道凭据：

```sh
python3 tools/conversation-images-smoke.py --binary "$bin" --web-dir "$web" --report "$logs/conversation-images-$sha.json"
python3 tools/conversation-images-live.py --binary "$bin" --web-dir "$web" --source-data-dir /home/campanus/apps/dsh/shared/data --report "$logs/conversation-images-live-$sha.json"
python3 -m unittest discover -s tools/tests -v
```

随后运行`image-analysis-smoke.py`、`wasm-workbench-smoke.py`、`memory-candidate-smoke.py`、`weixin-smoke.py`、`summary-smoke.py`、`memory-smoke.py`、`smoke.py`和`character-smoke.py`，均传同一`--binary "$bin" --web-dir "$web"`和独立`--report`。回归结果：

| 项目 | 结果与证据 |
| --- | --- |
| 新会话图片实际进程、模拟provider | [24项通过](validation/conversation-images-aarch64.json) |
| 真实DeepSeek Vision→Native→文字追问 | [8项通过](validation/conversation-images-live-aarch64.json)：合成随机图形的形状、颜色、数量回答匹配实际像素，重复请求不重派；1次视觉请求、2个Native完成Run，无隐式记忆 / 渠道工作 |
| Python兼容与夹具 | 19项通过；reader8、`conversation_images`及失效来源元数据阻止静默语义降级 |
| 旧独立图片诊断 | [34项通过](validation/legacy-image-conversation-aarch64.json) |
| WAT开发诊断 | [20项通过](validation/diagnostic-wasm-conversation-aarch64.json)，模拟模型、实际受限worker；不重复声称真实模型或完整插件工厂验收 |
| 记忆候选 / 微信管理 | [40项](validation/candidate-conversation-aarch64.json) / [26项](validation/weixin-conversation-aarch64.json)通过 |
| 摘要 / 长期记忆 | [24项](validation/summary-conversation-aarch64.json) / [38项](validation/memory-conversation-aarch64.json)通过 |
| M1 / 角色卡 | [19项](validation/m1-conversation-aarch64.json) / [55项](validation/character-conversation-aarch64.json)通过 |

旧M1脚本报告不含运行SHA/二进制字段，归属由固定HEAD、当前制品路径与串行命令记录确认；新版报告保留各自实际字段，不补造旧报告元数据。

生产浏览器只读核对`/admin/#diagnostics`：主导航没有图片分析或计算工坊，开发诊断默认collapsed；显示Ubuntu正式基线、正确Web SHA，服务已连接。安全截图位于忽略的`.local/production-evidence/diagnostics-bd914f8.png`；该检查没有向生产会话提交测试图片或运行计算原型。

## 保留的范围

本轮仅Web单PNG图片对话；文本助手根据有来源的视觉描述回答，未开放角色卡vision偏好、QQ / 微信图片、音频或完整附件服务。原型仍只是开发诊断，正式Rust模板 / WIT / Wasmtime Component插件工厂和完整Harness权限桥未交付。真实QQ角色补验由另一任务独立记录，不将其结果扩大到图片或微信路径。整体资源压测、备份恢复等未在本轮重测。
