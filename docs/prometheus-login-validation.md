# Prometheus 初始化与登录修复

2026-10-31，Asia/Shanghai。用户反馈从`D:\DSH\.local\prometheus\dev\bootstrap-token.json`读取token后初始化失败。

## 诊断

00:37检查：8419运行5cc647cd8ce304408563bc9a154b3ccebda06f61，数据目录为`D:\DSH\.local\prometheus\instance`，尚无管理员。dev文件与instance文件的token不同；当时二者均未过期。dev凭证到期上海时间00:49:26，instance凭证到期00:42:11。没有打印token、密码或其哈希，也没有向正式实例提交测试账户。

根因是不同实例凭证混用，初始化页面又提示不带数据目录的`npm run bootstrap`，该命令默认写dev。认证拒绝属于预期安全行为，原界面指引和“错误或过期”的合并提示不够明确。

## 修复与本地验证

未初始化页面显示实际凭证路径、状态、上海时间到期时间及显式`--data-dir`命令，支持刷新状态。接口只返回必要引导元数据，不返回秘密；初始化完成后不返回路径和命令。错误分别提示凭证不匹配、过期或文件无效。输入去除首尾空白，路径/整段JSON给出具体纠正说明。

Windows检查：类型检查、构建通过；后端38项通过，浏览器7项通过。浏览器初始化场景实际执行：粘贴整段JSON被客户端拒绝→错误token被服务端拒绝→过期凭证被拒绝→更新凭证并刷新→有效token（含首尾空白）创建账户→凭证文件删除→退出→使用用户名和密码重新登录成功。另验证状态响应不含token、损坏文件受控拒绝、已初始化状态不泄露路径、错误密码拒绝。测试均在独立数据目录。

## 固定版本交付

首个修复制品c55d49050dadeb12ff2320570299f78d34c526e0提交推送后在Windows构建，18项固定制品检查、7项浏览器检查通过。00:44:45+08:00核对PID2604及5cc647c版本、无管理员或在途任务后停止进程；recover-lock确认退出再清理锁，不记作优雅退出。完整备份`.local/prometheus/backups/instance-2026-10-30T16-44-45-442Z-e5017e33-86a9-4f28-85be-eceb85df7d72/`的5个文件、SQLite完整性及ACL检查通过。

00:45:14+08:00在8419启动c55d490，PID25364；保留身份、配置、性格、资料与主密钥。实际接口显示instance准确路径和expired状态，显式数据目录命令成功更新凭证。补充实际浏览器检查发现390像素视口下表单横向宽度420像素；加入flex最小宽度修复，并将初始化页面的移动宽度加入首建浏览器回归，该7项复测通过。最终制品及实际切换结果随后追加，未把这次移动显示失败计为通过。

上一可用Windows版本为5cc647cd8ce304408563bc9a154b3ccebda06f61，原制品与完整备份保留。用户原有Rust/Breeze变更保持未提交。

## 最终Windows验收

- 最终应用SHA：`248181c5bbbc2159ee4030bd8ae237a8b53916b2`，已推送GitHub的codex/prometheus-typescript分支并核对远端；从Git archive构建，前后端同SHA。
- 环境：Windows11 Pro 10.0.26200 x64、Node24.22.1、PowerShell7。后端与前端构建分别通过，锁文件安装审计0漏洞；18项固定制品检查通过，固定制品7项浏览器回归通过（含初始化移动宽度），命令退出0。后端38项在本地同源代码检查通过，本轮后续仅修改CSS与页面回归。
- 制品：`.local/prometheus/releases/248181c5bbbc2159ee4030bd8ae237a8b53916b2/runtime/`；ZIP SHA256：`ca04eb98b99a47b57c715c520ecfb895ee070f867b0f5f3267e879b2597a5bc2`。
- 00:48:46+08:00核验中间版本c55d490的PID25364、无管理员或在途任务后停止；完整备份`.local/prometheus/backups/instance-2026-10-30T16-48-46-137Z-01e19967-2be7-45dc-9284-96dcf197edc4/`，5文件、SQLite完整性与ACL通过。停止方式同上，不宣称优雅退出。
- 00:48:48+08:00在原instance目录启动最终版本，前台PID12196，8419端口；live/ready返回最终完整SHA，退出前台进程未执行，不安装服务或自启动。
- 00:49:15+08:00实际8419页面检查通过：正确instance路径、明确命令、刷新后凭证有效状态；1440与390像素视口无横向溢出，JavaScript错误0，未填写或创建正式账户。截图`.local/prometheus/screenshots/login-fixed-desktop.png`和`login-fixed-mobile.png`已检查。
- 最终核对身份、管理员数0、资料数0、配置revision1、性格revision1、主密钥均保留，schema2完整性ok。已用显式instance命令更新初始化凭证，秘密未输出。私有证据`.local/prometheus/login-evidence/`，部署元数据为instance/deployment.json。

实际初始化/退出/密码重登发生在隔离浏览器测试账户，正式实例仅做无写账户的页面、路径和状态验证。用户仍需自行选择密码并用正式目录文件中的token完成首次创建；不使用dev凭证、不设置默认密码、不修改用户密码。
