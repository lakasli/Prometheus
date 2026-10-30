# Windows 本地开发、Git 上传与部署测试流程

用户于 **2026-10-30** 确认正式运行与部署目标改为 **Windows**，不再部署到树莓派。当前流程为：**本地开发 → 必要检查 → Git 提交推送 → Windows 固定 SHA 构建与部署 → Windows 运行测试 → 记录**。仓库级约束见 [AGENTS.md](../AGENTS.md)。仅修改文档或规则时只做静态检查、提交和推送，不启动或重启应用。

## 1. 当前目标与实际状态

| 项目 | 当前约定 / 已观察状态 |
| --- | --- |
| 默认工作目录 | `D:\DSH`，其他电脑以实际仓库为准 |
| Git 远端 | `http://8.136.201.30:8418/Campanus/DSH.git`，仅作为代码托管地址 |
| 正式运行目标 | Windows 原生进程；当前电脑为首个适配与验证目标，无默认远端部署主机 |
| 本次环境采样 | 2026-10-30：Windows 11 专业版，版本 `10.0.26200`，64 位；其他版本 / 架构需另验 |
| Rust | 仓库 `rust-toolchain.toml` 固定 1.88.0；当前编译目标为 `x86_64-pc-windows-msvc` |
| 构建依赖 | MSVC C/C++ Build Tools、Windows SDK、Node.js / npm；Python 验收脚本另需实际可用的解释器 |
| 开发数据 | `.local/dev`，受 Git 忽略；启动前核验 NTFS ACL，不复用历史生产数据库做冒烟测试 |
| 启动方式 | PowerShell 前台运行 `dshd.exe serve`；Windows 服务、自启动及自动发布 / 回滚尚未实现 |
| 默认管理入口 | `http://127.0.0.1:8419/admin/`，Rust 同源提供静态页面与 API |
| 本次前端结果 | `npm.cmd ci --prefix web/admin` 与 `npm.cmd run build --prefix web/admin` 成功 |
| 本次后端结果 | `cargo build -p dshd --locked` 失败：执行 `wasm-encoder v0.228.0` 的构建程序时被 Windows 应用程序控制策略阻止，错误 4551；未成功启动服务 |
| 首次尝试源码 | `79c561e798b5e44e92cb88491336e56b1757d2ab`；之后已同步 Git 远端新实现，本轮仅文档静态检查，未重新构建或启动新源码 |
| 平台迁移边界 | 旧 Linux worker 隔离、Piper 运行依赖与自动部署方式均未完成 Windows 适配 / 验收 |

上述采样与首次尝试不是持续健康状态，也不是已发布的 Windows 版本。目标变更不会自动修复本机构建阻塞。由系统管理者核实可信构建程序的执行许可；Agent 不自动关闭、绕过或降低系统安全策略，不用切回树莓派替代 Windows 验收。

## 2. 开发前检查与本地启动

开始前核对分支、origin 和未提交变更，保留用户内容，不做强制重置或无关清理。检查实际 Windows 版本、CPU 架构、可用内存、磁盘、工具链和原生依赖；不再固定套用 Pi 5 / 8GB / aarch64 的配置。

在 PowerShell 中逐条执行；上一步成功后再继续：

```powershell
Set-Location D:\DSH
git status --short --branch
git remote -v
rustc --version
cargo --version
node --version
npm.cmd --version
npm.cmd ci --prefix web/admin
npm.cmd run build --prefix web/admin
cargo build -p dshd --locked
```

先完成编译再生成 15 分钟有效的初始化凭证，避免首次构建耗时使其失效。首次创建管理账户前执行：

```powershell
& .\target\debug\dshd.exe bootstrap-token --data-dir .local/dev
```

从本机私有的 `.local/dev/bootstrap-token.json` 读取一次性凭证，在浏览器创建管理账户；已有账户直接登录。不要把凭证值输出到日志、聊天、截图或 Git。Windows 管理账户与 DSH Web 管理账户相互独立，没有默认管理口令。

录入真实凭据前，核验数据、secret store、凭证文件与备份的 NTFS ACL，只允许授权运行账户及必要的系统管理主体访问。现有代码中的 Unix `0600` / `0700` 设置不表示 Windows ACL 已配置。

启动开发服务：

```powershell
& .\target\debug\dshd.exe serve --data-dir .local/dev --listen 127.0.0.1:8419 --origin http://127.0.0.1:8419 --web-dir web/admin/dist
```

