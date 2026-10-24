# 图片分析验收

2026-10-24，Asia/Shanghai。此切片正在本地整体验证，尚未部署；当前正式运行版本为候选审核`0a98c63221d13733299b75858a33f4c298328f3c`。使用范围见 [说明](image-analysis.md)。

本地142后端+4Cordis、严格Clippy、格式与15项Python检查通过；图片模块12项覆盖PNG校验、元数据剥离、透明度、固定provider、幂等、授权、lease及到期 / 重启。最终开发binary的进程黑盒`tools/image-analysis-smoke.py`通过34项（含真实30秒超时），候选40项回归通过；不接触生产或外部模型。前端76项、类型检查和构建通过，覆盖显式上传、blob回收、未知请求幂等及401 / 409。

整体验证发现并修复：本地blob预览精确加入img-src；普通Run也持有真实HTTP future退出前的lease，否则配置并发1时取消会短暂重叠。四项新增运行回归用实际挂起HTTP和DELETE API证明候选 / 图片 / 下一普通Run均不能提前抢槽，并覆盖abort、重启和坏冻结JSON；另有凭据删除与运行计数的API回归，取消中旧凭据不能删除。官方服务两张合成图的独立预检结果不替代本工作台真实模型验收。

真实Chromium在18431最终1项流程通过（13.4s），真实登录、空owner GET、401 / CSRF403 / 非owner404后，所有有效分析和状态均明确使用fixture；合成PNG在浏览器原生解码为240×160。验证未点击不上传、未知提交后原ID重试、改问题新ID、串行轮询、纯文本HTML、409刷新、TTL / 取消 / 401及blob回收，无外部请求、持久storage或聊天 / 记忆写入。1440 / 390 / 320截图已人工检查，无横溢出，位于忽略目录`.local/image-e2e/screenshots/image-analysis-fixture-<宽度>.png`。
