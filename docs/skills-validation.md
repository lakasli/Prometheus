# Skill 与角色卡交付验收

日期：2026-10-25，Asia/Shanghai（+08:00）。本记录覆盖 Skill 管理、两个默认 Skill、对话执行与绘画，以及角色卡双栏和草稿介绍同步。

运行交付提交：`1920ac9a17175406958282de844a254be01b8677`，已推送并核实 origin/main 包含该提交。本文后续记录提交只更新文档，不改变正式服务的运行 SHA。

本地前端归档 SHA-256：`4d667d7cda383a82035946e4139ef477ca61b8046d3b12af289cdb5952cb649d`。Linux aarch64 二进制 SHA-256：`178c03f754c410be2c65dccc708ae0f5bdfc88d0507c16465468ac9c617b26a9`。

| 本地检查 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo build --locked -p dshd` | 退出 0；kernel 4 项、daemon 181 项通过 |
| `npm test`、`npm run build` | 退出 0；前端 120 项通过，静态资源构建成功；最终归档通过 `deploy/package-web.ps1 -Commit <SHA>` 重新 `npm ci` / 构建 |
| `python -S -m unittest discover -s tools/tests` | 退出 0，23 项通过，含旧版本回退兼容性门禁 |
| `tools/skills-smoke.py` | 实际独立进程，20 项通过，退出 0 |
| Playwright `characters.spec.ts skills.spec.ts` | 2 项通过，8.2 秒；桌面及 390 / 320 px 检查；已查看真实截图 |

本地初轮新增 worker 测试曾因合成凭据编号不是 UUID 失败，修正测试后完整复验通过；图片改为鉴权接口后黑盒的 JSON 助手误解码 PNG，已改为二进制读取并复验。Windows 构建曾被本轮运行中的隔离测试进程锁住可执行文件，核对并停止精确测试 PID 后重建成功。这些失败未记为通过。

部署前复核：目标为 campanus@192.168.189.191，Raspberry Pi 5 Model B Rev 1.1、Ubuntu 24.04.4 LTS、aarch64，内存总量 7933 MiB，采样可用 6739 MiB，无 swap；Cargo / Rust 1.88.0。上一可用 release 为 `595288f9b181fb156056666a8e63a3e855e9f05c`，正式服务 active / ready。远端 Git origin 符合项目配置，工作区无修改。

## 远端部署与验证

2026-10-25 **17:07:39 +08:00** 完成切换。部署命令由已推送 SHA 提取 `deploy/deploy.sh`，使用 `DSH_CARGO_MIRROR=rsproxy bash <脚本> <完整SHA> <前端归档> <上述归档摘要>`，整体退出 0。脚本在目标机以 `-j 2` 运行测试与 release 构建；Rust 测试 71.37 秒，release 构建 3 分 01 秒。构建期间旧服务继续在线。切换时首次探测曾遇到监听尚未就绪，后续完整 SHA 健康检查成功。

源码 `/home/campanus/apps/dsh/releases/<完整SHA>/source`、`current`、前端 `release.json` 和 `/health/ready` 均对应上述提交；`systemctl --user status dsh.service` 为 active / running。配置 revision 仍为 4；管理员、会话、消息、记忆、密钥、角色卡数量与部署前一致，新增两个默认 Skill。身份、时间、备份、计数与摘要见 [部署核对](validation/skills-deployment-aarch64.json)。

| 远端检查 | 结果与边界 |
| --- | --- |
| `cargo test --workspace --locked -j 2` / `cargo build --release --locked -p dshd -j 2` | 185 项 Rust 测试通过，release 成功，退出 0 |
| `tools/skills-smoke.py --binary <current>/bin/dshd --web-dir <current>/web --report <日志>` | 20 项通过，退出 0；真实 daemon + 回环模拟 provider；涵盖认证 / CSRF、版本冲突、冻结与幂等、默认项重启不复活、HTML、PNG 解码与鉴权、失败路径、不把生图字节带入后续上下文。见 [报告](validation/skills-aarch64.json) |
| 同参数 `tools/smoke.py` / `tools/character-smoke.py` | 19 / 55 项通过，退出 0；配置校验、应用、冲突、密钥不回显、发布与绑定、导入和重启回归。见 [基础管理](validation/skills-m1-aarch64.json)、[角色卡](validation/skills-character-aarch64.json) |
| `python3 -S -m unittest discover -s tools/tests` | 23 项通过，退出 0 |
| `tools/skills-live.py --binary <current>/bin/dshd --web-dir <current>/web --source-data-dir <shared>/data --report <日志>` | 9 项通过，退出 0；只读既有模型配置与凭据，在临时独立实例执行 3 次真实模型请求：制作 Skill → 显式审核保存、启用并使用 → HTML 绘画；验证重启与不产生记忆 / 摘要 / 渠道任务。见 [真实文本模型](validation/skills-live-aarch64.json) |
| 同 release 二进制与静态资源的独立实例，SSH 18421 隧道，Playwright 两个 spec | 2 项通过，13.3 秒，退出 0；实际远端 API 和模拟模型。角色卡选中左移、完整详情、发布后草稿改名与介绍、Skill 新建和审核保存、HTML iframe 实际 SVG、PNG 鉴权图片解码、390 / 320 px 无横向溢出，浏览器无未捕获异常或外部资源请求 |

远端浏览器截图已查看；本地忽略目录为 `.local/character-e2e/screenshots/` 和 `.local/skills-e2e/screenshots/`，私有连接资料在 `.local/skills-remote/`。真实模型 HTML 做了自包含内容与安全静态检查，其产出没有单独送入浏览器；浏览器渲染使用同契约的合成 HTML，二者未混记。

当前正式实例保持既有文本模型，原生生图开关未修改。原生 PNG 路径通过实际 HTTP 模拟和浏览器验证，**没有执行真实外部生图服务调用**。QQ / 微信的真实 Skill 平台收发和图片媒体发送未验收，不称为全部多模态或插件工厂完成。绘画开关是管理员核对服务商能力后的配置声明，不是自动能力探测；真实生图失败会明确显示失败。

日志根目录为 `/home/campanus/apps/dsh/shared/logs/`：`deploy-<SHA>.log`、`test-<SHA>.log`、`build-<SHA>.log`，以及 `skills-{aarch64,live-aarch64,m1-aarch64,character-aarch64,deployment-aarch64}-1920ac9.json`。首次切换前 SQLite 备份为 `shared/backups/pre-<SHA>-20261025T090738064875530.sqlite`；上一 release 保留。本轮未执行回滚或数据恢复，不宣称回滚试验通过。已使用的 Skill / 生图配置不能静默降级到不支持的旧读取器，须通过兼容性门禁规划恢复。

远端隔离浏览器实例及专用 18421 隧道在验收后按精确进程关闭；原有 18419 管理隧道保留。正式服务再次核对 active / ready。
