import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fileURLToPath } from "node:url";
import { Store } from "../dist/store.js";
import { now, privateDirectory } from "../dist/security.js";
import { fixture, until } from "./helpers.mjs";

async function provider(t) {
  const calls = [];
  const server = createServer(async (request, response) => {
    let input = "";
    for await (const chunk of request) input += chunk;
    calls.push(JSON.parse(input));
    response.writeHead(200, { "content-type": "application/json" });
    response.end(
      JSON.stringify({
        choices: [
          {
            message: {
              content: "合成输出，synthetic-provider-credential 必须隐藏",
            },
          },
        ],
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
  return { calls, url: `http://127.0.0.1:${server.address().port}` };
}
function completed(app, owner, text = "合成输入") {
  const conversation = app.runtime.createConversation(owner, "日志夹具"),
    id = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,'completed','合成输出',?,?)",
    id,
    conversation.id,
    owner,
    randomUUID(),
    text,
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
    randomUUID(),
    conversation.id,
    id,
    text,
    now(),
  );
  return { run: id, conversation: conversation.id };
}

test("journal filters paginate within authorization and never dereference another owner's run", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const owner = (await app.call("/auth/me")).data.user.id;
  const foreign = completed(app, "another-owner", "private-other-message");
  app.store.audit(owner, "run", "completed", "伪造引用夹具", foreign.run);
  app.store.audit(
    "another-owner",
    "diary",
    "completed",
    "literal 100%_ hidden",
  );
  for (let i = 0; i < 4; i++)
    app.store.audit(owner, "diary", "completed", `literal 100%_ visible ${i}`);
  app.store.audit(owner, "diary", "failed", "literal 100%_ failure");
  const ids = [];
  let cursor;
  do {
    const response = await app.call(
      `/runtime-logs?kind=diary&status=completed&q=${encodeURIComponent("100%_")}&limit=2${cursor ? `&cursor=${cursor}` : ""}`,
    );
    assert.equal(response.status, 200);
    assert.ok(
      response.data.items.every((item) => item.title.includes("visible")),
    );
    ids.push(...response.data.items.map((item) => item.id));
    cursor = response.data.next_cursor;
  } while (cursor);
  assert.equal(ids.length, 4);
  assert.equal(new Set(ids).size, 4);
  const content = (await app.call("/runtime-logs")).data;
  assert.equal(
    JSON.stringify(content).includes("private-other-message"),
    false,
  );
  assert.equal(
    content.items.find((item) => item.title === "伪造引用夹具").content,
    "",
  );
  const channel = completed(
    app,
    "qq:123:c2c:synthetic",
    "authorized-instance-channel",
  );
  const visible = await app.call(`/runtime-logs?run_id=${channel.run}`);
  assert.equal(visible.data.items[0].source, "qq");
  assert.equal(visible.data.items[0].content, "authorized-instance-channel");
  assert.equal(
    (await app.call("/runtime-logs", "GET", undefined, { auth: false })).status,
    401,
  );
  for (const invalid of [
    "limit=0",
    "cursor=-1",
    "kind=unsupported",
    "q=" + "x".repeat(201),
    "kind=run&kind=diary",
    "unknown=1",
  ])
    assert.equal((await app.call(`/runtime-logs?${invalid}`)).status, 400);
});

test("journal resolves actual inputs, outputs and model snapshots, redacts secrets and erases forgotten context", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const fact = (
    await app.call("/records/memory", "POST", {
      data: { content: "unique-journal-fact-to-forget" },
    })
  ).data;
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const run = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "ordinary-input synthetic-provider-credential",
      client_message_id: "journal",
    })
  ).data;
  await until(
    () => app.call(`/runs/${run.run_id}`),
    (response) => response.data.status === "completed",
  );
  const logs = await app.call(`/runtime-logs?run_id=${run.run_id}`);
  for (const kind of ["input", "output", "context", "run"])
    assert.ok(logs.data.items.some((item) => item.kind === kind));
  const context = logs.data.items.find((item) => item.kind === "context");
  assert.ok(context.content.includes("unique-journal-fact-to-forget"));
  assert.equal(context.details.exact_tokens, false);
  assert.equal(
    JSON.stringify(logs.data).includes("synthetic-provider-credential"),
    false,
  );
  assert.equal(
    app.store
      .get(
        "SELECT messages_json FROM prompt_snapshots WHERE run_id=?",
        run.run_id,
      )
      .messages_json.includes("synthetic-provider-credential"),
    false,
  );
  await app.call(`/records/memory/${fact.id}`, "DELETE", undefined, {
    headers: { "if-match": '"1"' },
  });
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM prompt_snapshots WHERE run_id=?",
      run.run_id,
    ).n,
    0,
  );
  const invalidated = (await app.call(`/runtime-logs?run_id=${run.run_id}`))
    .data;
  assert.equal(
    JSON.stringify(invalidated).includes("unique-journal-fact-to-forget"),
    false,
  );
  assert.equal(JSON.stringify(invalidated).includes("ordinary-input"), false);
  assert.ok(invalidated.items.every((item) => /失效/.test(item.content)));
  assert.ok(
    app.store
      .get(
        "SELECT content FROM messages WHERE run_id=? AND role='user'",
        run.run_id,
      )
      .content.includes("ordinary-input"),
  );
});

