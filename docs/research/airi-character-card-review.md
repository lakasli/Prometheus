# AIRI 角色卡源码研究记录

研究日期：2026-10-24，Asia/Shanghai。目标：查看 AIRI 角色卡的具体实现，为 DSH 独立设计数据契约和运行机制；不是引入或部署 AIRI。

## 研究快照与方法

- 仓库：[moeru-ai/airi](https://github.com/moeru-ai/airi)。
- 固定提交：`300773ef28299f2bead06abf97d998f5bb4055db`。
- 该提交时间：2026-10-24 16:00:30 +08:00；标题：`fix(core-agent,stage-ui): write chat sessions only through session actions (#3097)`。
- 采用浅克隆、blob过滤与稀疏检出，只读取角色卡、编解码、管理页面、提示词和相关测试。未安装AIRI依赖、执行其应用或测试；下文测试结论是“源码存在断言”，不等于本机运行通过。
- [固定版本许可证](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/LICENSE)为MIT。本次保留概念分析和来源链接，没有将上游实现、默认提示词、角色资源或构建产物纳入DSH。

临时研究目录使用仓库忽略的 `.local/research/airi-20261024-character-cards`；最终清理状态见本文末尾。

## 具体实现观察

| 部分 | 源码位置 | 核实结果 |
| --- | --- | --- |
| 通用内部模型 | [ccc/define/card.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/ccc/src/define/card.ts#L4) | Card使用camelCase组织身份、人格、场景、示例、开场白及可选扩展；不等于交换文件的字段命名 |
| CCv3编解码 | [ccc/codec/characterCardV3.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/ccc/src/codec/characterCardV3.ts#L73) | 标准envelope为chara_card_v3；通用parser允许未来字段、区分版本；不意味着AIRI其他流程均无损 |
| JSON导出 | [ccc/export/json.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/ccc/src/export/json.ts#L9) | 显式映射snake_case；首个greeting与备用greeting分开，示例对话转换为文本 |
| AIRI运行扩展 | [types/airiCard.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/types/airiCard.ts#L13) | extensions.airi含模块偏好、语音、唤醒词、形象引用与agents等；跨机器分享应和本机配置分离 |
| 分享包 | [airi-card-import-export.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/services/airi-card-import-export.ts#L17) | ZIP包含manifest.json、card.json及可选显示模型；manifest format为airi-character-card，version为1 |
| 分享白名单 | [同文件的字段投影与sanitizeAiri](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/services/airi-card-import-export.ts#L199) | 保留可审阅子集；agents清空，自定义扩展和本机资源引用被过滤；creator、tags、示例、世界书等不完整保留，是有损分享路径 |
| 模块继承 | [airi-card-modules.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/services/airi-card-modules.ts#L3) | 全局默认与当前卡选择分开；更换provider时不能挪用旧provider的model |
| 编辑校验 | [airi-card-editor.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/services/airi-card-editor.ts#L87) | 编辑器检查名称、点分版本和artistry JSON；不是DSH服务端文件、权限及资源边界的完整替代 |
| 稳定提示词 | [airi-card.ts的resolveSystemPrompt](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/stores/modules/airi-card.ts#L107) | 拼systemPrompt、description、personality、scenario及特定形象/绘图指令；位置敏感字段被明确排除 |
| 编辑与运行更新 | [airi-card.ts的updateCard](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/stores/modules/airi-card.ts#L356) | 编辑活动卡可能立即应用；不能仅凭UI的保存/激活按钮推断其具有不可变发布版本 |
| 会话人格 | [stores/chat.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/stores/chat.ts#L276) | 按session.characterId选人格，模型step可重新读取提示词；DSH持久队列需要更强的版本冻结 |
| 编辑器交互 | [CardEditor.vue](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-pages/src/pages/settings/airi-card/components/CardEditor.vue#L626) | 提供保存、保存并激活；分区编辑与未保存提醒可借鉴，具体运行语义由DSH重新定义 |
| 历史与权威源 | [airi-card.ts](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/stores/modules/airi-card.ts#L1010) | 该处版本恢复依赖同步历史，浏览器也保存卡状态；DSH改为服务端SQLite不可变revision |

相关测试文件也已阅读：[CCv3测试](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/ccc/test/characterCardV3.test.ts#L74)、[分享包白名单测试](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/services/airi-card-import-export.test.ts#L14)、[字段生效范围测试](https://github.com/moeru-ai/airi/blob/300773ef28299f2bead06abf97d998f5bb4055db/packages/stage-ui/src/stores/modules/airi-card.test.ts#L932)。最后一项明确断言历史后指令、开场白和示例不进入稳定system prompt；字段能保存不代表已参与模型行为。

## DSH重新设计的取舍

采用CCv3角色内容、扩展偏好、可审阅分享子集及模块归属原则。DSH独立增加：服务端权威存储、草稿/发布/绑定分离、会话固定revision、入队静态快照与执行前完整上下文快照、QQ事务去重、受限导入、版本化迁移和兼容回滚检查。

不照搬浏览器localStorage/leader驱动运行、不复制Live2D/VRM/Artistry或agents、不动态读取卡改变队列中的人格、不将外部provider/model字符串直接转成宿主配置。完整契约见 [DSH角色卡设计](../character-cards.md)。

## 清理与交付记录

已完成设计审阅和以下静态检查：JSON Schema Draft 2020-12元校验、原创示例校验、5种合法模块偏好变体及11个非法样例拒绝检查。验证器为临时本地jsonschema 4.23.0，不进入项目运行依赖。非法样例包含错误版本、未知字段、秘密字段、禁用Chat、不完整prefer、超长名称和本地ID混入分享格式；这些检查不代表角色服务已经实现或运行通过。

**临时源码尚未删除。** 已在删除前核实绝对路径位于本仓库 `.local/research/`，路径及祖先均非重解析点，上游origin、固定SHA及干净工作区符合本轮研究检出。自动审批策略仍拒绝删除，返回 `blocked by policy`，未提供更具体原因；未改用其他执行器绕过该限制。

待完成的清理仅针对 `D:\CodeProject\DSH\.local\research\airi-20261024-character-cards`，包含其.git对象库。该目录受 `.gitignore` 排除，不提交、不被构建引用。用户可在本地PowerShell完成已请求的清理：

```powershell
Remove-Item -LiteralPath 'D:\CodeProject\DSH\.local\research\airi-20261024-character-cards' -Recurse -Force
Test-Path -LiteralPath 'D:\CodeProject\DSH\.local\research\airi-20261024-character-cards'
```

预期第二条返回False；执行后应重新核验并把本文与README的状态更新为已删除。本轮不能报告源码清理完成。
