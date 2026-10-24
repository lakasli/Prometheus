# Harness 实机隔离可行性验证

2026-10-24 在获准使用的 Ubuntu 树莓派上执行。**当前 M3 未启用，完整 Harness 权限桥未交付。** 本次只验证操作系统边界和受限资源探针，不运行模型、不启用账号、不启动 Harness，也不修改或重启生产程序。

## 环境和范围

- 主机：`campanus@192.168.189.191`，Linux aarch64，Ubuntu 24.04.4 LTS，kernel `6.8.0-1064-raspi`，systemd `255 (255.4-1ubuntu8.17)`。这是 Ubuntu 验证，不是最终 Raspberry Pi OS Lite 验收。
- 实际 Node：`/home/campanus/.local/node/bin/node`，`v24.22.1`。用户默认 PATH 未包含该目录，不能据此判断 Node 未安装。已发现用户目录下的 Harness 包；本次没有运行它。
- 以普通用户调用现有用户级 systemd 管理器；不使用 sudo，不修改 sysctl、AppArmor、系统服务或安全策略，不安装软件。
- [脱敏 JSON](validation/harness-isolation-aarch64.json) 保留时间、命令模板、退出码和实际边界结果。临时路径、随机单元名、PID、命名空间编号、canary 正文/摘要及原始日志不入库。

## 实测结果

| 检查 | 实际结果 | 含义 |
| --- | --- | --- |
| `PrivateUsers=yes` | user namespace 改变；受控同 UID 宿主进程的 environ 和 FD 返回 EACCES；宿主对相同合成进程的基线读取成功 | 这部分生效，但不代表文件、网络和 PID 已隔离 |
| `ProtectHome=yes`、`ProtectSystem=strict` | mount namespace 未改变；合成 home canary 和未绑定 sibling 可读；`/`、`/etc` 未标记只读；普通用户自有 `/var/tmp` 目录仍可写 | 没有形成所需文件系统边界；没有尝试写真实 `/etc` 或生产数据 |
| `PrivateNetwork=yes` | network namespace 未改变；接口并非仅 loopback；可连接受控宿主 TCP loopback、路径 Unix socket 和 abstract Unix socket | 网络/IPC 边界未建立。没有对公网发出请求，也不声称公网已被阻断 |
| PID/proc | PID namespace 未改变；在已建立的 user namespace 内尝试 PID、mount+PID、nested user+PID 三条 `unshare` 路径均退出 1 / EPERM；不带 `NoNewPrivileges` 的独立单元复查仍失败 | 无法在当前授权约束下建立独立 PID/proc；不可用单个 `/proc` 读取被拒绝来代替完整 PID 隔离 |
| 固定只读 bind | 加入 `BindReadOnlyPaths`、固定 workspace bind、`ProtectProc=ptraceable`、`ProcSubset=pid` 后退出 `226/NAMESPACE` | 连固定 bind 的候选配置也无法启动；因此没有将“仅固定 bind 可读”记为通过 |
| `TasksMax=32` | 显式 opt-in 时 31 个受控线程加主线程达到 `pids.current=32`；`pids.events` 的 `max=1` | cgroup 进程/线程限制实际生效 |
| `MemoryMax=64M`、`MemorySwapMax=0` | 独立临时单元触页分配触发 OOM；`memory.events` 的 `oom_kill=1`、`memory.peak=67108864`，`SERVICE_RESULT=oom-kill`、`EXIT_STATUS=KILL` | 内存限制实际生效；只杀死探针单元，不是应用压测 |

基础单元的 `systemd-run` 返回 0，但日志明确提示 `PrivateNetwork` 缺少权限并继续执行；加强配置记录 mount namespacing 不受支持和 `226/NAMESPACE`。**属性显示为 yes、命令成功以及资源限额有效，都不能证明 M3 安全边界成立。** 当前不降低权限要求、不降级为仅 prompt 限制、不开放原生 bash/file/plugin 入口。

## 可重复探针

