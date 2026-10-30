import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { fixture, until } from "./helpers.mjs";
import { now } from "../dist/security.js";

test("full-text searches keep literal query semantics, owner scope and correction/forgetting visibility", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const created = await app.call("/records/memory", "POST", {
    data: { content: '中文全文搜索，保留 "NOT" 和 café' },
  });
  const owner = (await app.call("/auth/me")).data.user.id;
  app.store.run(
    "INSERT INTO records VALUES(?,'memory',?,'private',1,?,?,?,NULL)",
    randomUUID(),
    "other-owner",
    JSON.stringify({ content: "中文全文搜索 私有内容" }),
    now(),
    now(),
  );
  for (const query of ["中文全文", '"NOT"', "NOT", "café", "全文"]) {
    const result = await app.call(
      `/records/memory?q=${encodeURIComponent(query)}`,
    );
    assert.equal(result.status, 200);
    assert.equal(result.data.total, 1, query);
    assert.equal(result.data.items[0].owner_id, owner);
  }
  const corrected = await app.call(
    `/records/memory/${created.data.id}`,
    "PUT",
    { data: { content: "更正后的新事实" } },
    { headers: { "if-match": '"1"' } },
  );
  assert.equal(corrected.status, 200);
  assert.equal(
    (await app.call(`/records/memory?q=${encodeURIComponent("中文全文")}`)).data
      .total,
    0,
  );
  assert.equal(
    (await app.call(`/records/memory?q=${encodeURIComponent("新的")}`)).data
      .total,
    0,
  );
  const visible = await app.call(
    `/records/memory?q=${encodeURIComponent("新事实")}`,
  );
  assert.equal(visible.data.total, 1);
  await app.call(`/records/memory/${created.data.id}`, "DELETE", undefined, {
    headers: { "if-match": '"2"' },
  });
  assert.equal(
    (await app.call(`/records/memory?q=${encodeURIComponent("新事实")}`)).data
      .total,
    0,
  );
});

async function provider(t, output) {
  let calls = 0;
  const server = createServer(async (request, response) => {
    for await (const chunk of request) void chunk;
    calls++;
    response.writeHead(200, { "content-type": "application/json" });
    response.end(
      JSON.stringify({
        choices: [{ message: { content: JSON.stringify(output) } }],
      }),
    );
  });
  await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
  t.after(
    () =>
      new Promise((closed) => {
        server.closeAllConnections();
        server.close(closed);
      }),
  );
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    get calls() {
      return calls;
    },
  };
}

test("pre-dispatch budget failures become retryable failures without issuing a model request", async (t) => {
  const remote = await provider(t, { html: "<svg></svg>" });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const date = new Intl.DateTimeFormat("en-CA", {
    timeZone: "Asia/Shanghai",
  }).format(new Date());
  app.features.knowledge.setPolicy(`budget:${date}`, 0, { calls: 32 });
  const queued = await app.call("/knowledge/jobs", "POST", {
    kind: "drawing_html",
    sources: [],
    options: { request: "合成图" },
  });
  assert.equal(queued.status, 202);
  const failed = await until(
    () => app.call(`/knowledge/jobs/${queued.data.id}`),
    (value) => value.data.status === "failed",
  );
  assert.match(failed.data.error, /预算/);
  assert.ok(failed.data.revision > queued.data.revision);
  assert.equal(remote.calls, 0);
  app.features.knowledge.setPolicy(`budget:${date}`, 1, { calls: 0 });
  const retried = await app.call(
    `/knowledge/jobs/${queued.data.id}/retry`,
    "POST",
    {},
    { headers: { "if-match": `"${failed.data.revision}"` } },
  );
  assert.equal(retried.status, 200);
  await until(
    () => app.call(`/knowledge/jobs/${queued.data.id}`),
    (value) => value.data.status === "review_pending",
  );
  assert.equal(remote.calls, 1);
});

test("multi-batch diary approval preserves more than sixteen record references and recursive forgetting", async (t) => {
  const remote = await provider(t, {
    title: "跨批日记",
    date: "2026-10-31",
    period: "day",
    facts: "来源事实",
    feelings: "感受",
  });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const ids = [];
  for (let index = 0; index < 20; index++) {
    const result = await app.call("/records/memory", "POST", {
      data: { content: `合成事实${index}` },
    });
    assert.equal(result.status, 201);
    ids.push(result.data.id);
  }
  const created = await app.call("/knowledge/jobs", "POST", {
    kind: "diary",
    sources: ids.map((id) => ({ type: "record", id })),
    options: { date: "2026-10-31", period: "day" },
  });
  assert.equal(created.status, 202);
  const ready = await until(
    () => app.call(`/knowledge/jobs/${created.data.id}`),
    (value) => value.data.status === "review_pending",
  );
  assert.equal(ready.data.progress.calls, 2);
  const approved = await app.call(
    `/knowledge/jobs/${created.data.id}/review`,
    "POST",
    { approve: true },
    { headers: { "if-match": `"${ready.data.revision}"` } },
  );
  assert.equal(approved.status, 200);
  const diary = (await app.call(`/records/diary/${approved.data.saved_ids[0]}`))
    .data;
  assert.deepEqual(diary.data.sources, ids);
  assert.equal(diary.data.generator, "model_reviewed");
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM derivations WHERE record_id=?",
      diary.id,
    ).n,
    20,
  );
  await app.call(`/records/memory/${ids[19]}`, "DELETE", undefined, {
    headers: { "if-match": '"1"' },
  });
  assert.equal((await app.call(`/records/diary/${diary.id}`)).status, 404);
});

test("an expired explicit grant rejects even the owner's source before enqueue", async (t) => {
  const remote = await provider(t, { summary: "must not be requested" });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const owner = (await app.call("/auth/me")).data.user.id;
  const run = randomUUID(),
    message = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,'completed',?,?)",
    run,
    conversation.id,
    owner,
    "expired-grant",
    "合成消息",
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
    message,
    conversation.id,
    run,
    "合成消息",
    now(),
  );
  app.store.run(
    "INSERT INTO source_grants VALUES(?,1,0,1,0,0,?,0,?)",
    conversation.id,
    now() - 1,
    now(),
  );
  assert.throws(
    () =>
      app.features.knowledge.enqueue(owner, {
        kind: "summary",
        sources: [{ type: "message", id: message }],
      }),
    (error) => error.code === "source_model_forbidden",
  );
  assert.equal(remote.calls, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM model_jobs").n, 0);
});

test("forgetting facts permanently suppresses unsent replies across sleep context resets", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const owner = (await app.call("/auth/me")).data.user.id;
  const run = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,'completed',?,?,?)",
    run,
    conversation.id,
    owner,
    "stale-facts",
    "旧输入",
    "包含已遗忘事实的候选",
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO run_targets VALUES(?,'qq',?,'synthetic-key',?,'123')",
    run,
    app.store.seal({ kind: "c2c", target: "synthetic", message_id: "old" }),
    now() + 60,
  );
  app.runtime.invalidate(owner, { action: "forgotten", id: randomUUID() });
  app.runtime.resetConversationContexts();
  assert.equal(
    app.store.get("SELECT state FROM run_publications WHERE run_id=?", run)
      .state,
    "suppressed",
  );
  await new Promise((ready) => setTimeout(ready, 450));
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM delivery_parts WHERE run_id=?",
      run,
    ).n,
    0,
  );
});
