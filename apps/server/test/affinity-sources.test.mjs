import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { fixture } from "./helpers.mjs";
import { now, passwordHash, privateDirectory } from "../dist/security.js";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
const DAY = 86400,
  match = (n) => ({ headers: { "if-match": `"${n}"` } });
async function person(app, name = "独立互动档案", extra = {}) {
  const r = await app.call("/relationships/profiles", "POST", {
    name,
    reason: "明确人工核对建档",
    confirmed: true,
    ...extra,
  });
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return r.data;
}
async function setup(t) {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  app.features.automation.onScheduleTick = () => {};
  let p = await person(app),
    c = (
      await app.call("/conversations", "POST", { title: "本人已核对的会话" })
    ).data;
  p = (
    await app.call(
      `/relationships/profiles/${p.id}/bind`,
      "POST",
      { conversation_id: c.id, reason: "独立核对会话本人", confirmed: true },
      match(p.revision),
    )
  ).data;
  return { app, p, c };
}
function message(
  app,
  c,
  owner,
  {
    role = "user",
    status = "completed",
    at = now(),
    local = false,
    automatic = false,
  } = {},
) {
  const run = randomUUID(),
    id = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,?,'已收到',?,?)",
    run,
    c,
    owner,
    randomUUID(),
    "本人完成的原始互动",
    status,
    at,
    now(),
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,?,?,?)",
    id,
    c,
    run,
    role,
    "本人说今天读了书",
    now(),
  );
  if (local) app.store.run("INSERT INTO local_runs VALUES(?,'memory')", run);
  if (automatic)
    app.store.run("INSERT INTO automatic_runs VALUES(?,'trigger')", run);
  return { id, revision: 1, type: "message" };
}
async function add(
  app,
  p,
  delta,
  source,
  kind = "direct_interaction",
  key = randomUUID(),
) {
  return app.call(
    `/relationships/profiles/${p.id}/affinity/events`,
    "POST",
    {
      delta,
      source,
      kind,
      event_key: key,
      reviewed: true,
      reason: "独立人工核验本次互动的依据",
    },
    match(p.revision),
  );
}
const get = async (app, p) =>
  (await app.call(`/relationships/profiles/${p.id}/affinity`)).data;
