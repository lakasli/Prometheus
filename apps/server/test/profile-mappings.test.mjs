import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { fixture } from "./helpers.mjs";
import { now, passwordHash } from "../dist/security.js";
const match = (n) => ({ headers: { "if-match": `"${n}"` } });
async function setup(t) {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  app.features.automation.onScheduleTick = () => {};
  return app;
}
async function create(app, name, extra = {}) {
  const r = await app.call("/relationships/profiles", "POST", {
    name,
    notes: name + "的原始备注",
    reason: "分别人工核对建档",
    confirmed: true,
    ...extra,
  });
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return r.data;
}
const detail = async (app, p) =>
  (await app.call("/relationships/profiles/" + p.id)).data;
async function channel(app, key = "qq:c2c:mapping-fixture") {
  const id = randomUUID();
  app.store.run(
    "INSERT INTO conversations(id,owner_id,source_key,title,context_epoch,created_at) VALUES(?,?,?,'独立合成私聊',0,?)",
    id,
    key,
    key,
    now(),
  );
  return id;
}
async function score(app, p, delta) {
  const r = await app.call(
    `/relationships/profiles/${p.id}/affinity/events`,
    "POST",
    {
      delta,
      reason: "明确人工计分依据",
      reviewed: true,
      event_key: randomUUID(),
      kind: "manual_adjustment",
      source: { type: "manual" },
    },
    match(p.revision),
  );
  assert.equal(r.status, 200, JSON.stringify(r.data));
  return detail(app, p);
}
const merge = (app, p, q, extra = {}) =>
  app.call(
    `/relationships/profiles/${p.id}/merge`,
    "POST",
    {
      into_id: q.id,
      expected_into_revision: q.revision,
      confirmed: true,
      reason: "分别核对的同域来源映射",
      ...extra,
    },
    match(p.revision),
  );
const split = (app, p, q, extra = {}) =>
  app.call(
    `/relationships/profiles/${p.id}/split`,
    "POST",
    {
      expected_into_revision: q.revision,
      confirmed: true,
      reason: "核对原始档案后拆回映射",
      ...extra,
    },
    match(p.revision),
  );
async function field(app, p, text) {
  let r = await app.call(
    `/relationships/profiles/${p.id}/assertions`,
    "POST",
    {
      field: "hobby",
      value: text,
      source: { type: "manual" },
      statement_kind: "inference",
      reason: "明确保留推测标签",
    },
    match(p.revision),
  );
  assert.equal(r.status, 201);
  p = r.data;
  const a = p.assertions[0];
  r = await app.call(
    `/relationships/assertions/${a.id}/review`,
    "POST",
    { action: "approve", reason: "人工批准但不改变推测性质", supersedes: [] },
    match(a.revision),
  );
  assert.equal(r.status, 200);
  return r.data;
}

