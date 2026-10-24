# DSH：面向 Linux 的 Rust AI 助手

DSH 计划采用 Cordis 插件架构，以 Rust 实现主程序，连接 QQ、微信等渠道，提供长期记忆、DeepSeek Harness 接入、受控的插件自生成，以及图像理解、语音识别和语音合成。

当前仓库处于**架构设计阶段**，尚未提供可运行程序。

## 项目规则

开发遵循 **本地编写 → Git 提交和推送 → SSH 远端部署与测试**。仓库规则见 [AGENTS.md](AGENTS.md)，操作流程及远端环境记录见 [开发与远端部署流程](docs/development-workflow.md)。

## 架构文档

阅读 [项目架构设计](docs/architecture.md)，其中包含：

- Rust 与 Cordis、DeepSeek Harness 的技术边界和选型依据。
- 模块划分、插件接口、生命周期、隔离执行与热更新。
- QQ、微信 iLink、企业微信等渠道接入方案。
- 会话调度、身份权限、记忆检索与删除、多模态处理。
- AI 自制插件的构建、验证、发布和回滚流程。
- 数据模型、Linux 部署、开发路线与验收标准。

设计基线：2026-10-24。文档区分已核实的上游能力与本项目拟实现的接口；依赖版本和平台权限需要在开发阶段锁定、联调。

## 推荐实现路线

以 Rust 模块化单体起步，优先评估 `cordis-rs`；使用 WASM Component 承载生成插件，以 ACP 接入独立的 DeepSeek Harness 进程。纯 Rust 模式可使用原生 Agent 后端，接入官方 Harness 时需要其 Node.js 运行环境。

第一阶段先验证 Cordis 生命周期、Harness ACP 和一个真实消息渠道，再扩展记忆、多模态与插件工厂。具体范围和退出条件见架构文档。
