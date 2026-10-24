# 图片分析验收

2026-10-24，Asia/Shanghai。已于20:08固定SHA部署，完成远端原生测试、回归、真实DeepSeek识图及正式页面读取。使用范围见 [说明](image-analysis.md)。

本地142后端+4Cordis、严格Clippy、格式与15项Python检查通过；图片模块12项覆盖PNG校验、元数据剥离、透明度、固定provider、幂等、授权、lease及到期 / 重启。最终开发binary的进程黑盒`tools/image-analysis-smoke.py`通过34项（含真实30秒超时），候选40项回归通过；不接触生产或外部模型。前端76项、类型检查和构建通过，覆盖显式上传、blob回收、未知请求幂等及401 / 409。

整体验证发现并修复：本地blob预览精确加入img-src；普通Run也持有真实HTTP future退出前的lease，否则配置并发1时取消会短暂重叠。四项新增运行回归用实际挂起HTTP和DELETE API证明候选 / 图片 / 下一普通Run均不能提前抢槽，并覆盖abort、重启和坏冻结JSON；另有凭据删除与运行计数的API回归，取消中旧凭据不能删除。官方服务两张合成图的独立预检结果不替代本工作台真实模型验收。

真实Chromium在18431最终1项流程通过（13.4s），真实登录、空owner GET、401 / CSRF403 / 非owner404后，所有有效分析和状态均明确使用fixture；合成PNG在浏览器原生解码为240×160。验证未点击不上传、未知提交后原ID重试、改问题新ID、串行轮询、纯文本HTML、409刷新、TTL / 取消 / 401及blob回收，无外部请求、持久storage或聊天 / 记忆写入。1440 / 390 / 320截图已人工检查，无横溢出，位于忽略目录`.local/image-e2e/screenshots/image-analysis-fixture-<宽度>.png`。

## 部署一致性

- 运行SHA：`720a30de7ed38c0db544cc2beb64a8671a4fb1f3`；先推送分支及main，远端核对commit存在并属于origin/main，再从该SHA提取部署脚本。
- 部署时间：`2026-10-24T12:08:31.935915+00:00`（20:08:31，+08:00）；Pi5 Model B Rev1.1 / aarch64 / 7933MiB / Ubuntu24.04.4，Rust/Cargo1.88。graphical.target、display-manager inactive；未重装或作Lite验收。
- 前端归档SHA256：`ed9305579bd28b699205b706f6f2216ef8c7722074f9f5825bfe441c0484f469`；二进制SHA256：`91205b5ac25d9d8dcef3c02b38bd186e9df30747db5733565c4afb4fdd2dbe0d`。
- health、前端manifest与源码Git HEAD完整SHA一致，用户级systemd active/running、NRestarts=0。原管理员、模型配置、角色、稳定助手身份和数据继续保留。
- 前一运行SHA为`0a98c63221d13733299b75858a33f4c298328f3c`，一致性备份位于私有`shared/backups/pre-720a30de7ed38c0db544cc2beb64a8671a4fb1f3-20261024T120830532739589.sqlite`。本次升级成功，未执行回滚或数据库恢复。

`DSH_CARGO_MIRROR=rsproxy bash <固定SHA部署脚本> <SHA> <归档> <上述hash>`退出0；Pi的`cargo test --workspace --locked -j 2`原生142+4通过，`cargo build --release --locked -p dshd -j 2`退出0、2m13s。新增依赖锁定png0.18.1、base640.22.1、crc32fast1.5.3及Cargo.lock校验。日志为`shared/logs/{deploy,test,build}-<SHA>.log`。

## 远端验收

在`current/source`核对完整SHA后，脚本统一使用绝对`--binary current/bin/dshd --web-dir current/web --report shared/logs/<kind>-<SHA>.json`，数据均在独立私有临时实例，以下全部退出0：

| 脚本 | 结果与脱敏证据 |
| --- | --- |
| `python3 -m unittest discover -s tools/tests -v` | 15项：14兼容门禁与1像素fixture |
| `python3 tools/image-analysis-smoke.py` | [34项图片真实进程 / 模拟provider](validation/image-analysis-aarch64.json)，包含真实30秒超时 |
| `python3 tools/memory-candidate-smoke.py` | [40项候选](validation/candidate-image-aarch64.json) |
| `python3 tools/weixin-smoke.py` | [26项微信管理](validation/weixin-image-aarch64.json)，未重复官方取码 |
| `python3 tools/summary-smoke.py` | [24项摘要](validation/summary-image-aarch64.json) |
| `python3 tools/memory-smoke.py` | [38项记忆](validation/memory-image-aarch64.json) |
| `python3 tools/smoke.py` | [19项M1](validation/m1-image-aarch64.json) |
| `python3 tools/character-smoke.py` | [55项角色](validation/character-image-aarch64.json) |
| `python3 tools/image-analysis-live.py`另加`--source-data-dir shared/data` | [11项真实DeepSeek](validation/image-analysis-live-aarch64.json)，实际2次请求 |

真实图片脚本只读取原模型配置与该密钥，向临时Rust实例提交两张随机、不同形状 / 颜色的合成PNG，问题不包含答案。经过本工作台的实际解码、RGBA规范与去元数据后，Flash返回的形状、颜色和数量均匹配真实像素；相同请求ID回到已有completed任务，没有重复模型调用；完成后image BLOB为空，明确清除后问题 / 图片 / 结果均为空，任务metadata仍能去重。没有读取用户图片、聊天、记忆、角色或渠道账号，没有修改正式配置 / 数据，也未接入QQ / 微信。

正式Edge页面只读刷新显示`Web 720a30de7e`、配置的DeepSeek-flash及空任务列表；未选择或上传正式图片。安全截图为`.local/production-evidence/images-720a30d.png`。这些结果不替代普通对话图片附件、真实渠道媒体、ASR / TTS、资源压力或最终Lite验收；逻辑清除不代表WAL和离线备份同步物理清除。
