import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { fixture, until } from "./helpers.mjs";
import { now, passwordHash, privateDirectory } from "../dist/security.js";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
const match = (n) => ({ headers: { "if-match": `"${n}"` } });
const output = (extra = {}) => ({
  assertions: [
    {
      field: "hobby",
      value: "围棋",
      statement_kind: "self_report",
      reason: "明确来源称本人喜欢围棋",
    },
  ],
  edges: [],
  ...extra,
});
async function provider(t, handle) {
  const calls = [],
    server = createServer(async (req, res) => {
      const chunks = [];
      for await (const c of req) chunks.push(c);
      const request = JSON.parse(Buffer.concat(chunks));
      calls.push({ request, res });
      const response = await handle(request, calls.length, res);
      if (response !== undefined && !res.destroyed) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            choices: [
              {
                message: {
                  content: response.content ?? JSON.stringify(response),
                },
                finish_reason: response.finish_reason ?? "stop",
              },
            ],
          }),
        );
      }
    });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  t.after(
    () =>
      new Promise((r) => {
        server.closeAllConnections();
        server.close(r);
      }),
  );
  return { url: `http://127.0.0.1:${server.address().port}`, calls };
}
async function person(app, name = "已人工核对的甲", extra = {}) {
  const r = await app.call("/relationships/profiles", "POST", {
    name,
    reason: "本人明确核验建档",
    confirmed: true,
    ...extra,
  });
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return r.data;
}
async function setup(t, model) {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  app.features.automation.onScheduleTick = () => {};
  await app.features.knowledge.stop();
  if (model) await app.configure(model.url);
  let policy = (await app.call("/diary/policies")).data.items.find(
    (p) => p.source_key.startsWith("self:") && p.visibility === "private",
  );
  policy = (
    await app.call(
      `/diary/policies/${policy.id}`,
      "PATCH",
      {
        source_key: policy.source_key,
        visibility: "private",
        time_zone: policy.time_zone,
        enabled: true,
        allow_model: true,
        confirmed: true,
      },
      match(policy.revision),
    )
  ).data;
  const source = (
    await app.call(
      "/diary/events",
      "POST",
      {
        content: "甲明确说本人喜欢围棋，乙是同事。SOURCE_CANARY_SELECTED",
        happened_at: now() - 30,
      },
      { headers: { "idempotency-key": randomUUID() } },
    )
  ).data;
  return { app, p: await person(app), source, policy };
}
async function enqueue(app, p, s, extra = {}, key = randomUUID()) {
  return app.call(
    "/relationships/model-jobs",
    "POST",
    {
      person_id: p.id,
      source: { type: "diary_event", id: s.id, revision: s.revision },
      target_person_ids: [],
      confirmed: true,
      idempotency_key: key,
      ...extra,
    },
    match(p.revision),
  );
}
async function settle(app, id) {
  app.features.profileModel.tick();
  return (
    await until(
      () => app.call(`/relationships/model-jobs/${id}`),
      (r) =>
        [
          "completed",
          "failed",
          "invalidated",
          "interrupted",
          "cancelled",
        ].includes(r.data.status),
    )
  ).data;
}
test("one explicitly selected diary source creates pending bounded candidates and persists references only", async (t) => {
  const model = await provider(t, async (req) => {
    const target = JSON.parse(req.messages[1].content).allowed_targets[0];
    return output({
      edges: [
        {
          to_person_id: target.id,
          relationship_type: "colleague",
          statement_kind: "third_party",
          reason: "来源明确出现甲乙同事关系",
        },
      ],
    });
  });
  const { app, p, source } = await setup(t, model),
    q = await person(app, "已核对关系目标乙"),
    hidden = await person(app, "PRIVATE_NAME_CANARY_NOT_SELECTED");
  await app.call(
    "/diary/events",
    "POST",
    { content: "PRIVATE_DIARY_CANARY_NOT_SELECTED", happened_at: now() - 25 },
    { headers: { "idempotency-key": randomUUID() } },
  );
  const key = randomUUID(),
    submitted = await enqueue(
      app,
      p,
      source,
      { target_person_ids: [q.id] },
      key,
    );
  assert.equal(submitted.status, 202, JSON.stringify(submitted.data));
  const done = await settle(app, submitted.data.id);
  assert.equal(done.status, "completed", JSON.stringify(done));
  assert.equal(done.result.review_required, true);
  assert.equal(done.result.assertion_ids.length, 1);
  assert.equal(done.result.edge_ids.length, 1);
  const profile = (await app.call(`/relationships/profiles/${p.id}`)).data;
  assert.equal(profile.assertions[0].status, "pending");
  assert.equal(profile.edges[0].status, "pending");
  assert.equal((await app.call("/relationships/graph")).data.edges.length, 0);
  const request = JSON.stringify(model.calls[0].request);
  assert.ok(request.includes("SOURCE_CANARY_SELECTED"));
  assert.ok(request.includes(q.id));
  assert.ok(!request.includes(hidden.data.name));
  assert.ok(!request.includes("PRIVATE_DIARY_CANARY_NOT_SELECTED"));
  const job = app.store.get(
    "SELECT * FROM profile_model_jobs WHERE id=?",
    done.id,
  );
  assert.ok(!JSON.stringify(job).includes("围棋"));
  assert.ok(!job.targets_json.includes(q.data.name));
  assert.ok(!job.ref_json.includes("SOURCE_CANARY"));
  const attempt = app.store.get(
    "SELECT * FROM profile_model_attempts WHERE job_id=?",
    done.id,
  );
  assert.equal(attempt.config_json, null);
  assert.ok(!JSON.stringify(attempt).includes("围棋"));
  assert.equal(attempt.status, "completed");
  assert.equal(
    (await enqueue(app, p, source, { target_person_ids: [q.id] }, key)).data.id,
    done.id,
  );
  assert.equal(model.calls.length, 1);
  assert.equal(
    (await enqueue(app, p, source, { target_person_ids: [] }, key)).status,
    409,
  );
});
test("source consent, scope, target revisions and confirmation are checked before dispatch", async (t) => {
  const { app, p, source, policy } = await setup(t);
  assert.equal(
    (await enqueue(app, p, source, { confirmed: false })).status,
    400,
  );
  assert.equal(
    (
      await enqueue(app, p, source, {
        source: { type: "memory", id: source.id, revision: 1 },
      })
    ).status,
    400,
  );
  const cid = randomUUID(),
    owner = "qq:c2c:model-other";
  app.store.run(
    "INSERT INTO conversations(id,owner_id,source_key,title,context_epoch,created_at) VALUES(?,?,?,'另一域',0,?)",
    cid,
    owner,
    owner,
    now(),
  );
  const q = await person(app, "其他域人物", { source_conversation_id: cid });
  assert.equal(
    (await enqueue(app, p, source, { target_person_ids: [q.id] })).status,
    400,
  );
  const denied = await app.call(
    `/diary/policies/${policy.id}`,
    "PATCH",
    {
      source_key: policy.source_key,
      visibility: "private",
      time_zone: policy.time_zone,
      enabled: true,
      allow_model: false,
      confirmed: true,
    },
    match(policy.revision),
  );
  assert.equal(denied.status, 200);
  assert.equal((await enqueue(app, p, source)).status, 400);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    0,
  );
});
test("queued jobs cancel when person, selected target or deletion fence changes without borrowing a model slot", async (t) => {
  const { app, p, source } = await setup(t),
    q = await person(app, "已核对目标");
  const r = await enqueue(app, p, source, { target_person_ids: [q.id] });
  assert.equal(r.status, 202);
  await app.call(
    `/relationships/profiles/${q.id}`,
    "PATCH",
    { name: "目标更名", notes: "", reason: "人工更正" },
    match(q.revision),
  );
  app.features.profileModel.tick();
  assert.equal(
    (await app.call(`/relationships/model-jobs/${r.data.id}`)).data.status,
    "invalidated",
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    0,
  );
  assert.equal(app.runtime.runningCount, 0);
});
test("source correction interrupts the real request, clears candidates and cannot publish a late result", async (t) => {
  const model = await provider(t, async () => undefined),
    { app, p, source } = await setup(t, model),
    r = await enqueue(app, p, source);
  app.features.profileModel.tick();
  await until(
    async () => model.calls.length,
    (n) => n === 1,
  );
  assert.equal(app.runtime.runningCount, 1);
  const attempt = app.store.get(
    "SELECT * FROM profile_model_attempts WHERE job_id=?",
    r.data.id,
  );
  assert.ok(attempt.config_json);
  const changed = await app.call(
    `/diary/events/${source.id}`,
    "PATCH",
    { content: "本人已经更正，不再喜欢围棋", happened_at: now() - 30 },
    match(source.revision),
  );
  assert.equal(changed.status, 200);
  assert.equal(
    (await app.call(`/relationships/model-jobs/${r.data.id}`)).data.status,
    "invalidated",
  );
  await until(
    async () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_assertions").n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT config_json FROM profile_model_attempts WHERE id=?",
      attempt.id,
    ).config_json,
    null,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    1,
  );
  assert.ok(model.calls[0].res.destroyed);
});
test("operator cancellation uses revision checks and keeps the lease and credential reference until the request owner closes", async (t) => {
  const model = await provider(t, async () => undefined),
    { app, p, source } = await setup(t, model),
    r = await enqueue(app, p, source);
  app.features.profileModel.tick();
  await until(
    async () => model.calls.length,
    (n) => n === 1,
  );
  const current = (await app.call(`/relationships/model-jobs/${r.data.id}`))
    .data;
  assert.equal(
    (
      await app.call(
        `/relationships/model-jobs/${r.data.id}/cancel`,
        "POST",
        {},
        match(r.data.revision),
      )
    ).status,
    409,
  );
  const config = app.store.config(),
    ref = config.config.model.credential_ref,
    without = structuredClone(config.config);
  without.model.credential_ref = null;
  app.store.applyConfig(config.revision, without, p.owner_id);
  // This isolated fixture removes all rollback references so ONLY the physical
  // profile request can block deletion. Production history and guards stay intact.
  app.store.run(
    "UPDATE config_revisions SET config_json=?",
    JSON.stringify(without),
  );
  assert.throws(() => app.store.removeSecret(ref, p.owner_id), /凭据|引用/);
  const result = app.features.profileModel.cancel(
    p.owner_id,
    current.id,
    current.revision,
  );
  assert.equal(result.status, "cancelling");
  assert.equal(result.request_active, true);
  assert.equal(app.runtime.runningCount, 1);
  assert.throws(() => app.store.removeSecret(ref, p.owner_id), /凭据|引用/);
  await until(
    async () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.equal(
    (await app.call(`/relationships/model-jobs/${r.data.id}`)).data.status,
    "cancelled",
  );
  app.store.removeSecret(ref, p.owner_id);
  assert.throws(() => app.store.secret(ref, "model_api_key"));
});
test("invalid JSON, unknown keys, self-approval and unauthorized target reject the whole candidate transaction", async (t) => {
  const cases = [
    '{"assertions":[],"assertions":[],"edges":[]}',
    JSON.stringify(
      output({
        assertions: [
          {
            field: "hobby",
            value: "围棋",
            statement_kind: "confirmed",
            reason: "模型自行核实",
          },
        ],
      }),
    ),
    JSON.stringify({ ...output(), identity: "merge" }),
    JSON.stringify(
      output({
        edges: [
          {
            to_person_id: randomUUID(),
            relationship_type: "friend",
            statement_kind: "inference",
            reason: "模型猜测未选择的人",
          },
        ],
      }),
    ),
    JSON.stringify(
      output({
        assertions: [
          ...output().assertions,
          {
            field: "birthday",
            value: {
              calendar: "gregorian",
              precision: "date",
              year: 2025,
              month: 2,
              day: 29,
            },
            statement_kind: "self_report",
            reason: "无效日期",
          },
        ],
      }),
    ),
  ];
  let text = cases[0];
  cases.push(
    JSON.stringify(
      output({
        assertions: [
          ...output().assertions,
          {
            field: "preference",
            value: "应整批回退",
            statement_kind: "inference",
            reason: "api_key=synthetic-secret-output-should-be-rejected",
          },
        ],
      }),
    ),
  );
  const model = await provider(t, async () => ({ content: text })),
    { app, p, source } = await setup(t, model);
  for (const value of cases) {
    text = value;
    const queued = await enqueue(app, p, source),
      done = await settle(app, queued.data.id);
    assert.equal(done.status, "failed", JSON.stringify(done));
    assert.equal(
      app.store.get("SELECT count(*) AS n FROM profile_assertions").n,
      0,
    );
    assert.equal(app.store.get("SELECT count(*) AS n FROM profile_edges").n, 0);
  }
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    cases.length,
  );
});
test("length finish or outputs above 20000 bytes never become pending facts", async (t) => {
  let response = { content: JSON.stringify(output()), finish_reason: "length" };
  const model = await provider(t, async () => response),
    { app, p, source } = await setup(t, model);
  let done = await settle(app, (await enqueue(app, p, source)).data.id);
  assert.equal(done.status, "failed");
  response = {
    content: JSON.stringify(
      output({
        assertions: [{ ...output().assertions[0], value: "字".repeat(7000) }],
      }),
    ),
    finish_reason: "stop",
  };
  done = await settle(app, (await enqueue(app, p, source)).data.id);
  assert.equal(done.status, "failed");
  assert.equal(done.error_code, "profile_model_output_limit");
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_assertions").n,
    0,
  );
});
test("deduplication preserves source provenance and does not recreate an already reviewed identical candidate", async (t) => {
  const model = await provider(t, async () => output()),
    { app, p, source } = await setup(t, model);
  const first = await settle(app, (await enqueue(app, p, source)).data.id);
  assert.equal(first.status, "completed");
  const updated = (await app.call(`/relationships/profiles/${p.id}`)).data,
    second = await settle(app, (await enqueue(app, updated, source)).data.id);
  assert.equal(second.status, "completed");
  assert.deepEqual(second.result.assertion_ids, first.result.assertion_ids);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_assertions").n,
    1,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    2,
  );
});
test("rolling source and global budgets count failures and leave excess work queued without reserving a slot", async (t) => {
  const model = await provider(t, async () => ({ assertions: [], edges: [] })),
    { app, p, source } = await setup(t, model),
    jobs = [];
  for (let i = 0; i < 9; i++) {
    const j = await enqueue(app, p, source);
    assert.equal(j.status, 202);
    jobs.push(j.data);
  }
  for (const j of jobs.slice(0, 8))
    assert.equal((await settle(app, j.id)).status, "completed");
  app.features.profileModel.tick();
  const waiting = (await app.call(`/relationships/model-jobs/${jobs[8].id}`))
    .data;
  assert.equal(waiting.status, "queued");
  assert.equal(waiting.error_code, "daily_budget_wait");
  assert.equal(model.calls.length, 8);
  assert.equal(app.runtime.runningCount, 0);
  app.store.run(
    "UPDATE profile_model_attempts SET started_at=?",
    now() - 86401,
  );
  for (let i = 0; i < 24; i++)
    app.store.run(
      "INSERT INTO profile_model_attempts VALUES(?,?,?,'failed',NULL,1,'synthetic',NULL,'synthetic_failure',?,?)",
      randomUUID(),
      jobs[0].id,
      "unrelated-budget-owner",
      now(),
      now(),
    );
  app.features.profileModel.tick();
  assert.equal(
    (await app.call(`/relationships/model-jobs/${jobs[8].id}`)).data.error_code,
    "daily_budget_wait",
  );
  assert.equal(model.calls.length, 8);
});
test("the durable queue is globally bounded and identical receipts replay even when the queue is full", async (t) => {
  const { app, p, source } = await setup(t),
    key = randomUUID(),
    first = await enqueue(app, p, source, {}, key);
  for (let i = 1; i < 32; i++)
    assert.equal((await enqueue(app, p, source)).status, 202);
  assert.equal((await enqueue(app, p, source)).status, 429);
  assert.equal((await enqueue(app, p, source, {}, key)).data.id, first.data.id);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_jobs").n,
    32,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    0,
  );
});
test("profile work shares the single automatic physical slot with diary work while retaining interactive capacity", async (t) => {
  const model = await provider(t, async () => undefined),
    { app, p, source, policy } = await setup(t, model),
    job = await enqueue(app, p, source);
  const lease = app.runtime.beginCheck(true);
  app.features.profileModel.tick();
  assert.equal(model.calls.length, 0);
  assert.equal(
    (await app.call(`/relationships/model-jobs/${job.data.id}`)).data.status,
    "queued",
  );
  lease();
  app.features.profileModel.tick();
  await until(
    async () => model.calls.length,
    (n) => n === 1,
  );
  assert.equal(app.runtime.runningCount, 1);
  assert.throws(
    () => app.runtime.beginCheck(true),
    (e) => e.code === "model_slots_busy",
  );
  const interactive = app.runtime.beginCheck();
  assert.equal(app.runtime.runningCount, 2);
  interactive();
  const day = new Intl.DateTimeFormat("en-CA", {
    timeZone: policy.time_zone,
  }).format(new Date(source.happened_at * 1000));
  const period = await app.call("/diary/consolidate", "POST", {
    policy_id: policy.id,
    date: day,
    period: "day",
    model: true,
  });
  assert.equal(period.status, 200);
  app.features.diary.tick();
  assert.equal(app.store.get("SELECT count(*) AS n FROM diary_attempts").n, 0);
  assert.equal(app.runtime.runningCount, 1);
  const active = (await app.call(`/relationships/model-jobs/${job.data.id}`))
    .data;
  app.features.profileModel.cancel(p.owner_id, active.id, active.revision);
  await until(
    async () => app.runtime.runningCount,
    (n) => n === 0,
  );
});
test("completed candidates and every historical body disappear with their selected diary source", async (t) => {
  const model = await provider(t, async () => output()),
    { app, p, source } = await setup(t, model),
    job = await settle(app, (await enqueue(app, p, source)).data.id);
  assert.equal(job.status, "completed");
  const forgotten = await app.call(
    `/diary/events/${source.id}`,
    "DELETE",
    undefined,
    match(source.revision),
  );
  assert.equal(forgotten.status, 200);
  assert.equal(
    (await app.call(`/relationships/model-jobs/${job.id}`)).data.status,
    "invalidated",
  );
  assert.ok(
    app.store
      .all("SELECT value_json,reason FROM profile_assertions")
      .every((v) => v.value_json === null && v.reason === null),
  );
  assert.ok(
    app.store
      .all("SELECT value_json,reason FROM profile_assertion_versions")
      .every((v) => v.value_json === null && v.reason === null),
  );
  assert.ok(
    !JSON.stringify(app.store.all("SELECT * FROM profile_model_jobs")).includes(
      "围棋",
    ),
  );
});
test("queued references survive restart but an interrupted physical attempt is charged and never replayed automatically", async (t) => {
  const model = await provider(t, async () => ({ assertions: [], edges: [] })),
    { app, p, source } = await setup(t, model),
    one = await enqueue(app, p, source),
    two = await enqueue(app, p, source);
  app.store.run(
    "UPDATE profile_model_jobs SET status='running' WHERE id=?",
    two.data.id,
  );
  app.store.run(
    "INSERT INTO profile_model_attempts VALUES(?,?,?,'running',?,1,'synthetic',NULL,NULL,?,NULL)",
    randomUUID(),
    two.data.id,
    p.owner_id,
    JSON.stringify(app.store.config().config),
    now(),
  );
  await app.close();
  const reopened = await fixture({ dataDir: app.dataDir });
  t.after(() => reopened.close());
  await until(
    async () => reopened.features.profileModel.get(p.owner_id, one.data.id),
    (j) => j.status === "completed",
  );
  assert.equal(
    reopened.features.profileModel.get(p.owner_id, two.data.id).status,
    "interrupted",
  );
  assert.equal(
    reopened.store.get(
      "SELECT status FROM profile_model_attempts WHERE job_id=?",
      two.data.id,
    ).status,
    "interrupted",
  );
  assert.equal(
    reopened.store.get(
      "SELECT config_json FROM profile_model_attempts WHERE job_id=?",
      two.data.id,
    ).config_json,
    null,
  );
  assert.equal(model.calls.length, 1);
  assert.equal(
    reopened.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    2,
  );
});
test("profile jobs remain actor scoped and authentication and mutation guards apply to their endpoints", async (t) => {
  const { app, p, source } = await setup(t),
    job = await enqueue(app, p, source),
    other = randomUUID();
  app.store.run(
    "INSERT INTO admins VALUES(?,?,?,?)",
    other,
    "profile-model-other",
    await passwordHash("Other-profile-model-password!"),
    now(),
  );
  assert.throws(() => app.features.profileModel.get(other, job.data.id));
  assert.equal(
    app.features.profileModel.list(other, new URLSearchParams()).items.length,
    0,
  );
  assert.equal(
    (
      await app.call("/relationships/model-jobs", "GET", undefined, {
        auth: false,
      })
    ).status,
    401,
  );
  assert.equal(
    (
      await app.call(
        `/relationships/model-jobs/${job.data.id}`,
        "GET",
        undefined,
        { auth: false },
      )
    ).status,
    401,
  );
  assert.equal(
    (
      await app.call(
        `/relationships/model-jobs/${job.data.id}/cancel`,
        "POST",
        {},
        { headers: { "if-match": '"1"', "x-csrf-token": "invalid" } },
      )
    ).status,
    403,
  );
});
test("schema8 upgrade retains profile bodies and version history without fabricating model jobs", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/profile-model-schema8-" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db")),
    owner = randomUUID(),
    personId = randomUUID(),
    aid = randomUUID(),
    sid = randomUUID(),
    at = now();
  db.exec(readFileSync(new URL("./schema8.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','8')").run();
  db.prepare(
    "INSERT INTO metadata VALUES('assistant.identity','stable-before-profile-jobs')",
  ).run();
  db.prepare("INSERT INTO admins VALUES(?,?,?,?)").run(
    owner,
    "schema8-profile-owner",
    await passwordHash("Synthetic-schema8-profile!"),
    at,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',7,?,?,?,NULL)",
  ).run(
    personId,
    owner,
    JSON.stringify({ name: "旧schema8人物", notes: "原有备注" }),
    at,
    at,
  );
  db.prepare("INSERT INTO person_profiles VALUES(?,?,?,?,?,?)").run(
    personId,
    "stable-before-profile-jobs",
    owner,
    null,
    "独立人工核验",
    at,
  );
  const ref = {
    type: "manual",
    id: sid,
    revision: 1,
    policy_id: null,
    policy_revision: null,
    scope: null,
  };
  db.prepare(
    "INSERT INTO profile_assertions VALUES(?,?,?, ?,2,'approved','inference','manual',?,1,NULL,?, ?,?,'private',NULL,NULL,?,?,NULL)",
  ).run(
    aid,
    personId,
    "hobby",
    JSON.stringify("旧推测资料"),
    sid,
    JSON.stringify(ref),
    "保留推测标签",
    owner,
    at,
    at,
  );
  db.prepare(
    "INSERT INTO profile_assertion_versions VALUES(?,2,?,'approved',?,?,?)",
  ).run(aid, JSON.stringify("旧推测资料"), "人工审核不改变推测", owner, at);
  db.close();
  const app = await fixture({ dataDir: directory });
  t.after(() => app.close());
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "13",
  );
  assert.equal(app.store.assistantId, "stable-before-profile-jobs");
  const p = app.features.profiles.detail(owner, personId);
  assert.equal(p.revision, 7);
  assert.equal(p.assertions[0].value, "旧推测资料");
  assert.equal(p.assertions[0].statement_kind, "inference");
  assert.equal(
    app.features.profiles.history(owner, "assertion", aid).items[0].value,
    "旧推测资料",
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_jobs").n,
    0,
  );
  const path = join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((f) =>
        f.startsWith("schema8-"),
      ),
    ),
    old = new DatabaseSync(path, { readOnly: true });
  try {
    assert.equal(
      old.prepare("PRAGMA integrity_check").get().integrity_check,
      "ok",
    );
    assert.equal(
      old
        .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
        .get().value,
      "8",
    );
  } finally {
    old.close();
  }
});
test("a real sleep transition interrupts active profile work and leaves new work queued with its original consent", async (t) => {
  const model = await provider(t, async () => undefined),
    { app, p, source } = await setup(t, model),
    job = await enqueue(app, p, source);
  app.features.profileModel.tick();
  await until(
    async () => model.calls.length,
    (n) => n === 1,
  );
  const sleep = await app.call(
    "/rest/control",
    "POST",
    { action: "sleep", request_id: randomUUID(), duration_seconds: 60 },
    match(0),
  );
  assert.equal(sleep.status, 200);
  await until(
    async () => app.runtime.runningCount,
    (n) => n === 0,
  );
  const ended = (await app.call(`/relationships/model-jobs/${job.data.id}`))
    .data;
  assert.equal(ended.status, "interrupted");
  assert.equal(ended.error_code, "automatic_work_not_allowed");
  const next = await enqueue(app, p, source);
  app.features.profileModel.tick();
  assert.equal(
    (await app.call(`/relationships/model-jobs/${next.data.id}`)).data.status,
    "queued",
  );
  assert.equal(model.calls.length, 1);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_model_attempts").n,
    1,
  );
});
test("budget is charged to the captured QQ source owner and a blocked manager-owned head does not block that source", async (t) => {
  const model = await provider(t, async () => ({ assertions: [], edges: [] })),
    { app, p, source } = await setup(t, model),
    cid = randomUUID(),
    qq = "qq:c2c:profile-budget-source";
  app.store.run(
    "INSERT INTO conversations(id,owner_id,source_key,title,context_epoch,created_at) VALUES(?,?,?,'已授权的合成QQ来源',0,?)",
    cid,
    qq,
    qq,
    now(),
  );
  const grant = await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: qq,
      grant: {
        capture: true,
        allow_model: true,
        can_sleep: false,
        can_wake: false,
        expires_at: null,
      },
    },
    match(0),
  );
  assert.equal(grant.status, 200);
  const policy = await app.call("/diary/policies", "POST", {
    source_key: qq,
    visibility: "private",
    time_zone: "Asia/Shanghai",
    enabled: true,
    allow_model: true,
    confirmed: true,
  });
  assert.equal(policy.status, 201);
  const run = randomUUID(),
    message = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,'completed','已经收到',?,?)",
    run,
    cid,
    qq,
    randomUUID(),
    "明确授权后完成的用户陈述",
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
    message,
    cid,
    run,
    "我本人陈述喜欢围棋",
    now(),
  );
  app.features.diary.lastSweep = 0;
  app.features.diary.tick();
  const e = app.store.get(
    "SELECT * FROM diary_events WHERE origin_kind='message' AND origin_id=?",
    message,
  );
  assert.ok(e);
  const q = await person(app, "QQ来源域已核对人物", {
    source_conversation_id: cid,
  });
  for (let i = 0; i < 8; i++)
    assert.equal(
      (await settle(app, (await enqueue(app, p, source)).data.id)).status,
      "completed",
    );
  const blocked = await enqueue(app, p, source),
    other = await enqueue(app, q, e);
  assert.equal(other.status, 202, JSON.stringify(other.data));
  assert.equal((await settle(app, other.data.id)).status, "completed");
  assert.equal(
    (await app.call(`/relationships/model-jobs/${blocked.data.id}`)).data
      .error_code,
    "daily_budget_wait",
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM profile_model_attempts WHERE owner_id=?",
      qq,
    ).n,
    1,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM profile_model_attempts WHERE owner_id=?",
      p.owner_id,
    ).n,
    8,
  );
  assert.equal(model.calls.length, 9);
});
test("subjective diary events keep their label and cannot be promoted into reported facts by a model", async (t) => {
  let kind = "self_report";
  const model = await provider(t, async () =>
      output({
        assertions: [
          {
            field: "personality",
            value: "明确标注为主观推测",
            statement_kind: kind,
            reason: "主观事件仅作推测依据",
          },
        ],
      }),
    ),
    { app, p } = await setup(t, model),
    source = (
      await app.call(
        "/diary/events",
        "POST",
        {
          content: "SUBJECTIVE_EVENT_CANARY 我猜测他的心情不太好",
          statement_type: "subjective",
          happened_at: now() - 20,
        },
        { headers: { "idempotency-key": randomUUID() } },
      )
    ).data;
  let done = await settle(app, (await enqueue(app, p, source)).data.id);
  assert.equal(done.status, "failed");
  assert.equal(done.error_code, "profile_model_subjective_claim");
  const payload = JSON.parse(model.calls[0].request.messages[1].content);
  assert.equal(payload.source.statement_type, "subjective");
  assert.equal(payload.source.facts, "");
  assert.ok(payload.source.subjective_note.includes("SUBJECTIVE_EVENT_CANARY"));
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM profile_assertions").n,
    0,
  );
  kind = "inference";
  done = await settle(app, (await enqueue(app, p, source)).data.id);
  assert.equal(done.status, "completed");
  const candidate = (await app.call(`/relationships/profiles/${p.id}`)).data
    .assertions[0];
  assert.equal(candidate.status, "pending");
  assert.equal(candidate.statement_kind, "inference");
});
test("an explicitly selected diary period sends only facts and a later manual period correction invalidates its candidates", async (t) => {
  const model = await provider(t, async () => output()),
    { app, p, source, policy } = await setup(t, model),
    date = new Intl.DateTimeFormat("en-CA", {
      timeZone: policy.time_zone,
    }).format(new Date(source.happened_at * 1000));
  let period = (
    await app.call("/diary/consolidate", "POST", {
      policy_id: policy.id,
      date,
      period: "day",
      model: false,
    })
  ).data;
  period = (
    await app.call(
      `/diary/entries/${period.id}`,
      "PATCH",
      {
        facts: "PERIOD_FACTS_CANARY 本人陈述喜欢围棋",
        feelings: "PRIVATE_FEELINGS_CANARY 不应作为他人的事实",
      },
      match(period.revision),
    )
  ).data;
  const job = await enqueue(app, p, source, {
      source: { type: "diary_entry", id: period.id, revision: period.revision },
    }),
    done = await settle(app, job.data.id);
  assert.equal(done.status, "completed", JSON.stringify(done));
  const request = JSON.stringify(model.calls[0].request);
  assert.ok(request.includes("PERIOD_FACTS_CANARY"));
  assert.ok(!request.includes("PRIVATE_FEELINGS_CANARY"));
  assert.equal(
    JSON.parse(model.calls[0].request.messages[1].content).source
      .statement_type,
    "period_facts",
  );
  const changed = await app.call(
    `/diary/entries/${period.id}`,
    "PATCH",
    { facts: "已明确纠正的周期事实", feelings: "" },
    match(period.revision),
  );
  assert.equal(changed.status, 200);
  assert.equal(
    (await app.call(`/relationships/model-jobs/${job.data.id}`)).data.status,
    "invalidated",
  );
  assert.ok(
    app.store
      .all("SELECT value_json,reason FROM profile_assertion_versions")
      .every((a) => a.value_json === null && a.reason === null),
  );
});
