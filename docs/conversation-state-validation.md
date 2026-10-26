# 话题、约定与上下文预算验收

目标仍为：隔离 → 投递历史 → 生成中纠正 → 话题与约定，再扩展睡眠、主动联系和可执行动作。本页只记录当前切片证据，不称完整自然语言协调、40 场景语义达标或完整 M2 / M4 / M5。当前正式基线 `468fcee400601a590e7bc4c8c573d16a7c20bca9` 的结果见 [首批记录](conversation-coordination-validation.md)。

实现契约为 [会话状态](conversation-state.md)、[约定](conversation-commitments.md)与[预算](context-budget.md)。开发在独立工作树 `C:\Users\AIIT\.codex\worktrees\conversation-delivery\DSH` 完成，保留并发关系网提交及原工作目录已有进程。后续睡眠存储在另一独立工作树准备，未混入本切片的程序或验收结论。

## 本地检查

- 冻结源码最终 `cargo test --workspace --lib --locked --quiet --target-dir D:\CodeProject\DSH\target` 退出 0，daemon **549 项通过**，运行 21.73 秒。包括新增“同事实不同备注不冲突”和排队后失效发送门禁，报告 `.local/conversation-state/local-test-final.log`。正式 Linux 制品仍须从固定 SHA 编译并全测。
- 真实 worker 集成含完整末尾输入、超出最近 12 轮后的完整原问题续接、不同 owner 隔离、必需块超预算时零请求、未选中资料零 usage、策略换版不重写缓存 Prompt、约定换版阻断发布、在途物理 lease、匿名群空引用、FTS 故障和授权账本故障、实际投递证据下的保守呈现冷却。
- 发送领取另有 11 项实际 claim 回归：Outbox 已排队后约定 / participants 换版或 TTL 到期，QQ 新 / 兼容领取器与微信领取器重新核验；仅取消该 Run 尚未领取的段，保留 sending / sent / accepted / unknown、原发布决定和所有 lease。原 binding / context / Prompt 三证缺一则拒绝，读取旧结果不补造证明；完全没有新快照的历史路径不虚构新证明。另复验既有 QQ 5、微信 13、worker 16 和 helper 7 项，均实际通过；最终 549 已统一纳入最新强化。
- API / 领域专项覆盖来源审核、主体和对象、真实活动关联、重复与并发版本、未知时间、遗忘 / 撤权、同 scope 精确消费者、跨 TTL、敏感候选不派生和原始消息保留。普通日记版本更新不新增历史水位；来源精准失效不因提高整个 scope epoch 误伤无关在途上下文。
- Rust 格式检查通过；部署 reader / 停止 / 回退门禁 Python 59 项通过。新状态、约定、预算共 17 张表逐表核对：有数据（含删除水位、epoch、幂等记录和仅保存策略）阻旧 reader，空初始化不虚构已用语义。
- 前端 28 文件 / 185 项测试通过，类型检查和生产构建通过。预算位于配置“工作节奏”，独立保存；约定位于日历工具栏弹窗；日志只读提供上下文选择。此处测试使用模拟 API，不能代替后续真实浏览器 / Linux API 验收。
- 严格 Clippy 未通过，退出 101。首批最终报告与本轮冻结源码 `clippy-release.jsonl` 均为 48 条诊断、21 个按文件 / lint / 消息去重的键，新增 0；本轮新增两类已修正，原有 lint 债务未关闭。原始报告、初轮新增诊断及比较保存在 `.local/conversation-state/clippy*.json*`，对照为原工作目录 `.local/conversation-coordination/clippy-final.jsonl`。

本地独立构建命令：

```powershell
cargo rustc -p dshd --bin dshd --locked --target-dir D:\CodeProject\DSH\target -- -o C:\Users\AIIT\.codex\worktrees\conversation-delivery\DSH\.local\conversation-state\dshd-final.exe
```

