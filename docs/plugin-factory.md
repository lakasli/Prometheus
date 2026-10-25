# Rust / WIT Component 插件工厂

本功能推进 [目标架构第 11 节](architecture.md#11-ai-自制功能插件)，与“任务与诊断 → 开发诊断”中的 WAT / Wasmi 原型分别存在。工厂采用真实 Rust 源码、版本化 WIT、WASM Component 和 Wasmtime，不将整数 WAT 原型改名作为交付。部署和实际验收状态由独立验证记录及运行 SHA 确认；本文件本身不构成验收通过。

当前可实现的权限策略为 **空权限、无状态、JSON 输入 / 输出工具**。网络、KV、私人记忆、消息发送、进程和任意文件能力没有向 Component 提供。完整受控 host imports / broker、天气 HTTP 工具、插件状态迁移、第三方包安装、依赖审查流程和模型自主选择工具的循环仍属于后续架构目标；当前纵切不能称为完整 M5。

## 从需求到工具

1. 管理者填写 `user.` 前缀 ID、三段版本、需求、输入 / 输出 JSON Schema 和 2–8 个测试。可以显式提交已有函数体，否则模型只生成 `fn plugin(input: &str) -> String` 的 Rust 函数体。模型不接触生产凭据、聊天历史或记忆，也不能批准、启用自己的代码。
2. 需求和源码保存为当前管理者私有版本。构建、测试、批准、启用分别操作，任何一步失败都不会自动授予调用权限。生成请求通过现有后台 worker 和请求 lease 计入全局并发，HTTP future 被丢弃之后才释放名额。
3. 独立构建器将固定 canonical ABI 模板与函数体组合，直接调用锁定 Rust 工具链；没有 Cargo 解析、第三方依赖、`build.rs`、proc macro 或在线下载。`wasm32-unknown-unknown` 核心模块必须没有 imports，再嵌入 WIT 元数据并封装成 Component。
4. 独立测试在 Wasmtime 中检查真实 `health` / `invoke` 契约和全部测试输出。测试报告绑定源码、Component、发布包与隔离策略。测试通过仅成为 `eligible`，不会自动批准。
5. 管理者审阅源码、契约和报告，通过带当前 `If-Match` 的独立批准动作核对源码 / Component / 发布包 SHA-256；随后再启用。新版本切换保留上一版本；回滚同样校验版本摘要和路由 revision。当前策略没有持久插件状态，回滚不涉及数据恢复。

状态包括 `specified / generating / generated / building / built / testing / eligible / approved / active / quarantined / rolled_back`，另有各阶段失败状态。后台任务有独立 `queued / running / completed / failed / cancelled / interrupted` 和 revision。服务重启将未完成任务标为中断，不虚构编译或投递成功。

## WIT 与制品

固定接口位于 [world.wit](../crates/dshd/plugin-template/world.wit)：

```wit
package dsh:plugin@1.0.0;

world plugin {
  export health: func() -> bool;
  export invoke: func(input: string) -> string;
}
```

`invoke` 的字符串为 JSON；宿主在调用前、返回后分别校验 schema，输出作为不可信数据处理。当前 schema 子集支持明确 `type`、`properties`、`required`、布尔 `additionalProperties`、`items`、`enum` 和说明文字。未实现的关键字明确拒绝，避免假称检查通过。

每个版本绑定需求 / 测试 / Schema、生成函数体 SHA-256、Component SHA-256、构建环境摘要、WIT / 模板摘要、Host API、Wasmtime 版本、隔离策略、空权限集及状态 schema。构建环境记录实际 `rustc --version`、编译器与链接器 SHA-256，不记录用户目录或任何凭据。编译器显式使用固定 sysroot 内绝对 `rust-lld` 路径，避免 Rust 1.88 相对 PATH 启动回退所需的匿名 socketpair；不为此开放 socket / IPC。制品保存在当前实例的私有 SQLite 中；Web 只向其所属管理账户返回源码、摘要及测试结果，不公开 Component 下载或秘密配置。

每个管理者最多 100 个版本、2,000 个工厂任务；函数体最多 64 KiB，规格最多 48 KiB，Component 最多 4 MiB，每次输入最多 16 KiB、输出最多 64 KiB。版本不可覆盖；修改需求或源码应创建新版本。

## 实际隔离门禁

此策略与此前不可用的完整 Harness 沙箱相互独立。它不依靠用户级 `ProtectHome` / `PrivateNetwork` 属性来证明边界，也不开放 Harness 工具。完整 M3 的原有待办维持不变。

编译器和 Component 都在独立、短生命周期、精确 UUID 命名的 `dsh-plugin-<UUID>.service` 中运行，最多一个工厂工作器。启动时核对实际 cgroup 的 `memory.max=1 GiB`、`memory.swap.max=0` 和 `pids.max=32`；服务有 90 秒总运行期限，宿主另有 100 秒超时与精确 control-group 停止、退出确认。

工作器在读取源代码或 Component 字节之前：

- 关闭 3 以上所有继承 FD，清空环境；不传递管理数据库、生产密钥、SSH key、登录目录或 DBus socket。
- 安装 Linux Landlock ABI ≥ 4：只读工具链和系统运行库，唯一可写路径为本次随机私有 scratch；不授予生产目录、`/proc`、`/run`、`/sys` 或全局 `/tmp`。
- 安装架构校验的 seccomp 系统调用允许清单，默认拒绝未列出的未来 syscall。Socket / Unix IPC、进程内存读取、ptrace、peer 信号、pidfd、io_uring、挂载 / namespace、密钥环和文件 metadata 修改等不在清单中。`fcntl` 限描述符操作，`prlimit64` 只允许自身，防止旁路控制同 UID 进程。
- 用本次独立合成 canary 和 `env_clear` 的合成 peer 验证禁止读取 sibling 文件 / 目录、peer 的 environ / FD、越界写入、INET / INET6 / UNIX sockets、peer 的零信号 / 空内存读，并验证 scratch 可写。探针不读取实际生产 secret 或真实用户内容。

`candidate_ready` 只表示内核和工具链可用于尝试校验。每次 daemon 初始化都清除旧 `verified` 状态；只有实际八项探针及精确退出确认通过，`available` 才为真。管理页面允许重新检查环境。构建 / 调用前要求本进程已验证，每次工作器仍重做隔离探针；任何退出无法确认都会关闭可用状态、保留执行名额和对应 ledger，不提前清除目录或开启下一任务。

Component 运行采用空 `Linker`，没有 WASI、socket、环境、文件预打开或 host imports。Wasmtime 另设每调用 5,000,000 fuel、15 秒 epoch 截止、64 MiB guest memory、256 KiB guest stack、单实例 / 单 memory 和输出上限。实际测试覆盖燃料、内存、禁止导入、错误输出与契约拒绝；不能用“编译成功”代替这些检查。

## 管理 API 与聊天调用

前缀为 `/api/admin/v1/plugin-factory`。全部接口要求登录；写操作另由统一鉴权层校验 Origin / CSRF。版本与任务均按服务端 `Admin.id` 过滤。客户端传入另一个 owner、QQ openid 或文档内身份不能扩权。

| 路径 | 操作 |
| --- | --- |
| `/` | GET 本人版本 / 任务 / 路由；POST 新需求或已有函数体 |
| `/check` | POST 真实隔离检查，返回脱敏策略与逐项结果 |
| `/versions/{id}` | GET 本人版本、源码、契约、构建来源与测试 |
| `/versions/{id}/build`、`/test` | POST 独立排队构建或测试；需要 If-Match 和 UUID 请求 ID |
| `/versions/{id}/approve` | POST 显式人工审阅和三个摘要；需要 If-Match |
| `/versions/{id}/activate`、`/rollback` | POST 版本摘要与当前 route revision；需要 If-Match |
| `/versions/{id}/quarantine` | POST 撤销路由并隔离版本；需要 If-Match |
| `/jobs/{id}`、`/jobs/{id}/cancel` | GET 状态或 POST 取消；取消需要 If-Match，实际退出前 lease 不释放 |
| `/plugins/{plugin_id}/invoke` | POST 对当前 owner 已批准且激活的工具执行 JSON 调用 |

聊天工具入口为显式 `/plugin`：列当前聊天身份已启用的工具；`/plugin user.example <JSON>` 才执行对应工具。调用只返回不可信结果，由既有会话和 Outbox 链路记录与发送，插件本身没有消息发送权。管理账户、QQ、微信身份不会自动互相授予私人插件；没有可信身份绑定的渠道会显示该身份没有已启用工具。模型自动选择 / 调用的循环尚未交付。

## 工具链与可重复检查

远端使用现有 Rust 1.88.0，并单独安装 `wasm32-unknown-unknown` target。编译器默认定位该用户的固定 1.88.0 aarch64 工具链；操作系统部署者可使用进程配置 `DSH_PLUGIN_RUSTC` 指定已审查的绝对真实编译器路径，前端不能任意指定或执行系统命令。

Wasmtime 固定 `36.0.19` LTS，兼容现有 Rust，并包含 2026-10-19 的安全修复；不用没有相应修复的旧 33.x。`wit-component / wit-parser` 固定 `0.228.0`，与本项目现有 parser 协调。正式构建使用提交中的 `Cargo.lock`，树莓派限制构建并发，前端仍本地构建后随固定 SHA 发布。

```sh
python3 tools/plugin-factory-smoke.py \
  --binary /home/campanus/apps/dsh/current/bin/dshd \
  --web-dir /home/campanus/apps/dsh/current/web \
  --report /home/campanus/apps/dsh/shared/logs/plugin-factory-smoke.json
```

该检查创建临时数据库、合成管理员和 loopback 模型，调用真实 Linux 隔离编译器与 Component 工作器，覆盖版本激活 / 回滚 / 重启、认证 / Origin / CSRF / owner 边界、摘要审批及资源 / 导入拒绝。`model_is_mock=true` 不代表真实模型生成验收；不使用生产 QQ 账号，不伪称 QQ 插件联调通过。

一手依据：[Wasmtime 安全模型](https://docs.wasmtime.dev/security.html)、[36.0.19 发布记录](https://github.com/bytecodealliance/wasmtime/releases/tag/v36.0.19)、[当前安全公告](https://github.com/bytecodealliance/wasmtime/security/advisories)、[Linux 6.8 Landlock 文档](https://www.kernel.org/doc/html/v6.8/userspace-api/landlock.html)、[WIT Component 模型](https://component-model.bytecodealliance.org/)。