test("a channel manager merges and splits QQ and Weixin profiles without taking source ownership or combining fields, identities, edges, or ledgers", async (t) => {
  const app = await setup(t);
  for (const owner of [
    "qq:c2c:mapping-fixture",
    "weixin:personal:mapping-fixture",
  ]) {
    const cid = await channel(app, owner);
    let p = await create(app, "来源甲", { source_conversation_id: cid }),
      q = await create(app, "来源乙", { source_conversation_id: cid });
    const actor = p.manager_id;
    p = await field(app, p, "只属于甲的推测");
    p = await score(app, p, 2);
    q = await score(app, q, -1);
    const bound = await app.call(
      `/relationships/profiles/${p.id}/bind`,
      "POST",
      { conversation_id: cid, reason: "独立核对账号", confirmed: true },
      match(p.revision),
    );
    assert.equal(bound.status, 200, JSON.stringify(bound.data));
    p = bound.data;
    const beforeUses = app.features.profiles.native(cid, owner);
    assert.ok(beforeUses.text.includes("只属于甲的推测"));
    const edge = await app.call(
      "/relationships/edges",
      "POST",
      {
        from_id: p.id,
        to_id: q.id,
        expected_to_revision: q.revision,
        relationship_type: "同域认识",
        statement_kind: "third_party",
        source: { type: "manual" },
        reason: "保留原有端点",
      },
      match(p.revision),
    );
    assert.equal(edge.status, 201);
    p = edge.data;
    q = await detail(app, q);
    const edgeId = p.edges[0].id,
      events = app.store.all(
        "SELECT * FROM affinity_events WHERE person_id IN (?,?) ORDER BY id",
        p.id,
        q.id,
      ),
      identities = app.store.all(
        "SELECT * FROM profile_identities WHERE person_id=?",
        p.id,
      );
    const result = await merge(app, p, q);
    assert.equal(result.status, 200, JSON.stringify(result.data));
    p = result.data;
    q = await detail(app, q);
    assert.equal(p.merged_into, q.id);
    assert.equal(p.owner_id, owner);
    assert.equal(q.owner_id, owner);
    assert.equal(p.data.notes, "来源甲的原始备注");
    assert.equal(q.assertions.length, 0);
    assert.equal(p.assertions[0].value, "只属于甲的推测");
    assert.deepEqual(
      app.store.all(
        "SELECT * FROM affinity_events WHERE person_id IN (?,?) ORDER BY id",
        p.id,
        q.id,
      ),
      events,
    );
    assert.deepEqual(
      app.store.all("SELECT * FROM profile_identities WHERE person_id=?", p.id),
      identities,
    );
    assert.equal(
      app.store.get(
        "SELECT from_person_id FROM profile_edges WHERE id=?",
        edgeId,
      ).from_person_id,
      p.id,
    );
    assert.equal(app.features.affinitySources.get(actor, p.id).affinity, 52);
    assert.equal(app.features.affinitySources.get(actor, q.id).affinity, 49);
    assert.equal(
      app.store.get(
        "SELECT owner_id FROM person_mapping_history WHERE person_id=?",
        p.id,
      ).owner_id,
      owner,
    );
    assert.equal(
      app.store.get(
        "SELECT owner_id FROM audit_events ORDER BY id DESC LIMIT 1",
      ).owner_id,
      actor,
    );
    const response = await split(app, p, q);
    assert.equal(response.status, 200, JSON.stringify(response.data));
    p = response.data;
    assert.equal(p.merged_into, null);
    assert.equal(p.identities[0].id, identities[0].id);
    assert.equal(app.features.affinitySources.get(actor, p.id).affinity, 52);
    assert.equal(app.features.affinitySources.get(actor, q.id).affinity, 49);
    assert.equal(
      app.store.get(
        "SELECT owner_id FROM audit_events ORDER BY id DESC LIMIT 1",
      ).owner_id,
      actor,
    );
  }
});

test("both mapping endpoints require management and exact source owner and visibility, including the retained legacy path", async (t) => {
  const app = await setup(t),
    p = await create(app, "原私有档案"),
    shared = await create(app, "同owner共享档案", { visibility: "shared" }),
    cid = await channel(app),
    otherSource = await create(app, "其他来源", {
      source_conversation_id: cid,
    });
  for (const q of [shared, otherSource]) {
    const r = await merge(app, p, q);
    assert.equal(r.status, 400, JSON.stringify(r.data));
    assert.equal(r.data.error.code, "merge_scope_conflict");
    const legacy = await app.call(
      `/relationships/people/${p.id}/merge`,
      "POST",
      {
        into_id: q.id,
        expected_from_revision: p.revision,
        expected_into_revision: q.revision,
        confirmed: true,
        reason: "拒绝扩大来源范围",
      },
    );
    assert.equal(legacy.status, 400);
  }
  const second = randomUUID();
  app.store.run(
    "INSERT INTO admins VALUES(?,?,?,?)",
    second,
    "mapping-other",
    await passwordHash("Synthetic-mapping-other!"),
    now(),
  );
  await app.call("/auth/login", "POST", {
    username: "mapping-other",
    password: "Synthetic-mapping-other!",
  });
  const foreign = await create(app, "他人共享只读", { visibility: "shared" });
  await app.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  assert.equal(
    (await app.call("/relationships/profiles/" + foreign.id)).data.can_manage,
    false,
  );
  assert.equal((await merge(app, p, foreign)).status, 404);
  app.store.run(
    "UPDATE person_profiles SET manager_id=? WHERE person_id=?",
    second,
    otherSource.id,
  );
  const ownPeer = await create(app, "该来源第二档案", {
    source_conversation_id: cid,
  });
  assert.equal((await merge(app, ownPeer, otherSource)).status, 404);
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${p.id}/merge`,
        "POST",
        {},
        { auth: false },
      )
    ).status,
    401,
  );
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${p.id}/merge`,
        "POST",
        {},
        { headers: { "x-csrf-token": "" } },
      )
    ).status,
    403,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM person_aliases").n, 0);
});

