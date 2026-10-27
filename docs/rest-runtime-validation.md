# 睡眠运行集成本地验收记录

记录日期：2026-10-27，时区 Asia/Shanghai（UTC+08:00）。工作树为 `C:\Users\AIIT\.codex\worktrees\rest-runtime\DSH`，分支 `codex/rest-runtime`，记录时 Git HEAD 为 `b68e27665817dd5b1e92a1cb233410755c0a6505`。该 SHA 只是工作树基线，**不包含本页所述未提交的 Rest 代码**；后续 host-final 协议修复也已带入当前工作树。最终提交 / 推送、发布 SHA、远端构建部署均尚未执行，不能用基线 SHA 冒充本轮制品版本。

本轮已接入认证管理 API / 前端组件、原子初始化、宿主时钟、控制候选、worker / scheduler、三个实际发送 claim 和有界日记整理。当前契约见 [睡眠协调](rest-coordination.md) 与 [整理执行](rest-consolidation.md)。生产实例未执行入睡、自然控制或来源授权变更；默认计划睡眠 / 自然控制仍关闭，默认睡眠时长与定时唤醒为空。

## 本地检查结果

最终 Rust 库日志于 02:24:18 写入：**631 / 631 通过，25.53 秒**。该次包括下列阶段专项及 host-final 协议回归。之后 Root 只执行 `cargo fmt --all` 排版，无语义修改。各阶段快照存在重叠，**不能把 631、早批 48、专项 21 / 14 / 20 相加成测试总数**。

| 检查与源码阶段 | 真实结果 | 日志 / 记录与范围 |
| --- | --- | --- |
| 最新完整 Rust 库 | 631 通过，0 失败，25.53 秒 | `.local/rest-full-library-final.log`；Root 执行，包含最新 API、claim、worker、初始化与 host-final 协议 |
| 初版 Store 导入 | 20 通过；独立存储阶段 | `.local/rest-integration/imported-store-validation.json`；原 `rest-coordination` 工作树 / 基线的历史准备证据，当时未集成运行 |
| Rest 集成早批 | 48 通过，0 失败 | `.local/rest-consolidation/rest-tests-sixth.log`；当时的整理 17、Store 20、API 9、Clock 1、claim 1，并非最终全量 |
| 整理及新增管理 API 钩子 | 21 通过，0 失败 | `.local/rest-consolidation/api-hooks-tests-first.log`；含原 17 整理测试、完整 Router 的 3 个新案例及 credential partial-schema 案例 |
| 协调运行专项 | 7 通过，0 失败 | `.local/rest-runtime-tests-r3.log`；直接控制、队列上限、原子启动、展示检查点与重启媒体 unknown |
| 三种实际发送 claim | 14 通过，0 失败 | `.local/rest-publication-tests-second.log` 与 `.local/rest-publication-validation-local.json`；真实 SQLite 领取事务，平台回执为合成数据 |
| worker 上下文 / cache 竞态 | 20 通过，0 失败 | `.local/rest-worker-context-queued-forget.log` 与 `.local/rest-worker-context-validation-local.json`；含真实 loopback HTTP 屏障，非外部 provider |
| Python 完整工具测试 | 120 通过，0 失败，5.339 秒 | `.local/rest-python-tests-final.log`；`python -S -B -m unittest discover` 的 tools/tests 集合。兼容 gate 的 55 项是其中子集，不相加 |
| Rest 兼容 gate 单独验证 | 55 通过，0 失败 | `.local/rest-consolidation/compatibility-final.log`；新增 Rest 13 个测试方法，并遍历 21 表 / 18 辅助表子例，含实际 CLI exit 2 / 0 |
| 前端单元 / mock API | 30 文件、204 测试通过；其中 Rest 专项 17 | `.local/rest-ui-unit.log`、`.local/rest-ui-validation-local.json`；类型检查和 Vite build 通过，尚无真实浏览器通过结论 |
| 排版与 diff 检查 | 最终 fmt check 0、diff check 0 | Root 执行 `cargo fmt --all` 后复验；`.local/rest-fmt-final.log`。最初 fmt check 只有排版失败，不隐藏该次失败 |
| 严格 Clippy | **exit 101，未通过** | `.local/rest-clippy-final.jsonl`、`.local/rest-clippy-final-stderr.log`、`.local/rest-clippy-final-comparison.json`；48 诊断 / 21 种不同组合，与原基线完全同集，新增 0。原有诊断仍未清除 |

上述 `.local` 为保留在本地工作树的操作证据，不随 Git 发布；发布归档时须连同最终 SHA 和命令记录另行保留，不能只引用一份不在制品中的日志作为远端验收。当前新 e2e spec 只是准备中的脚本，未运行，不能记为浏览器或部署通过。

可复验的专项命令如下，均从本工作树执行；这些命令不构建或覆盖被其他本地任务使用的主 `target/debug/dshd.exe`：

```powershell
cargo test -p dshd rest_consolidation::tests --lib --offline --target-dir D:\CodeProject\DSH\target
cargo test -p dshd --lib rest_publication::tests --offline --target-dir D:\CodeProject\DSH\target --jobs 1
cargo test -p dshd --lib worker::context_tests --offline --target-dir D:\CodeProject\DSH\target --jobs 1
python -S -B -m unittest tools.tests.test_deploy_compatibility -v
```

## 已实际验证的边界

初始化采用一个 savepoint 建立 Store 14 表、整理元数据 3 表、管理幂等 1 表与运行 3 表，共 21 表；已有部分 schema 不自动修成默认 Awake。管理操作通过实际 Router 的认证、Host / Origin、CSRF、版本 / 幂等检查；来源 grant 从真实冻结绑定恢复 owner、conversation 和实际 speaker，群 owner 不代替某个发言者，管理 grant 不变成资料、工具或接收方联系许可。