test("invalidation while awaiting vision cannot repopulate a stale prompt snapshot", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  let entered = false,
    resume;
  app.runtime.describeMedia = async () => {
    entered = true;
    await new Promise((resolve) => {
      resume = resolve;
    });
    return "old-visual-description";
  };
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const run = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "wait-for-description",
      client_message_id: "vision-race",
    })
  ).data;
  await until(() => Promise.resolve(entered), Boolean);
  await app.call("/records/memory", "POST", { data: { content: "新上下文" } });
  resume();
  await until(
    () => Promise.resolve(app.runtime.runningCount),
    (count) => count === 0,
  );
  assert.equal(
    (await app.call(`/runs/${run.run_id}`)).data.status,
    "cancelled",
  );
  assert.equal(remote.calls.length, 0);
  assert.equal(
    (
      await app.call(
        `/runtime-logs?kind=run&status=cancelled&run_id=${run.run_id}`,
      )
    ).data.items.length,
    1,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM prompt_snapshots").n,
    0,
  );
});

test("pre-dispatch context refresh failures terminate the queued run without a model request", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  let refreshes = 0;
  app.runtime.refreshSourceGrants = () => {
    if (++refreshes > 1) throw new Error("synthetic refresh failure");
  };
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const queued = await app.call(
    `/conversations/${conversation.id}/messages`,
    "POST",
    { content: "refresh failure", client_message_id: "pre-dispatch" },
  );
  assert.equal(queued.status, 202);
  await until(
    () => app.call(`/runs/${queued.data.run_id}`),
    (value) => value.data.status === "failed",
  );
  assert.equal(remote.calls.length, 0);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM runs WHERE status='queued'").n,
    0,
  );
});

test("journal retention caps metadata and removes the corresponding context without deleting original messages", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const owner = (await app.call("/auth/me")).data.user.id;
  const run = completed(app, owner);
  app.store.run(
    "INSERT INTO prompt_snapshots VALUES(?,?,0,'[]',?)",
    run.run,
    owner,
    now(),
  );
  app.store.audit(owner, "context", "recorded", "oldest context", run.run);
  const insert = app.store.database.prepare(
    "INSERT INTO audit_events(owner_id,kind,status,title,created_at) VALUES(?,'system','recorded',?,?)",
  );
  app.store.transaction(() => {
    for (let i = 0; i < 20001; i++)
      insert.run(owner, i === 2 ? "older-search-target" : `event-${i}`, now());
  });
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM audit_events").n,
    20000,
  );
  let page = (await app.call("/runtime-logs?q=older-search-target")).data;
  assert.equal(page.items.length, 0);
  assert.ok(page.next_cursor);
  for (let scan = 0; !page.items.length && page.next_cursor && scan < 6; scan++)
    page = (
      await app.call(
        `/runtime-logs?q=older-search-target&cursor=${page.next_cursor}`,
      )
    ).data;
  assert.equal(page.items[0]?.title, "older-search-target");
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM prompt_snapshots").n,
    0,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM messages WHERE run_id=?", run.run)
      .n,
    1,
  );
  insert.run(owner, "expired", now() - 31 * 86400);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM audit_events WHERE title='expired'",
    ).n,
    0,
  );
});

