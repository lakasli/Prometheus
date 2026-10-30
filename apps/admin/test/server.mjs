import { resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";
import { readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
const provider = createServer(async (request, response) => {
  let raw = "";
  for await (const chunk of request) raw += chunk;
  const input = JSON.parse(raw);
  const system = String(input.messages?.[0]?.content ?? "");
  let content = "合成浏览器回复。";
  if (system.includes('"candidates"'))
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
  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify({ choices: [{ message: { content } }] }));
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