test("merge and split compare both reviewed revisions and retain safe repair of a historical mixed-visibility mapping", async (t) => {
  const app = await setup(t);
  let p = await create(app, "甲"),
    q = await create(app, "乙");
  assert.equal(
    (await merge(app, p, q, { expected_into_revision: q.revision + 1 })).status,
    409,
  );
  assert.equal((await merge(app, p, q, { confirmed: false })).status, 400);
  assert.equal(
    (await merge(app, p, q, { reason: "password=synthetic-secret-value" }))
      .status,
    400,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM person_aliases").n, 0);
  const m = await merge(app, p, q);
  assert.equal(m.status, 200);
  const old = p;
  p = m.data;
  q = await detail(app, q);
  assert.equal((await split(app, old, q)).status, 409);
  assert.equal(
    (await split(app, p, { ...q, revision: q.revision + 1 })).status,
    409,
  );
  assert.equal((await split(app, p, q, { confirmed: false })).status, 400);
  // The earlier TypeScript implementation allowed this same-owner mapping. A split only removes it.
  app.store.run("UPDATE records SET visibility='shared' WHERE id=?", q.id);
  const s = await split(app, p, q);
  assert.equal(s.status, 200, JSON.stringify(s.data));
  assert.equal(s.data.merged_into, null);
  assert.equal((await detail(app, q)).visibility, "shared");
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM person_mapping_history").n,
    2,
  );
});

test("identity groups and chained mappings cannot be merged and new fields are directed to the primary profile", async (t) => {
  const app = await setup(t);
  let p = await create(app, "甲"),
    q = await create(app, "乙"),
    r = await create(app, "丙");
  const group = await app.call(
    `/relationships/profiles/${p.id}/link`,
    "POST",
    {
      other_id: q.id,
      expected_other_revision: q.revision,
      expected_group_revision: 0,
      reason: "独立核对跨账号归属",
      confirmed: true,
    },
    match(p.revision),
  );
  assert.equal(group.status, 200);
  p = group.data;
  q = await detail(app, q);
  assert.equal((await merge(app, p, r)).status, 400);
  const ungroup = await app.call(
    `/relationships/profiles/${p.id}/unlink`,
    "POST",
    {
      expected_group_revision: p.group.revision,
      reason: "明确拆分后再判断",
      confirmed: true,
    },
    match(p.revision),
  );
  assert.equal(ungroup.status, 200);
  p = ungroup.data;
  q = await detail(app, q);
  const result = await merge(app, p, q);
  assert.equal(result.status, 200);
  p = result.data;
  q = await detail(app, q);
  assert.equal((await merge(app, r, p)).status, 400);
  assert.equal((await merge(app, q, r)).status, 400);
  const proposal = await app.call(
    `/relationships/profiles/${p.id}/assertions`,
    "POST",
    {
      field: "hobby",
      value: "不可新加到别名",
      statement_kind: "inference",
      source: { type: "manual" },
      reason: "验证合并入口",
    },
    match(p.revision),
  );
  assert.equal(proposal.status, 400);
  assert.equal(proposal.data.error.code, "person_merged");
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM profile_assertions WHERE person_id=?",
      p.id,
    ).n,
    0,
  );
});

