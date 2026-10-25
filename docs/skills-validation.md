# Skill 与角色卡交付验收

日期：2026-10-25，Asia/Shanghai（+08:00）。本记录覆盖 Skill 管理、两个默认 Skill、对话执行与绘画，以及角色卡双栏和草稿介绍同步。

本地已完成前端 120 项测试、Rust workspace kernel 4 项与 daemon 181 项测试、Python 23 项检查，以及实际隔离服务的角色卡 / Skill 两项 Playwright 浏览器测试。Skill 隔离进程验收通过 20 项，生图接口使用回环模拟服务；不是外部生图平台验收。最终按需图片读取、未认证拒绝及跨管理员访问拒绝已复验。

部署前复核：目标为 campanus@192.168.189.191，Raspberry Pi 5 Model B Rev 1.1、Ubuntu 24.04.4 LTS、aarch64，内存总量 7933 MiB，采样可用 6739 MiB，无 swap；Cargo / Rust 1.88.0。上一可用 release 为 `595288f9b181fb156056666a8e63a3e855e9f05c`，正式服务 active / ready。远端 Git origin 符合项目配置，工作区无修改。

最终部署 SHA、前端归档摘要、远端测试、真实文本模型验收及日志将在执行后补记。当前段落不表示已完成远端新版本交付。

