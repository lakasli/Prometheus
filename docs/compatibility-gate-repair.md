# 部署门禁工具修复

2026-10-27，应用 `25ff608fba7e7a137f86ce0f31fa84382e4ec815` 的 Linux 构建、完整测试和隔离模型流程已通过，但旧门禁未登记 `rest_deferred_presentations` 的三个新增列，切换前拒绝部署。此问题属于部署工具，修复不改应用二进制、前端或用户数据库。

当前列清单明确列出 `self_plan_status`、`self_plan_json`、`self_plan_hash`。旧五列或新八列均可识别，部分新增、未知列仍失败；有日程核对检查点或通用安排数据时仍要求 `daily_life:2`。

停服务后的两道门禁均保留。新门禁先只读检查原库；旧门禁若精确声明原有 Rest 列契约，则在新门禁已经通过的前提下，检查一份权限为 0600、目录私有的 SQLite 临时备份，只在副本去掉已知三个新增列。旧字段、全部行和其他功能约束保留；旧门禁的拒绝照常阻断。未知、部分或未来 schema 不投影，不删除生产记录绕过检查。临时目录由标准临时文件机制创建和回收，生产库只读打开。

只有部署工具修复时，可用已推送完整 SHA 指定 `DSH_COMPATIBILITY_SOURCE_SHA`。部署脚本从同一已核验 Git 仓库检出该 SHA 的干净工具目录，要求该提交是 `origin/main` 的祖先；不从任意路径或移动分支加载门禁。运行时目标 SHA、前端 SHA 和二进制 SHA 不改变，`shared/deployment.json` 单独记录 `compatibility_tools_commit`。如需回退，门禁仍检查目标版本能力，代码与数据恢复分别处理。

```sh
DSH_COMPATIBILITY_SOURCE_SHA=<已推送的完整工具提交SHA> \
DSH_BUILD_JOBS=1 DSH_CARGO_MIRROR=rsproxy TMPDIR=<私有临时目录> \
bash <从该工具提交导出的deploy.sh> \
  25ff608fba7e7a137f86ce0f31fa84382e4ec815 \
  <该应用SHA对应的前端包> <该包SHA256>
```

此路径仍运行目标应用的锁定 Rust 测试和 release 构建。已有相同源码 / SHA 的构建缓存可复用，不把工具提交伪装成新应用编译身份。测试覆盖真实旧门禁副本拒绝原库、投影后通过、原库字节与正文保留、旧门禁额外拒绝、未知 / 部分列阻断、reader2 与停服务 / 失败恢复分支。