test("a graph response freezes the authorized list page and omits merged, pending, and off-page endpoints", async (t) => {
  const app = await setup(t);
  let p = await create(app, "图页-甲"),
    q = await create(app, "图页-乙"),
    alias = await create(app, "图页-合并来源");
  const first = await app.call(
    "/relationships/edges",
    "POST",
    {
      from_id: p.id,
      to_id: q.id,
      expected_to_revision: q.revision,
      relationship_type: "已审核边",
      statement_kind: "self_report",
      source: { type: "manual" },
      reason: "明确端点",
    },
    match(p.revision),
  );
  assert.equal(first.status, 201);
  p = first.data;
  q = await detail(app, q);
  let edge = p.edges[0];
  const reviewed = await app.call(
    `/relationships/edges/${edge.id}/review`,
    "POST",
    { action: "approve", reason: "人工核对有向边", supersedes: [] },
    match(edge.revision),
  );
  assert.equal(reviewed.status, 200);
  p = await detail(app, p);
  q = await detail(app, q);
  const pending = await app.call(
    "/relationships/edges",
    "POST",
    {
      from_id: q.id,
      to_id: p.id,
      expected_to_revision: p.revision,
      relationship_type: "待审不参与布局",
      statement_kind: "inference",
      source: { type: "manual" },
      reason: "只保存候选",
    },
    match(q.revision),
  );
  assert.equal(pending.status, 201);
  p = await detail(app, p);
  q = pending.data;
  const m = await merge(app, alias, q);
  assert.equal(m.status, 200);
  alias = m.data;
  const g = (
    await app.call("/relationships/graph?q=" + encodeURIComponent("图页-"))
  ).data;
  assert.equal(g.max_nodes, 50);
  assert.equal(g.page_only, true);
  assert.equal(g.page_items.length, 3);
  assert.equal(g.page_items.find((v) => v.id === alias.id).merged_into, q.id);
  assert.equal(g.items.length, 2);
  assert.deepEqual(
    g.edges.map((e) => e.id),
    [edge.id],
  );
  assert.ok(g.page_items.every((p) => p.can_manage));
  const filtered = (
    await app.call("/relationships/graph?q=" + encodeURIComponent("图页-甲"))
  ).data;
  assert.deepEqual(
    filtered.items.map((p) => p.id),
    [p.id],
  );
  assert.equal(filtered.edges.length, 0);
  assert.equal(filtered.total, 1);
  assert.equal(
    (await app.call("/relationships/graph?offset=999")).data.items.length,
    0,
  );
});

test("mapping changes clear selected Native provenance and retain separately reviewable original endpoint data after restart", async (t) => {
  const app = await setup(t);
  let p = await create(app, "本人甲"),
    q = await create(app, "本人乙");
  p = await field(app, p, "字段甲来源独立");
  q = await field(app, q, "字段乙来源独立");
  const c = (
    await app.call("/conversations", "POST", { title: "已核对本人私聊" })
  ).data;
  p = (
    await app.call(
      `/relationships/profiles/${p.id}/bind`,
      "POST",
      { conversation_id: c.id, reason: "人工核对本人", confirmed: true },
      match(p.revision),
    )
  ).data;
  const before = app.features.profiles.native(c.id, p.owner_id);
  assert.ok(before.text.includes("字段甲来源独立"));
  assert.ok(!before.text.includes("字段乙来源独立"));
  const epoch = app.store.get(
    "SELECT context_epoch FROM conversations WHERE id=?",
    c.id,
  ).context_epoch;
  const m = await merge(app, p, q);
  assert.equal(m.status, 200);
  p = m.data;
  q = await detail(app, q);
  const joined = app.features.profiles.native(c.id, p.owner_id);
  assert.ok(joined.text.includes("字段甲来源独立"));
  assert.ok(joined.text.includes("字段乙来源独立"));
  assert.ok(
    app.store.get("SELECT context_epoch FROM conversations WHERE id=?", c.id)
      .context_epoch > epoch,
  );
  const dataDir = app.dataDir;
  await app.close();
  const reopened = await fixture({ dataDir });
  t.after(() => reopened.close());
  await reopened.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  p = await detail(reopened, p);
  q = await detail(reopened, q);
  assert.equal(p.merged_into, q.id);
  assert.equal(p.assertions[0].value, "字段甲来源独立");
  assert.equal(q.assertions[0].value, "字段乙来源独立");
  const s = await split(reopened, p, q);
  assert.equal(s.status, 200);
  const separated = reopened.features.profiles.native(c.id, p.owner_id);
  assert.ok(separated.text.includes("字段甲来源独立"));
  assert.ok(!separated.text.includes("字段乙来源独立"));
  assert.equal(
    reopened.store.get("PRAGMA integrity_check").integrity_check,
    "ok",
  );
});
