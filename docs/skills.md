# Skill 管理与绘画

Skill 是管理员保存的可复用任务指令，不授予工具、文件、网络或系统权限，不替代 Rust / WIT 插件工厂与 Harness 隔离。左侧「Skill 管理」提供创建、编辑、启停、删除和版本冲突检测。默认安装「制作 Skill」与「绘画」；管理员修改、停用或删除默认项后，重启不会覆盖这些操作。

对话工作台从「本次 Skill」选择后发送任务，选择只用于一次消息。文字渠道和 CLI 可以使用 `/skill <编号> <任务内容>`；编号在管理详情中可见。用户消息入队时冻结 Skill 内容、版本、执行方式及模型配置；同一消息编号重试返回原任务，不能借重试改换 Skill。后续编辑不会改变已提交的任务。Skill 输出不进入自动会话摘要或记忆候选来源。

「制作 Skill」先由模型输出 JSON 草稿。对话中点击「审核并保存为 Skill」进入可编辑详情，核对后手动保存；草稿默认停用，模型不能自行安装或激活能力。自定义指令须在 16,000 UTF-8 字节内，名称最多 128 字节，介绍最多 1,024 字节，最多保存 64 个 Skill。

「绘画」有两条实际执行路径：

- 「助手与模型」未启用原生生图时，使用当前文本模型生成 HTML / CSS / 内联 SVG，普通对话直接显示画作并提供 HTML 下载。预览移除主动元素、事件、外部引用和跳转，使用无权限 iframe sandbox 及禁止脚本、网络、表单的 CSP；支持 SVG `<use href="#id">` 内部引用。不会执行模型提供的 JavaScript。
- 管理员确认当前模型支持兼容的 `/images/generations` 和 `b64_json` PNG 后，可保存应用 `model.image_generation=true`。Web 绘画直接调用同一地址、模型和凭证的生图接口，传入冻结的绘画指令及用户要求；限制单张 2048×2048 以内、4 MiB 以内 PNG，实际解码去元数据后展示及下载。PNG 经有会话所有权校验的接口按需读取，历史列表不重复传输 Base64。上游失败明确报告，不伪装成已生成或静默换用 HTML。接口参数参考 [OpenAI Images API](https://developers.openai.com/api/reference/resources/images/methods/generate)。

当前实例默认的文本模型不因此自动获得生图能力。能力开关属于服务商支持声明，勾选或文本连接测试本身不证明真实生图通过；切换模型后应重新核对。QQ / 微信仍为文字渠道，绘画使用 HTML 代码文本，不宣称图片媒体发送已经实现。当前不同时附加理解图片并调用 Skill。

管理 API 为 `/api/admin/v1/skills`（GET / POST）、`/skills/{id}`（PATCH / DELETE，JSON 内 `expected_revision`）及 `/skills/images/{run_id}`（GET PNG）。所有读取须管理员认证，写入同时检查 Origin / CSRF，更新与删除使用乐观版本。Skill 在 SQLite 中持久化，审计只存对象引用，不存密钥。新增数据表为 additive；已使用的 Skill 快照、用户编辑和原生生图配置要求具备 `skills:1` 的版本读取，部署回退检查拒绝静默降级，保留升级前备份，不自动恢复覆盖用户数据。

相关检查：`cargo test --workspace --locked`、`npm test`、`npm run build`、`python -S -m unittest discover -s tools/tests`。隔离进程验收为 `tools/skills-smoke.py`，真实文本模型验收为 `tools/skills-live.py`；浏览器使用 `tools/skills-browser-fixture.py` 与 `web/admin/tests/e2e/{characters,skills}.spec.ts`。最终部署 SHA、实际测试范围与未执行项见 [本轮验收](skills-validation.md)。

后续默认 Skill「设置触发任务」目前仅有 [指令草案](skills/task-trigger-setup.md) 和 [停用配置示例](examples/task-trigger-setup.skill.json)，并未安装。它按 [任务触发设计](task-triggers.md) 输出 `task_trigger_draft`，需独立的草稿解析、管理表单及服务器能力；现有 Skill 指令功能本身不能创建或启用定时任务。
