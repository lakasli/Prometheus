# Rust / WIT / Wasmtime 插件工厂实机验收

日期：2026-10-25（Asia/Shanghai）。本记录仅覆盖空权限、无状态 JSON 工具纵切；没有将 WAT / Wasmi 诊断原型作为正式插件，也不表示原架构完整 M5 或 Harness 权限桥完成。契约、范围和操作命令见 [插件工厂](plugin-factory.md)。

## 固定环境与发布

目标 `campanus@192.168.189.191`：Raspberry Pi 5 Model B Rev 1.1、Ubuntu 24.04.4 LTS、aarch64、Rust 1.88.0、约 7,933 MiB RAM、无 swap。现有系统、桌面、用户数据和凭据保持原有位置；前端在 Windows 构建静态包，Rust 在树莓派单并发构建。

正式运行目标为 `615c7587c1c4b8cee5af69b4d13e572ed5534f49`，前端包 SHA-256 为 `c0505d49567507079e4a6616b7863572a5c3bc9c6290f47ab4b83fb76716cf06`。上一可用版本为 `25d973820568d72c7ace88bd8239b15c8ee29f56`。部署通过同 SHA 的 `deploy/deploy.sh`，日志位于目标 `shared/logs/test-<完整SHA>.log` 与 `build-<完整SHA>.log`。

正式发布于 2026-10-25 22:34（+08:00）完成，命令退出 0，`current`、`/health/ready.commit`、发布源码 HEAD 和前端 `release.json` 均为上述 SHA，systemd 服务 active / health ready。正式二进制 SHA-256 为 `316eb2e29d6742ccda5a185342f6fb13adafac1b4880277b6fc451d929bed564`，release 编译耗时 8 分 16 秒。切换前由部署脚本保存兼容性检查及私有 SQLite 备份。

## 失败诊断与逐次修复

| 固定提交 | 观察与处理 |
| --- | --- |
| `6f196e8f8d2513c124e1fbdd59c8ae4eac9af95b` | aarch64 `c_char` 符号性编译错误；没有切换生产。改用 `std::ffi::c_char`。 |
| `387652c94ac4abc887506abe5ea74112e604ce42` | systemd transient unit 初始 loaded / inactive 与 cgroup 创建竞态，隔离检查失败。持续等待实际 cgroup 强制值，未验证前仍不发送输入。可信固定模板编译另发现相对 linker 路径引发 Rust fallback 的 socketpair；改为 sysroot 内绝对 `rust-lld`，没有开放 socket。 |
| `25d973820568d72c7ace88bd8239b15c8ee29f56` | 32 个新工作器的真实隔离检查通过，Rust 收集链接器管道时因 `ioctl(FIONBIO)` 被拒而失败；保留日志。后续仅允许该请求，并在读输入前验证其他 ioctl 为 EPERM。 |
| `871dd57a5a425b4173836c9a353e19c7437793ff` | 首次私有 debug 预验收在记录 27 次探针后遇到 15 秒客户端超时，整轮按失败保留。重复预验收完成 32 次探针及真实 Rust 编译，Component 实例化因 COW 映像的 `F_ADD_SEALS` 被拒。关闭 COW 初始化，采用匿名内存；没有放宽 `fcntl`。工厂请求的客户端等待与宿主 100 秒上限对齐。 |
| `3da8044bca8529a0436c638c9f6d2432735396c5` | 私有 debug 预验收 18 项通过。随后额外收紧 legacy clone，只允许实测 pthread / posix_spawn 两组 flags。 |
| `615c7587c1c4b8cee5af69b4d13e572ed5534f49` | Linux 7 项插件专项测试及私有 debug 18 项完整预验收通过，32 个新工作器均验证策略 v3。正式 Linux workspace 测试 252 项 daemon、4 项 kernel 全通过。正式 release 结果另列。 |

原始失败报告与诊断保留在目标 `shared/logs/plugin-factory-6f196e8.json`、`plugin-factory-387652c.json`、`plugin-factory-25d9738.json`、`plugin-factory-preflight-871dd57.json`、`plugin-factory-preflight-repeat-871dd57.json`。可信合成 fixture 的 strace 及失败私有目录未删除，不包含真实生产秘密或用户正文；不将 debug 二进制或旧前端作为正式发布证据。

## 实际隔离与生命周期

策略为 `landlock4-seccomp-stateless-v3`，执行配置为 `stateless-json-anonymous-memory-v1`，均绑定发布包摘要。工作器先核对实际 cgroup / rlimit，关闭继承 FD，再安装 Landlock / seccomp，最后才读取源码或 Component。每次都验证八项 canary、FIONBIO / 其他 ioctl 边界、clone flags 与 clone3 返回值；仅启动前检查工具链不构成可用证据。