test("expired grants invalidate reads without the background timer and renewal excludes messages from the lapse", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const owner = (await app.call("/auth/me")).data.user.id;
  const source = completed(app, owner);
  app.features.knowledge.setGrant(owner, source.conversation, 0, {
    capture: true,
    allow_model: true,
    can_sleep: false,
    can_wake: false,
    expires_at: now() + 600,
  });
  const fact = (
    await app.call("/records/memory", "POST", {
      data: { content: "grant-derived-unique-fact" },
    })
  ).data;
  app.store.run(
    "INSERT INTO derivations VALUES(?,'grant',?,1)",
    fact.id,
    source.conversation,
  );
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const run = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "authorized context",
      client_message_id: "grant-journal",
    })
  ).data;
  await until(
    () => app.call(`/runs/${run.run_id}`),
    (value) => value.data.status === "completed",
  );
  assert.ok(
    app.store
      .get(
        "SELECT messages_json FROM prompt_snapshots WHERE run_id=?",
        run.run_id,
      )
      .messages_json.includes("grant-derived-unique-fact"),
  );
  await app.features.knowledge.stop();
  app.store.run(
    "UPDATE source_grants SET expires_at=? WHERE source_key=?",
    now() - 1,
    source.conversation,
  );
  const logs = await app.call(`/runtime-logs?run_id=${run.run_id}`);
  assert.equal(
    JSON.stringify(logs.data).includes("grant-derived-unique-fact"),
    false,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM prompt_snapshots WHERE run_id=?",
      run.run_id,
    ).n,
    0,
  );
  const gapRun = randomUUID(),
    gapMessage = randomUUID();
  const epoch = app.store.get(
    "SELECT context_epoch FROM conversations WHERE id=?",
    source.conversation,
  ).context_epoch;
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,created_at,updated_at) VALUES(?,?,?,?,?,'{}',?,'completed',?,?)",
    gapRun,
    source.conversation,
    owner,
    randomUUID(),
    "gap",
    epoch,
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,'user','during-expired-grant',?)",
    gapMessage,
    source.conversation,
    gapRun,
    now(),
  );
  app.features.knowledge.setGrant(owner, source.conversation, 1, {
    capture: true,
    allow_model: true,
    can_sleep: false,
    can_wake: false,
    expires_at: now() + 600,
  });
  assert.throws(
    () =>
      app.features.knowledge.enqueue(
        owner,
        { kind: "summary", sources: [{ type: "message", id: gapMessage }] },
        true,
      ),
    (error) => error.code === "source_capture_forbidden",
  );
});

test("schema2 upgrades snapshot the prior database and backfill message references only once", () => {
  const path = privateDirectory(
    resolve(
      fileURLToPath(new URL("../../../", import.meta.url)),
      `.local/prometheus/tests/journal-upgrade-${randomUUID()}`,
    ),
  );
  const original = new DatabaseSync(join(path, "prometheus.db"));
  original.exec(
    readFileSync(new URL("./schema2.sql", import.meta.url), "utf8"),
  );
  original
    .prepare("INSERT INTO metadata VALUES('prometheus.schema','2')")
    .run();
  original
    .prepare(
      "INSERT INTO metadata VALUES('assistant.identity','journal-before-upgrade')",
    )
    .run();
  const conversation = randomUUID(),
    run = randomUUID();
  original
    .prepare(
      "INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?,'owner','old',?)",
    )
    .run(conversation, now());
  original
    .prepare(
      "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,created_at,updated_at) VALUES(?,?,'owner','old','old input','{}',0,'completed',?,?)",
    )
    .run(run, conversation, now(), now());
  original
    .prepare("INSERT INTO messages VALUES(?,?,?,'user','historical input',?)")
    .run(randomUUID(), conversation, run, now());
  original.close();
  const upgraded = new Store(path);
  assert.equal(upgraded.assistantId, "journal-before-upgrade");
  assert.equal(
    upgraded.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "6",
  );
  assert.equal(
    upgraded.get("SELECT count(*) AS n FROM audit_events WHERE kind='input'").n,
    1,
  );
  upgraded.close();
  const snapshot = join(
    path,
    "backups",
    readdirSync(join(path, "backups")).find((name) =>
      name.startsWith("schema2-"),
    ),
  );
  const before = new DatabaseSync(snapshot, { readOnly: true });
  assert.equal(
    before
      .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .get().value,
    "2",
  );
  assert.equal(
    before
      .prepare(
        "SELECT count(*) AS n FROM sqlite_master WHERE name='prompt_snapshots'",
      )
      .get().n,
    0,
  );
  before.close();
  const reopened = new Store(path);
  assert.equal(
    reopened.get("SELECT count(*) AS n FROM audit_events WHERE kind='input'").n,
    1,
  );
  reopened.close();
});
