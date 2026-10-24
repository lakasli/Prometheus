# 角色主题 UI 部署与验证

日期：2026-10-24，Asia/Shanghai（+08:00）。本轮将管理控制台适配为用户提供的蓝发鲸鱼女仆角色风格：深海靛蓝、珍珠白、浅海蓝与少量金色，配合 Q 版角色、鲸鱼品牌标记、缎带细节和统一圆润 SVG 图标。登录、总览、模型、QQ、对话与历史页面均使用同一主题。视觉不会改写正式助手名称、提示词或已保存配置。

## 固定发布与环境

- 发布代码：`4790b5de771e3df3c02e843c338364313df3e557`，部署前已提交推送，并分别通过本地 `git ls-remote` 和远端 `fetch` / `merge-base --is-ancestor` 核验。
- 前端制品：`admin-4790b5de771e3df3c02e843c338364313df3e557.tar.gz`，SHA256 为 `759cc81b9029658da832068eb5b06a0aa866592e164d95575a17a590c5b57526`。
- 目标：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1 / aarch64，内存总量 7933 MiB，预检可用 6448 MiB。
- 实测系统：Ubuntu 24.04.4 LTS，默认 `graphical.target`，display-manager inactive，ubuntu-desktop / gdm3 / lightdm / xserver-xorg-core 均未安装。本轮沿用已授权的 Ubuntu 验证，不是 Raspberry Pi OS Lite 验收，未重装系统。
- Rust / Cargo 1.88.0，发布构建并发上限 2。本轮 SSH 非交互 PATH 未找到 Node；前端在 Windows 构建并交付静态制品，运行不需要 Node 服务。
- 部署目录 `/home/campanus/apps/dsh` 及 repo 均归 campanus 所有，origin 匹配项目，部署前源码工作区干净。
- 2026-10-24 16:48:43 +08 完成切换，`current` 指向 `releases/4790b5de771e3df3c02e843c338364313df3e557`，源码 HEAD、前端 manifest 和后端 `/health/ready` 返回同一完整 SHA。

## 本地与远端结果

| 检查 | 执行与结果 |
| --- | --- |
| 本地前端 | `npm test`：16/16；`npm run build`：Vue/TypeScript 与生产构建通过；Prettier、`git diff --check` 通过，退出 0 |
| 本地 Chromium | `npx playwright test`：完整用例通过；人工检查登录、总览、配置、对话及移动截图，修复品牌旁多余字符和手机登录正文与角色重叠后复验 |
| 树莓派原生 Rust | 部署脚本执行 `cargo test --workspace --locked -j 2`：4 项 kernel + 10 项 daemon 测试通过；`cargo build --release --locked -p dshd -j 2` 成功，构建 1m40s，均退出 0 |
| 正式服务 | 用户级 `dsh.service` active/running，health ready；新增 brand-mark.svg / favicon.svg / mascot.png 均 200，MIME 分别为 image/svg+xml / image/svg+xml / image/png |
| 管理鉴权 | 携带正确 Host / Origin 的未登录 config、secrets、conversations 请求均为 401；直接以非允许 Host 请求 API 为 403，未绕过来源限制 |
| 远端黑盒 | 已发布源码的 `tools/smoke.py` 使用临时数据库及模拟模型，19/19 通过，退出 0；覆盖 CSRF、密钥不回显、配置生效 / 版本冲突、CLI、任务去重及重启恢复；[脱敏 JSON](validation/character-ui-aarch64.json) |
| 远端制品浏览器 | 同一 release 二进制和静态资源，在独立私有数据目录及 18421 SSH 隧道运行；`DSH_E2E_DATA_DIR=<本地隔离目录> DSH_E2E_BASE_URL=http://127.0.0.1:18421 npx playwright test`，1 passed (22.8s)，退出 0 |
| 浏览器覆盖 | 首次初始化、登录退出、未登录 API 拒绝、配置字段校验 / 保存 / 实际应用、QQ 两种接入方式、历史和对话空状态、全部五页 390px 导航、320px 关键页、登录正文与角色不相交、无横向溢出、图片真实加载及 MIME、无浏览器异常、无持久凭证存储 |
| 正式数据保留 | 对比切换前 SQLite 备份：active_revision 仍为 4，活动配置内容与管理员数量不变，会话和消息数量保留，退出 0 |

远端浏览器验收时间约为 16:48:59～16:50:29 +08。截图保存在本机忽略目录 `.local/browser-e2e/screenshots/`，共 14 张，包括桌面初始化 / 登录 / 五页与手机页面。临时测试进程与 18421 隧道已经关闭，关闭后正式服务仍 active/ready；原 18419 管理隧道保留。正式服务内存单次 systemd 采样约 4 MiB，仅为空闲状态观测，不代表压力测试。

## 命令与证据位置

本地打包：`./deploy/package-web.ps1 -Commit 4790b5de771e3df3c02e843c338364313df3e557`，退出 0。制品以 scp 上传至用户私有缓存；部署脚本从远端已获取的同一提交通过 `git show <SHA>:deploy/deploy.sh` 提取，执行：

```sh
DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy.sh \
  4790b5de771e3df3c02e843c338364313df3e557 \
  /home/campanus/.cache/dsh/admin-4790b5de771e3df3c02e843c338364313df3e557.tar.gz \
  759cc81b9029658da832068eb5b06a0aa866592e164d95575a17a590c5b57526

python3 /home/campanus/apps/dsh/current/source/tools/smoke.py \
  --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --report /home/campanus/apps/dsh/shared/logs/character-ui-smoke-4790b5d.json
```

部署及黑盒命令均退出 0。远端日志位于 `/home/campanus/apps/dsh/shared/logs/`：

- `deploy-character-ui-4790b5de771e3df3c02e843c338364313df3e557.log`
- `test-4790b5de771e3df3c02e843c338364313df3e557.log`
- `build-4790b5de771e3df3c02e843c338364313df3e557.log`
- `character-ui-predeploy.json`、`character-ui-data-check.json`、`character-ui-smoke-4790b5d.json`

上一可用 release 为 `17b7ee061c4ccb724352c7d4c837975b25f34c4c`，已保留。切换前用 SQLite backup API 保存 `/home/campanus/apps/dsh/shared/backups/before-character-ui-20261024T084619Z.db`，权限 0600；真实数据及密钥仍在独立 shared 目录。本轮未改 schema、未执行回滚或数据恢复；必要时按 `deploy/rollback.sh` 切回已保留制品，数据恢复须独立处理。

首次自动 SSH 认证失败后，经用户明确授权配置本机专用 Ed25519 密钥：私钥只保存在 `C:\Users\AIIT\.ssh\dsh_rpi_ed25519`，ACL 仅允许当前 Windows 用户；公钥追加至目标账户 authorized_keys，权限 0600，保留已有条目。本机 SSH config 已为该目标指定密钥，`ssh -o BatchMode=yes campanus@192.168.189.191 true` 验证通过，并在 AGENTS.md 中声明。密码、私钥和临时浏览器测试凭证均未进入 Git 或报告。

管理入口仍为 [http://127.0.0.1:18419/admin/](http://127.0.0.1:18419/admin/)，需要原 SSH 隧道和独立管理账户。本轮未重复真实模型 / QQ 外部联调、最终 Lite 验证、压力测试或故障注入回滚；隔离模拟测试不代表这些项目通过。角色素材路径、来源及内建 imagegen 提示词见 [前端说明](../web/admin/README.md#角色视觉)。
