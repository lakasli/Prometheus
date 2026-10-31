import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { fixture, until } from "./helpers.mjs";
import { now, passwordHash } from "../dist/security.js";
import { createServer } from "node:http";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { privateDirectory } from "../dist/security.js";
const match = (revision) => ({ headers: { "if-match": `"${revision}"` } });
async function create(app, name = "待核对人物", extra = {}) {
  const r = await app.call("/relationships/profiles", "POST", {
    name,
    notes: "不作为自动核实字段的人工备注",
    reason: "明确人工核对建档",
    confirmed: true,
    ...extra,
  });
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return r.data;
}
async function propose(app, p, value = "喜爱古诗", extra = {}) {
  const r = await app.call(
    `/relationships/profiles/${p.id}/assertions`,
    "POST",
    {
      field: "hobby",
      value,
      statement_kind: "self_report",
      source: { type: "manual" },
      reason: "本人明确陈述",
      ...extra,
    },
    match(p.revision),
  );
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return r.data;
}
async function review(app, p, a, extra = {}) {
  return app.call(
    `/relationships/assertions/${a.id}/review`,
    "POST",
    {
      action: "approve",
      reason: "核对来源但不改变陈述类型",
      supersedes: [],
      ...extra,
    },
    match(a.revision),
  );
}
async function conversation(app) {
  return (await app.call("/conversations", "POST", { title: "本人合成会话" }))
    .data;
}
function message(app, c, owner, text = "本人说喜欢古诗") {
  const run = randomUUID(),
    id = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,'completed','已收到',?,?)",
    run,
    c,
    owner,
    randomUUID(),
    text,
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
    id,
    c,
    run,
    text,
    now(),
  );
  return id;
}
test("profile statements retain labels, enforce exact conflict replacement and immutable review versions", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app);
  p = await propose(app, p, "这是一项推测", { statement_kind: "inference" });
  let a = p.assertions[0];
  const approved = await review(app, p, a);
  assert.equal(approved.status, 200);
  p = approved.data;
  assert.equal(p.assertions[0].statement_kind, "inference");
  p = await propose(app, p, "本人后续明确纠正");
  const pending = p.assertions.find((a) => a.status === "pending"),
    old = p.assertions.find((a) => a.status === "approved");
  const clash = await review(app, p, pending);
  assert.equal(clash.status, 409);
  assert.equal(clash.data.error.code, "profile_conflict");
  const replaced = await review(app, p, pending, {
    supersedes: [{ id: old.id, revision: old.revision }],
  });
  assert.equal(replaced.status, 200);
  assert.equal(
    replaced.data.assertions.filter((a) => a.status === "approved").length,
    1,
  );
  assert.equal(
    replaced.data.assertions.find((a) => a.id === old.id).status,
    "superseded",
  );
  assert.equal(
    (
      await review(app, p, pending, {
        supersedes: [{ id: old.id, revision: old.revision }],
      })
    ).status,
    409,
  );
  assert.equal(
    app.store.all(
      "SELECT * FROM profile_assertion_versions WHERE assertion_id=?",
      old.id,
    ).length,
    3,
  );
});
test("structured age and birthday preserve civil facts without inferring a year or calendar conversion", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app);
  for (const [field, value] of [
    [
      "birthday",
      { calendar: "gregorian", precision: "month_day", month: 2, day: 29 },
    ],
    [
      "birthday",
      {
        calendar: "lunar",
        precision: "date",
        year: 2000,
        month: 8,
        day: 15,
        leap_month: false,
      },
    ],
    ["age", { years: 26, stated_at: "2026-10-31" }],
  ])
    p = await propose(app, p, value, { field });
  assert.deepEqual(
    p.assertions.find((a) => a.value.calendar === "gregorian").value,
    { calendar: "gregorian", precision: "month_day", month: 2, day: 29 },
  );
  for (const value of [
    {
      calendar: "gregorian",
      precision: "month_day",
      year: 2000,
      month: 2,
      day: 29,
    },
    { calendar: "gregorian", precision: "date", year: 2025, month: 2, day: 29 },
    { calendar: "lunar", precision: "month_day", month: 8, day: 31 },
  ])
    assert.equal(
      (
        await app.call(
          `/relationships/profiles/${p.id}/assertions`,
          "POST",
          {
            field: "birthday",
            value,
            statement_kind: "confirmed",
            source: { type: "manual" },
            reason: "测试无效日期",
          },
          match(p.revision),
        )
      ).status,
      400,
    );
});
test("directed edges require both endpoint revisions and graph includes approved current page only", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app, "张甲"),
    q = await create(app, "李乙");
  let r = await app.call(
    "/relationships/edges",
    "POST",
    {
      from_id: p.id,
      to_id: q.id,
      expected_to_revision: q.revision,
      relationship_type: "同事",
      statement_kind: "third_party",
      source: { type: "manual" },
      reason: "第三方明确转述",
    },
    match(p.revision),
  );
  assert.equal(r.status, 201, JSON.stringify(r.data));
  p = r.data;
  const edge = p.edges[0];
  assert.equal((await app.call("/relationships/graph")).data.edges.length, 0);
  r = await app.call(
    `/relationships/edges/${edge.id}/review`,
    "POST",
    { action: "approve", reason: "核对转述来源", supersedes: [] },
    match(edge.revision),
  );
  assert.equal(r.status, 200, JSON.stringify(r.data));
  const graph = (await app.call("/relationships/graph")).data;
  assert.equal(graph.edges.length, 1);
  assert.equal(graph.edges[0].from_id, p.id);
  assert.equal(graph.edges[0].to_id, q.id);
  assert.equal(graph.edges[0].statement_kind, "third_party");
  assert.equal(
    (await app.call("/relationships/graph?q=张甲")).data.edges.length,
    0,
  );
});
test("diary source corrections erase all derived field versions and routine person edits preserve raw diary sources", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app),
    c = await conversation(app);
  const raw = message(app, c.id, p.owner_id);
  const e = await app.call(
    "/diary/events",
    "POST",
    { content: "本人说喜欢诗词", happened_at: now() - 10 },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(e.status, 201, JSON.stringify(e.data));
  p = await propose(app, p, "诗词", {
    source: { type: "diary_event", id: e.data.id, revision: e.data.revision },
  });
  const a = p.assertions[0];
  p = (await review(app, p, a)).data;
  assert.equal(
    app.store.get(
      "SELECT value FROM metadata WHERE key LIKE 'source_cutoff:%'",
    ),
    undefined,
  );
  p = (
    await app.call(
      `/relationships/profiles/${p.id}`,
      "PATCH",
      { name: "已核对人物", notes: "新备注", reason: "只更正姓名" },
      match(p.revision),
    )
  ).data;
  assert.equal(
    app.features.profiles.sources.resolve(p.manager_id, {
      type: "message",
      id: raw,
      revision: 1,
    }).content,
    "本人说喜欢古诗",
  );
  const corrected = await app.call(
    `/diary/events/${e.data.id}`,
    "PATCH",
    { content: "本人更正为喜欢历史", happened_at: now() - 10 },
    match(e.data.revision),
  );
  assert.equal(corrected.status, 200, JSON.stringify(corrected.data));
  assert.equal(
    app.store.get("SELECT value_json FROM profile_assertions WHERE id=?", a.id)
      .value_json,
    null,
  );
  assert.ok(
    app.store
      .all(
        "SELECT value_json,reason FROM profile_assertion_versions WHERE assertion_id=?",
        a.id,
      )
      .every((v) => v.value_json === null && v.reason === null),
  );
  p = (await app.call(`/relationships/profiles/${p.id}`)).data;
  assert.equal(p.assertions.length, 0);
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${p.id}/assertions`,
        "POST",
        {
          field: "hobby",
          value: "旧文本",
          statement_kind: "self_report",
          source: {
            type: "diary_event",
            id: e.data.id,
            revision: e.data.revision,
          },
          reason: "重放旧来源",
        },
        match(p.revision),
      )
    ).status,
    400,
  );
});
test("verified identity is required for Native context; same-name profiles and ordinary notes do not leak", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app, "同名人"),
    other = await create(app, "同名人"),
    c = await conversation(app);
  p = await propose(app, p);
  p = (await review(app, p, p.assertions[0])).data;
  other = await propose(app, other, "不应混入同名人的秘密");
  other = (await review(app, other, other.assertions[0])).data;
  assert.equal(app.features.profiles.native(c.id, p.owner_id).text, "");
  const bound = await app.call(
    `/relationships/profiles/${p.id}/bind`,
    "POST",
    {
      conversation_id: c.id,
      alias: "已当面核对的账号",
      reason: "明确本人身份核对",
      confirmed: true,
    },
    match(p.revision),
  );
  assert.equal(bound.status, 200, JSON.stringify(bound.data));
  p = bound.data;
  const selected = app.features.profiles.native(c.id, p.owner_id);
  assert.ok(selected.text.includes("喜爱古诗"));
  assert.ok(!selected.text.includes("不应混入"));
  assert.ok(!selected.text.includes("人工备注"));
  assert.equal(selected.uses.filter((u) => u.assertion_id).length, 1);
  const binding = p.identities[0];
  p = (
    await app.call(
      `/relationships/profiles/${p.id}/unbind`,
      "POST",
      {
        identity_id: binding.id,
        expected_identity_revision: binding.revision,
        reason: "身份核对撤回",
      },
      match(p.revision),
    )
  ).data;
  assert.equal(app.features.profiles.native(c.id, p.owner_id).text, "");
  assert.equal(p.identities[0].reason, null);
});
test("cross-account identity groups never combine visibility, affinity or contact permission", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app, "本人域"),
    c = await conversation(app);
  const cid = randomUUID(),
    owner = "qq:c2c:synthetic-profile";
  app.store.run(
    "INSERT INTO conversations(id,owner_id,source_key,title,context_epoch,created_at) VALUES(?,?,?,'合成跨域',0,?)",
    cid,
    owner,
    owner,
    now(),
  );
  let q = await create(app, "其他私有域", { source_conversation_id: cid });
  q = await propose(app, q, "不能跨域出现的字段");
  q = (await review(app, q, q.assertions[0])).data;
  p = (
    await app.call(
      `/relationships/profiles/${p.id}/bind`,
      "POST",
      { conversation_id: c.id, reason: "本人明确确认", confirmed: true },
      match(p.revision),
    )
  ).data;
  const linked = await app.call(
    `/relationships/profiles/${p.id}/link`,
    "POST",
    {
      other_id: q.id,
      expected_other_revision: q.revision,
      expected_group_revision: 0,
      reason: "明确核对为同一真实人",
      confirmed: true,
    },
    match(p.revision),
  );
  assert.equal(linked.status, 200, JSON.stringify(linked.data));
  p = linked.data;
  assert.equal(p.group.members.length, 2);
  assert.equal(p.group.permissions_combined, false);
  assert.equal(p.group.affinity_combined, false);
  const selected = app.features.profiles.native(c.id, p.owner_id);
  assert.ok(!selected.text.includes("其他私有域"));
  assert.ok(!selected.text.includes("不能跨域"));
  const outsider = randomUUID();
  app.store.run(
    "INSERT INTO admins VALUES(?,?,?,?)",
    outsider,
    "profile-other",
    await passwordHash("Synthetic-other-profile!"),
    now(),
  );
  assert.throws(() => app.features.profiles.detail(outsider, p.id));
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${p.id}/contact`,
        "POST",
        { status: "granted" },
        match(1),
      )
    ).status,
    400,
  );
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${p.id}/contact`,
        "POST",
        {
          status: "pending_verification",
          conversation_id: c.id,
          evidence: "已有对话中提出的许可，平台尚未验证",
        },
        match(1),
      )
    ).data.delivery_available,
    false,
  );
});
test("forgetting a profile clears field/edge/name/identity/contact history without deleting another endpoint", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app),
    q = await create(app, "保留端点");
  p = await propose(app, p);
  p = (await review(app, p, p.assertions[0])).data;
  const e = await app.call(
    "/relationships/edges",
    "POST",
    {
      from_id: p.id,
      to_id: q.id,
      expected_to_revision: q.revision,
      relationship_type: "朋友",
      statement_kind: "confirmed",
      source: { type: "manual" },
      reason: "本人明确核对",
    },
    match(p.revision),
  );
  assert.equal(e.status, 201);
  p = e.data;
  const r = await app.call(
    `/relationships/profiles/${p.id}`,
    "DELETE",
    undefined,
    match(p.revision),
  );
  assert.equal(r.status, 200, JSON.stringify(r.data));
  assert.ok(
    app.store
      .all(
        "SELECT name,reason FROM person_name_versions WHERE person_id=?",
        p.id,
      )
      .every((v) => v.name === null && v.reason === null),
  );
  assert.ok(
    app.store
      .all("SELECT value_json,reason FROM profile_assertion_versions")
      .every((v) => v.value_json === null && v.reason === null),
  );
  assert.ok(
    app.store
      .all("SELECT relationship_type,reason FROM profile_edge_versions")
      .every((v) => v.relationship_type === null && v.reason === null),
  );
  assert.equal((await app.call(`/relationships/profiles/${q.id}`)).status, 200);
});
test("Native records actual selected revisions and source expiry cancels an in-flight stale publication", async (t) => {
  const requests = [];
  let held;
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const c of req) chunks.push(c);
    requests.push(JSON.parse(Buffer.concat(chunks)));
    if (requests.length === 1) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          choices: [{ message: { content: "已读取当前可见资料" } }],
        }),
      );
    } else held = res;
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  t.after(
    () =>
      new Promise((r) => {
        server.closeAllConnections();
        server.close(r);
      }),
  );
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(`http://127.0.0.1:${server.address().port}`);
  let p = await create(app),
    c = await conversation(app);
  p = await propose(app, p, "native-visible-current-proof");
  p = (await review(app, p, p.assertions[0])).data;
  p = (
    await app.call(
      `/relationships/profiles/${p.id}/bind`,
      "POST",
      { conversation_id: c.id, reason: "本人已核对账号", confirmed: true },
      match(p.revision),
    )
  ).data;
  const submit = async () => {
    const r = await app.call(`/conversations/${c.id}/messages`, "POST", {
      content: "你记得我的爱好吗",
      client_message_id: randomUUID(),
    });
    assert.equal(r.status, 202);
    return r.data.run_id;
  };
  const first = await submit();
  await until(
    () => app.call(`/runs/${first}`),
    (r) => r.data.status === "completed",
  );
  assert.ok(
    requests[0].messages[0].content.includes("native-visible-current-proof"),
  );
  const uses = app.store.all(
    "SELECT * FROM profile_context_uses WHERE run_id=?",
    first,
  );
  assert.equal(uses.filter((u) => u.assertion_id).length, 1);
  assert.equal(
    uses.find((u) => u.assertion_id).assertion_revision,
    p.assertions[0].revision,
  );
  const active = await submit();
  await until(
    async () => requests.length,
    (n) => n === 2,
  );
  const a = p.assertions[0];
  const r = await app.call(
    `/relationships/assertions/${a.id}/review`,
    "POST",
    { action: "forget", reason: "本人撤回这条资料", supersedes: [] },
    match(a.revision),
  );
  assert.equal(r.status, 200);
  assert.equal(
    app.store.get("SELECT status FROM runs WHERE id=?", active).status,
    "cancelled",
  );
  assert.equal(
    app.store.get("SELECT run_id FROM prompt_snapshots WHERE run_id=?", active),
    undefined,
  );
  if (!held.destroyed)
    held.end(
      JSON.stringify({
        choices: [{ message: { content: "不能发布的旧资料回复" } }],
      }),
    );
  await until(
    async () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.notEqual(
    app.store.get("SELECT response FROM runs WHERE id=?", active).response,
    "不能发布的旧资料回复",
  );
});
test("source authorization, exact message provenance and expiration remain independent of identity binding", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const c = await conversation(app);
  let p = await create(app);
  const raw = message(app, c.id, p.owner_id);
  p = await propose(app, p, "本人原文摘取", {
    source: { type: "message", id: raw, revision: 1 },
  });
  p = (await review(app, p, p.assertions[0])).data;
  p = (
    await app.call(
      `/relationships/profiles/${p.id}/bind`,
      "POST",
      { conversation_id: c.id, reason: "明确核对账号", confirmed: true },
      match(p.revision),
    )
  ).data;
  assert.ok(
    !app.features.profiles
      .native(c.id, p.owner_id)
      .text.includes("本人原文摘取"),
  );
  const future = p.assertions[0];
  app.store.run(
    "UPDATE profile_assertions SET valid_until=? WHERE id=?",
    now() - 1,
    future.id,
  );
  assert.ok(
    !app.features.profiles
      .native(c.id, p.owner_id)
      .text.includes("本人原文摘取"),
  );
  assert.ok(
    (await app.call(`/relationships/profiles/${p.id}`)).data.assertions[0]
      .expired,
  );
  const id = randomUUID(),
    owner = "qq:group:synthetic";
  app.store.run(
    "INSERT INTO conversations(id,owner_id,source_key,title,context_epoch,created_at) VALUES(?,?,?,'群合成会话',0,?)",
    id,
    owner,
    owner,
    now(),
  );
  let group = await create(app, "不能代表群成员", {
    source_conversation_id: id,
  });
  assert.equal(
    (
      await app.call(
        `/relationships/profiles/${group.id}/bind`,
        "POST",
        { conversation_id: id, reason: "尝试误绑定", confirmed: true },
        match(group.revision),
      )
    ).status,
    400,
  );
  const groupRaw = message(app, id, owner);
  assert.throws(() =>
    app.features.profiles.sources.resolve(p.manager_id, {
      type: "message",
      id: groupRaw,
      revision: 1,
    }),
  );
  const assistant = app.store.get(
    "SELECT id FROM messages WHERE role='assistant' LIMIT 1",
  );
  if (assistant)
    assert.throws(() =>
      app.features.profiles.sources.resolve(p.manager_id, {
        type: "message",
        id: assistant.id,
        revision: 1,
      }),
    );
});
test("a corrected memory invalidates historical values while its newly selected revision remains usable", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await create(app);
  let memory = (
    await app.call("/records/memory", "POST", {
      data: { content: "本人明确记录的爱好" },
    })
  ).data;
  p = await propose(app, p, "旧爱好", {
    source: { type: "memory", id: memory.id, revision: memory.revision },
  });
  p = (await review(app, p, p.assertions[0])).data;
  memory = (
    await app.call(
      `/records/memory/${memory.id}`,
      "PUT",
      { data: { content: "本人更正后的爱好" } },
      match(memory.revision),
    )
  ).data;
  p = (await app.call(`/relationships/profiles/${p.id}`)).data;
  assert.equal(p.assertions.length, 0);
  p = await propose(app, p, "新爱好", {
    source: { type: "memory", id: memory.id, revision: memory.revision },
  });
  assert.equal(p.assertions[0].value, "新爱好");
});
test("schema7 upgrade retains existing names, ledgers, data and an integrity-checked original snapshot", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/profiles-schema7-" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db")),
    owner = randomUUID(),
    id = randomUUID(),
    at = now();
  db.exec(readFileSync(new URL("./schema7.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','7')").run();
  db.prepare(
    "INSERT INTO metadata VALUES('assistant.identity','stable-profile-schema7')",
  ).run();
  db.prepare(
    "INSERT INTO metadata VALUES('affinity.ledger.initialized','1')",
  ).run();
  db.prepare("INSERT INTO metadata VALUES('affinity.heartbeat',?)").run(
    String(at),
  );
  db.prepare("INSERT INTO admins VALUES(?,?,?,?)").run(
    owner,
    "schema7-owner",
    await passwordHash("Synthetic-profile-upgrade!"),
    at,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',4,?,?,?,NULL)",
  ).run(
    id,
    owner,
    JSON.stringify({ name: "原有人物", notes: "不推测转成字段" }),
    at,
    at,
  );
  db.prepare("INSERT INTO affinity_states VALUES(?,?,?,?,?, ?,?)").run(
    id,
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
  assert.equal(app.store.assistantId, "stable-profile-schema7");
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "15",
  );
  const p = app.features.profiles.detail(owner, id);
  assert.equal(p.data.name, "原有人物");
  assert.equal(p.assertions.length, 0);
  assert.equal(p.name_history[0].revision, 4);
  assert.equal(p.name_history[0].reason, null);
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      id,
    ).score_micro,
    49125000,
  );
  const path = join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((n) =>
        n.startsWith("schema7-"),
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
      "7",
    );
    assert.equal(
      old.prepare("SELECT count(*) AS n FROM affinity_states").get().n,
      1,
    );
  } finally {
    old.close();
  }
});
test("graph and version history immediately recheck source expiration before a background sweep", async (t) => {
  for (const firstRead of ["graph", "history"]) {
    const app = await fixture();
    t.after(() => app.close());
    await app.bootstrap();
    app.features.automation.stop();
    await app.features.knowledge.stop();
    let p = await create(app, "来源图起点"),
      q = await create(app, "来源图终点");
    const c = await conversation(app),
      scope = c.id;
    app.store.run(
      "INSERT INTO source_grants VALUES(?,1,1,0,0,0,NULL,0,?)",
      scope,
      now(),
    );
    const raw = message(app, c.id, p.owner_id);
    p = await propose(app, p, "已授权的原文资料", {
      source: { type: "message", id: raw, revision: 1 },
    });
    p = (await review(app, p, p.assertions[0])).data;
    const a = p.assertions[0];
    const made = await app.call(
      "/relationships/edges",
      "POST",
      {
        from_id: p.id,
        to_id: q.id,
        expected_to_revision: q.revision,
        relationship_type: "本人陈述的旧同事关系",
        statement_kind: "self_report",
        source: { type: "message", id: raw, revision: 1 },
        reason: "从同一授权原文核对",
      },
      match(p.revision),
    );
    assert.equal(made.status, 201);
    const e = made.data.edges[0];
    const approved = await app.call(
      `/relationships/edges/${e.id}/review`,
      "POST",
      { action: "approve", reason: "独立核对原文", supersedes: [] },
      match(e.revision),
    );
    assert.equal(approved.status, 200);
    app.store.run(
      "UPDATE source_grants SET expires_at=? WHERE source_key=?",
      now() - 1,
      scope,
    );
    if (firstRead === "graph")
      assert.equal(
        (await app.call("/relationships/graph")).data.edges.length,
        0,
      );
    const history = await app.call(`/relationships/assertions/${a.id}/history`);
    assert.equal(history.status, 200);
    assert.ok(
      history.data.items.every((v) => v.value === null && v.reason === null),
    );
    assert.equal((await app.call("/relationships/graph")).data.edges.length, 0);
    assert.ok(
      (await app.call(`/relationships/edges/${e.id}/history`)).data.items.every(
        (v) => v.relationship_type === null && v.reason === null,
      ),
    );
  }
});
