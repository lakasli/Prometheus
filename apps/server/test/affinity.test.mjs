import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";
import { fixture } from "./helpers.mjs";
import { now, privateDirectory, passwordHash } from "../dist/security.js";
const DAY = 86400;
const match = (revision) => ({ headers: { "if-match": `"${revision}"` } });
async function person(app, name = "合成档案") {
  return (
    await app.call("/records/people", "POST", {
      data: { name, notes: `${name}的独立资料` },
    })
  ).data;
}
async function event(app, profile, delta, key = randomUUID(), extra = {}) {
  return app.call(
    `/relationships/people/${profile.id}/event`,
    "POST",
    {
      delta,
      reason: "明确人工核对的调整",
      reviewed: true,
      event_key: key,
      kind: "manual_adjustment",
      ...extra,
    },
    match(profile.revision),
  );
}
test("affinity settles old policy intervals before changes and keeps fixed-point grace and bounds", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await person(app),
    owner = p.owner_id,
    ledger = app.features.relationships.affinity,
    at = now();
  let policy = (await app.call("/relationships/policy")).data;
  assert.equal(policy.value.initial, 50);
  assert.equal(policy.value.grace_seconds, 3 * DAY);
  app.store.run(
    "UPDATE records SET created_at=? WHERE id=?",
    at - 4 * DAY,
    p.id,
  );
  app.store.run(
    "UPDATE affinity_states SET settled_at=? WHERE person_id=?",
    at - 4 * DAY,
    p.id,
  );
  assert.equal(ledger.settle(p.id, at).score_micro, 49e6);
  const changed = {
    ...policy.value,
    initial: 90,
    decay_per_day: 2,
    confirmed: true,
    reason: "调整未来衰减率",
  };
  const saved = await app.call(
    "/relationships/policy",
    "PUT",
    changed,
    match(policy.revision),
  );
  assert.equal(saved.status, 200);
  const effective = Number(
    app.store.get(
      "SELECT effective_at FROM affinity_policy_history WHERE owner_id=? AND revision=?",
      owner,
      saved.data.revision,
    ).effective_at,
  );
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      p.id,
    ).score_micro,
    49e6 - Math.floor(((effective - at) * 1e6) / DAY),
  );
  const projectedMicro =
    49e6 -
    Math.floor(((effective - at) * 1e6 + (at + DAY - effective) * 2e6) / DAY);
  assert.equal(ledger.settle(p.id, at + DAY).score_micro, projectedMicro);
  assert.equal(ledger.settle(p.id, at + DAY).score_micro, projectedMicro);
  assert.equal(
    (
      await app.call(
        "/relationships/policy",
        "PUT",
        changed,
        match(policy.revision),
      )
    ).status,
    409,
  );
  assert.equal(
    (await app.call("/relationships/policy/history")).data.items.length,
    2,
  );
  const projected = await app.call("/relationships/policy/preview", "POST", {
    policy: changed,
    idle_seconds: 4 * DAY,
  });
  assert.equal(projected.data.projected_from_initial, 88);
  assert.equal(ledger.policy(owner).revision, saved.data.revision);
  const fresh = await person(app, "新档案");
  assert.equal(ledger.describe(fresh).affinity, 90);
});
test("manual adjustments are idempotent, revision-checked and charge absolute requested changes even at bounds", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await person(app);
  app.store.run(
    "UPDATE affinity_states SET score_micro=99000000 WHERE person_id=?",
    p.id,
  );
  const first = await event(app, p, 5, "stable-key");
  assert.equal(first.status, 200);
  assert.equal(first.data.affinity, 100);
  assert.equal(
    (await event(app, p, 5, "stable-key")).data.revision,
    first.data.revision,
  );
  assert.equal((await event(app, p, 4, "stable-key")).status, 409);
  assert.equal((await event(app, p, 1)).status, 409);
  const second = await event(app, first.data, 5);
  assert.equal(second.status, 200);
  assert.equal(second.data.affinity, 100);
  const limited = await event(app, second.data, -1);
  assert.equal(limited.status, 400);
  assert.equal(limited.data.error.code, "daily_affinity_limit");
  assert.equal(
    app.store.get(
      "SELECT last_direct_at FROM affinity_states WHERE person_id=?",
      p.id,
    ).last_direct_at,
    null,
  );
  assert.equal(
    (
      await event(app, second.data, 1, randomUUID(), {
        kind: "direct_interaction",
      })
    ).status,
    503,
  );
  assert.equal(
    (
      await app.call(
        `/relationships/people/${p.id}/event`,
        "POST",
        {},
        { auth: false },
      )
    ).status,
    401,
  );
});
test("downtime decay caps an unobserved interval once and persists its fence across restart", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await person(app),
    at = now();
  app.store.run(
    "UPDATE records SET created_at=? WHERE id=?",
    at - 60 * DAY,
    p.id,
  );
  app.store.run(
    "UPDATE affinity_states SET settled_at=? WHERE person_id=?",
    at - 30 * DAY,
    p.id,
  );
  app.store.run(
    "UPDATE metadata SET value=? WHERE key='affinity.heartbeat'",
    String(at - 30 * DAY),
  );
  app.features.relationships.affinity.recover(at);
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      p.id,
    ).score_micro,
    43e6,
  );
  app.features.relationships.affinity.recover(at);
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      p.id,
    ).score_micro,
    43e6,
  );
  await app.close();
  const restarted = await fixture({ dataDir: app.dataDir });
  t.after(() => restarted.close());
  const restoredAt = now();
  assert.equal(
    restarted.features.relationships.affinity.settle(p.id, restoredAt)
      .score_micro,
    43e6 - Math.floor(((restoredAt - at) * 1e6) / DAY),
  );
});
test("merge and explicit split preserve original notes, directed endpoints and independent ledgers", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let a = await person(app, "甲"),
    b = await person(app, "乙"),
    c = await person(app, "丙");
  a = (await event(app, a, 3)).data;
  b = (await event(app, b, -2)).data;
  const edge = (
    await app.call("/records/relationships", "POST", {
      data: { from_id: a.id, to_id: c.id, description: "保持原端点" },
    })
  ).data;
  const request = {
    into_id: b.id,
    expected_from_revision: a.revision,
    expected_into_revision: b.revision,
    confirmed: true,
    reason: "人工确认重复档案",
  };
  const merged = await app.call(
    `/relationships/people/${a.id}/merge`,
    "POST",
    request,
  );
  assert.equal(merged.status, 200);
  assert.equal(merged.data.affinity_combined, false);
  const source = (await app.call(`/records/people/${a.id}`)).data,
    target = (await app.call(`/records/people/${b.id}`)).data;
  assert.equal(source.data.notes, "甲的独立资料");
  assert.equal(target.data.notes, "乙的独立资料");
  assert.equal(
    (await app.call(`/records/relationships/${edge.id}`)).data.data.from_id,
    a.id,
  );
  assert.equal(
    app.store.get(
      "SELECT person_id FROM affinity_events WHERE person_id=?",
      a.id,
    ).person_id,
    a.id,
  );
  assert.equal(
    app.features.relationships.affinity.describe(source).affinity,
    53,
  );
  assert.equal(
    app.features.relationships.affinity.describe(target).affinity,
    48,
  );
  assert.equal(
    (
      await app.call(
        `/records/people/${b.id}`,
        "DELETE",
        undefined,
        match(target.revision),
      )
    ).status,
    400,
  );
  assert.equal(
    (
      await app.call(`/relationships/people/${c.id}/merge`, "POST", {
        ...request,
        into_id: a.id,
        expected_from_revision: c.revision,
        expected_into_revision: source.revision,
      })
    ).status,
    400,
  );
  assert.equal(
    (
      await app.call(`/relationships/people/${a.id}/split`, "POST", {
        expected_from_revision: a.revision,
        expected_into_revision: target.revision,
        confirmed: true,
        reason: "核对后拆分",
      })
    ).status,
    409,
  );
  const split = await app.call(`/relationships/people/${a.id}/split`, "POST", {
    expected_from_revision: source.revision,
    expected_into_revision: target.revision,
    confirmed: true,
    reason: "核对后拆分",
  });
  assert.equal(split.status, 200);
  assert.equal(
    (await app.call(`/records/relationships/${edge.id}`)).data.revision,
    edge.revision,
  );
  assert.equal(
    (
      await app.call(
        `/records/people/${a.id}`,
        "DELETE",
        undefined,
        match(split.data.revision),
      )
    ).status,
    200,
  );
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      a.id,
    ),
    undefined,
  );
  assert.equal(
    app.store.get("SELECT reason FROM affinity_events WHERE person_id=?", a.id)
      .reason,
    null,
  );
  assert.equal(
    app.features.relationships.affinity.describe(target).affinity,
    48,
  );
});
test("relationship ledgers and mapping mutations remain private to the owning account", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const a = await person(app, "账户甲的档案"),
    b = await person(app, "账户甲的另一个档案");
  assert.equal(
    (
      await app.call(`/relationships/people/${a.id}/merge`, "POST", {
        into_id: b.id,
        expected_from_revision: 1,
        expected_into_revision: 1,
        confirmed: true,
        reason: "账户甲的映射",
      })
    ).status,
    200,
  );
  app.store.run(
    "INSERT INTO admins VALUES(?,'other-ledger-owner',?,?)",
    randomUUID(),
    await passwordHash("Other-ledger-owner-2026!"),
    now(),
  );
  await app.call("/auth/login", "POST", {
    username: "other-ledger-owner",
    password: "Other-ledger-owner-2026!",
  });
  assert.equal((await event(app, a, 1)).status, 404);
  assert.equal(
    (
      await app.call(`/relationships/people/${a.id}/split`, "POST", {
        expected_from_revision: 2,
        expected_into_revision: 2,
        confirmed: true,
        reason: "越域拆分",
      })
    ).status,
    404,
  );
  assert.deepEqual((await app.call("/relationships/people")).data.items, []);
  assert.deepEqual(
    (await app.call(`/relationships/policy/history?owner=${a.owner_id}`)).data
      .items,
    [],
  );
});