async function event(app, content = "明确记录的同域事实") {
  const r = await app.call(
    "/diary/events",
    "POST",
    { content, happened_at: now() - 5 },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(r.status, 201);
  return { type: "diary_event", id: r.data.id, revision: r.data.revision };
}
function baseline(
  app,
  p,
  {
    score = 50,
    settled = now(),
    created = now(),
    last = null,
    remainder = 0,
  } = {},
) {
  app.store.run("UPDATE records SET created_at=? WHERE id=?", created, p.id);
  app.store.run(
    "UPDATE affinity_states SET score_micro=?,settled_at=?,last_direct_at=?,remainder=? WHERE person_id=?",
    Math.round(score * 1e6),
    settled,
    last,
    remainder,
    p.id,
  );
  app.store.run("DELETE FROM affinity_replay_bases WHERE person_id=?", p.id);
  app.store.run("DELETE FROM affinity_transitions WHERE person_id=?", p.id);
  app.features.relationships.affinity.replayLedger.ensure(p.id);
}
test("direct interaction uses a completed user message in an independently bound conversation and counts its actual input time", async (t) => {
  const { app, p, c } = await setup(t),
    at = now() - DAY,
    s = message(app, c.id, p.owner_id, { at }),
    key = randomUUID();
  let r = await add(app, p, 3, s, "direct_interaction", key);
  assert.equal(r.status, 200, JSON.stringify(r.data));
  assert.equal(r.data.affinity, 53);
  assert.equal(r.data.last_direct_at, at);
  assert.equal(r.data.delivery_available, false);
  const current = r.data;
  r = await add(app, p, 3, s, "direct_interaction", key);
  assert.equal(r.status, 200);
  assert.equal(r.data.affinity, 53);
  r = await add(app, current, 3, s);
  assert.equal(r.status, 409);
  assert.equal(r.data.error.code, "direct_interaction_already_counted");
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM affinity_events WHERE person_id=?",
      p.id,
    ).n,
    1,
  );
  const ledger = await get(app, p),
    manual = await add(app, ledger, 1, { type: "manual" }, "manual_adjustment");
  assert.equal(manual.status, 200);
  assert.equal(manual.data.last_direct_at, at);
  assert.equal(manual.data.affinity, 54);
});
test("unbound, wrong-owner, assistant, incomplete, local, automatic and future messages cannot become direct interaction evidence", async (t) => {
  const { app, p, c } = await setup(t),
    q = await person(app, "同名但不是本人");
  const source = message(app, c.id, p.owner_id);
  assert.equal((await add(app, q, 1, source)).status, 400);
  for (const option of [
    { role: "assistant" },
    { status: "running" },
    { local: true },
    { automatic: true },
    { at: now() + 30 },
  ])
    assert.equal(
      (await add(app, p, 1, message(app, c.id, p.owner_id, option))).status,
      400,
      JSON.stringify(option),
    );
  assert.equal((await add(app, p, 1, await event(app))).status, 400);
  assert.equal(app.store.get("SELECT count(*) AS n FROM affinity_events").n, 0);
});
test("withdrawing a clipped source replays requested values at bounds and never refunds the daily absolute quota", async (t) => {
  const { app, p } = await setup(t);
  baseline(app, p, { score: 99 });
  const source = await event(app),
    one = await add(app, p, 5, source, "manual_adjustment");
  assert.equal(one.status, 200);
  assert.equal(one.data.affinity, 100);
  const two = await add(
    app,
    one.data,
    5,
    { type: "manual" },
    "manual_adjustment",
  );
  assert.equal(two.status, 200);
  assert.equal(two.data.affinity, 100);
  const events = two.data.events;
  assert.equal(events.find((e) => e.source_type === "manual").delta, 0);
  const removed = await app.call(
    `/diary/events/${source.id}`,
    "DELETE",
    undefined,
    match(source.revision),
  );
  assert.equal(removed.status, 200);
  const replayed = await get(app, p);
  assert.equal(replayed.affinity, 100);
  assert.ok(replayed.replays.length >= 1);
  assert.equal(
    replayed.events.find((e) => e.source_id === source.id).reason,
    null,
  );
  assert.equal(replayed.events.find((e) => e.source_id === source.id).delta, 0);
  const limited = await add(
    app,
    replayed,
    -1,
    { type: "manual" },
    "manual_adjustment",
  );
  assert.equal(limited.status, 400);
  assert.equal(limited.data.error.code, "daily_affinity_limit");
});
test("source replay preserves versioned grace, old/new decay intervals and the original outage cap", async (t) => {
  const { app, p, c } = await setup(t),
    core = app.features.relationships.affinity,
    start = now() + DAY,
    t1 = start + 5 * DAY,
    t2 = t1 + 10 * DAY,
    t3 = t2 + 20 * DAY;
  baseline(app, p, { score: 50, settled: start, created: now() - 5 * DAY });
  const source = message(app, c.id, p.owner_id),
    one = await add(app, p, 5, source);
  assert.equal(one.status, 200);
  assert.equal(core.settle(p.id, t1).score_micro, 52e6);
  const old = core.policy(p.owner_id);
  core.save(p.owner_id, old.revision, {
    ...old.value,
    decay_per_day: 2,
    grace_seconds: 0,
    confirmed: true,
    reason: "只改变未来区间",
  });
  const second = await add(
    app,
    one.data,
    1,
    { type: "manual" },
    "manual_adjustment",
  );
  assert.equal(second.status, 200);
  assert.equal(core.settle(p.id, t2).score_micro, 33e6);
  app.store.run(
    "UPDATE metadata SET value=? WHERE key='affinity.heartbeat'",
    String(t2),
  );
  core.recover(t3);
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      p.id,
    ).score_micro,
    19e6,
  );
  const before = await get(app, p),
    direct = before.events.find((e) => e.kind === "direct_interaction");
  const revoked = await app.call(
    `/relationships/profiles/${p.id}/affinity/events/${direct.id}/revoke`,
    "POST",
    {},
    match(before.revision),
  );
  assert.equal(revoked.status, 200, JSON.stringify(revoked.data));
  assert.equal(revoked.data.affinity, 12);
  assert.equal(revoked.data.last_direct_at, null);
  assert.equal(revoked.data.settled_at, t3);
  assert.equal(revoked.data.policy_revision, core.policy(p.owner_id).revision);
  core.recover(t3);
  assert.equal((await get(app, p)).affinity, 12);
});
test("unbinding a mistaken identity replays its attributed events and rebind cannot reuse the same message", async (t) => {
  const { app, p, c } = await setup(t),
    source = message(app, c.id, p.owner_id);
  const counted = await add(app, p, 2, source);
  assert.equal(counted.status, 200);
  const identity = p.identities[0],
    unbound = await app.call(
      `/relationships/profiles/${p.id}/unbind`,
      "POST",
      {
        identity_id: identity.id,
        expected_identity_revision: identity.revision,
        reason: "独立核验发现归因有误",
      },
      match(counted.data.revision),
    );
  assert.equal(unbound.status, 200, JSON.stringify(unbound.data));
  assert.equal((await get(app, p)).affinity, 50);
  const q = await person(app, "纠正后的实际人物"),
    bound = await app.call(
      `/relationships/profiles/${q.id}/bind`,
      "POST",
      {
        conversation_id: c.id,
        confirmed: true,
        reason: "重新独立核验当前账号",
      },
      match(q.revision),
    );
  assert.equal(bound.status, 200);
  assert.equal(
    (await add(app, bound.data, 2, source)).data.error.code,
    "direct_interaction_already_counted",
  );
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${bound.data.id}/affinity/sources`,
      )
    ).data.items.length,
    0,
  );
});
test("model permission removal alone preserves human evidence while capture revocation cannot resurrect a prior interaction", async (t) => {
  const { app, p, c } = await setup(t);
  const grant = {
    source_key: c.id,
    grant: {
      capture: true,
      allow_model: true,
      can_sleep: false,
      can_wake: false,
      expires_at: null,
    },
  };
  assert.equal(
    (await app.call("/knowledge/source/grant", "PUT", grant, match(0))).status,
    200,
  );
  const source = message(app, c.id, p.owner_id),
    one = await add(app, p, 2, source);
  assert.equal(one.status, 200);
  grant.grant.allow_model = false;
  assert.equal(
    (await app.call("/knowledge/source/grant", "PUT", grant, match(1))).status,
    200,
  );
  assert.equal((await get(app, p)).affinity, 52);
  grant.grant.capture = false;
  assert.equal(
    (await app.call("/knowledge/source/grant", "PUT", grant, match(2))).status,
    200,
  );
  assert.equal((await get(app, p)).affinity, 50);
  grant.grant.capture = true;
  assert.equal(
    (await app.call("/knowledge/source/grant", "PUT", grant, match(3))).status,
    200,
  );
  assert.equal((await get(app, p)).affinity, 50);
  const current = await get(app, p);
  assert.equal((await add(app, current, 2, source)).status, 400);
});
test("canonical raw-message correction removes direct evidence and its reason before another source can replay it", async (t) => {
  const { app, p, c } = await setup(t);
  const granted = await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: c.id,
      grant: {
        capture: true,
        allow_model: false,
        can_sleep: false,
        can_wake: false,
        expires_at: null,
      },
    },
    match(0),
  );
  assert.equal(granted.status, 200);
  const policy = await app.call("/diary/policies", "POST", {
    source_key: c.id,
    time_zone: "Asia/Shanghai",
    visibility: "private",
    enabled: true,
    allow_model: false,
    confirmed: true,
  });
  assert.equal(policy.status, 201);
  const source = message(app, c.id, p.owner_id),
    counted = await add(app, p, 3, source);
  assert.equal(counted.status, 200);
  app.features.diary.lastSweep = 0;
  app.features.diary.tick();
  const captured = app.store.get(
    "SELECT * FROM diary_events WHERE origin_kind='message' AND origin_id=?",
    source.id,
  );
  assert.ok(captured);
  const r = await app.call(
    `/diary/events/${captured.id}`,
    "PATCH",
    { content: "本人更正该条原始陈述", happened_at: now() },
    match(captured.revision),
  );
  assert.equal(r.status, 200);
  const result = await get(app, p);
  assert.equal(result.affinity, 50);
  assert.equal(result.last_direct_at, null);
  assert.ok(result.events.every((e) => e.reason === null));
  assert.equal((await add(app, result, 3, source)).status, 400);
});
test("human diary evidence survives a model-only switch, while a factual memory correction removes its old attribution", async (t) => {
  const { app, p } = await setup(t),
    s = await event(app),
    one = await add(app, p, 2, s, "manual_adjustment");
  assert.equal(one.status, 200);
  let policy = (await app.call("/diary/policies")).data.items.find(
    (p) => p.source_key.startsWith("self:") && p.visibility === "private",
  );
  policy = (
    await app.call(
      `/diary/policies/${policy.id}`,
      "PATCH",
      {
        source_key: policy.source_key,
        time_zone: policy.time_zone,
        visibility: "private",
        enabled: true,
        allow_model: true,
        confirmed: true,
      },
      match(policy.revision),
    )
  ).data;
  assert.equal((await get(app, p)).affinity, 52);
  const memory = (
      await app.call("/records/memory", "POST", {
        data: { content: "经人工明确记录的事实" },
      })
    ).data,
    current = await get(app, p),
    two = await add(
      app,
      current,
      2,
      { type: "memory", id: memory.id, revision: memory.revision },
      "manual_adjustment",
    );
  assert.equal(two.status, 200);
  const changed = await app.call(
    `/records/memory/${memory.id}`,
    "PUT",
    { data: { content: "已明确纠正的事实" } },
    match(memory.revision),
  );
  assert.equal(changed.status, 200);
  const after = await get(app, p);
  assert.equal(after.affinity, 52);
  assert.equal(
    after.events.find((e) => e.source_type === "memory").reason,
    null,
  );
  assert.ok(after.events.find((e) => e.source_type === "diary_event").reason);
  assert.equal(
    (
      await add(
        app,
        after,
        1,
        { type: "memory", id: memory.id, revision: memory.revision },
        "manual_adjustment",
      )
    ).status,
    400,
  );
});
test("source expiry is rechecked on ledger access and requesting the same key with another value cannot change attribution", async (t) => {
  const { app, p, c } = await setup(t);
  await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: c.id,
      grant: {
        capture: true,
        allow_model: false,
        can_sleep: false,
        can_wake: false,
        expires_at: now() + 60,
      },
    },
    match(0),
  );
  const source = message(app, c.id, p.owner_id),
    key = randomUUID(),
    one = await add(app, p, 2, source, "direct_interaction", key);
  assert.equal(one.status, 200);
  assert.equal(
    (await add(app, one.data, 3, source, "direct_interaction", key)).status,
    409,
  );
  app.store.run(
    "UPDATE source_grants SET expires_at=? WHERE source_key=?",
    now() - 1,
    c.id,
  );
  const result = await get(app, p);
  assert.equal(result.affinity, 50);
  assert.equal(result.last_direct_at, null);
  assert.equal(result.events[0].reason, null);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM affinity_event_sources").n,
    1,
  );
});
test("same-owner merge and split never move direct receipts or sum ledgers, and full forgetting clears replay state", async (t) => {
  const { app, p, c } = await setup(t),
    source = message(app, c.id, p.owner_id),
    counted = await add(app, p, 2, source),
    q = await person(app, "独立目标");
  assert.equal(counted.status, 200);
  const merged = await app.call(`/relationships/people/${p.id}/merge`, "POST", {
    into_id: q.id,
    expected_from_revision: counted.data.revision,
    expected_into_revision: q.revision,
    confirmed: true,
    reason: "核对同域合并映射",
  });
  assert.equal(merged.status, 200);
  assert.equal((await get(app, p)).affinity, 52);
  assert.equal((await get(app, q)).affinity, 50);
  const a = await get(app, p),
    b = await get(app, q),
    split = await app.call(`/relationships/people/${p.id}/split`, "POST", {
      expected_from_revision: a.revision,
      expected_into_revision: b.revision,
      confirmed: true,
      reason: "重新核对为不同人物",
    });
  assert.equal(split.status, 200);
  const last = await get(app, p),
    gone = await app.call(
      `/relationships/profiles/${p.id}`,
      "DELETE",
      undefined,
      match(last.revision),
    );
  assert.equal(gone.status, 200);
  for (const table of [
    "affinity_states",
    "affinity_replay_bases",
    "affinity_transitions",
    "affinity_replays",
  ])
    assert.equal(
      app.store.get(
        `SELECT count(*) AS n FROM ${table} WHERE person_id=?`,
        p.id,
      ).n,
      0,
      table,
    );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM affinity_event_sources WHERE direct_message_id=? AND invalidated_at IS NOT NULL",
      source.id,
    ).n,
    1,
  );
  assert.equal(
    app.store.get("SELECT reason FROM affinity_events WHERE person_id=?", p.id)
      .reason,
    null,
  );
});
test("manager may score an authorized QQ source without becoming its owner and another administrator cannot read the private ledger", async (t) => {
  const { app, p } = await setup(t),
    cid = randomUUID(),
    owner = "qq:c2c:affinity-synthetic";
  app.store.run(
    "INSERT INTO conversations(id,owner_id,source_key,title,context_epoch,created_at) VALUES(?,?,?,'合成QQ来源',0,?)",
    cid,
    owner,
    owner,
    now(),
  );
  await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: owner,
      grant: {
        capture: true,
        allow_model: false,
        can_sleep: false,
        can_wake: false,
        expires_at: null,
      },
    },
    match(0),
  );
  let q = await person(app, "来源域人物", { source_conversation_id: cid });
  q = (
    await app.call(
      `/relationships/profiles/${q.id}/bind`,
      "POST",
      { conversation_id: cid, reason: "核对本人私聊", confirmed: true },
      match(q.revision),
    )
  ).data;
  const source = message(app, cid, owner),
    r = await add(app, q, 2, source);
  assert.equal(r.status, 200, JSON.stringify(r.data));
  assert.equal(r.data.owner_id, owner);
  assert.equal(r.data.affinity, 52);
  assert.equal((await get(app, p)).affinity, 50);
  const policy = (
    await app.call(`/relationships/profiles/${q.id}/affinity/policy`)
  ).data;
  const saved = await app.call(
    `/relationships/profiles/${q.id}/affinity/policy`,
    "PUT",
    { ...policy.value, confirmed: true, reason: "管理者明确确认未来规则" },
    match(policy.revision),
  );
  assert.equal(saved.status, 200, JSON.stringify(saved.data));
  assert.equal(
    app.store.get("SELECT owner_id FROM audit_events ORDER BY id DESC LIMIT 1")
      .owner_id,
    p.owner_id,
  );
  const withdrawn = await app.call(
    `/relationships/profiles/${q.id}/affinity/events/${r.data.events[0].id}/revoke`,
    "POST",
    {},
    match(r.data.revision),
  );
  assert.equal(withdrawn.status, 200, JSON.stringify(withdrawn.data));
  assert.equal(withdrawn.data.affinity, 50);
  assert.equal(
    app.store.get("SELECT owner_id FROM audit_events ORDER BY id DESC LIMIT 1")
      .owner_id,
    p.owner_id,
  );
  const other = randomUUID();
  app.store.run(
    "INSERT INTO admins VALUES(?,?,?,?)",
    other,
    "affinity-other",
    await passwordHash("Synthetic-other-affinity!"),
    now(),
  );
  assert.throws(() => app.features.affinitySources.get(other, q.id));
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${q.id}/affinity`,
        "GET",
        undefined,
        { auth: false },
      )
    ).status,
    401,
  );
});
test("schema9 checkpoint preserves a historical score and remainder and only new sourced events can be withdrawn", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/affinity-schema9-" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db")),
    owner = randomUUID(),
    pid = randomUUID(),
    at = now();
  db.exec(readFileSync(new URL("./schema9.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','9')").run();
  db.prepare(
    "INSERT INTO metadata VALUES('assistant.identity','affinity-before-schema10')",
  ).run();
  db.prepare(
    "INSERT INTO metadata VALUES('affinity.ledger.initialized','1')",
  ).run();
  db.prepare("INSERT INTO metadata VALUES('affinity.heartbeat',?)").run(
    String(at),
  );
  db.prepare("INSERT INTO admins VALUES(?,?,?,?)").run(
    owner,
    "affinity-schema9-owner",
    await passwordHash("Synthetic-schema9-affinity!"),
    at,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',3,?,?,?,NULL)",
  ).run(
    pid,
    owner,
    JSON.stringify({ name: "旧账本人物", notes: "保留旧资料" }),
    at,
    at,
  );
  db.prepare("INSERT INTO affinity_states VALUES(?,?,?,?,?,?,?)").run(
    pid,
    owner,
    49125000,
    at,
    null,
    321,
    0,
  );
  db.close();
  const app = await fixture({ dataDir: directory });
  t.after(() => app.close());
  assert.equal(app.store.assistantId, "affinity-before-schema10");
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "14",
  );
  const row = app.store.get(
    "SELECT * FROM affinity_states WHERE person_id=?",
    pid,
  );
  assert.equal(row.score_micro, 49125000);
  assert.equal(row.remainder, 321);
  const base = app.store.get(
    "SELECT * FROM affinity_replay_bases WHERE person_id=?",
    pid,
  );
  assert.equal(JSON.parse(base.state_json).score_micro, 49125000);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM affinity_event_sources").n,
    0,
  );
  const added = app.features.affinitySources.add(owner, pid, 3, {
    kind: "manual_adjustment",
    delta: 2,
    reason: "新阶段明确人工调整",
    reviewed: true,
    event_key: randomUUID(),
    source: { type: "manual" },
  });
  assert.equal(added.affinity, 51.125);
  const removed = app.features.affinitySources.revoke(
    owner,
    pid,
    added.events[0].id,
    added.revision,
    {},
  );
  assert.equal(removed.affinity, 49.125);
  assert.equal(
    app.store.get(
      "SELECT remainder FROM affinity_states WHERE person_id=?",
      pid,
    ).remainder,
    321,
  );
  const snapshot = new DatabaseSync(
    join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((n) =>
        n.startsWith("schema9-"),
      ),
    ),
    { readOnly: true },
  );
  try {
    assert.equal(
      snapshot.prepare("PRAGMA integrity_check").get().integrity_check,
      "ok",
    );
    assert.equal(
      snapshot
        .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
        .get().value,
      "9",
    );
  } finally {
    snapshot.close();
  }
});
test("fractional decay and replays preserve the same remainder regardless of query frequency", async (t) => {
  const { app, p } = await setup(t),
    q = await person(app, "同状态独立账本"),
    core = app.features.relationships.affinity,
    policy = core.policy(p.owner_id);
  core.save(p.owner_id, policy.revision, {
    ...policy.value,
    decay_per_day: 0.123456,
    grace_seconds: 0,
    confirmed: true,
    reason: "核对固定点精度",
  });
  const start = now() + DAY;
  for (const x of [p, q])
    baseline(app, x, {
      score: 60,
      settled: start,
      created: now() - 10 * DAY,
      remainder: 123,
    });
  const source = await event(app);
  assert.equal((await add(app, p, 2, source, "manual_adjustment")).status, 200);
  assert.equal(
    (await add(app, q, 2, { type: "manual" }, "manual_adjustment")).status,
    200,
  );
  for (let i = 1; i <= 100; i++) core.settle(p.id, start + i * 7919);
  core.settle(q.id, start + 791900);
  const a = app.store.get(
      "SELECT score_micro,remainder,settled_at FROM affinity_states WHERE person_id=?",
      p.id,
    ),
    b = app.store.get(
      "SELECT score_micro,remainder,settled_at FROM affinity_states WHERE person_id=?",
      q.id,
    );
  assert.deepEqual(a, b);
  const numerator = 791900n * 123456n + 123n;
  assert.equal(a.score_micro, 62000000 - Number(numerator / 86400n));
  assert.equal(a.remainder, Number(numerator % 86400n));
  await app.call(
    `/diary/events/${source.id}`,
    "DELETE",
    undefined,
    match(source.revision),
  );
  const after = app.store.get(
    "SELECT score_micro,remainder,settled_at FROM affinity_states WHERE person_id=?",
    p.id,
  );
  assert.equal(after.score_micro, 60000000 - Number(numerator / 86400n));
  assert.equal(after.remainder, Number(numerator % 86400n));
  assert.equal(after.settled_at, start + 791900);
});
test("direct attribution and its non-reusable receipt survive an actual service restart", async (t) => {
  const { app, p, c } = await setup(t),
    source = message(app, c.id, p.owner_id, { at: now() - DAY }),
    first = await add(app, p, 2, source);
  assert.equal(first.status, 200);
  await app.close();
  const reopened = await fixture({ dataDir: app.dataDir });
  t.after(() => reopened.close());
  assert.equal(
    (
      await reopened.call("/auth/login", "POST", {
        username: "tester",
        password: "Synthetic-password-2026!",
      })
    ).status,
    200,
  );
  const current = await get(reopened, p);
  assert.equal(current.affinity, 52);
  assert.equal(current.last_direct_at, first.data.last_direct_at);
  const direct = current.events.find((e) => e.kind === "direct_interaction"),
    r = await reopened.call(
      `/relationships/profiles/${p.id}/affinity/events/${direct.id}/revoke`,
      "POST",
      {},
      match(current.revision),
    );
  assert.equal(r.status, 200);
  assert.equal(r.data.affinity, 50);
  assert.equal(
    (await add(reopened, r.data, 2, source)).data.error.code,
    "direct_interaction_already_counted",
  );
  assert.equal(
    reopened.store.get(
      "SELECT count(*) AS n FROM affinity_event_sources WHERE direct_message_id=?",
      source.id,
    ).n,
    1,
  );
});
