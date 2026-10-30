# Prometheus 第二批迁移验收

本轮从首轮Windows版本49c3ed46b9f6737c8913ad2eff78ee7bc20a8049继续迁移。完整功能与未完成范围见[逐项清单](prometheus-migration.md)。合成模型、协议服务与数据库断言不计为真实QQ / 微信、外部模型或客户端验收。

## 本地Windows检查

2026-10-31 00:10（Asia/Shanghai），Windows11 Pro 10.0.26200 x64、Node24.22.1、npm12.2.1。主机Intel Core Ultra7 155H、22逻辑CPU、31.6GiB内存；本轮检查可用内存约15.9GiB、D盘可用413GiB。运行链路使用Node，不调用Rust或更改系统应用控制策略。

- `npm.cmd run typecheck`：TypeScript与Vue检查通过。
- `npm.cmd test`：前端构建通过；后端36项通过、0失败、0跳过，约20.7秒。
- `npm.cmd run test:e2e --workspace @prometheus/admin`：7项通过，约9.2秒。
- `npm.cmd run format:check`、`git diff --check`（本轮文件）：通过。
- `npm.cmd audit --omit=dev`：0漏洞。

后端覆盖QQ签名、批次、去重、逐段回执，WS identify与撤销，iLink本人/游标/上下文；77项目录构造及字段拒绝（无平台派发）；候选审核、撤销、遗忘水位、多批日记；触发预演与唯一firing、睡眠原期限重启恢复；PNG校验、视觉进入Native、显式生图拒绝URL；预算provider绑定、本地记忆；鉴权、秘密、取消；schema1快照及schema2升级、完整实例主密钥恢复；更正抑制与同域合并。

浏览器覆盖首建登录、桌面和移动宽度、记忆更正遗忘、性格/配置冲突、各类资料、来源审核/触发启用/日历/预算、好感/停用建议/HTML预览。隔离iframe中的合成恶意脚本未改动父页面。截图在`.local/prometheus/screenshots/`，已检查排版；账户与数据均为独立合成夹具。

## 原失败与修复

首轮全量回归1项旧断言把新作息入口当作503门禁，随后按实际路由更正并通过。浏览器旧断言把QQ当作禁用，新实现应标注真实平台待验收；新增好感卡片后关系断言需限定目标记录；建议轮询最初命中静态按钮文字，改为等待审核按钮。失败未计入通过数。

审查修复：自动任务完成不得再次触发自身；授权起点绑定消息序号；睡醒停止周期整理并保留检查点；更正抑制未发候选；触发提议审核后保存停用规则；人物合并不更新无关关系版本。未移除执行门禁。

## 正式发布

本节在提交推送、固定SHA构建、完整实例备份与Windows切换完成后记录。当前正式进程仍为上一版49c3ed46b9f6737c8913ad2eff78ee7bc20a8049，本地结果不替代正式验收。