该开发制品标识仍为 development，SHA256 `eb7c94444eed470ef6d7951e910b551229dbf9c5ed47a787b6348ab9f3b4b8eb`；不能作为正式运行 SHA，也不能将后续仅格式 / 参数组织变化称为已经重新部署。

## 回放与失败保留

原创合成数据为 `tools/fixtures/conversation-regressions-v1.json`，40 组覆盖六类。`conversation-replay-smoke.py` 实际运行临时 Rust 进程、认证管理接口、签名合成 QQ 入站、loopback 模型、持久队列 / 回执、重启和 SQLite 证据。固定原始锚点与实际 host 锚点、时间平移分别记录，不修改主机时钟；模拟历史跨日不称真实等待跨午夜。

首轮完整回放 40 个 host case、145 条 host 断言通过，原报告 `replay-40-final-first.json` 保留为诊断。加强活动精确 ID / revision / 状态和单槽确定性后，冻结脚本最终全轮 **40 个 host case、147 条 host 断言通过**，90 次 loopback 请求，退出 0；报告 `replay-40-final.json`，runner SHA256 `a864df0d0fdf68d645e2333d7b33b366e6ec23880b5ea4edf14a336186d259ac`。两轮不合并计数。

**40 组表达全部未评估**，17 条原始 criterion 没有对应机器 host 检查，另有 12 条显式未评估 host 检查。case 通过只意味着其记录的 host 断言通过，不代表全部语义通过。QQ API 调用 0、持久 pacing fence 保留，微信账户未启用。原锚点 15:00 平移至实际 23:39:05，偏移约 31,145.7 秒，不改主机时钟。该开发制品早于最后发送领取强化；领取回归由最终 549 项覆盖，固定 Linux 制品仍须重跑全 40 场景。

前期失败均保留：有新 provenance fence 后不能再直接伪造 queued Run；过长合成回复不能绕原每项字数限制；来源不含“明天”时不能自造 time_text；host-only 排除字段不能从 JSON 反序列化注入。修正夹具后保留原功能约束。真实授权账本故障还发现程序会把具体不可核验错误覆盖为普通版本变化，已修复为无生成结果时保留原安全错误，仍然零 provider / Outbox。

E06 / F06 初版仅查字符串可能将 unscheduled 当 scheduled，已加强为精确约定、真实活动 / occurrence 及无执行 / 投递证据检查。管理员审核明确 `source_assent_verified=false`，未冒充来源同意或自动联系许可。各轮报告为 `.local/conversation-state/replay-*.json`，最终结果不能从 loopback 回复推断真实模型表达。

`conversation-state-live.py --self-test` 离线 9 项通过，0 外部模型请求、0 生产数据读取。正式脚本为 12 个代表场景设置最多 24 次新真实请求、单一转发并发、1024 输出和 96,000 字节上限；真实转发须核对实际 Run lease、冻结 Prompt 与预算。输出与 host 状态保留供审读，HTTP 200 不等于程序完成或表达合格；尚未执行前维持 review_pending，也不冒充原 40 场景全部真实模型 / 人工对照。

## 数据恢复与当前发布边界

2026-10-26 23:10:07，使用部署前 `pre-468fcee400601a590e7bc4c8c573d16a7c20bca9-20261026T144449627434068.sqlite` 在私有临时目录恢复独立库：源备份 campanus / 0600，目标目录 0700、独立库 0600；quick_check 均 ok，外键违规 0，7 表数量为 4 / 92 / 0 / 0 / 0 / 0 / 0，配置 revision 7、性格与身份摘要精确一致，实际 468 reader 检查退出 0。报告 `shared/logs/recovery-468fcee.json`。未切服务、生产指针或配置，**仅为独立 SQLite 恢复检查，不是生产灾难恢复验收**。

本切片的提交、推送、正式 Linux 构建、固定运行 SHA、远端回归、真实模型与新浏览器验收在完成后追加。尚未执行的项目不记通过；新状态或策略已有记录后，不能盲降至忽略其语义的 468 版本。睡眠、真实主动联系与可执行动作仍未由本切片交付。
