# 本地开发、Git 上传与远端部署测试流程

本项目统一采用：**本地开发 → Git 提交和推送 → SSH 远端部署 → Linux 测试**。仓库级约束见 [AGENTS.md](../AGENTS.md)。

## 环境与目标

| 项目 | 约定 / 已验证状态 |
| --- | --- |
| 本地仓库 | `D:\CodeProject\DSH` |
| Git 远端 | `http://8.136.201.30:8418/Campanus/DSH.git` |
| 目标硬件 | 树莓派 5，8GB 内存，aarch64；用户已确定的部署基线 |
| 正式目标系统 | `192.168.189.191`现有Ubuntu 24.04.4 LTS / aarch64；用户2026-10-24明确确认，不再以Lite迁移为目标 |
| 部署与测试主机 | `192.168.189.191` |
| SSH 用户 | `campanus` |
| SSH 端口 | `22` |
| 登录命令 | `ssh campanus@192.168.189.191` |
| 当前实测平台 | 2026-10-24：Raspberry Pi 5 Model B Rev 1.1，aarch64，约7.7 GiB内存，Ubuntu 24.04.4 LTS |
| 系统决策 | 现有Ubuntu已成为正式基线，取代旧Lite最终目标；不重装、不为旧要求移除桌面或改变启动目标。历史曾记录graphical.target，该事实不作为本次变更项 |
| 已验证用户目录 | `/home/campanus` |
| Git | 当前 SSH 会话中可用，路径 `/usr/bin/git` |
| Rust 工具链 | 本轮用户级安装 Rust / Cargo 1.88.0，路径 `/home/campanus/.cargo/bin` |
| Node 工具链 | 本轮实测 Node 24.22.1、npm 11.19.1；M1静态前端运行不依赖Node服务 |
| 当前应用部署状态 | 图片对话与导航调整已部署到`bd914f8ea73ffad5697248ddf3df716f7208299a`，见 [本轮记录](conversation-images-validation.md)；[M0/M1记录](m0-m1-validation.md)保留为历史结果 |
| Web 管理方式 | 用户浏览器访问 Rust 提供的静态管理页面；经 SSH 隧道或 HTTPS 代理连接 |

SSH 密码使用用户已提供的会话凭证，不在本文件保存。后续可使用本地凭证管理器或 SSH key；不得把真实值写入脚本或提交记录。

本次只读SSH复核确认机型为Raspberry Pi 5 Model B Rev 1.1、系统Ubuntu 24.04.4 LTS、架构aarch64；MemTotal为8,318,562,304字节，采样时MemAvailable为6,726,705,152字节，swap为0。可用内存是2026-10-24本次采样值，不是固定应用配额；每次部署仍需重新核验。历史验收报告 / JSON不追改，其中“Ubuntu临时例外 / Lite最终目标”由本次用户决定取代。

## 1. 本地编写与检查

开始前核对 `git status`、分支和 origin，更新对远端提交的了解；存在用户修改时保留其内容，不做强制重置。

所有源代码修改、测试修复、依赖更新和部署脚本编写在本地完成。检查内容按实际文件确定：

- Markdown：检查链接、代码块与配置示例。
- Rust：执行当前Cargo workspace适用的格式、编译与测试命令。
- 部署脚本：检查平台、路径、参数、退出码和秘密配置处理。
- 依赖 Linux / aarch64 的行为：在远端执行最终验证，不以 Windows 上的静态检查代替。
- 前端：本地或CI构建 `web/admin` 静态资源并执行相关检查，和Rust API / 制品使用同一发布版本；不将前端开发服务器作为生产服务。

当前 M1 可执行命令见 README。测试区分 Rust 集成、浏览器、模拟 provider 和真实外部账号，不互相替代；对尚未执行的检查明确报告。

功能和界面变更须对应 `docs/architecture.md` 中的目标与信息架构。阶段验证原型归“任务与诊断 → 开发诊断”默认折叠区域，不新增产品主导航，不以诊断工具冒充普通对话、多模态或正式插件能力；重大架构偏离在实现前说明影响并由用户确认，不能事后改架构替实现背书。