每个工作器约束为 1 GiB cgroup memory、零 swap、32 tasks、60 秒 CPU、90 秒 unit runtime；宿主 100 秒超时，精确 UUID unit 停止和空 cgroup 确认之后才释放容量。调用另有每输入 5,000,000 fuel、64 MiB guest memory、256 KiB wasm stack、15 秒 epoch、16 KiB 输入和 64 KiB 输出上限。空 Linker 没有 WASI 或任何 host imports。

32 次探针均使用新建独立 unit，验证 sibling 秘密 canary / 目录、peer `/proc` environ / FD、越界写、INET / INET6 / UNIX socket、peer 读取 / 信号均被拒，以及 scratch 可写。探针只访问本次合成 sibling 和 env_clear peer，不读取真正私钥、生产凭据或真实进程环境。clone 的安全探针即使没有 seccomp 也只会 EINVAL，不创建子进程或向 peer 发信号；另有完整 64 位变异 BPF 单测。

完整脚本覆盖认证 / Origin / CSRF / owner 边界、生成幂等、真实 Rust 编译、WIT Component 契约、测试不自动批准、摘要 / revision 审批、独立激活、未知输入、重启、第二版本、回滚、隔离下架、燃料 / 内存 / imports / 错误预期拒绝及全部工作器退出。

正式 release 的 [18 项验收报告](validation/plugin-factory-615c758-aarch64.json)全部通过，32 次新工作器均为策略 v3，并完成全部准确退出；`model_is_mock=true`，真实编译与 Component 执行不是模拟。另存 [debug 预验收报告](validation/plugin-factory-preflight-615c758-aarch64.json)，二者二进制摘要不同，不能互换。所有测试插件、批准与路由均位于私有临时数据库，成功后完成精确 unit 清理及目录清理。

正式验收附近的非穷尽 45 次主机采样，最低可用内存为 7,630,209,024 字节；未捕获短生命周期 worker 的峰值，因此本轮不报告其 RSS 或压力吞吐。对应资源报告保留在目标 `shared/logs/plugin-factory-resources-615c758.json`，`worker_peak_measurement_available=false`。cgroup 实际强制限额的证明来自每个工作器自身与 supervisor 的读取和探针，不用空采样充当运行证据。

## 单次真实模型与聊天命令

真实模型验收使用临时私有 daemon，只从生产数据库只读取得模型配置及引用的既有 key；不复制聊天、记忆、用户文件、QQ / 微信配置或生产插件。只发送合成整数求和需求，生成请求预算 1。测试批准由临时合成管理员执行，明确不等同于真人审阅或生产批准。

[单次真实模型报告](validation/plugin-factory-live-615c758-aarch64.json)中 7 项全部通过：生成未批准的 Rust 源码、实际编译及 Component 契约测试、临时合成管理员独立批准 / 启用、未知输入 `[123,-46]` 得到 77、普通 Web 会话显式 `/plugin user.synthetic-sum [123,-46]` 得到 77、隔离下架后调用拒绝、生成请求恰好 1 且没有长期记忆或存活 worker。此报告 `human_approval_tested=false`、`production_plugin_approved=false`，没有生产插件被自动批准，也不把 Web owner 的工具授权给 QQ openid。

当前工具仅在显式 `/plugin` 命令下调用既有已批准版本；结果作为不可信数据交给原会话 / Outbox 路径。模型自主选择工具的循环、网络 / KV / 记忆 / 发送 broker、状态迁移和第三方依赖审查仍未交付。QQ 插件调用须有可信身份授权后另验，不能由 QQ 语音 / 图片 / 视频发送成功推导。

## 页面与检查范围

本地 `cargo check --locked`、格式检查、Python 编译检查通过；6 项跨平台插件测试通过。Linux 增加 clone 参数过滤测试后插件专项共 7 项。工厂前端 3 项测试、typecheck 和生产构建通过，覆盖源码转义、显式摘要审批 / 单独启用及冲突不自动重试。

已在实际认证 Edge 页面 `http://127.0.0.1:18419/admin/#plugins` 刷新并观察 `Web 615c7587c1`、Rust 插件工厂、新建 / 版本列表及清楚的空权限范围；点击“检查构建环境”得到真实 `passed=true`、策略 v3、8 项探针及 clone / ioctl / FD / rlimit 门禁全 true。生产版本列表仍为空，没有通过页面创建、批准或启用生产插件。源码转义与批准复核交互的详细覆盖来自前端测试，未将此只读页面观察描述成真人源码审批。

未执行生产回滚演练或真人源码审批；旧失败、协议模拟、私有 debug 预验收、正式 release、真实模型和客户端观察分别记录，不互相替代。