实际 claim 专项调用 QQ 有序句段、QQ 旧文本及微信真实领取函数，验证休眠 hold、固定确认优先、精确载荷 / grant / generation、原平台窗口、顺序与 pacing、两个连接不重复领取，以及 unknown 前驱不能绕过。取消或拒绝只影响尚未领取部分，已有 sending / sent / unknown 和 receipt 不改写。这些是本地领取事务证据，没有调用真实 QQ / 微信平台。

worker 真实 loopback HTTP 测试用屏障保持请求在途，观察 native lease 仍占用且下一次 claim 被阻止；入睡后只缓存原始生成，不发布回复，醒来复用同一份冻结 Prompt / raw generation，provider 计数仍为一次。唤醒测试中时钟信任与磁盘门禁为 false，唤醒仍推进；HTTP 真实结束 / 丢弃后才归还物理占用。

来源失效覆盖两类实际竞态：请求在途时调用真实 memory forget，晚到模型结果不能重新填入已经失效的 raw cache；只使用旧显式 memory、没有新的 state / commitment usage 时，忘记也会在原事务立刻清除已 queued 的缓存并取消实际消费者，同会话未使用来源的 queued Run 与另一来源域保持。测试没有删除证明行来制造结果。普通日记 / 人物派生更版不借此盲推历史 floor。

Raw generation 和 plan 只作为有界待展示检查点，不能当作已回复或已投递。新媒体步骤启动前置 `media_inflight=1`，每个完成的 plan 检查点回到 0；独占锁确认旧进程退出后，inflight 媒体保留 unknown / failed，不自动重放，已完成的原结果才可按原期限重新排队。最终模型 dispatch、展示和实际发送 claim 仍校验原身份、来源与授权。

完整管理 Router 测试证明：取消 Run 与删除其 cache 同事务，cache 删除故障会回滚逻辑取消；外域 cache、native / diary-model 物理 lease、sending Outbox 及 unknown / sent 句段保留。Rest attempt 真实未结束时，活动 profile 已移走凭证、周期已逻辑 wake / unknown 仍拒绝删原凭证；另一个未引用凭证可正常删除。Profiles / attempts 仅一个表存在时拒绝删除，真实 attempt 结束后原凭证才可删。

整理执行使用临时 AppState 和真实 loopback 模型 HTTP，精确读取同 owner / conversation 的冻结引用。已测试同 cycle 日→已结束周 / 月 ProducedRef、当前版本 / epoch / 撤权、人工修订 CAS、截止后新消息、摘要和 attempt fence。模型只能选择可逐行验证的原文，宿主保留来源陈述、计划 / 取消 / unknown、未经独立核验及不证明完成 / 送达的标签。真实 unknown 投递投影未向模型或日记复制未确认的候选正文；真实 diary forget 清除 canonical 派生正文，后续 prepare 不复活它。

## 保留的失败与阶段差异

- `.local/rest-consolidation/tests-first.log` 保留首轮编译的借用 / 生命周期错误；修正后才有后续专项通过。`.local/rest-consolidation/rest-tests-fifth.log` 保留并行集成时 publication 模块尚未声明的编译失败，未把它记成通过。
- `.local/rest-runtime-tests.log` 保留协调模块首轮生命周期编译失败；后续 r2 / r3 分开保留。
- `.local/rest-publication-tests-first.log` 为 12 / 14，两个微信合成 fixture 使用了不合法的三段 owner；改成实际四段来源形状后第二轮 14 / 14。没有放宽生产权限检查来让测试通过。
- `.local/rest-worker-context-first.log` 为新增 queued-only forget 案例之前的 19 / 19；后续 20 / 20 和当前全库 631 分别保留。`.local/rest-full-library.log` 的 623 / 623 是较早工作树快照，不替代最新 631。
- `.local/rest-consolidation/clippy-first.jsonl` 保留新增 lint 修正前的结果；最新严格检查仍为 101，只能报告与基线同集、新增 0，不能报告严格 Clippy 通过。
- 兼容首轮 `.local/rest-consolidation/compatibility-first.log` 有本机 Python 用户 site 的 `_distutils_hack` 噪声；后续 `-S -B` 隔离验证与完整 120 项结果分别保留。没有改写失败文件或删除日志来形成成功记录。

## 未完成与未执行

单个日 / 周 / 月期超过 16 个来源引用时仍为 `partial / source_batch_limit`。完整月份通常超过这一单批上限，多批归并尚未实现；稀疏月记的本地成功不能推广成所有完整月份已经整理。人物和好感没有独立睡眠写入授权入口，有资料周期明确记录 `independent_permission_unavailable` 与零写入，候选、批准、好感及主动联系不互相代授权限。

生产入睡 / 唤醒、生产作息策略、真实 QQ 控制确认、真实微信控制、外部 provider 睡眠整理、远端 Linux 构建 / 部署 / 恢复、真实浏览器及跨日 / 完整月性能验收均未执行。本轮没有改变生产配置或读取生产资料，没有关闭操作系统或停止接收器。默认 off 不代表这些未验收路径已经完成。

唤醒逻辑在 NTP、磁盘和模型门禁前执行，不等待模型 / worker 完成；它仍需要数据库成功提交。数据库或协调失败会返回错误并记录 `rest coordinator unavailable`，不能显示为已经持久唤醒或伪造一个成功状态。故障恢复与 UI / 远端演练仍须作为后续固定 SHA 验收项目。
