# 视觉接口预检

2026-10-24 19:30（+08:00）使用现有DeepSeek账号完成两次真实HTTPS请求。随机生成的合成PNG分别含不同形状与颜色，文本没有给出答案；模型返回的形状、颜色和数量均与实际像素匹配。[脱敏结果](validation/vision-preflight-aarch64.json)

探针代码提交为`ccfa2dffef5ba668670fe2a66d65980a2fec4562`，先推送main，远端fetch核对后从该固定SHA提取到私有cache运行：

```sh
python3 /home/campanus/.cache/dsh/vision-preflight-ccfa2dffef5ba668670fe2a66d65980a2fec4562.py \
  --source-data-dir /home/campanus/apps/dsh/shared/data \
  --report /home/campanus/apps/dsh/shared/logs/vision-preflight-ccfa2dffef5ba668670fe2a66d65980a2fec4562.json
```

退出0；两次请求、没有自动重试。脚本SHA256与结果中的`probe_source_sha256`一致。只读取现有模型配置、该凭据的元数据与私有文件，没有读取真实聊天、记忆、角色或图片，没有修改正式配置 / 数据、连接渠道或重启应用。

接口依据[官方视觉文档](https://api-docs.deepseek.com/guides/vision/)核对，使用`deepseek-flash`、标准Chat Completions图像块与内联PNG；另按[思考模式文档](https://api-docs.deepseek.com/guides/thinking_mode/)显式禁用思考，返回有界JSON。没有把`deepseek-chat`的纯文字可用性当作视觉证据，也没有向第三方上传用户图片。

这只证明所测账号、模型及合成图片的接口可用性。图片上传管理、完整附件ACL、图像对话、ASR、TTS仍未交付；不能据此将产品capabilities中的vision改为已交付。后续图像功能应另按本地检查、Git、固定SHA部署和远端验收发布。