test("schema4 upgrade preserves saved scales and current legacy values without inventing lost merge provenance", async (t) => {
  const root = fileURLToPath(new URL("../../../", import.meta.url)),
    directory = privateDirectory(
      resolve(root, `.local/prometheus/tests/affinity-upgrade-${randomUUID()}`),
    );
  const db = new DatabaseSync(join(directory, "prometheus.db"));
  db.exec(readFileSync(new URL("./schema4.sql", import.meta.url), "utf8"));
  const owner = randomUUID(),
    id = randomUUID(),
    lost = randomUUID(),
    at = now();
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','4')").run();
  db.prepare("INSERT INTO admins VALUES(?,'previous-affinity-owner',?,?)").run(
    owner,
    await passwordHash("Synthetic-affinity-upgrade-2026!"),
    at,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',1,?,?,?,NULL)",
  ).run(
    id,
    owner,
    JSON.stringify({ name: "旧档案", notes: "保留" }),
    at - 100,
    at - 100,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',2,NULL,?,?,?)",
  ).run(lost, owner, at - 100, at, at);
  db.prepare("INSERT INTO affinity_events VALUES(?,?,?,?,?,?)").run(
    randomUUID(),
    owner,
    id,
    2,
    "旧调整",
    at - 50,
  );
  db.prepare("INSERT INTO person_aliases VALUES(?,?,?,?)").run(
    lost,
    id,
    owner,
    at,
  );
  const custom = {
    initial: 10,
    min: -100,
    max: 100,
    decay_per_day: 0,
    max_event_delta: 10,
  };
  db.prepare("INSERT INTO feature_policies VALUES(?,3,?,?)").run(
    `affinity:${owner}`,
    JSON.stringify(custom),
    at,
  );
  db.close();
  const app = await fixture({ dataDir: directory });
  t.after(() => app.close());
  assert.equal(app.features.relationships.policy(owner).revision, 3);
  assert.equal(app.features.relationships.policy(owner).value.initial, 10);
  const profile = app.runtime.records.get("people", id, owner);
  assert.equal(
    app.features.relationships.affinity.describe(profile).affinity,
    12,
  );
  assert.equal(
    app.features.relationships.legacyMappings(owner)[0].reversible,
    false,
  );
  assert.equal(
    app.store.get("SELECT deleted_at FROM records WHERE id=?", lost).deleted_at,
    at,
  );
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "7",
  );
});
