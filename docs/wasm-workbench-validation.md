# 纯计算工坊验收

本记录对应[受限设计](wasm-workbench.md)。已完成本地检查、提交推送、固定SHA部署、Linux资源与流程验证、真实DeepSeek生成验证及旧功能回归。

本地Windows验证：后端164项测试、4项生命周期测试和严格Clippy通过；前端91项组件测试、类型检查及构建通过。独立Chromium管理页验收1项通过，1440、390、320像素布局检查无横向溢出。Windows真实后端正确报告worker不可用；浏览器的生成、审批、执行流程使用合成响应，浏览器未编译或执行WASM，此证据不代表Linux进程资源限制已通过。已补测运行期资源门禁丢失会关闭后续生成，避免继续调用模型；普通guest错误不关闭工坊。

## 发布与宿主

- 运行提交：`7e4bb131f67fc7bc2f05e33302969abfb4150290`，已推送main及`codex/long-run-memory`；前后端完整SHA一致。
- 部署时间：2026-10-24T13:02:18.549929Z（21:02:18 +08:00）；目标`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1、aarch64、7933MiB、Ubuntu24.04.4。保留最终Raspberry Pi OS Lite基线，未重装。
- 前端归档SHA256：`dc043fe90504c3f5232b98067bf71723efb141ab75572f9891a5bd8cf50bc563`。
- 实际运行二进制SHA256：`cf4a4eb0669a669c19e9db7501854484d1a0772063c77b82ff2aa76bd8a4f32a`。
- 前一release：`720a30de7ed38c0db544cc2beb64a8671a4fb1f3`；切换前备份：`/home/campanus/apps/dsh/shared/backups/pre-7e4bb131f67fc7bc2f05e33302969abfb4150290-20261024T130217126673542.sqlite`。
- `dsh.service`为active/running、NRestarts=0，`/health/ready`返回上述SHA。启动轮询首次curl连接失败后正常ready，整个部署命令退出0。未执行回滚或数据恢复，不称已验证恢复。

部署核实树莓派当前display-manager inactive、默认graphical.target；指定六个常见桌面包查询均未检出（dpkg-query退出1），不能据此将Ubuntu验证冒充Lite验收或完整桌面包审计。

## 实际命令与结果

固定SHA的前端由`deploy/package-web.ps1 -Commit <完整SHA>`构建，校验本地归档哈希后上传。远端从已推送提交取`deploy/deploy.sh`，以`DSH_CARGO_MIRROR=rsproxy bash <脚本> <完整SHA> <归档> <固定SHA256>`执行，原生`cargo test --workspace --locked -j 2`（164+4通过）与release构建成功后才切换。测试、构建与部署日志分别位于`/home/campanus/apps/dsh/shared/logs/{test,build,deploy}-<完整SHA>.log`。

以下命令在`current/source`、核实HEAD等于完整SHA后依次执行，binary为`/home/campanus/apps/dsh/current/bin/dshd`，web为`/home/campanus/apps/dsh/current/web`。组合脚本及每项命令均退出0，报告保存在同一私有logs目录：

```sh
python3 -I tools/wasm-worker-check.py --binary "$bin" --report "$logs/wasm-worker-$sha.json" --source-sha "$sha" --resource-tests
python3 tools/wasm-workbench-smoke.py --binary "$bin" --web-dir "$web" --report "$logs/wasm-smoke-$sha.json"
python3 tools/wasm-workbench-live.py --binary "$bin" --web-dir "$web" --source-data-dir /home/campanus/apps/dsh/shared/data --report "$logs/wasm-live-$sha.json"
python3 -m unittest discover -s tools/tests -v
```

| 检查 | 结果与证据 |
| --- | --- |
| 真实引擎与Linux资源 | [22项通过](validation/wasm-worker-aarch64.json)：5个独立exact unit，实际cgroup128MiB / swap0 / pids1、5秒RuntimeMax、worker环境仅LANG=C；真实OOM为oom-kill、超时为timeout，均确认退出，无pending units |
| 工坊真实进程流程、模拟模型 | [20项通过](validation/wasm-workbench-aarch64.json)：实际编译/测试/执行；鉴权、owner、CSRF、UUID、哈希审批、超限、错误代码、取消模型请求、撤销、TTL和元数据上限 |
| 真实DeepSeek | [7项通过](validation/wasm-workbench-live-aarch64.json)：一次合成加法需求生成、全部样例通过；明确API审批后以未给模型的输入123、-46执行得77；同UUID不重复调用，撤销阻止新执行，清除私有执行输入和结果 |
| Python兼容门禁与图像夹具 | 17项通过，空表/探针不抬高reader，使用过或撤销后的制品/任务阻止能力降级 |
| 现有图片分析 | [34项通过](validation/image-wasm-aarch64.json) |
| 记忆候选 / 微信管理 | [40项](validation/candidate-wasm-aarch64.json) / [26项](validation/weixin-wasm-aarch64.json)通过 |
| 摘要 / 长期记忆 | [24项](validation/summary-wasm-aarch64.json) / [38项](validation/memory-wasm-aarch64.json)通过 |
| M1 / 角色卡 | [19项](validation/m1-wasm-aarch64.json) / [55项](validation/character-wasm-aarch64.json)通过 |

本轮M1旧脚本报告不含提交或二进制字段，其版本归属由固定HEAD、同一制品路径和串行命令记录确认；其他新版报告各自保留SHA / 二进制或声明来源的准确字段，不补造旧报告字段。

生产浏览器经SSH隧道打开`/admin/#wasm`，已认证页面显示工坊可用、现有deepseek-chat模型及正确Web SHA，任务/制品为空；未在生产账号生成或批准测试制品。截图保存在忽略的`.local/production-evidence/wasm-7e4bb13.png`。真实模型验证只在临时数据库复制已有模型配置及凭据引用，不读取生产聊天、记忆、角色或用户代码。

## 验收边界

Linux流程测试的撤销发生在排队/完成竞争窗口，`revoked_active_worker_tested=false`，未将其冒记为撤销正在运行的guest实测；在途结果拦截及lease状态机另有后端测试。资源压力检查仅验证各独立worker，不代表整体8GB长期并发压力测试。真实模型仅合成整数需求，没有替用户审核任意生成程序。

这是受限纯计算闭环，未启用聊天工具、WASI、任意宿主API或完整Harness。ASR/TTS、渠道媒体和完整WIT/Rust插件工厂仍未交付。微信扫码后真实收发仍待用户参与；未重新触发手机二维码。证据提交本身只更新文档，不为此再次部署或重启。
