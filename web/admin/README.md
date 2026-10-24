# DSH 管理前端

Vue 3 + TypeScript 静态管理页面，生产资源由 Rust 服务提供，入口为 `/admin/`，API 为同源 `/api/admin/v1/`。

## 本地检查与构建

在此目录使用 Node.js 24 LTS 和 npm：

```sh
npm ci
npm test
npm run build
```

`build` 先执行 Vue / TypeScript 类型检查，再生成 `dist/`。生产环境不运行 Vite dev 或 preview 服务。部署时将静态资源与同一提交的 Rust 服务一同发布；`dist/`、`node_modules/` 不提交 Git。

可以在构建环境中设置非秘密变量 `VITE_BUILD_SHA`，页脚将显示构建提交标识。不得在 `VITE_*` 变量中放置模型或渠道密钥。

## M1 页面

- 首次管理员初始化、登录和退出。
- 服务总览、内存采样、队列和当前配置版本。
- 助手提示词、模型服务和 API Key 更新。
- QQ WebSocket / Webhook 模式、App ID 与 App Secret 更新。
- 配置草稿、后端校验、连接测试、应用与历史版本恢复。
- 持久化文本会话和真实模型回复。

记忆、多模态和插件工厂仅标注为后续阶段，不提供假操作。

## 会话与密钥

登录会话使用后端 HttpOnly Cookie。CSRF token 只在页面内存中保存，刷新后从已认证的 `/auth/me` 恢复；不使用浏览器持久存储保存凭证。替换密钥先写入后端 secret store，配置草稿只携带返回的引用，表单不读取原始密钥。

开发使用 `npm run dev` 时，Vite 将 `/api` 代理到本机 `127.0.0.1:8419`；后端的允许 Origin 必须与实际 Vite 地址一致。远端正式访问遵循仓库中的 SSH 隧道或 HTTPS 反向代理流程。
