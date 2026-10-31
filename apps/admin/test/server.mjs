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
