# 管理前端视觉调整与验收

2026-10-24，根据用户对侧栏滚动条、原生下拉菜单不统一的反馈，重新检查九个主页面及开发诊断、记忆候选和摘要区域。本轮保留现有靛蓝 / 珍珠白配色、角色插画和产品导航，调整布局收纳、控件风格与文字层级。

运行交付提交：`df8933f89733e95dcacee8bcd9ee3ac8b6e6bc9c`。本记录后续单独提交，仅补充验收证据，不为文档提交重启应用。

## 发现与调整

| 问题 | 本轮处理 |
| --- | --- |
| 整个侧栏滚动，品牌及账号会离开视口 | 三行 Grid：品牌固定、中段导航可收缩、设备与账号固定。常用尺寸全部入口可见；矮屏只滚动导航，隐藏中段滚动条，保留滚轮和键盘可达性 |
| 后续能力、装饰文案占用常驻高度 | 收入底部“语音 · 插件工厂”展开入口；说明以浮层展示，不挤压导航；外点、Escape、切页关闭 |
| 原生 select 弹出选项沿用系统灰色菜单 | 统一 AdminSelect 的圆角、边框、阴影、选中标记、悬停、焦点和禁用态；使用 body 浮层，依据视口空间上下展开 |
| 只有一个可选值仍显示下拉箭头 | 单一已选值使用只读字段，不呈现无意义菜单；不启用尚未实现的角色视觉 / 语音偏好 |
| 角色卡文本继承标签粗体 | 标签 12px / 600，正文 14px / 400，说明 12px / 400；整理模块状态文案 |
| 聊天、历史、配置流程文字偏小偏淡 | 提高聊天正文与输入字号、时间和流程说明字号及对比度；收紧空状态留白，统一普通面板标题层级 |
| checkbox 继承普通输入框 padding | 全局统一 16px、零 padding、主题强调色；QQ 专用开关保持其独立外观 |
| 文件选择、角色卡动作条不一致 | 统一原生文件按钮；导入使用键盘可达按钮；编辑动作条移到字段之前并提示校验 / 预览 / 测试会先保存草稿 |
| 切页继承旧滚动位置，手机关闭抽屉仍可聚焦 | 页面切换与登录完成回到顶部；移动端关闭侧栏设 inert / aria-hidden，并增加显式关闭按钮 |

本轮未修复前一轮识别的配置历史直接刷新误空、总览读取未保存表单值、普通聊天草稿切页丢失等业务状态问题；不将视觉调整报告为这些逻辑问题已修复。

## 本地验证

- `npm ci`：退出 0，127 个依赖，审计 0 漏洞。
- `npm run build`：类型检查和 Vite 生产构建通过，退出 0。
- `npm test`：14 个文件、114 项通过，包含选择控件键盘、禁用、只读、值类型及外部更新测试。
- `cargo build --locked -p dshd`：Windows 本地隔离浏览器服务构建通过；不是 Linux 制品。
- `git diff --cached --check`：通过。
- 隔离 Chromium：management、memory、memory-candidates、summary、characters 与 ui-design 共 8 项通过。使用新建 `.local/ui-design-qa` 和 `.local/ui-design-characters` 数据目录、随机测试账户与 loopback Origin，不使用生产配置 / 会话 / 凭据，不发起真实模型或平台消息。对应临时服务已停止。
- UI 几何验证通过：1920×1080、1366×768、1024×600 的侧栏顶部与底部锚定；390×844、390×480 的导航收纳、内部滚动及关闭；下拉浮层在视口内，支持键盘切换、Escape 和外点关闭。QQ 下拉用例在登录后的监听区间未发现写请求；其他集成用例正常写入各自隔离测试数据。
- 九页桌面及不同高度的截图：忽略目录 `.local/ui-design-evidence/`；角色卡桌面 / 手机截图：`.local/character-e2e/screenshots/`。已实际查看截图并通过浏览器检查 QQ 弹层。

## Ubuntu 部署与远端验证