保持前台 PowerShell 运行，访问 `http://127.0.0.1:8419/admin/`；使用 `Ctrl+C` 请求正常退出。不要另启一个共用该数据目录的 daemon。生产页面由 Rust 提供，不使用常驻 Vite dev / preview 服务。

## 3. 按改动执行必要检查

- 文档 / 规则：检查差异、Markdown 链接、代码块与命令的一致性，不为此重新部署程序。
- Rust：执行适用的格式、编译与测试；区分 Windows 实际通过、平台专用跳过和失败，不以 Linux 结果填充 Windows 证据。
- 前端：执行相关测试、类型检查与静态构建，前端资源和管理 API 属于同一版本。
- 渠道 / 模型 / 媒体：在隔离数据中先做相关模拟检查，再按授权范围做真实集成；模拟平台与真实客户端结果分别记录。
- 平台适配：检查路径、文件 ACL、原生库、进程树取消 / 退出、并发与资源占用。依赖 Linux 的功能在 Windows 适配通过前保持不可用。

可用检查命令按本次范围选择，不要求文档变更运行全套测试：

```powershell
cargo fmt --all -- --check
cargo test --workspace --locked
npm.cmd test --prefix web/admin
npm.cmd run build --prefix web/admin
python -S tools/smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/assistant-logs-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/memory-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/summary-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/task-triggers-smoke.py --binary target/debug/dshd.exe --web-dir web/admin/dist
python -S tools/tests/test_deploy_compatibility.py
```

Python 命令须指向实际安装的解释器，不能把 Windows 应用执行别名的存在当成解释器可用。上述检查使用临时私有数据库 / 合成资料；不能代替真实模型、渠道或尚未适配的隔离工作器验收。

## 4. Git 交付与固定版本发布

只暂存本次相关文件，遵守当前提交策略，不绕过 Hook、不强推、不重写历史。正式部署前记录完整 `git rev-parse HEAD`，成功推送并核实目标 Git 分支包含该提交。Git 服务器保持原地址；它不要求同时部署到旧树莓派。

开发启动可使用工作区版本，但须注明 `development`。正式发布须从干净、已提交且已推送的完整 SHA 构建，设置后端 `DSH_BUILD_SHA` 和前端 `VITE_BUILD_SHA`，分别构建 `cargo build -p dshd --release --locked` 与前端静态资源；完成后恢复本次修改的进程环境变量。前端 [打包脚本](../deploy/package-web.ps1) 可为当前已提交 SHA 构建压缩包与 SHA-256，它不负责部署 Windows 后端。

Windows 自动发布脚本尚未实现。实际发布前必须补齐并核验以下步骤，不能把建议布局当成已经安装的服务：

1. 核实目标目录归属、已有内容、运行账户、数据 ACL 和启动参数，记录上一可用 Windows SHA、配置版本与备份；首次没有已验证版本则明确标注。
2. 在独立版本目录放置同一 SHA 的 `dshd.exe`、前端静态资源、发布能力清单及所需原生依赖，记录制品摘要。
3. 真实配置、SQLite、附件、记忆、日志和模型运行库使用独立持久目录；发布不能覆盖或重建用户数据。
4. 变更数据库语义前做 SQLite 一致性备份，校验目标 release 的 reader / capability 门禁。旧 Linux 部署脚本中的检查不能因迁移 Windows 而被省略。
5. 正常停止已核实的本项目进程，按确认的 Windows 参数启动目标 `.exe`；不依靠修改目录指针就声称进程已切换版本。
6. 验证健康接口的完整 SHA、前端 build ID、管理认证及相关功能；成功才记为可用版本，失败保留诊断并按兼容恢复路径处理。

建议正式目录（规划，尚未创建）如下，使用前仍须核实实际位置：

```text
D:\DSH-runtime\
├── releases\<完整 commit SHA>\  # dshd.exe、静态 web、能力清单与依赖
└── shared\
    ├── config\                # 受限真实配置与凭证引用
    ├── data\                  # SQLite、记忆和附件
    └── logs\                  # 私有运行与验收日志
```

Windows 服务 / 自启动的注册、运行账户与恢复策略须以后续实际实现和测试为准。当前不虚构服务名称，也不把旧 systemd unit 直接转作 Windows 服务。

## 5. 健康检查、浏览器与交付证据

在另一个 PowerShell 中验证：

```powershell
Invoke-RestMethod http://127.0.0.1:8419/health/live
Invoke-RestMethod http://127.0.0.1:8419/health/ready
```

