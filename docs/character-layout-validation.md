# 角色卡网格与页面页脚验收

日期：2026-10-25，Asia/Shanghai（+08:00）。本轮按用户截图调整角色卡页面：左侧保留卡库说明、默认角色、刷新、新建与导入操作，右侧用卡片网格展示角色；选择卡片后在网格下方编辑。全局底部声明在短页面靠近视口底部，长页面则位于全部内容之后，保持正常滚动。

运行交付提交为下述固定 SHA；本记录后续单独提交，仅补充证据，不为文档提交重启应用。

## 发布版本与环境

- 本轮代码 SHA：`595288f9b181fb156056666a8e63a3e855e9f05c`，提交说明为 `fix: display character cards in right-side grid and anchor page footer`。
- 前端归档：`admin-595288f9b181fb156056666a8e63a3e855e9f05c.tar.gz`，SHA256：`579734ac1307781f79b5ef0a8f1b8d6e946dd13bb3b8ca4a9e65330691ee1137`；本地 `Get-FileHash -Algorithm SHA256` 复核一致。
- 目标：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1，Ubuntu 24.04.4 LTS / aarch64。
- 部署预检内存：总量 7933 MiB，可用 6771 MiB；可用量为本轮采样值。远端 Rust / Cargo 1.88.0，构建并发上限 2。
- 上一可用 release：`df8933f89733e95dcacee8bcd9ee3ac8b6e6bc9c`。
- 本轮只调整前端结构与样式、补充浏览器布局断言，不修改数据库 schema、角色发布语义、默认绑定规则或模型配置。

本地提交与推送成功，`git ls-remote origin refs/heads/main` 返回相同完整 SHA；远端 fetch 后通过 `merge-base --is-ancestor <SHA> origin/main` 核实包含该提交。部署前目录归属与 origin 正确、源码工作区干净。

## 本地检查与浏览器结果

| 检查                      | 结果                                                                                                           |
| ------------------------- | -------------------------------------------------------------------------------------------------------------- |
| 前端单元测试              | `npm test`，114 项通过                                                                                         |
| 类型检查与生产构建        | `npm run build` 通过                                                                                           |
| 格式与差异检查            | Prettier、`git diff --check` 通过                                                                              |
| 角色卡真实本地 API 浏览器 | `characters.spec.ts`，1 项通过；覆盖创建、保存、校验、发布、导入、默认角色与独立测试会话，以及本轮新增布局断言 |
| 管理界面浏览器回归        | `ui-design.spec.ts`，3 项通过                                                                                  |
| 多尺寸几何检查            | 1912、1366、1024、390、320 px 宽度均无横向溢出                                                                 |
| 短页面页脚                | 1912 × 948 时页脚底部 y=926；1366 × 768 时 y=746，均距视口底部 22 px                                           |
| 长页面页脚                | 角色编辑器展开后，页脚位于编辑器之后；窄屏随内容滚动                                                           |
| 聊天页兼容性              | 桌面工作台高度实测 693 px；全局 flex 调整没有压缩聊天工作台                                                    |

角色卡测试保留原有未保存编辑保护，并补充右侧 `.character-gallery .character-grid` 内存在卡片、桌面网格位于管理栏右侧、短页页脚靠底及长编辑页页脚位于内容之后的断言。390 / 320 px 的原有溢出检查继续保留。

页面以正常文档流排列：主内容撑满可用高度，页脚的自动上边距只吸收短页空白；未使用固定定位覆盖正文。卡片网格在窄屏转为单列，编辑器继续使用原有状态与 API 流程。

本地布局截图与几何数据位于忽略目录 `.local/card-layout-e2e/`：`gallery-desktop.png`、`gallery-320.png`、`chat-desktop.png`、`geometry.json`。这些结果来自本地隔离服务，不代替树莓派正式制品验收。

## 远端部署与验收

| 项目                   | 结果                                                                                                                                                                              |
| ---------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 固定 SHA 原生测试      | `cargo test --workspace --locked -j 2`：kernel 4 项、daemon 171 项全部通过，退出 0；daemon 测试耗时 58.87 秒                                                                      |
| 原生 release 构建      | `cargo build --release --locked -p dshd -j 2` 成功，退出 0                                                                                                                        |
| 版本切换与正式健康状态 | 2026-10-25 16:33:57 +08:00 完成切换；`current`、源码 HEAD、前端 manifest、`/health/ready` 均对应上述完整 SHA，用户级 `dsh.service` active/running                                 |
| 远端制品浏览器         | 同 release 二进制及静态资源，在独立私有数据目录启动，18421 SSH 隧道访问；`characters.spec.ts ui-design.spec.ts` 共 4 项通过，19.7 秒，退出 0                                      |
| 远端页面几何与截图     | 1912、1366、1024、390、320 px 无横向溢出；桌面卡片位于右侧，短页页脚距底部 22 px，长编辑页位于内容后；聊天区域高度 693 px。桌面 / 320 px 截图已实际查看                           |
| API 鉴权与配置行为     | 隔离黑盒 `smoke.py` 19 项、`character-smoke.py` 55 项通过，均退出 0；覆盖未认证拒绝、CSRF、密钥不回显、配置校验 / 生效 / 冲突、角色保存 / 发布 / 默认绑定                         |
| 脱敏报告               | [基础管理](validation/card-layout-m1-aarch64.json)、[角色卡](validation/card-layout-character-aarch64.json)、[部署身份与数据保留](validation/card-layout-deployment-aarch64.json) |

远端黑盒命令均使用 `--binary /home/campanus/apps/dsh/current/bin/dshd --web-dir /home/campanus/apps/dsh/current/web --report <报告路径>`。浏览器使用项目 Playwright Chromium 与随机隔离管理员，不使用正式凭证；布局截图、几何数据位于本机忽略目录 `.local/card-layout-remote/`。

部署脚本从远端已推送的目标提交通过 `git show <SHA>:deploy/deploy.sh` 提取，以 `DSH_CARGO_MIRROR=rsproxy bash <脚本> <SHA> <前端归档> <上述SHA256>` 执行，整体退出 0。启动后的首次健康探测曾遇到监听尚未就绪，后续完整 SHA 校验成功；未将首次探测失败报告为最终健康失败。

远端日志位于 `/home/campanus/apps/dsh/shared/logs/`：`deploy-<完整SHA>.log`、`test-<完整SHA>.log`、`build-<完整SHA>.log`、`card-layout-{m1,character}-595288f.log` 及同名 JSON 报告。切换前备份、制品摘要及数据保留核对详见部署 JSON；上一 release 已保留。本轮没有执行回滚或数据恢复。

验收后已按精确 PID、二进制和数据目录核对并关闭远端隔离实例，关闭专用 18421 隧道；正式服务复验仍 active/ready，原 18419 管理隧道保留。

本轮无须重复真实模型推理、QQ / 微信消息或媒体外部联调；未执行项目不得计为通过。真实配置、管理员凭证、会话、记忆、附件及密钥继续独立保存在部署数据目录，不进入 Git 或验收报告。