## 2. Git 提交与上传

检查差异，只暂存本次相关文件；提交后推送到本次工作对应的远端分支，不默认覆盖其他分支。记录完整的 `git rev-parse HEAD` 结果，并确认远端分支包含该提交。

一次部署关联一个明确 SHA。若测试后需要修复，产生新提交并再次推送，保留前一轮部署和测试记录。

## 3. SSH 远端准备

通过 `ssh campanus@192.168.189.191` 登录，先核验树莓派5机型、8GB内存对应的实际可用量、aarch64及`/etc/os-release`所示现有Ubuntu系统，再检查目录归属、磁盘空间、工具链与运行依赖。若实际系统与记录不同，先记录差异；本流程不授权重装系统、迁移Lite、移除桌面或改变现有启动目标。SSH连通不等于应用部署通过。

本项目部署根目录为 `/home/campanus/apps/dsh`。每次执行部署脚本前核对目录归属及已有内容，遇到未管理的源代码、服务或修改立即停止，不能覆盖。

建议目录布局：

```text
/home/campanus/apps/dsh/
├── repo/                  # 获取已推送提交的 Git 仓库
├── releases/<commit-sha>/ # 对应固定版本的源码或制品
├── current                # 指向当前启用的 release
└── shared/
    ├── config/            # 真实配置与凭证，独立权限
    ├── data/              # 数据库、记忆和附件
    └── logs/              # 日志；也可使用系统日志服务
```

现有主机若已有 DSH 部署，应沿用并记录其实际目录，不能为了符合布局而迁移或覆盖已有数据。远端拉取 Git 时使用其已有授权；不能假设 Windows 本地的 Git 凭证已自动传递到 Linux。

## 4. 按固定版本部署

1. 记录当前可用 SHA、配置版本、服务状态和数据备份位置。
2. 确认远端仓库 origin 与目标项目一致，工作区不存在未保存修改。
3. 获取已经推送的目标提交，在独立 release 中检出该完整 SHA，并核对 `git rev-parse HEAD`。
4. 在现有Ubuntu 24.04.4 LTS / aarch64目标环境构建，或取得经过验证的Linux aarch64制品及前端静态资源；使用锁定依赖，保存构建结果与SHA的关联。树莓派上构建限制并行度，避免与在线服务争抢内存。
5. 将真实配置和数据通过独立路径提供，避免制品更新覆盖它们。
6. 执行实际部署脚本规定的切换与启动，核对进程或健康信息显示的版本。
7. 测试通过后，将其标记为当前可用版本；失败则按回滚步骤恢复。

`current` 的切换与进程重启需要按实际运行方式协调；不能仅修改指针就报告进程已升级。没有部署脚本、服务名称或启动命令时，应先在本地补齐并上传这些内容，不能猜测后直接操作机器上的其他服务。

## 5. 远端测试

对每次涉及运行行为的改动，先跑对应自动化测试，再启动应用并验证健康状态与本次功能。根据改动范围覆盖渠道消息、记忆读写、Harness、插件或图像与语音链路，不必无目的重复所有高成本外部测试。

Web 管理功能还需验证：远程登录、未登录访问被拒、配置保存与冲突处理、连接测试、应用 / 回退状态，以及密钥不回显。运行期间记录内存、worker 数量和队列状态，确认符合树莓派 5 / 8GB 基线。

失败时获取脱敏日志与错误，回本地修复，再执行“提交 → 推送 → 按新 SHA 部署 → 测试”。远端不直接编辑受 Git 管理的源码、配置模板或测试文件。

每轮报告至少包括：

```text
目标：campanus@192.168.189.191
提交：<完整 commit SHA>
时间：<带时区的部署 / 测试时间>
源码 / 制品目录：<实际路径>
构建：<执行命令、退出状态>
部署：<启动方式、实际运行版本>
测试：<执行命令、结果、未执行项>
诊断：<必要日志位置，内容脱敏>
回滚：<上一可用版本、是否执行、复验结果>
```