开发构建可能报告 `commit=development`，不能据此声称正式固定 SHA 发布完成。正式发布核对完整 SHA 后，再验证 `/admin/`、首次设置 / 登录、未登录管理 API 被拒、相关配置生效与密钥不回显。页面变更补充浏览器检查；渠道、记忆、插件或媒体变更补充相应集成。健康通过不代表全部功能可用。

本机直接访问管理地址，无需 SSH。其他设备访问使用受控 HTTPS 反向代理，保持前后端同源、后端 loopback，配置精确 `--origin https://<管理域名>` 和 `--secure-cookie`；管理与公开 Webhook 分别控制，不开放普通局域网 HTTP 管理端口。

运行行为交付记录至少包含：

```text
目标：<实际 Windows 主机 / 系统版本 / CPU 架构>
提交：<完整 commit SHA；开发启动注明 development>
时间：<带时区的部署 / 测试时间>
制品 / 数据 / 日志目录：<实际绝对路径>
构建：<前端与后端命令、退出状态、SHA / 摘要>
启动：<实际启动参数、进程与运行版本，不含秘密>
测试：<健康 / 浏览器 / 功能结果，失败与未执行项>
平台边界：<尚未适配或被门禁关闭的功能>
回滚：<上一兼容 Windows 版本、备份位置、是否执行及复验结果>
```

构建、进程启动、健康通过和功能验收分开记录；出现策略拦截或测试失败时保留诊断，不能记成通过。按实际 Windows 可用资源压测，保留既有并发上限与空闲回收，不为历史会话常驻工作器。

## 6. Windows 适配待办与历史 Linux 边界

| 范围 | 当前边界 / 后续验证 |
| --- | --- |
| Rust 与静态管理页 | 前端本机构建成功；后端构建被策略阻止，启动、认证和相关功能待 Windows 实测 |
| WAT / Wasmi 原型 | 现有 systemd / cgroup 门禁仅适用 Linux；Windows 执行保持不可用，且无管理页入口 |
| Component 插件工厂 | 现有 systemd / Landlock / seccomp 隔离仅适用 Linux；Windows 工作器隔离、构建、测试、审批与调用须重新验收，不开放无隔离执行 |
| 完整 Harness / Python 动作 | 原权限桥与安全边界继续保留为目标，不能通过切换平台自动启用 |
| Piper TTS | 旧安装脚本及 lock 仅用于 Linux aarch64 / Python 3.12；Windows 解释器、依赖、路径、资源限制、取消和真实合成待适配，未通过时保留文字降级 |
| 日程 / 日记 / 关系 / 睡眠 | 同步保留既有实现；须在 Windows 验证权限、调度、重启与恢复。`rest_clock` 非 Linux 仅使用进程级 epoch，重启连续性不明时提前唤醒，不计为 Linux 原期限恢复通过 |
| 正式发布 / 回滚 | Windows 自动脚本与服务管理待实现；数据库 reader / capability、备份与恢复要求继续保留 |

原树莓派 `campanus@192.168.189.191`、`/home/campanus/apps/dsh`、Ubuntu / aarch64、SSH 隧道和 rsproxy 单并发流程只属于历史环境，不再作为默认目标或自动执行步骤。`deploy/deploy.sh`、`deploy/rollback.sh`、`deploy/dsh.service.in`、`deploy/install-piper.sh` 与 `deploy/piper-aarch64-py312.lock` 保留原实现，历史报告与 `docs/validation/` 不改写为 Windows 结果。原部署文档可从 Git 历史查询；独立验收记录保留实际命令与证据。

后续历史应用版本为 `25ff608fba7e7a137f86ce0f31fa84382e4ec815`，门禁工具单独固定 `8efb4b10f8e3593361ad28ddaa0a82ee0f0b645e`，见 [通用安排与图片验收](general-plans-images-validation.md)。2026-10-27 的 `https://192.168.189.191:8086/admin/`、Caddy、专用 CA 和配置恢复见 [历史 HTTPS 验收](lan-https-validation.md)；这些不是当前 Windows 的地址、证书或部署步骤，不自动迁移或复用。

目标变更本身不授权连接、停止或清理旧主机，也不自动迁移历史数据库、附件或凭据。确需迁移数据时另行确认范围、做一致性备份、验证恢复与 Windows 文件权限。代码回滚与数据恢复分别处理；不能通过切换旧二进制、删除新表或降低 reader 标记来绕过兼容门禁。