本地提交已推送 `origin/main`，`git ls-remote` 返回同一完整 SHA。前端归档由 `deploy/package-web.ps1 -Commit <SHA>` 构建：

`admin-df8933f89733e95dcacee8bcd9ee3ac8b6e6bc9c.tar.gz`

SHA256：`c8b2caaac1360bd04f19465be448fe6f7f48cf2d88bd94e035cee3b6abedaaf3`。

部署前 SSH 核实：Raspberry Pi 5 Model B Rev 1.1，Ubuntu 24.04.4 LTS / aarch64，7933 MiB 总内存、约 6420 MiB 可用、无 swap；Rust / Cargo 1.88.0，部署目录及 repo 均属 campanus，工作区干净，磁盘可用约 35 GiB。沿用已有系统和服务，未改变主机启动目标。

上一可用版本：`bd914f8ea73ffad5697248ddf3df716f7208299a`。从已推送目标 SHA 取出 `deploy/deploy.sh`，以 `DSH_CARGO_MIRROR=rsproxy`、上述归档及哈希部署；脚本限制构建并行度 2，独立 release，切换前 SQLite 备份，保留配置和用户数据。

- 部署完成：2026-10-24 22:51:08 +08:00（`2026-10-24T14:51:08.787029+00:00`），目标 `campanus@192.168.189.191`，命令整体退出 0。
- 远端 `cargo test --workspace --locked -j 2`：171 项后端、4 项生命周期通过；`cargo build --release --locked -p dshd -j 2` 退出 0，release 构建用时约 3 分钟。
- `current/source` HEAD、Web manifest、`/health/ready` 均为上述完整 SHA。`dsh.service` 为 active / running，NRestarts=0。启动首个 ready 探测曾连接失败，随后正常 ready；未将首次探测失败当作最终健康结果。
- 运行二进制 SHA256：`062173064077a216e6c7424f7a1a679fdf56b0ed54e5838e03cad914c774f15a`。
- 切换前备份：`/home/campanus/apps/dsh/shared/backups/pre-df8933f89733e95dcacee8bcd9ee3ac8b6e6bc9c-20261024T145107394024964.sqlite`。保留上一 release，未执行回滚或数据恢复，不声称已验收恢复流程。
- 远端实际运行 `python3 tools/smoke.py --binary <current/bin/dshd> --web-dir <current/web> --report <logs/ui-design-m1-SHA.json>` 与 `character-smoke.py` 同参数，各退出 0：[基础管理 19 项通过](validation/ui-design-m1-aarch64.json)、[角色卡 55 项通过](validation/ui-design-character-aarch64.json)。覆盖 API 鉴权、CSRF、密钥不回显、配置保存 / 校验 / 生效 / 冲突 / 回退，以及角色草稿 / 发布边界；使用隔离临时数据库与模拟 provider，没有真实模型或平台联调。
- 生产浏览器只读检查总览、QQ 菜单与角色卡：Web 显示 `df8933f897`，固定侧栏和账号可见、菜单样式统一、角色输入正文与工具条正确；仅展开及关闭菜单、读取既有角色，没有保存、应用、发布或发消息。截图位于 `.local/ui-design-evidence/production-{overview,dropdown,character}.png`；浏览器临时视口覆盖已恢复。
- 日志：`/home/campanus/apps/dsh/shared/logs/{deploy,test,build}-df8933f89733e95dcacee8bcd9ee3ac8b6e6bc9c.log`，黑盒日志为同目录 `ui-design-{m1,character}-<SHA>.log`。部署身份及制品摘要另见 [部署证据](validation/ui-design-deployment-aarch64.json)。

## 范围

此次为视觉与导航交付，不涉及记忆身份、权限、模型请求协议或数据库迁移。远端黑盒验证使用私有临时数据库；模拟 provider 和浏览器夹具不能作为真实模型、QQ 或微信平台联调的通过证据。本轮不安排真实模型调用、扫码或渠道消息测试。
