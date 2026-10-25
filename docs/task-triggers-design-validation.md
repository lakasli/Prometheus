# 任务触发设计静态验证

日期：2026-10-25，Asia/Shanghai。用户本轮明确选择“先交付详细设计”和“中国法定节假日与调休安排”。本轮为 T0 设计交付，应用运行版本不变；未创建任务、调用模型、安装默认 Skill 或部署调度器。

## 交付范围

- [详细设计](task-triggers.md)：规则语义、日历、事件、四层状态、版本冻结、事务、权限、资源、UI / API、分期与运行验收矩阵。
- [Draft 2020-12 JSON Schema](schemas/dsh-task-trigger-v1.schema.json) 与 `docs/examples/task-triggers/` 的 11 个例子：指定时刻、启用后秒延迟、重复秒间隔、工作日、休息日、固定星期、月末、闰日、日期集、事件和助手任务。
- [设置触发任务 Skill](skills/task-trigger-setup.md) 与 [对应 JSON](examples/task-trigger-setup.skill.json)：指令正文一致，7,826 UTF-8 字节，符合现有 16,000 字节限制；示例默认停用，不包含管理授权或秘密字段。
- [离线设计校验脚本](../tools/validate-task-trigger-design.py) 和 [脱敏静态结果](validation/task-trigger-design.json)。脚本检查设计结构，不实现或运行调度算法。

## 实际检查

| 检查 | 结果 |
| --- | --- |
| JSON Schema 元结构及 11 份示例 | 通过；显式验证 RFC3339 风格的真实公历时间，避免 jsonschema 缺少可选格式依赖时跳过 date-time 检查 |
| 4 个额外合法结构 | 通过：60 秒模型间隔、有限提醒补跑、单次合并补跑、绑定 drawing Skill 的 HTML 输出 |
| 41 个拒绝用例 | 通过：伪造 owner / enabled / 密钥 / URL，错误日期和秒数、缺必填项、重复时刻、非法日历与时区组合、事件脚本 / 私有正文筛选、低于 60 秒模型周期、不合法补跑与输出组合等 |
| Skill 文本 / JSON 一致及 UTF-8 限额 | 通过，enabled=false，kind=instruction |
| 官方日期依据 | 已读取北京市政府转载的国务院办公厅 2026 年安排，核验 10-10 补班和 10-01 放假；Python 只校验了对应星期，没有将它冒称为日历引擎测试 |
| 文档与独立审查 | 已检查规则、后端可靠性、页面 / Skill 三个方向；修正重叠 / 补跑窗口、相对预览、日历时区、启用授权、事件溢出、跨日预算和输出方式约束 |

校验脚本依赖当前本地已存在的 `jsonschema 4.25.1`，未安装新的应用依赖。普通环境可执行：

```powershell
python tools/validate-task-trigger-design.py --report .local/task-trigger-design/static-validation.json
```

本机用户 site 的无关 `.pth` 初始化有既有警告，本次使用 `python -S` 加显式依赖搜索路径执行同一脚本，跳过 `.pth` 副作用，没有修改 Python 环境；最终退出 0。

```powershell
@'
import runpy, site, sys
sys.path.extend([site.getusersitepackages(), *site.getsitepackages()])
sys.argv = ['tools/validate-task-trigger-design.py', '--report', 'docs/validation/task-trigger-design.json']
runpy.run_path(sys.argv[0], run_name='__main__')
'@ | python -S -
```

## 尚未执行的检查

Schema 不能代替实际时区 / 偏移一致性、官方日历覆盖、事件来源 ACL 与字段类型、真实依赖 revision / kind、正文 UTF-8 和 trim、重复键 / 深度 / 节点上限、active_window 可满足性、补跑窗口大于 grace、动态预算和可信时钟检查。这些已列为服务器后续语义校验与运行验收要求，当前没有假定它们已经实现。

未运行调度器、Rust / Vue 构建、浏览器、停机恢复、真实模型、渠道或远端部署测试；本轮没有改变应用行为，不以重新启动生产服务代替设计检查。正式运行版本仍以已有 [Skill 部署记录](skills-validation.md) 为准。

设计文件及静态检查记录按仓库规则提交、推送。后续 T1–T4 实现必须分别执行固定 SHA 部署和远端运行验收，不能把本轮静态通过当成功能已上线。
