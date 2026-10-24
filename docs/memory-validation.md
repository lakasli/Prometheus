# 助手显式长期记忆验收

状态：显式助手长期记忆切片已部署，2026-10-24 17:42—17:46（+08:00）在获准使用的Ubuntu树莓派完成验收。记忆说明见 [memory.md](memory.md)。M2自动摘要及微信仍未交付，不将此切片称为M2全部完成。

本轮用户确认助手拥有自身的稳定记忆，优先顺序为长期记忆、微信、Harness权限桥、多模态和插件工厂。角色卡稳定代码基线为 `154ebf8c5e8b9901b39f4de8fb0a116129c6888a`，已合入随后文档提交 `3d48017`。本地独立分支为 `codex/long-run-memory`，不覆盖人物卡片工作区。

最终本地证据：后端55项及Cordis4项、严格Clippy / fmt / 构建、前端30项 / 类型检查 / 生产构建、原M1黑盒19项、角色黑盒55项、记忆进程黑盒38项、部署兼容7项全部通过。记忆、原管理页、角色卡三个真实Chromium场景分别在18426 / 18421隔离实例通过，核对320 / 390像素布局及桌面截图；包含真实API并发冲突、导出与遗忘。角色用例固定18421的保护保持启用，未为统一端口运行而放宽。模拟provider / 模拟渠道域不是外部模型或真实QQ联调。

## 固定版本与部署

- 运行提交：`60ef557388203150d56ab41348649601e735630c`，先提交并推送功能分支及main，再核实origin/main完整SHA。
- 目标：`campanus@192.168.189.191`；Raspberry Pi 5 Model B Rev 1.1、aarch64、7933 MiB物理内存、Ubuntu 24.04.4 LTS。默认graphical.target、display-manager inactive；没有执行系统重装，不能冒充Lite验收。
- 前端归档SHA256：`a99cf235acc67140a5c680b23ba885d0cdac9b5ec6de804e709cbcf2c65c0f05`；二进制SHA256：`eca09520a98414ea271603be2c2e2a836613fc37589d4dd424a527491a807bb7`。
- 当前release：`/home/campanus/apps/dsh/releases/60ef557388203150d56ab41348649601e735630c`；源码、二进制及前端分别在其`source/`、`bin/dshd`、`web/`。
- 上一可用版本：`154ebf8c5e8b9901b39f4de8fb0a116129c6888a`。部署前停服务，使用SQLite一致性备份保存到私有`shared/backups/pre-60ef557388203150d56ab41348649601e735630c-20261024T094216335319850.sqlite`，属campanus、0600。
- 部署记录时间：`2026-10-24T09:42:17.780318+00:00`，即17:42:17（+08:00）。用户systemd `active/running`、NRestarts=0、Linger=yes；health与静态release.json均匹配完整SHA。管理浏览器刷新后确认“助手记忆”页和Web构建版本。未修改生产模型 / QQ配置或既有角色卡。

本地`deploy/package-web.ps1`在已提交工作树构建，支持linked worktree的Git制品目录；上传后仅运行目标SHA中的部署脚本：

```sh
DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-60ef557388203150d56ab41348649601e735630c.sh \
  60ef557388203150d56ab41348649601e735630c \
  /home/campanus/.cache/dsh/admin-60ef557388203150d56ab41348649601e735630c.tar.gz \
  a99cf235acc67140a5c680b23ba885d0cdac9b5ec6de804e709cbcf2c65c0f05
```

退出0。脚本在Pi以`cargo test --workspace --locked -j 2`及`cargo build --release --locked -p dshd -j 2`原生测试 / 构建，55后端+4Cordis通过。日志为`shared/logs/test-<SHA>.log`、`build-<SHA>.log`、`deploy-<SHA>.log`。切换期间首个健康轮询尚未监听，随后ready通过；未发生部署失败或回滚。

## 远端验收

以下在`current/source`核对Git SHA后执行，命令均退出0，使用对应`current/bin/dshd`及`current/web`：

| 命令 | 结果与证据 |
| --- | --- |
| `python3 -m unittest discover -s tools/tests -v` | 7项部署兼容门禁通过，含空表回退及记忆墓碑 / 水位禁止语义降级 |
| `python3 tools/memory-smoke.py --binary <binary> --web-dir <web> --report <private-log>` | 38项真实进程黑盒，使用隔离模拟provider；[脱敏记录](validation/memory-aarch64.json) |
| `python3 tools/smoke.py --binary <binary> --web-dir <web> --report <private-log>` | 原M1 19项回归；[记录](validation/m1-memory-aarch64.json) |
| `python3 tools/character-smoke.py --binary <binary> --web-dir <web> --report <private-log>` | 原角色卡55项回归；[记录](validation/character-memory-aarch64.json) |
| `python3 tools/memory-live.py --binary <binary> --web-dir <web> --source-data-dir <shared-data> --report <private-log>` | 6项真实DeepSeek验收：配置可用、记住、跨新会话、更正排除旧echo、原会话遗忘、重启后新会话不复活；[脱敏记录](validation/memory-live-aarch64.json) |

完整参数路径见 [记忆说明](memory.md)。远端JSON原件为`shared/logs/memory-<SHA>.json`、`memory-live-<SHA>.json`、`m1-memory-<SHA>.json`、`character-memory-<SHA>.json`。真实模型脚本只读既有模型配置 / 凭证，在临时私有实例中测试合成事实；未读取真实聊天、改正式数据库或发送QQ消息。

本地Chromium覆盖未登录 / CSRF、共享确认、真实API并发修改 / If-Match、草稿保留与合并、查询、导出、具体遗忘确认及桌面 / 320 / 390布局。远端受保护入口已通过现有Edge登录会话打开，并核对页面显示的Web SHA；截图留于忽略的`.local/production-evidence/memory-60ef557.png`。这次远端页面检查只读，没有向正式记忆库写测试条目。

## 边界与回滚

QQ新增记忆的传输闭环尚待用户发新私聊；真实ingest函数、去重、Outbox及来源域隔离有自动化 / 模拟证据，不能替代新版本真实QQ投递。未执行群 / 频道记忆写入（当前明确限制为管理员管理）、故障注入回滚、数据库恢复、压力测试或最终Lite系统回归。

空记忆表初始化保留reader1；一旦生产写入记忆，reader2及能力门禁会禁止回退到不理解记忆 / 遗忘的角色卡旧版。私有备份保留，但恢复数据是独立操作，不能直接切旧提交冒充数据也已恢复。此次实际升级成功，未执行回滚。自动摘要、自动抽取、微信与后续阶段仍继续实施。
