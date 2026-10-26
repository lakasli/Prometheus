# 人际关系网络与兼容管理入口移除验收

日期：2026-10-26（Asia/Shanghai）。本次仅修改管理页面及相关测试；生产数据库、来源授权、模型配置和性格的保存逻辑不变。

## 交付范围

人际关系页采用人物列表、关系网络、人物详情三栏布局。圆形节点按当前页已审核关联数量调整大小，关系布局由固定次数的确定性计算生成，没有动画计时器或常驻 worker。仅显示当前页最多 50 位未合并人物及两端可见的已审核有向边，不根据同名、间接认识、待审或页外数据补造连线。单击、Enter、空格与列表选择联动；支持滚轮 / 按钮缩放、拖动空白与重置。窄屏纵向排列。资料和关系审核、身份映射、来源与明确遗忘流程保留。

日记与资料页删除“历史记忆与会话摘要兼容管理”区域及其三个组件挂载，不再自动请求旧记忆、候选或摘要管理接口。历史数据、后台 API、对话摘要运行机制、迁移及删除水位保留。旧界面浏览器脚本移至 `web/admin/tests/historical/`，不作为当前页面可用的证据；相关后台和组件单测继续保留。

## 本地检查

- `npm run typecheck`、`npm run build`：退出 0。
- `npm test`：22 个测试文件、155 项通过；包括审核及端点过滤、键盘联动、缩放边界、筛选后的迟到详情、50 人碰撞与边界、双向箭头几何以及旧管理组件不挂载 / 不请求。
- Prettier 与 `git diff --check`：通过。
- 隔离 Windows 进程与 Playwright：10 位合成人物、12 条已审核有向关系和 1 条待审关系，三栏、孤立节点、键盘、缩放、滚轮、拖动及 390px 无横向溢出通过；没有使用生产人物或真实渠道。

首次浏览器运行发现重置图标被画布 SVG 样式影响、遮挡缩放按钮，导致放大按钮点击超时，不计为通过。诊断摘要保留于 `.local/relationship-network-browser-initial-failure.json`；修复样式与测试选择器后重新验证通过。初次截图保留在 `.local/relationship-network-e2e/evidence/`，通过截图位于 `.local/relationship-network-e2e-final/evidence/`。

## 发布身份

- 应用提交：`2fbf100ba40f0593aa8f4baa9a108d119ef11df6`；已成功推送并核对 `origin/main`。
- 前端包：`admin-2fbf100ba40f0593aa8f4baa9a108d119ef11df6.tar.gz`。
- 包 SHA-256：`6cbf6f250a0e87fa156f8c911d1be171243f80f6a45b4cc49b283187be812651`；本地与远端相同。
- 上一可用应用：`774d89b0e87185fcadb76e537ab08ecd8e1afedd`。
- 目标：`campanus@192.168.189.191`，Raspberry Pi 5 Model B Rev 1.1 / Ubuntu 24.04.4 LTS / aarch64；Rust / Cargo 1.88.0。部署前实测 MemTotal 8,318,562,304 字节、MemAvailable 7,703,326,720 字节；构建单并发。

## 部署与远端复验

19:28:36（UTC+08）部署成功。Linux 完整测试、发布构建和部署脚本均退出 0：kernel 4 项、dshd 382 项，共 386 项，0 失败；dshd 测试耗时 217.62 秒，release 构建耗时 9 分 20 秒。重启后的首次健康探测短暂连接失败，脚本重试后 ready；源码 HEAD、静态 manifest、运行健康接口均核对为本次应用 SHA，systemd 为 active/running，退出状态 0。