源码为 [tools/m3/sandbox-probe.py](../tools/m3/sandbox-probe.py)，只依赖 Python 标准库和主机已安装的 systemd/unshare。默认模式检查隔离，明确跳过线程压力与 OOM；资源试验必须加 `--resource-tests`。

```sh
python3 tools/m3/sandbox-probe.py
python3 tools/m3/sandbox-probe.py --resource-tests
```

工具退出 0 表示探针完成并清理成功，**不是隔离验收通过**。应检查 JSON 中的 `m3_enabled`、各边界和每个子单元退出码。工具始终不自动启用 M3。默认和 opt-in 两种模式均已在上述主机实际执行，分别于 `2026-10-24T11:20:02Z`、`11:20:04Z` 完成，工具退出均为 0；资源单元预期的 `systemd-run=1` 保留在子结果内。

探针安全边界：

- 拒绝 root 执行。只在当前用户拥有且非符号链接的 `~/.cache` 和 `/var/tmp` 内，通过 `mkdtemp` 创建自己的 0700 目录；不接受调用者指定的删除目录或写入报告路径，报告只写 stdout。
- 只读写自己的合成文件，另只读取必要系统元数据和受控进程的 `/proc` 项。canary 使用随机合成数据，只报告可见性/一致性，不输出正文或摘要。不会读取真实进程环境或 FD。
- 只创建唯一随机名称的 transient unit，使用 `--collect`、运行超时、control-group 停止。结束时只停止自己的确切单元名，检查非活动状态后才删除 scratch；不使用单元通配符清理。
- 删除前检查绝对路径、父目录、前缀、目录所有者、根目录不是符号链接，并要求 `shutil.rmtree` 具备抵抗符号链接替换的实现。若单元无法停止，不删除其依赖目录。
- TCP 只访问自己新开的 loopback 监听端口；Unix socket 也完全受控，无模型/微信/公网请求。独立 Node 只执行 `--version`，不启动 Harness。
- 默认每单元已有 96 MiB / 32 tasks / 25 秒上限。opt-in 线程试验最多 64 个受控线程；内存试验使用独立 64 MiB、swap 0、16 tasks、20 秒单元，若限额失效最多触页分配 128 MiB 后退出 42。不会无限分配或自动重试。

需在远端运行仓库外副本时，先在本地完成源码检查，再上传到自己 `mktemp` 创建的私有缓存。以下 PowerShell 示例验证远端返回的临时路径再使用；输出可重定向到本地忽略目录：

```powershell
$probeCache = (ssh -o BatchMode=yes campanus@192.168.189.191 'umask 077; mktemp -d /home/campanus/.cache/dsh-m3-upload-XXXXXXXX').Trim()
if ($probeCache -notmatch '^/home/campanus/\.cache/dsh-m3-upload-[A-Za-z0-9]{8}$') { throw 'unexpected probe cache' }
scp tools/m3/sandbox-probe.py "campanus@192.168.189.191:$probeCache/sandbox-probe.py"
ssh -o BatchMode=yes campanus@192.168.189.191 "python3 $probeCache/sandbox-probe.py"
# 仅在明确需要验证资源限制时：
ssh -o BatchMode=yes campanus@192.168.189.191 "python3 $probeCache/sandbox-probe.py --resource-tests"
```

本次探针创建的内部 scratch、canary、受控进程/socket 均已清理，确切 transient unit 均停止。上传源码的外层私有缓存不由工具擅自删除；它不含生产数据。公开结果不包含该缓存的随机路径。

## 后续交付限制

在当前无 sudo、不可调整主机安全策略的约束下，没有可实施且已经证明成立的完整沙箱方案。资源受限的禁工具 M0 探针仍仅是实验，不能作为 M3 权限桥上线依据。

后续必须先获得并独立验收实际可用的文件系统、网络/IPC、PID/proc 边界，例如经过审核的专用隔离启动设施；本次没有安装或配置该设施。即使主机隔离之后可用，仍需完成工具 namespace/能力校验、原生入口关闭、可信调用关联、单次授权、取消/回收以及角色和记忆投影的运行验证，才可交付 M3。纯探针与文档提交不改变生产运行行为，无需为此重启应用。
