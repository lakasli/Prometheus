import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { fixture, until } from "./helpers.mjs";
import { now } from "../dist/security.js";
import { isWorkday } from "../dist/automation.js";

async function model(t, handler) {
  const calls = [];
  const server = createServer(async (req, res) => {
    let data = "";
    for await (const chunk of req) data += chunk;
    const value = JSON.parse(data);
    calls.push(value);
    const output = await handler(value);
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify({
        choices: [
          {
            message: {
              content:
                typeof output === "string" ? output : JSON.stringify(output),
            },
          },
        ],
      }),
    );
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(
    () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(resolve);
      }),
  );
  return { url: `http://127.0.0.1:${server.address().port}`, calls };
}
async function messages(app, count = 2) {
  const conversation = (
    await app.call("/conversations", "POST", { title: "合成来源" })
  ).data;
  const owner = (await app.call("/auth/me")).data.user.id;
  const ids = [];
  for (let index = 0; index < count; index++) {
    const run = randomUUID(),
      id = randomUUID();
    app.store.run(
      "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,'completed','合成回执',?,?)",
      run,
      conversation.id,
      owner,
      randomUUID(),
      `来源${index}`,
      now(),
      now(),
    );
    app.store.run(
      "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
      id,
      conversation.id,
      run,
      `合成事实${index}`,
      now(),
    );
    ids.push(id);
  }
  return { conversation, owner, ids };
}
test("forgetting a reviewed fact blocks its old raw messages from being extracted again while preserving history", async (t) => {
  const remote = await model(t, () => ({
      candidates: [{ content: "仅测试的事实" }],
    })),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const source = await messages(app, 1);
  const created = await app.call("/knowledge/jobs", "POST", {
    kind: "memory_candidates",
    sources: source.ids.map((id) => ({ type: "message", id })),
  });
  const ready = await until(
    () => app.call(`/knowledge/jobs/${created.data.id}`),
    (v) => v.data.status === "review_pending",
  );
  const approved = await app.call(
    `/knowledge/jobs/${created.data.id}/review`,
    "POST",
    { approve: true },
    { headers: { "if-match": `"${ready.data.revision}"` } },
  );
  const record = approved.data.saved_ids[0];
  assert.equal(
    (
      await app.call(`/records/memory/${record}`, "DELETE", undefined, {
        headers: { "if-match": '"1"' },
      })
    ).status,
    200,
  );
  const retry = await app.call("/knowledge/jobs", "POST", {
    kind: "memory_candidates",
    sources: source.ids.map((id) => ({ type: "message", id })),
  });
  assert.equal(retry.status, 409);
  const history = await app.call(
    `/conversations/${source.conversation.id}/messages`,
  );
  assert.equal(history.data.items.length, 1);
  const picker = await app.call(
    `/knowledge/source/messages?source_key=${source.conversation.id}`,
  );
  assert.equal(picker.data.items[0].eligible, false);
});
test("model trigger proposals become disabled validated drafts after review and never run from model approval alone", async (t) => {
  const remote = await model(t, () => ({
      title: "合成模型提醒",
      rule: { type: "once", after_seconds: 60 },
      action: { type: "text", content: "仅本地提醒" },
      enabled: true,
    })),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const created = await app.call("/knowledge/jobs", "POST", {
    kind: "trigger_draft",
    sources: [],
    options: { request: "一分钟后提醒" },
  });
  const ready = await until(
    () => app.call(`/knowledge/jobs/${created.data.id}`),
    (v) => v.data.status === "review_pending",
  );
  assert.equal(ready.data.output.enabled, false);
  const saved = await app.call(
    `/knowledge/jobs/${created.data.id}/review`,
    "POST",
    { approve: true },
    { headers: { "if-match": `"${ready.data.revision}"` } },
  );
  assert.equal(saved.status, 200);
  const rules = (await app.call("/automation/triggers")).data.items;
  assert.equal(rules.length, 1);
  assert.equal(rules[0].enabled, false);
  assert.equal(rules[0].next_at, null);
});
test("memory candidates require explicit sources and review; approval stays private and deletion clears derived data", async (t) => {
  const remote = await model(t, () => ({
    candidates: [{ content: "明确来源中的合成事实" }],
  }));
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const source = await messages(app);
  const job = await app.call("/knowledge/jobs", "POST", {
    kind: "memory_candidates",
    sources: source.ids.map((id) => ({ type: "message", id })),
    idempotency_key: "candidate-case",
  });
  assert.equal(job.status, 202);
  const ready = await until(
    () => app.call(`/knowledge/jobs/${job.data.id}`),
    (value) => value.data.status === "review_pending",
  );
  assert.equal((await app.call("/records/memory")).data.total, 0);
  const approved = await app.call(
    `/knowledge/jobs/${job.data.id}/review`,
    "POST",
    { approve: true },
    { headers: { "if-match": `"${ready.data.revision}"` } },
  );
  assert.equal(approved.status, 200);
  assert.equal(approved.data.output, null);
  const memories = (await app.call("/records/memory")).data.items;
  assert.equal(memories.length, 1);
  assert.equal(memories[0].visibility, "private");
  await app.call(`/records/memory/${memories[0].id}`, "DELETE", undefined, {
    headers: { "if-match": '"1"' },
  });
  assert.equal(
    (await app.call(`/knowledge/jobs/${job.data.id}`)).data.output,
    null,
  );
});
test("multi-batch diary stores resumable cumulative output and references all selected sources", async (t) => {
  const remote = await model(t, () => ({
    title: "累计合成日记",
    date: "2026-10-30",
    period: "day",
    facts: "本轮来源中的事实。",
    feelings: "感受独立标记。",
  }));
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const source = await messages(app, 20);
  const created = await app.call("/knowledge/jobs", "POST", {
    kind: "diary",
    sources: source.ids.map((id) => ({ type: "message", id })),
    options: { date: "2026-10-30", period: "day" },
  });
  const ready = await until(
    () => app.call(`/knowledge/jobs/${created.data.id}`),
    (value) => value.data.status === "review_pending",
  );
  assert.equal(ready.data.progress.calls, 2);
  assert.equal(ready.data.progress.processed, 20);
  assert.equal(remote.calls.length, 2);
  assert.equal(
    JSON.parse(remote.calls[1].messages[1].content).prior_fragments.length,
    1,
  );
  const approved = await app.call(
    `/knowledge/jobs/${created.data.id}/review`,
    "POST",
    { approve: true },
    { headers: { "if-match": `"${ready.data.revision}"` } },
  );
  assert.equal(approved.status, 200);
  assert.equal(app.store.get("SELECT count(*) AS n FROM derivations").n, 20);
});
test("source-model permission is independent of capture and revocation invalidates pending results", async (t) => {
  const remote = await model(t, () => ({ summary: "合成摘要" }));
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const source = await messages(app);
  const grant = await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: source.conversation.id,
      grant: {
        capture: true,
        allow_model: true,
        can_sleep: false,
        can_wake: false,
        expires_at: null,
      },
    },
    { headers: { "if-match": '"0"' } },
  );
  assert.equal(grant.status, 200);
  const created = await app.call("/knowledge/jobs", "POST", {
    kind: "summary",
    sources: source.ids.map((id) => ({ type: "message", id })),
  });
  await until(
    () => app.call(`/knowledge/jobs/${created.data.id}`),
    (value) => value.data.status === "completed",
  );
  assert.ok(app.features.knowledge.summary(source.conversation.id));
  const revoke = await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: source.conversation.id,
      grant: {
        capture: false,
        allow_model: false,
        can_sleep: false,
        can_wake: false,
        expires_at: null,
      },
    },
    { headers: { "if-match": '"1"' } },
  );
  assert.equal(revoke.status, 200);
  assert.equal(app.features.knowledge.summary(source.conversation.id), "");
  assert.equal(
    (await app.call(`/knowledge/jobs/${created.data.id}`)).data.output,
    null,
  );
});
test("trigger drafts are disabled until preview and approval; persisted firings are unique and authenticated", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const created = await app.call("/automation/triggers", "POST", {
    rule: { type: "once", after_seconds: 1 },
    action: { type: "text", title: "合成提醒", content: "唯一的本地提醒" },
  });
  assert.equal(created.status, 201);
  assert.equal(created.data.enabled, false);
  const preview = (
    await app.call(`/automation/triggers/${created.data.id}/preview`)
  ).data;
  assert.equal(preview.external_send, false);
  assert.equal(
    (
      await app.call(
        `/automation/triggers/${created.data.id}/enable`,
        "PUT",
        { enabled: true, preview_digest: "wrong" },
        { headers: { "if-match": '"1"' } },
      )
    ).status,
    400,
  );
  assert.equal(
    (
      await app.call(
        `/automation/triggers/${created.data.id}/enable`,
        "PUT",
        { enabled: true, preview_digest: preview.digest },
        { headers: { "if-match": '"1"' } },
      )
    ).status,
    200,
  );
  await until(
    () => app.call("/automation/results"),
    (value) =>
      value.data.items.length === 1 &&
      value.data.items[0].status === "completed",
    4000,
  );
  assert.equal(
    (await app.call("/automation/results")).data.items[0].result,
    "唯一的本地提醒",
  );
  assert.equal((await app.call("/channels/deliveries")).data.items.length, 0);
  assert.equal(isWorkday("2026-10-10"), true);
  assert.equal(isWorkday("2026-10-01"), false);
  assert.throws(() => isWorkday("2027-01-01"));
});
test("sleep preserves the original wake deadline across restart and allows independent wake", async () => {
  const app = await fixture();
  await app.bootstrap();
  const policy = (await app.call("/rest/policy")).data;
  policy.value.drain_seconds = 0;
  policy.value.default_sleep_seconds = 3;
  assert.equal(
    (
      await app.call("/rest/policy", "PUT", policy.value, {
        headers: { "if-match": '"0"' },
      })
    ).status,
    200,
  );
  const control = await app.call(
    "/rest/control",
    "POST",
    { action: "sleep", request_id: "sleep-restart", duration_seconds: 3 },
    { headers: { "if-match": '"0"' } },
  );
  assert.equal(control.status, 200);
  const deadline = control.data.state.wake_at;
  await app.close();
  const restarted = await fixture({ dataDir: app.dataDir });
  try {
    assert.equal(
      restarted.features.automation.restStatus().state.wake_at,
      deadline,
    );
    await until(
      async () => restarted.features.automation.restStatus(),
      (value) => value.state.phase === "awake",
      5000,
    );
    assert.equal(
      restarted.store.get("SELECT wake_at FROM rest_cycles").wake_at,
      deadline,
    );
  } finally {
    await restarted.close();
  }
});