| 检查 | 结果 / 证据 |
| --- | --- |
| 远端构建与测试 | `shared/logs/test-2fbf100ba40f0593aa8f4baa9a108d119ef11df6.log`、`build-2fbf100ba40f0593aa8f4baa9a108d119ef11df6.log` |
| 关系隔离 mock | 15 项通过，退出 0；`shared/logs/relationship-network-2fbf100-relationships-mock.json` |
| 日记隔离 mock | 9 项通过，退出 0；`shared/logs/relationship-network-2fbf100-diary-mock.json` |
| 固定 SHA 浏览器 | 1 个完整流程通过，测试 2.8 秒；运行健康和静态 manifest 均断言完整 SHA，真实进程 / API，1600px 桌面和 390px 手机，无脚本错误及横向溢出 |
| 认证与 CSRF | 生产人物、关系及日记未登录查询均 401；隔离浏览器登录后无 CSRF 的人物写入返回 403 |
| 生产保留 | 19:29:03 只读比对：全部记录计数及 16 项配置 / 性格 / 稳定身份 / 来源与联系 / 自主策略指纹一致 |
| 备份 | `shared/backups/pre-2fbf100ba40f0593aa8f4baa9a108d119ef11df6-20261026T112834826406358.sqlite`；只读 quick_check 为 `ok` |

生产仍有 35 次运行、68 条消息、4 个会话和 1 条会话摘要，历史记忆、人物与日记记录均为 0。没有向生产写入合成人物、变更来源授权或覆盖已保存性格。两项进程验收均仅使用合成来源和 mock 模型，不记为真实外部模型或 QQ / 微信联调。

浏览器使用正式 release 的同一二进制及静态目录，数据单独位于 `shared/validation/relationship-network-2fbf100-browser/data`，经本地 18425 SSH 隧道访问，不登录生产数据。截图已人工查看，位于 `.local/relationship-network-remote/evidence/`；关系 / 日记 mock 进程由脚本回收，浏览器精确 unit `dsh-relationship-network-2fbf100-qa.service` 已停止，状态 inactive/dead、Result=success、ExecMainStatus=0。两次 Windows 隔离进程及本轮 QA 隧道也已关闭。

结构化汇总见 [本次证据](validation/relationship-network-aarch64.json)。备份 SHA-256 为 `d134d384634df35d5070b032a60babb2d7b8cef2fc95f1c1e4113cd626153fec`，二进制 SHA-256 为 `02308184b59021d0522a849ba85099474a429808a0ff9a13e5174a27771cfe65`。

## 执行命令与回退

本地在应用提交成功并核对远端分支后执行：

```powershell
./deploy/package-web.ps1 -Commit 2fbf100ba40f0593aa8f4baa9a108d119ef11df6
scp .git/artifacts/admin-2fbf100ba40f0593aa8f4baa9a108d119ef11df6.tar.gz campanus@192.168.189.191:/home/campanus/.cache/dsh/
```

远端从同一 Git SHA 读取部署脚本后执行（`2fbf100` 是本轮缓存脚本文件名，参数使用完整 SHA）：

```bash
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy bash /home/campanus/.cache/dsh/deploy-2fbf100.sh \
  2fbf100ba40f0593aa8f4baa9a108d119ef11df6 \
  /home/campanus/.cache/dsh/admin-2fbf100ba40f0593aa8f4baa9a108d119ef11df6.tar.gz \
  6cbf6f250a0e87fa156f8c911d1be171243f80f6a45b4cc49b283187be812651
root=/home/campanus/apps/dsh
python3 "$root/current/source/tools/relationships-smoke.py" --binary "$root/current/bin/dshd" --web-dir "$root/current/web" --report "$root/shared/logs/relationship-network-2fbf100-relationships-mock.json"
python3 "$root/current/source/tools/diary-smoke.py" --binary "$root/current/bin/dshd" --web-dir "$root/current/web" --report "$root/shared/logs/relationship-network-2fbf100-diary-mock.json"
```

浏览器测试指定本轮隔离端口、数据和证据目录，设置 `DSH_E2E_EXPECT_SHA` 后执行 `npx playwright test tests/e2e/relationship-network.spec.ts --reporter=list,json`。生产管理入口仍为 `http://127.0.0.1:18419/admin/` 的既有 SSH 隧道；没有启动生产前端开发服务器。

上一可用 reader2 版本为 `774d89b0e87185fcadb76e537ab08ecd8e1afedd`；需要回退时在当前源码目录执行 `bash deploy/rollback.sh 774d89b0e87185fcadb76e537ab08ecd8e1afedd`，并通过兼容门禁和健康复验。此次未执行回退，也未恢复或替换生产数据库。代码回退与备份恢复仍分开处理。
