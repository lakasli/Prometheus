import assert from "node:assert/strict";
import { createServer } from "node:http";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { platform, arch, release } from "node:os";
const runtimeRoot = resolve(process.argv[2] ?? "");
const manifest = JSON.parse(
  readFileSync(join(runtimeRoot, "release.json"), "utf8").replace(
    /^\ufeff/,
    "",
  ),
);
assert.match(manifest.commit, /^[0-9a-f]{40}$/);
for (const file of manifest.files) {
  const target = resolve(runtimeRoot, file.path);
  assert.ok(
    target.toLowerCase().startsWith(runtimeRoot.toLowerCase() + "\\") ||
      target.startsWith(runtimeRoot + "/"),
  );
  assert.equal(
    createHash("sha256").update(readFileSync(target)).digest("hex"),
    file.sha256,
    file.path,
  );
}
const { startServer, build } = await import(
  pathToFileURL(join(runtimeRoot, "apps/server/dist/server.js")).href
);
const { bootstrapToken } = await import(
  pathToFileURL(join(runtimeRoot, "apps/server/dist/auth.js")).href
);
const { privateDirectory } = await import(
  pathToFileURL(join(runtimeRoot, "apps/server/dist/security.js")).href
);
assert.equal(build.commit, manifest.commit);
const provider = createServer(async (request, response) => {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  const input = JSON.parse(Buffer.concat(chunks));
  const instruction = String(input.messages[0]?.content ?? "");
  const generated = instruction.includes('"candidates"')
    ? JSON.stringify({ candidates: [{ content: "有来源的固定制品合成事实" }] })
    : null;
  response.writeHead(200, { "content-type": "application/json" });
  response.end(
    JSON.stringify({
      choices: [
        {
          message: {
            content: generated ?? `合成响应：${input.messages.at(-1).content}`,
          },
        },
      ],
    }),
  );
});
await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const dataDir = resolve(
  `.local/prometheus/release-validation/${manifest.commit}/${randomUUID()}`,
);
const basePort = 18459,
  origin = `http://127.0.0.1:${basePort}`;
const app = await startServer({
  dataDir,
  webDir: join(runtimeRoot, "apps/admin/dist"),
  host: "127.0.0.1",
  port: basePort,
  origin,
  secureCookie: false,
  allowLoopbackModel: true,
});
const results = [];
let cookie = "",
  csrf = "";
