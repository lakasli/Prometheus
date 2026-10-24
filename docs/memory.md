# 助手自身的长期记忆

记忆属于DSH助手这个独立个体。`settings['assistant.identity.v1']`保存稳定UUID，角色卡revision只控制表达配置；换渠道、新建会话、切换角色和重启不创建另一个记忆主体。无需绑定人的Web / QQ / 微信账号。

## 当前切片

- `shared`：实例管理员在Web明确确认发布，助手可以在各渠道引用。
- `private`：助手为来源账号域保密的记录。owner由服务端从来源会话推导，模型和请求体不能指定它；同来源域的新会话可延续，其他账号 / 群聊不可读取。
- 显式保存事实或经历，保留来源会话 / 消息、创建人、版本和审计。尚无自动抽取、摘要、TTL、语义向量或人的跨渠道绑定。
- 中文FTS5 trigram搜索，短于三个字符时在已授权范围内做字面查询。运行时先按权限检索，最多6条 / 8000字节；没有字面命中时补入有界的近期可见记录，不把它称为语义检索。
- 每条最多4000 UTF-8字节，助手有效条目最多5000，单个私有来源域最多500；管理列表分页上限100。

管理API为 `/api/admin/v1/memory`、`/memory/{id}` 和 `/memory/export`，需要实例管理员会话。写操作检查Origin / CSRF，纠正和遗忘必须携带当前 `If-Match`；冲突不会覆盖另一份修改。共享创建和私有转共享还需显式 `publish:true`。导出包含私有内容，仅供管理员妥善保存，不包含模型密钥。

Web / CLI / QQ使用相同控制命令：

```text
/memory list [关键词]
/memory show <ID>
/memory remember <内容>
/memory correct <ID>@<版本> <新内容>
/memory forget <ID>@<版本>
```

命令直接处理持久Run，不需要模型密钥。记忆变更、回复和待发记录同事务提交，重复入站 / 客户端消息ID不重复写入。命令文本与确认回复不进入后续模型历史。聊天命令仅创建 / 更改本来源域私有记录；共享条目只读，发布由管理页面完成。群聊和频道因尚无发送者写入授权模型，仅支持读取，由实例管理员维护其记忆。角色草稿测试会话不读取长期记忆、不写入或修改记忆，避免测试污染助手经历。

## 纠正与遗忘

每次模型请求持久记录实际使用的记忆ID / revision，并与完整prompt快照同事务冻结。纠正、收回可见范围或遗忘时，同事务使关联prompt快照失效，提升受影响会话历史水位，阻止旧回复把旧事实重新带回来。正在使用旧上下文的模型Run被取消，旧结果不能落库；尚未发送的旧回复取消。已在发送中的平台请求和已发送消息不宣称可撤回。

遗忘清空记忆正文、全部历史版本正文和FTS条目，保留无正文tombstone及删除序号。原始聊天记录仍可在历史中查看，但相关旧轮次不再自动送进未来模型上下文。原始消息保留、离线备份清理及灾难恢复后的删除水位重放是独立流程，当前不承诺物理擦除全部副本。

首次成功写入记忆后，全局数据库reader版本升为2；即使全部遗忘也不降级。部署门禁要求目标版本同时理解助手记忆和上下文失效，防止回滚后无声绕过遗忘。空记忆表的首次初始化保持reader1，可在未产生记忆语义前恢复角色卡版本。代码回滚与从私有备份恢复数据库分别处理。

## 验证命令

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm.cmd test --prefix web/admin
npm.cmd run build --prefix web/admin
python -S tools/memory-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist --report .local/memory-smoke-local.json
python -S -m unittest discover -s tools/tests -v
```

真实模型验收仅在树莓派读取现有模型配置 / 凭证，在临时私有实例使用合成事实，不读取真实聊天，不修改正式数据库或发QQ消息：

```sh
python3 tools/memory-live.py --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --source-data-dir /home/campanus/apps/dsh/shared/data \
  --report /home/campanus/apps/dsh/shared/logs/memory-live.json
```

本地 / 远端、模拟 / 真实模型、模拟渠道域 / 真实QQ结果分别记录于 [验收记录](memory-validation.md)，不相互替代。