SSH 连通性检查不等于应用部署测试。当前实际部署命令如下，SHA与制品摘要从本地步骤获取，不能自行填入另一分支的移动头。

```powershell
# 本地代码通过检查并提交推送后：
$deployCommit = (git rev-parse HEAD).Trim()
./deploy/package-web.ps1 -Commit $deployCommit
# 将输出的 .git/artifacts/admin-<SHA>.tar.gz 复制到远端用户私有缓存。
scp .git/artifacts/admin-<SHA>.tar.gz campanus@192.168.189.191:/home/campanus/.cache/dsh/
```

```sh
# 远端 repo 首次不存在时先克隆；已有目录须先核验 origin 和工作区。
mkdir -p /home/campanus/apps/dsh
git clone http://8.136.201.30:8418/Campanus/DSH.git /home/campanus/apps/dsh/repo
# 后续部署先fetch，并从目标SHA检出脚本到私有缓存后执行；不得运行未推送脚本。
git -C /home/campanus/apps/dsh/repo fetch origin
git -C /home/campanus/apps/dsh/repo show <SHA>:deploy/deploy.sh > /home/campanus/.cache/dsh/deploy.sh
bash /home/campanus/.cache/dsh/deploy.sh <SHA> /home/campanus/.cache/dsh/admin-<SHA>.tar.gz <SHA256>
systemctl --user status dsh.service
journalctl --user -u dsh.service --no-pager -n 30
```

部署脚本检出独立 release、在树莓派限并行编译测试、校验前端SHA、切换用户级systemd并核对健康接口中的完整提交。`loginctl enable-linger campanus` 用于退出SSH后继续运行；本轮已开启。失败构建与测试日志保存在 `shared/logs/`；首次部署没有上一应用版本时不能声称已验证跨版本回滚。

官方 crates.io 连接不稳定时，可在部署命令前设置 `DSH_CARGO_MIRROR=rsproxy` 使用 HTTPS 镜像；仍以 `Cargo.lock` 和下载校验锁定依赖，不修改全局 Cargo 配置或源代码。默认不启用镜像。

在私有临时数据库执行黑盒验证：

```sh
python3 /home/campanus/apps/dsh/current/source/tools/smoke.py \
  --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --report /home/campanus/apps/dsh/shared/logs/m1-smoke.json
```

初始化正式管理账户时，在SSH中运行 `current/bin/dshd bootstrap-token --data-dir shared/data`（工作目录为部署根目录），从受限文件复制一次性凭证到Web页面；不要把凭证输出进部署日志或提交到仓库。

### Web 管理访问路径

Rust 服务默认监听 `127.0.0.1:8419`，页面路径 `/admin/`。SSH 隧道方式在用户电脑执行：

```sh
ssh -N -L 18419:127.0.0.1:8419 campanus@192.168.189.191
```

随后在该电脑浏览器打开 `http://127.0.0.1:18419/admin/`，仍需登录 DSH 管理账户。隧道保持运行期间有效；具体上线状态以验证记录为准。

日常通过局域网链接访问时，在树莓派配置 HTTPS 反向代理，提供 `https://<树莓派局域网地址或域名>/admin/` 并代理到 Rust 本地端口；需部署受浏览器信任的证书及正确的代理来源配置。跨互联网访问优先经 VPN 或受控入口，不直接开放未保护的管理端口。页面与 API 使用同一来源，业务 Webhook 的公开策略与管理入口分别配置。

## 6. 回滚与数据保留

部署前准备上一可用 release 和恢复步骤。启动、健康检查或关键功能验证失败时，恢复上一版本及对应配置并复验，不删除失败版本的必要诊断记录。

代码制品可按版本切换，数据库和附件不能随意覆盖。涉及数据迁移时先确定备份和恢复方式；不可逆迁移不能依赖切换旧制品解决。

保留用户数据、记忆、凭证和其他项目内容；清理只针对已确认属于本项目且允许删除的临时构建或旧制品。