const call = async (path, method = "GET", data, headers = {}) => {
  const response = await fetch(`${origin}/api/admin/v1${path}`, {
    method,
    headers: {
      origin,
      cookie,
      "x-csrf-token": csrf,
      ...(data === undefined ? {} : { "content-type": "application/json" }),
      ...headers,
    },
    body: data === undefined ? undefined : JSON.stringify(data),
    signal: AbortSignal.timeout(10000),
  });
  const body = await response.json();
  if (response.headers.get("set-cookie"))
    cookie = response.headers.get("set-cookie").split(";")[0];
  if (body.csrf_token) csrf = body.csrf_token;
  return { status: response.status, data: body };
};
try {
  for (const endpoint of ["/health/live", "/health/ready"]) {
    const response = await fetch(`${origin}${endpoint}`);
    const body = await response.json();
    assert.equal(response.status, 200);
    assert.equal(body.commit, manifest.commit);
    results.push(`${endpoint}:same-sha`);
  }
  assert.equal((await call("/status")).status, 401);
  results.push("unauthenticated-api-denied");
  const tokenPath = bootstrapToken(dataDir),
    token = JSON.parse(readFileSync(tokenPath, "utf8")).token;
  assert.equal(
    (
      await call("/auth/bootstrap", "POST", {
        username: "release-test",
        password: "Release-synthetic-password-2026!",
        bootstrap_token: token,
      })
    ).status,
    201,
  );
  assert.equal(existsSync(tokenPath), false);
  results.push("one-time-bootstrap-and-session");
  const secret = (
    await call("/secrets", "POST", {
      kind: "model_api_key",
      value: "release-synthetic-key",
    })
  ).data;
  const config = (await call("/config")).data;
  config.config.model = {
    model: "synthetic",
    base_url: `http://127.0.0.1:${provider.address().port}`,
    credential_ref: secret.id,
  };
  assert.equal(
    (
      await call(
        "/config",
        "PUT",
        { config: config.config },
        { "if-match": '"1"' },
      )
    ).status,
    200,
  );
  results.push("encrypted-secret-and-config-apply");
  const memory = (
    await call("/records/memory", "POST", {
      data: { content: "release-synthetic-memory" },
    })
  ).data;
  assert.equal(memory.revision, 1);
  results.push("persistent-private-memory");
  const conversation = (
    await call("/conversations", "POST", { title: "发布验收" })
  ).data;
  const run = (
    await call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "固定版本文字链路",
      client_message_id: randomUUID(),
    })
  ).data;
  let outcome;
  for (let index = 0; index < 100; index++) {
    outcome = (await call(`/runs/${run.run_id}`)).data;
    if (!["queued", "running"].includes(outcome.status)) break;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.equal(outcome.status, "completed");
  assert.equal(outcome.response, "合成响应：固定版本文字链路");
  results.push("synthetic-model-roundtrip-not-external-provider");
  const recordKinds = {
    diary: {
      title: "合成日记",
      content: "有明确来源",
      date: "2026-10-30",
      sources: [memory.id],
    },
    schedule: {
      title: "合成日程",
      start_at: 1793361600,
      end_at: 1793365200,
      status: "planned",
    },
    people: { name: "合成人物", notes: "发布测试" },
    skills: { name: "合成方法", instructions: "返回简短文字", enabled: true },
  };
  for (const [kind, data] of Object.entries(recordKinds)) {
    assert.equal(
      (await call(`/records/${kind}`, "POST", { data })).status,
      201,
    );
    results.push(`${kind}-persisted`);
  }
  for (const path of [
    "/plugins/execute",
    "/speech/synthesize",
    "/harness/start",
  ])
    assert.equal((await call(path, "POST", {})).status, 503);
  results.push("worker-gates-denied");
  const sourceMessages = (
    await call(`/conversations/${conversation.id}/messages`)
  ).data.items;
  const job = (
    await call("/knowledge/jobs", "POST", {
      kind: "memory_candidates",
      sources: [{ type: "message", id: sourceMessages[0].id }],
      idempotency_key: "fixed-artifact-candidate",
    })
  ).data;
  let ready;
  for (let index = 0; index < 100; index++) {
    ready = (await call(`/knowledge/jobs/${job.id}`)).data;
    if (ready.status === "review_pending") break;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.equal(ready.status, "review_pending");
  const reviewed = await call(
    `/knowledge/jobs/${job.id}/review`,
    "POST",
    { approve: true },
    { "if-match": `"${ready.revision}"` },
  );
  assert.equal(reviewed.status, 200);
  assert.equal(reviewed.data.saved_ids.length, 1);
  results.push("explicit-source-candidate-reviewed-private");
  const trigger = (
    await call("/automation/triggers", "POST", {
      rule: { type: "once", after_seconds: 1 },
      action: { type: "text", title: "制品提醒", content: "仅认证本地结果" },
    })
  ).data;
  const preview = (await call(`/automation/triggers/${trigger.id}/preview`))
    .data;
  assert.equal(
    (
      await call(
        `/automation/triggers/${trigger.id}/enable`,
        "PUT",
        { enabled: true, preview_digest: preview.digest },
        { "if-match": '"1"' },
      )
    ).status,
    200,
  );
  let firings;
  for (let index = 0; index < 120; index++) {
    firings = (await call("/automation/results")).data.items;
    if (firings[0]?.status === "completed") break;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  assert.equal(firings[0]?.result, "仅认证本地结果");
  results.push("durable-trigger-preview-and-authenticated-result");
  const asleep = await call(
    "/rest/control",
    "POST",
    { action: "sleep", duration_seconds: 60, request_id: "release-sleep" },
    { "if-match": '"0"' },
  );
  assert.equal(asleep.status, 200);
  const rest = (await call("/rest/status")).data;
  assert.equal(
    (
      await call(
        "/rest/control",
        "POST",
        { action: "wake", request_id: "release-wake" },
        { "if-match": `"${rest.state.revision}"` },
      )
    ).status,
    200,
  );
  assert.equal((await call("/rest/status")).data.state.phase, "awake");
  results.push("rest-sleep-wake-with-version");
  assert.equal((await call("/qq/api/catalog")).data.operations.length, 77);
  assert.equal((await call("/channels/status")).status, 200);
  results.push("channel-control-and-77-operation-catalog");
  assert.equal((await call("/context/policy")).status, 200);
  assert.equal((await call("/media/policy")).data.value.vision_enabled, false);
  results.push("context-and-opt-in-media-policy");
  const state = JSON.stringify((await call("/export")).data);
  assert.equal(state.includes("release-synthetic-key"), false);
  results.push("credential-free-export");
  const reportDir = privateDirectory(
    resolve(".local/prometheus/release-validation/reports"),
  );
  const report = {
    name: "Prometheus",
    commit: manifest.commit,
    tested_at: new Date().toISOString(),
    timezone: "Asia/Shanghai",
    windows: {
      os: platform(),
      version: release(),
      architecture: arch(),
      node: process.version,
    },
    data_directory: dataDir,
    startup: "passed",
    live: "passed",
    ready: "passed",
    results,
    external_model: "not-executed",
    real_qq_weixin: "not-executed",
    previous_verified_windows_version:
      manifest.previous_verified_windows_version,
  };
  const reportPath = join(reportDir, `${manifest.commit}.json`);
  writeFileSync(reportPath, JSON.stringify(report, null, 2));
  console.log(
    JSON.stringify(
      {
        report: reportPath,
        commit: manifest.commit,
        checks: results.length,
        status: "passed",
      },
      null,
      2,
    ),
  );
} finally {
  await app.close();
  await new Promise((resolve) => {
    provider.closeAllConnections();
    provider.close(resolve);
  });
}
