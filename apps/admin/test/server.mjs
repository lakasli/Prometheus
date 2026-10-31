import { resolve } from "node:path";
import { randomUUID, createCipheriv } from "node:crypto";
import { pathToFileURL } from "node:url";
import { readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
const wxBytes = Buffer.from("Synthetic browser opaque file; not interpreted."),
  wxKey = Buffer.from("0123456789abcdef");
let wxDelivered = false;
function speechWave() {
  const b = Buffer.alloc(6444);
  b.write("RIFF");
  b.writeUInt32LE(b.length - 8, 4);
  b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16);
  b.writeUInt16LE(1, 20);
  b.writeUInt16LE(1, 22);
  b.writeUInt32LE(16000, 24);
  b.writeUInt32LE(32000, 28);
  b.writeUInt16LE(2, 32);
  b.writeUInt16LE(16, 34);
  b.write("data", 36);
  b.writeUInt32LE(b.length - 44, 40);
  return b;
}
const provider = createServer(async (request, response) => {
  let raw = "";
  for await (const chunk of request) raw += chunk;
  if (request.method === "PUT") {
    response.writeHead(200);
    response.end();
    return;
  }
  if (request.url === "/wx-download") {
    const cipher = createCipheriv("aes-128-ecb", wxKey, null);
    response.writeHead(200);
    response.end(Buffer.concat([cipher.update(wxBytes), cipher.final()]));
    return;
  }
  if (request.url === "/wx-upload") {
    response.writeHead(200, {
      "x-encrypted-param": "synthetic-browser-encrypted-download",
    });
    response.end();
    return;
  }
  const input = raw ? JSON.parse(raw) : {};
  const path = request.url;
  if (path.startsWith("/ilink/")) {
    let value = { ret: 0 };
    if (path.startsWith("/ilink/bot/get_bot_qrcode"))
      value = {
        qrcode: "synthetic-browser-qr",
        qrcode_img_content: "https://example.invalid/browser-qr",
      };
    else if (path.startsWith("/ilink/bot/get_qrcode_status"))
      value = {
        status: "confirmed",
        ilink_bot_id: "browser@im.bot",
        ilink_user_id: "browser@im.wechat",
        bot_token: "synthetic-browser-wx-token",
        baseurl: modelUrl,
      };
    else if (path === "/ilink/bot/getupdates") {
      value = {
        ret: 0,
        get_updates_buf: "browser-cursor",
        msgs: wxDelivered
          ? []
          : [
              {
                message_id: "browser-wx-file",
                from_user_id: "browser@im.wechat",
                to_user_id: "browser@im.bot",
                message_type: 1,
                message_state: 2,
                create_time_ms: Date.now(),
                context_token: "synthetic-browser-wx-context",
                item_list: [
                  {
                    type: 4,
                    file_item: {
                      len: String(wxBytes.length),
                      media: {
                        full_url: modelUrl + "/wx-download",
                        aes_key: wxKey.toString("base64"),
                        encrypt_type: 1,
                      },
                    },
                  },
                ],
              },
            ],
      };
      wxDelivered = true;
    } else if (path === "/ilink/bot/getuploadurl")
      value = { ret: 0, upload_full_url: modelUrl + "/wx-upload" };
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(value));
    return;
  }
  if (path === "/app/getAppAccessToken" || path.startsWith("/v2/")) {
    let result = {};
    if (path === "/app/getAppAccessToken")
      result = { access_token: "synthetic-browser-qq-token", expires_in: 3600 };
    else if (path.endsWith("/upload_prepare"))
      result = {
        upload_id: "synthetic-browser-upload",
        block_size: "2097152",
        parts: Array.from(
          { length: Math.ceil(Number(input.file_size) / 2097152) },
          (_, i) => ({ index: i + 1, presigned_url: modelUrl + "/put/" + i }),
        ),
      };
    else if (path.endsWith("/files"))
      result = { file_info: "synthetic-browser-file-info" };
    else if (path.endsWith("/messages"))
      result = { id: "synthetic-browser-sent-" + randomUUID() };
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(result));
    return;
  }
  const system = String(input.messages?.[0]?.content ?? "");
  let content = "合成浏览器回复。";
  if (
    system.includes("PROMETHEUS_DAILY_LIFE_V1") &&
    input.messages.at(-1)?.content.includes("SELF_TEXT_UI")
  ) {
    const available = JSON.parse(
      system.split("\n").find((s) => s.startsWith('{"available":')),
    ).available;
    content = JSON.stringify({
      reply: "这份文字已经登记，实际保存时间会单独记录。",
      self_activity: {
        slot_id: available.slots[0].slot_id,
        title: "浏览器自主文字安排",
        content:
          '完整的合成文字成果。<script>document.body.dataset.dailyUnsafe="executed"</script> https://example.invalid/正文不是访问指令',
      },
    });
  } else if (system.startsWith("PROMETHEUS_PROFILE_CANDIDATES_V1")) {
    const payload = JSON.parse(input.messages[1].content);
    if (payload.source.facts.includes("PROFILE_UI_SLOW"))
      await new Promise((r) => setTimeout(r, 5000));
    content = JSON.stringify({
      assertions: [
        {
          field: "hobby",
          value: "浏览器日记人物候选",
          statement_kind: "inference",
          reason: "本次明确选择的日记事实，仅保留推测标签",
        },
      ],
      edges: payload.allowed_targets.length
        ? [
            {
              to_person_id: payload.allowed_targets[0].id,
              relationship_type: "colleague",
              statement_kind: "third_party",
              reason: "本次已核对目标的来源陈述",
            },
          ]
        : [],
    });
  } else if (system.startsWith("PROMETHEUS_DIARY_GENERATOR_V2"))
    content = JSON.stringify({
      facts: "浏览器固定来源的完整日记归并。",
      feelings: "独立的合成感受。",
    });
  else if (system.includes('"candidates"'))
    content = JSON.stringify({
      candidates: [{ content: "浏览器选中的来源中可以核实的合成事实。" }],
    });
  else if (system.includes('"facts"'))
    content = JSON.stringify({
      title: "来源明确的模型日记",
      date: "2026-10-31",
      period: "day",
      facts: "浏览器明确来源中的合成事实。",
      feelings: "独立标注的感受。",
    });
  else if (system.includes('"html"'))
    content = JSON.stringify({
      html: '<!doctype html><html><body><h1>合成插画</h1><script>parent.document.body.dataset.unsafe="executed"</script></body></html>',
    });
  else if (system.includes('"summary"'))
    content = JSON.stringify({ summary: "合成会话摘要。" });
  else if (system.includes('"rule"'))
    content = JSON.stringify({
      title: "合成规划提醒",
      rule: { type: "once", after_seconds: 60 },
      action: { type: "text", content: "待单独启用的提醒" },
      enabled: false,
    });
  if (response.destroyed) return;
  response.writeHead(200, { "content-type": "application/json" });
  response.end(
    JSON.stringify({
      choices: [{ message: { content }, finish_reason: "stop" }],
    }),
  );
});
await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const modelUrl = `http://127.0.0.1:${provider.address().port}`;
const root = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? ".");
const { startServer } = await import(
  pathToFileURL(resolve(root, "apps/server/dist/server.js")).href
);
const { bootstrapToken } = await import(
  pathToFileURL(resolve(root, "apps/server/dist/auth.js")).href
);
const dataDir = resolve(`.local/prometheus/e2e/${randomUUID()}`);
const app = await startServer({
  dataDir,
  webDir: resolve(root, "apps/admin/dist"),
  host: "127.0.0.1",
  port: 18449,
  origin: "http://127.0.0.1:18449",
  secureCookie: false,
  allowLoopbackModel: true,
  testLocalMediaRunner: async (kind, _input, _options, signal) => {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(resolve, 250);
      signal.addEventListener(
        "abort",
        () => {
          clearTimeout(timer);
          reject(Error("synthetic cancelled"));
        },
        { once: true },
      );
    });
    return {
      receipt: {
        status: "completed",
        kind,
        isolation: { app_container: 1, capabilities: 0, in_job: 1 },
        result: { provider: "synthetic-browser-Piper" },
      },
      files: new Map([["speech.wav", speechWave()]]),
      isolation: {
        active_processes: 0,
        peak_memory_bytes: 1048576,
        cpu_100ns: 100000,
      },
      elapsed_ms: 250,
    };
  },
  testChannelEndpoints: {
    qq: modelUrl,
    weixin: modelUrl,
    qqUpload: modelUrl,
    weixinCdn: modelUrl,
  },
});
const tokenFile = bootstrapToken(dataDir);
const stateFile = resolve(".local/prometheus/e2e-state.json");
// This state only contains fixture file locations, never token values.
writeFileSync(
  stateFile,
  JSON.stringify({
    dataDir,
    tokenFile,
    modelUrl,
    commit: JSON.parse(
      readFileSync(resolve(root, "apps/server/dist/assets/build.json"), "utf8"),
    ).commit,
  }),
);
const close = async () => {
  await app.close();
  await new Promise((resolve) => {
    provider.closeAllConnections();
    provider.close(resolve);
  });
  process.exitCode = 0;
};
process.once("SIGINT", close);
process.once("SIGTERM", close);
console.log("Prometheus synthetic browser fixture ready on loopback.");
