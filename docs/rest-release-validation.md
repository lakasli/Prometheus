# 睡眠功能集成与发布验收（2026-10-27）

依据架构书 9.6–9.7 与当前接续顺序，将已提交的 `322714af85c7f0809f628cee6bb9fd08f772dfb1` 睡眠分支合入 `335319e` 主线，保留回复四项上限、上下文预算、来源与投递证据。实现契约见 [睡眠协调](rest-coordination.md) 与 [整理执行](rest-consolidation.md)。旧分支的本地测试与失败仍在 [原记录](rest-runtime-validation.md)，不改写成此次通过。

本轮额外修复 QQ 的独立入队路径未登记睡眠控制候选的问题；只在实际身份、原文和 Inbox 同一事务中登记普通直接输入，媒体与专用命令不取得控制能力。自然控制仍须独立来源 grant 和启用的作息策略。入睡确认加入宿主确定的唤醒日期、时间与 IANA 时区，仍受精确载荷、generation、原平台期限和发送前复核约束。

新增 `tools/rest-runtime-smoke.py` 使用真实 Rust 进程、签名合成 QQ 入站与 loopback HTTP。初始化账号前设置持久发送 fence；验证默认关闭、认证 / Origin / CSRF / 版本、控制幂等、接收与健康不中断、在途请求 lease、原始结果检查点、重启与原期限、来源独立授权及撤销。Linux 使用 boot ID 验证继续原期限；Windows 的进程时钟换代按已有契约提前唤醒，不当作 Linux 期限恢复证明。测试从不连接真实 QQ / 微信，不读取生产会话。

日记旧回归在无 provider 时先验证两次相同整理的幂等性，再配置 provider 并验证真实模型作业。这样背景模型合法换版不能抢在两个 API 请求之间造成夹具误判；原恒等断言、九项业务检查及撤销 / 遗忘检查保留。

## 本地与已存在正式版的定向检查

- 合并基线 Rust 库 631 项通过，26.04 秒；修复后专项、完整检查及进程结果在实际执行后追加。
- 前端 30 文件 / 204 项通过，类型检查与构建通过；Python tools/tests 135 项通过。未把原分支的检查数量与本轮相加。
- 新进程测试首轮在授权 QQ 自然入睡处失败，前 16 项通过；由此定位并修复上述实际接线遗漏，报告保留 `.local/rest-release/rest-runtime-local-r1.json`。修复后须另存复验，不能改写原失败。
- 修复后 r2 / r3 因 Windows 重启不继承手动时钟确认，新的入睡被正确拒绝；补上本临时实例重启后的显式时钟确认，未改变业务门禁。r4 **22 项通过**，7.87 秒，报告 `rest-runtime-local-r4.json`。其中 Windows 提前唤醒与 Linux 原期限恢复明确分开，不把前者记作后者通过。
- 最终本地 `cargo test --workspace --locked --quiet`：daemon **631 项通过**、26.22 秒，内核生命周期另 **4 项通过**；完整日志 `.local/rest-release/rust-final.log`。新确认时间专项在原七项协调测试内，未重复计数。日记进程原 **9 项通过**，5.55 秒，报告 `diary-local-r1.json`。格式与 diff 检查通过。
- 部署前对当时正式 `ac9b70389507455050aea32862d9a268a7b9c940`，从独立干净 `53a5a89d8069b2846c2eff6eb4db913410cb13a0` 测试工作树运行 `--cases GROUP,S05 --max-real-requests 6`：2 个宿主场景通过，严格 5 次真实 HTTPS 200，峰值并发 1、重试 0。报告 `shared/logs/conversation-state-ac9b703-targeted-real.json`，输出同名 `.log`。状态 `host_complete_review_pending`，表达仍待审；不把前两轮 22 / 23 次失败记录重写或合并成通过统计，也不当作本次新睡眠制品的真实模型验收。

## 发布与远端结果

准备阶段 SSH 曾超时，恢复后核验目标为 Raspberry Pi 5 Model B Rev 1.1、Ubuntu 24.04.4 LTS、aarch64，Rust / Cargo 1.88.0。可用内存采样约 7,376 MiB、swap 0，磁盘剩余 24 GiB；目录归属 campanus、origin 正确、repo 工作区干净。上一正式版本为 `ac9b70389507455050aea32862d9a268a7b9c940`，`dsh.service` active/running，ready 返回同 SHA。

最终提交、推送、制品 SHA、远端构建 / 启动 / 浏览器 / 进程验收和数据保留核对在实际执行后追加。当前段落不是部署完成证明。

## 保留的功能边界

默认自然控制、定时作息、来源采集与模型出域不自动启用；正式数据和已保存性格不被升级覆盖。单期超过 16 个来源仍 `partial/source_batch_limit`，完整月的多批归并仍待实现。人物 / 好感没有独立睡眠写入授权时保持零写入。真实 QQ / 微信控制确认、真实渠道暂停 / 恢复与客户端展示未测试，不能由合成入站或队列证据替代。完整睡眠整理、主动联系、Python 动作和完整 M2 / M4 / M5 不因此宣称完成。
