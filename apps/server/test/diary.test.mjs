import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fixture, until } from "./helpers.mjs";
import { now, passwordHash, privateDirectory } from "../dist/security.js";
import { diaryBounds } from "../dist/diary-time.js";
import { strictModelJson } from "../dist/schedule-json.js";
const match = (revision) => ({ headers: { "if-match": `"${revision}"` } });
const instant = (date) => Date.parse(`${date}T12:00:00+08:00`) / 1000;
async function event(
  app,
  date,
  content = "来自明确人工记录的事实",
  extra = {},
  key = randomUUID(),
) {
  const response = await app.call(
    "/diary/events",
    "POST",
    { content, happened_at: instant(date), ...extra },
    { headers: { "idempotency-key": key } },
  );
  assert.equal(response.status, 201, JSON.stringify(response.data));
  return response.data;
}
async function self(app, model = false) {
  let p = (await app.call("/diary/policies")).data.items.find(
    (p) => p.visibility === "private" && p.source_key.startsWith("self:"),
  );
  if (model)
    p = (
      await app.call(
        `/diary/policies/${p.id}`,
        "PATCH",
        {
          source_key: p.source_key,
          visibility: p.visibility,
          time_zone: p.time_zone,
          enabled: true,
          allow_model: true,
          confirmed: true,
        },
        match(p.revision),
      )
    ).data;
  return p;
}
const seal = (app, p, date, period = "day", model = false, extra = {}) =>
  app.call("/diary/consolidate", "POST", {
    policy_id: p.id,
    date,
    period,
    model,
    ...extra,
  });
function seededRun(
  app,
  conversation,
  owner,
  {
    status = "completed",
    text = "新授权之后的真实结构合成文字",
    assistant = "合成本人文字",
    target = false,
  } = {},
) {
  const id = randomUUID(),
    user = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,?,?,?,?)",
    id,
    conversation,
    owner,
    randomUUID(),
    text,
    status,
    assistant,
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
    user,
    conversation,
    id,
    text,
    now(),
  );
  if (assistant)
    app.store.run(
      "INSERT INTO messages VALUES(?,?,?,'assistant',?,?)",
      randomUUID(),
      conversation,
      id,
      assistant,
      now(),
    );
  if (target)
    app.store.run(
      "INSERT INTO run_targets VALUES(?,'qq',?,'synthetic-missing-credential',?,'fixture-account')",
      id,
      JSON.stringify({ open_id: "fixture-target" }),
      now() + 60,
    );
  return { id, user };
}
function tick(app, sweep = false) {
  if (sweep) app.features.diary.lastSweep = 0;
  app.features.diary.tick();
}
async function provider(
  t,
  handler = () => ({
    content: JSON.stringify({
      facts: "合成模型的来源总结",
      feelings: "明确主观感受",
    }),
    finish_reason: "stop",
  }),
) {
  const calls = [],
    server = createServer(async (req, res) => {
      let raw = "";
      for await (const chunk of req) raw += chunk;
      const request = JSON.parse(raw),
        call = { request, res, bytes: Buffer.byteLength(raw) };
      calls.push(call);
      const response = await handler(call, calls.length);
      if (response !== undefined && !res.destroyed) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            choices: [
              {
                message: { content: response.content },
                finish_reason: response.finish_reason,
              },
            ],
          }),
        );
      }
    });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  t.after(
    () =>
      new Promise((done) => {
        server.closeAllConnections();
        server.close(done);
      }),
  );
  return { url: `http://127.0.0.1:${server.address().port}`, calls };
}
test("diary civil boundaries follow actual DST days, Mondays and natural months", () => {
  assert.equal(
    diaryBounds("day", "2026-03-08", "America/New_York").end_at -
      diaryBounds("day", "2026-03-08", "America/New_York").start_at,
    23 * 3600,
  );
  assert.equal(
    diaryBounds("day", "2026-11-01", "America/New_York").end_at -
      diaryBounds("day", "2026-11-01", "America/New_York").start_at,
    25 * 3600,
  );
  assert.equal(
    diaryBounds("week", "2026-10-31", "Asia/Shanghai").date,
    "2026-10-26",
  );
  assert.equal(diaryBounds("month", "2026-02-15", "Asia/Shanghai").days, 28);
  assert.throws(() => diaryBounds("day", "2026-02-30", "Asia/Shanghai"));
  assert.deepEqual(
    strictModelJson('```JSON\n{"facts":"明确来源","feelings":""}\n```'),
    { facts: "明确来源", feelings: "" },
  );
  assert.deepEqual(
    strictModelJson('```\n{"facts":"明确来源","feelings":""}\n```'),
    { facts: "明确来源", feelings: "" },
  );
  assert.throws(() => strictModelJson("```python\n{}\n```"));
});

test("a skipped civil date records the calendar gap without inventing a zero-interaction experience", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  let p = await self(app);
  const saved = await app.call(
    `/diary/policies/${p.id}`,
    "PATCH",
    {
      source_key: p.source_key,
      visibility: p.visibility,
      time_zone: "Pacific/Apia",
      enabled: true,
      allow_model: false,
      confirmed: true,
    },
    match(p.revision),
  );
  assert.equal(saved.status, 200);
  p = saved.data;
  const d = await seal(app, p, "2011-12-30");
  assert.equal(d.status, 200);
  assert.equal(d.data.version.observation, "skipped_civil_day");
  assert.match(d.data.version.facts, /跳过/);
  assert.equal(d.data.version.source_count, 0);
  assert.equal(d.data.task.state, "empty");
});

test("human diary revisions are preserved through automatic consolidation and only an exact explicit rebase replaces them", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await self(app, true),
    e = await event(app, "2026-09-23", "最初明确记录的来源"),
    d = (await seal(app, p, "2026-09-23")).data;
  const manual = await app.call(
    `/diary/entries/${d.id}`,
    "PATCH",
    { facts: "人工核对并锁定的正文", feelings: "人工感受" },
    match(d.revision),
  );
  assert.equal(manual.status, 200);
  assert.equal(manual.data.locked, 1);
  assert.equal(
    (await seal(app, p, "2026-09-23")).data.version.generator,
    "manual",
  );
  await app.call(
    `/diary/events/${e.id}`,
    "PATCH",
    { content: "来源已经明确更正", happened_at: instant("2026-09-23") },
    match(1),
  );
  assert.equal((await seal(app, p, "2026-09-23")).data.state, "invalidated");
  assert.equal(
    (
      await seal(app, p, "2026-09-23", "day", false, {
        replace_manual: true,
        expected_revision: 1,
      })
    ).status,
    409,
  );
  const fresh = await seal(app, p, "2026-09-23", "day", false, {
    replace_manual: true,
    expected_revision: manual.data.revision,
  });
  assert.equal(fresh.status, 200);
  assert.equal(fresh.data.locked, 0);
  assert.equal(fresh.data.version.generator, "evidence_excerpt_v1");
  assert.match(fresh.data.version.facts, /来源已经明确更正/);
  assert.equal(fresh.data.version.facts.includes("人工核对并锁定"), false);
});

test("automatic sealing uses a persistent bounded date checkpoint and model work stops when the clock is untrusted", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await self(app);
  for (let i = 1; i <= 8; i++)
    await event(app, `2026-09-${String(i).padStart(2, "0")}`);
  app.store.run(
    "UPDATE diary_policies SET created_at=?,sealed_through=? WHERE id=?",
    instant("2026-09-01"),
    "2026-08-31",
    p.id,
  );
  tick(app, true);
  const through = app.store.get(
    "SELECT sealed_through FROM diary_policies WHERE id=?",
    p.id,
  ).sealed_through;
  assert.equal(through, "2026-09-04");
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_entries e JOIN diary_entry_versions v ON v.entry_id=e.id AND v.revision=e.revision WHERE e.policy_id=? AND e.period='day' AND v.source_count>0",
      p.id,
    ).n,
    4,
  );
  app.features.diary.clockTrusted = () => false;
  tick(app, true);
  assert.equal(
    app.store.get("SELECT sealed_through FROM diary_policies WHERE id=?", p.id)
      .sealed_through,
    through,
  );
  assert.equal(
    (await seal(app, p, "2026-09-05")).data.error.code,
    "diary_clock_untrusted",
  );
  app.features.diary.clockTrusted = () => true;
  tick(app, true);
  assert.equal(
    app.store.get("SELECT sealed_through FROM diary_policies WHERE id=?", p.id)
      .sealed_through,
    "2026-09-08",
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM diary_attempts").n, 0);
});

test("Native diary retrieval requires model permission, bounds UTF8 snapshots, labels recent fallback and excludes isolated runs", async (t) => {
  const remote = await provider(t, () => ({
      content: "合成Native回执",
      finish_reason: "stop",
    })),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const owner = (await app.call("/auth/me")).data.user.id;
  let p = await self(app);
  await event(
    app,
    "2026-09-19",
    "日记出域边界的独特原文-" + "完整材料".repeat(900),
  );
  let d = (await seal(app, p, "2026-09-19")).data;
  assert.equal((await app.call(`/records/diary/${d.id}`)).status, 404);
  assert.equal(
    app.runtime.records
      .nativeContext(owner, "最近怎么样")
      .refs.filter((r) => r.kind === "diary").length,
    0,
  );
  p = await self(app, true);
  d = (await seal(app, p, "2026-09-19")).data;
  const projected = (await app.call(`/records/diary/${d.id}`)).data;
  assert.equal(projected.id, d.id);
  assert.equal(
    (
      await app.call(
        `/records/diary/${d.id}`,
        "PUT",
        { data: { title: "绕开账本", date: "2026-09-19", content: "x" } },
        match(projected.revision),
      )
    ).status,
    400,
  );
  const c = (
      await app.call("/conversations", "POST", { title: "普通对话可见日记" })
    ).data,
    r = await app.call(`/conversations/${c.id}/messages`, "POST", {
      content: "最近怎么样",
      client_message_id: randomUUID(),
    });
  assert.equal(r.status, 202);
  await until(
    () => app.call(`/runs/${r.data.run_id}`),
    (r) => r.data.status === "completed",
  );
  const request = remote.calls[0].request;
  assert.match(request.messages[0].content, /日记出域边界的独特原文/);
  assert.match(request.messages[0].content, /recent_visible_day/);
  assert.ok(!request.messages[0].content.includes("完整材料".repeat(400)));
  const selected = JSON.parse(
    app.store.get(
      "SELECT details_json FROM context_selections WHERE run_id=?",
      r.data.run_id,
    ).details_json,
  );
  assert.ok(selected.diary_json_bytes <= 8000);
  assert.equal(
    selected.record_sources.find((s) => s.id === d.id).selection_reason,
    "recent_visible_day",
  );
  const isolated = app.runtime.enqueue(
    c.id,
    owner,
    "隔离任务",
    randomUUID(),
    undefined,
    { isolated: true },
  );
  await until(
    () =>
      Promise.resolve(
        app.store.get("SELECT status FROM runs WHERE id=?", isolated.run_id)
          ?.status,
      ),
    (s) => s === "completed",
  );
  assert.equal(
    remote.calls[1].request.messages[0].content.includes(
      "日记出域边界的独特原文",
    ),
    false,
  );
  await app.call(
    `/diary/policies/${p.id}`,
    "PATCH",
    {
      source_key: p.source_key,
      visibility: p.visibility,
      time_zone: p.time_zone,
      enabled: true,
      allow_model: false,
      confirmed: true,
    },
    match(p.revision),
  );
  assert.equal(
    app.runtime.records
      .nativeContext(owner, "最近怎么样")
      .text.includes("日记出域边界的独特原文"),
    false,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM prompt_snapshots").n,
    0,
  );
});

test("an ongoing day records its cutoff and is never reported as a fully closed civil period", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await self(app, true),
    date = new Intl.DateTimeFormat("en-CA", {
      timeZone: "Asia/Shanghai",
    }).format(new Date());
  const e = await app.call(
    "/diary/events",
    "POST",
    { content: "截至当前已经发生的合成事件", happened_at: now() - 1 },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(e.status, 201);
  const d = (await seal(app, p, date)).data;
  assert.equal(d.state, "open");
  assert.equal(d.coverage.period_closed, false);
  const record = (await app.call(`/records/diary/${d.id}`)).data;
  assert.equal(record.data.period_closed, false);
  assert.match(record.data.content, /周期尚未结束/);
});

test("event and period FTS are scoped, paginate, respect civil dates and erase tombstoned text", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await self(app),
    events = [];
  for (let i = 0; i < 5; i++)
    events.push(
      await event(
        app,
        "2026-09-16",
        `可检索凭据以外的独立事件 diary-needle-${i}`,
      ),
    );
  const found = await app.call(
    `/diary/events?policy_id=${p.id}&q=diary-needle&limit=2`,
  );
  assert.equal(found.status, 200, JSON.stringify(found.data));
  assert.equal(found.data.total, 5);
  assert.equal(found.data.items.length, 2);
  assert.equal(
    (
      await app.call(
        `/diary/events?policy_id=${p.id}&date=2026-09-17&q=diary-needle`,
      )
    ).data.total,
    0,
  );
  const d = (await seal(app, p, "2026-09-16")).data;
  assert.equal((await app.call(`/diary/entries?q=diary-needle`)).data.total, 1);
  for (const e of events)
    assert.equal(
      (await app.call(`/diary/events/${e.id}`, "DELETE", undefined, match(1)))
        .status,
      200,
    );
  assert.equal((await app.call("/diary/events?q=diary-needle")).data.total, 0);
  assert.equal((await app.call("/diary/entries?q=diary-needle")).data.total, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM diary_event_fts").n, 0);
  assert.equal(
    (await app.call(`/diary/entries/${d.id}`)).data.state,
    "invalidated",
  );
});

test("sleep consolidation adopts the existing capture/model grant and only waits for its own canonical diary cycle", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const owner = (await app.call("/auth/me")).data.user.id,
    c = (
      await app.call("/conversations", "POST", { title: "入睡前已授权的来源" })
    ).data;
  await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: c.id,
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
  seededRun(app, c.id, owner);
  const rp = (await app.call("/rest/policy")).data;
  assert.equal(
    (
      await app.call(
        "/rest/policy",
        "PUT",
        { ...rp.value, drain_seconds: 1, consolidation_seconds: 5 },
        match(rp.revision),
      )
    ).status,
    200,
  );
  const sleep = await app.call(
    "/rest/control",
    "POST",
    {
      action: "sleep",
      duration_seconds: 10,
      request_id: randomUUID(),
    },
    match(0),
  );
  assert.equal(sleep.status, 200, JSON.stringify(sleep.data));
  const asleep = await until(
    () => app.call("/rest/status"),
    (r) => r.data.state.phase === "asleep",
    7000,
  );
  assert.ok(asleep.data.state.id);
  assert.equal(remote.calls.length, 1);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_tasks WHERE cycle_id=? AND state='completed'",
      asleep.data.state.id,
    ).n,
    1,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM model_jobs WHERE idempotency_key LIKE 'rest:%'",
    ).n,
    0,
  );
  const policy = app.store.get(
    "SELECT * FROM diary_policies WHERE source_key=?",
    c.id,
  );
  assert.equal(policy.enabled_after, 0);
  assert.equal(policy.grant_revision, 1);
});

test("incomplete fixed parents wait without a lease while another ready period proceeds", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const p = await self(app, true);
  await event(app, "2026-09-17", "长资料".repeat(1000));
  await event(app, "2026-09-17", "长资料二".repeat(900));
  const day = (await seal(app, p, "2026-09-17")).data;
  assert.equal(day.coverage.complete_raw, false);
  const week = (await seal(app, p, "2026-09-17", "week", true)).data;
  tick(app);
  await event(app, "2026-09-22");
  const ready = (await seal(app, p, "2026-09-22", "day", true)).data;
  tick(app);
  await until(
    () => app.call(`/diary/entries/${ready.id}`),
    (r) => r.data.task.state === "completed",
  );
  const waiting = (await app.call(`/diary/entries/${week.id}`)).data;
  assert.equal(waiting.task.state, "waiting_parents");
  assert.equal(waiting.task.error_code, "parents_incomplete");
  assert.equal(waiting.attempts.length, 0);
  assert.equal(remote.calls.length, 1);
});

test("existing explicit record corrections replace the canonical revision and their forgotten origins do not reimport", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const memory = (
    await app.call("/records/memory", "POST", {
      data: { content: "来源甲原文" },
    })
  ).data;
  let e = app.store.get(
    "SELECT * FROM diary_events WHERE origin_kind='record' AND origin_id=?",
    memory.id,
  );
  assert.equal(e.content, "来源甲原文");
  const date = new Intl.DateTimeFormat("en-CA", {
      timeZone: "Asia/Shanghai",
    }).format(new Date()),
    p = await self(app);
  await seal(app, p, date);
  assert.equal(
    (
      await app.call(
        `/records/memory/${memory.id}`,
        "PUT",
        { data: { content: "来源甲已核对的新原文" } },
        match(1),
      )
    ).status,
    200,
  );
  e = app.store.get("SELECT * FROM diary_events WHERE id=?", e.id);
  assert.equal(e.content, "来源甲已核对的新原文");
  assert.equal(e.revision, 2);
  assert.equal(
    app.store.get(
      "SELECT content FROM diary_event_versions WHERE event_id=? AND revision=1",
      e.id,
    ).content,
    null,
  );
  assert.equal(
    (
      await app.call(
        `/records/memory/${memory.id}`,
        "DELETE",
        undefined,
        match(2),
      )
    ).status,
    200,
  );
  tick(app, true);
  assert.equal(
    app.store.get("SELECT content FROM diary_events WHERE id=?", e.id).content,
    null,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='record' AND origin_id=?",
      memory.id,
    ).n,
    1,
  );
});

test("schema6 upgrade preserves existing drafts, current affinity and explicit data with an integrity checked old snapshot", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/diary-schema6-" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db")),
    owner = randomUUID(),
    person = randomUUID(),
    memory = randomUUID(),
    draft = randomUUID(),
    at = now();
  db.exec(readFileSync(new URL("./schema6.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','6')").run();
  db.prepare("INSERT INTO admins VALUES(?,'old-schema6-owner',?,?)").run(
    owner,
    await passwordHash("Synthetic-diary-upgrade-2026!"),
    at,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',1,?,?,?,NULL)",
  ).run(
    person,
    owner,
    JSON.stringify({ name: "已有人物", notes: "保留" }),
    at,
    at,
  );
  db.prepare("INSERT INTO affinity_states VALUES(?,?,?,?,?,?,?)").run(
    person,
    owner,
    49125000,
    at,
    at,
    321,
    1,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'memory',?,'private',1,?,?,?,NULL)",
  ).run(memory, owner, JSON.stringify({ content: "已有显式记录" }), at, at);
  db.prepare(
    "INSERT INTO config_drafts(id,owner_id,base_revision,config_json,created_at) VALUES(?,?,?,?,?)",
  ).run(draft, owner, 1, "{}", at);
  db.close();
  const app = await fixture({ dataDir: directory });
  t.after(() => app.close());
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "11",
  );
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      person,
    ).score_micro,
    49125000,
  );
  assert.equal(
    app.store.get(
      "SELECT remainder FROM affinity_states WHERE person_id=?",
      person,
    ).remainder,
    321,
  );
  assert.equal(
    app.store.get("SELECT config_json FROM config_drafts WHERE id=?", draft)
      .config_json,
    "{}",
  );
  assert.equal(
    app.store.get(
      "SELECT content FROM diary_events WHERE origin_kind='record' AND origin_id=?",
      memory,
    ).content,
    "已有显式记录",
  );
  const snapshot = new DatabaseSync(
    join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((f) =>
        f.startsWith("schema6-"),
      ),
    ),
    { readOnly: true },
  );
  try {
    assert.equal(
      snapshot
        .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
        .get().value,
      "6",
    );
    assert.equal(
      snapshot.prepare("PRAGMA integrity_check").get().integrity_check,
      "ok",
    );
  } finally {
    snapshot.close();
  }
});

test("capture handles late completions and final confirmed delivery, keeps collection separate from model permission and never resurrects revoked messages", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const owner = (await app.call("/auth/me")).data.user.id,
    c = (await app.call("/conversations", "POST", { title: "独立来源" })).data;
  const grant = (capture, allow_model) => ({
    source_key: c.id,
    grant: {
      capture,
      allow_model,
      can_sleep: false,
      can_wake: false,
      expires_at: null,
    },
  });
  const old = seededRun(app, c.id, owner, { text: "授权前原文不能回填" });
  assert.equal(
    (
      await app.call(
        "/knowledge/source/grant",
        "PUT",
        grant(true, false),
        match(0),
      )
    ).status,
    200,
  );
  const p = await app.call("/diary/policies", "POST", {
    source_key: c.id,
    time_zone: "America/New_York",
    visibility: "private",
    enabled: true,
    allow_model: false,
    confirmed: true,
  });
  assert.equal(p.status, 201, JSON.stringify(p.data));
  const late = seededRun(app, c.id, owner, {
      status: "running",
      text: "较早输入稍后完成",
    }),
    newer = seededRun(app, c.id, owner, { text: "较新输入先完成" });
  tick(app, true);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_id=?",
      old.user,
    ).n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_id=?",
      newer.user,
    ).n,
    1,
  );
  app.store.run("UPDATE runs SET status='completed' WHERE id=?", late.id);
  tick(app, true);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_id=?",
      late.user,
    ).n,
    1,
  );
  const channel = seededRun(app, c.id, owner, {
      target: true,
      assistant: "禁止把未发送的末段作为来源",
    }),
    part1 = randomUUID(),
    part2 = randomUUID();
  app.store.run(
    "INSERT INTO delivery_parts VALUES(?,?,0,'text',?,'sending',NULL,NULL,?,?)",
    part1,
    channel.id,
    "已确认发送的首段",
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO delivery_parts VALUES(?,?,1,'text',?,'unknown',NULL,NULL,?,?)",
    part2,
    channel.id,
    "未确认末段",
    now(),
    now(),
  );
  tick(app, true);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='message' AND origin_id IN (SELECT id FROM messages WHERE run_id=? AND role='assistant')",
      channel.id,
    ).n,
    0,
  );
  app.store.run("UPDATE delivery_parts SET status='sent' WHERE id=?", part1);
  tick(app, true);
  const assistantEvent = app.store.get(
    "SELECT * FROM diary_events WHERE origin_id IN (SELECT id FROM messages WHERE run_id=? AND role='assistant')",
    channel.id,
  );
  assert.match(assistantEvent.content, /已确认发送的首段/);
  assert.equal(assistantEvent.content.includes("末段"), false);
  const before = app.store.get(
    "SELECT count(*) AS n FROM diary_events WHERE deleted_at IS NULL",
  ).n;
  assert.equal(
    (
      await app.call(
        "/knowledge/source/grant",
        "PUT",
        grant(true, true),
        match(1),
      )
    ).status,
    200,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE deleted_at IS NULL",
    ).n,
    before,
  );
  assert.equal(
    (
      await app.call(
        "/knowledge/source/grant",
        "PUT",
        grant(false, false),
        match(2),
      )
    ).status,
    200,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE policy_id=? AND content IS NOT NULL",
      p.data.id,
    ).n,
    0,
  );
  assert.equal(
    (
      await app.call(
        "/knowledge/source/grant",
        "PUT",
        grant(true, true),
        match(3),
      )
    ).status,
    200,
  );
  tick(app, true);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE policy_id=? AND content IS NOT NULL",
      p.data.id,
    ).n,
    0,
  );
  const fresh = seededRun(app, c.id, owner, { text: "重新授权之后的全新输入" });
  tick(app, true);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_id=? AND content IS NOT NULL",
      fresh.user,
    ).n,
    1,
  );
  const captured = app.store.get(
    "SELECT * FROM diary_events WHERE origin_id=?",
    fresh.user,
  );
  assert.equal(
    (
      await app.call(
        `/diary/events/${captured.id}`,
        "PATCH",
        {
          content: "本人明确纠正后的来源陈述",
          happened_at: now() - 1,
          statement_type: "user_statement",
        },
        match(captured.revision),
      )
    ).status,
    200,
  );
  tick(app, true);
  assert.equal(
    app.store.get("SELECT content FROM diary_events WHERE id=?", captured.id)
      .content,
    "本人明确纠正后的来源陈述",
  );
  const forbidden = await app.call("/knowledge/jobs", "POST", {
    kind: "memory_candidates",
    sources: [{ type: "message", id: fresh.user }],
  });
  assert.equal(forbidden.data.error.code, "source_invalidated");
  assert.equal(
    (
      await app.call(
        `/diary/events/${captured.id}`,
        "DELETE",
        undefined,
        match(captured.revision + 1),
      )
    ).status,
    200,
  );
  assert.equal(
    app.store.get("SELECT content FROM messages WHERE id=?", fresh.user)
      .content,
    "重新授权之后的全新输入",
  );
});

test("policy cancellation keeps the physical request and credential reference until close and cannot publish a late diary", async (t) => {
  const remote = await provider(t, () => undefined),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const p = await self(app, true);
  await event(app, "2026-09-14");
  const d = (await seal(app, p, "2026-09-14", "day", true)).data;
  tick(app);
  await until(
    () => Promise.resolve(remote.calls.length),
    (n) => n === 1,
  );
  const config = app.store.config().config,
    key = config.model.credential_ref;
  config.model.credential_ref = null;
  app.store.run(
    "UPDATE config_revisions SET config_json=?",
    JSON.stringify(config),
  );
  assert.throws(
    () => app.store.removeSecret(key, "fixture"),
    (e) => e.code === "credential_in_use",
  );
  const current = (await app.call("/diary/policies")).data.items.find(
    (x) => x.id === p.id,
  );
  app.features.diary.savePolicy(
    current.manager_id,
    {
      source_key: current.source_key,
      visibility: current.visibility,
      time_zone: current.time_zone,
      enabled: true,
      allow_model: false,
      confirmed: true,
    },
    p.id,
    current.revision,
  );
  assert.equal(app.runtime.runningCount, 1);
  assert.throws(
    () => app.store.removeSecret(key, "fixture"),
    (e) => e.code === "credential_in_use",
  );
  await until(
    () => Promise.resolve(app.runtime.runningCount),
    (n) => n === 0,
  );
  assert.equal(
    app.store.get("SELECT config_json FROM diary_attempts").config_json,
    null,
  );
  app.store.removeSecret(key, "fixture");
  const final = (await app.call(`/diary/entries/${d.id}`)).data;
  assert.equal(final.state, "invalidated");
  assert.equal(final.version.facts, null);
  assert.equal(app.store.get("SELECT count(*) AS n FROM diary_fragments").n, 0);
  assert.equal(
    app.store.get("SELECT status FROM diary_attempts").status,
    "invalidated",
  );
});
test("explicit events, immutable seals, CAS correction and period tombstones survive restart", async (t) => {
  let app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const p = await self(app),
    key = randomUUID(),
    e = await event(app, "2026-09-10", "独立事件", {}, key);
  assert.equal((await event(app, "2026-09-10", "独立事件", {}, key)).id, e.id);
  let d = await seal(app, p, "2026-09-10");
  assert.equal(d.status, 200, JSON.stringify(d.data));
  assert.equal(d.data.version.generator, "evidence_excerpt_v1");
  assert.equal(d.data.version.source_count, 1);
  assert.equal(d.data.coverage.complete_raw, true);
  const id = d.data.id;
  assert.equal((await seal(app, p, "2026-09-10")).data.revision, 1);
  assert.equal(
    (
      await app.call(
        `/records/diary/${id}`,
        "PUT",
        {
          data: {
            title: "未授出域许可，不创建Native投影",
            content: "x",
            date: "2026-09-10",
          },
        },
        match(1),
      )
    ).status,
    404,
  );
  const corrected = await app.call(
    `/diary/events/${e.id}`,
    "PATCH",
    { content: "明确更正的事件", happened_at: instant("2026-09-10") },
    match(1),
  );
  assert.equal(corrected.status, 200);
  assert.equal(
    (await app.call(`/diary/entries/${id}`)).data.state,
    "invalidated",
  );
  assert.equal(
    app.store.get("SELECT facts FROM diary_entry_versions WHERE entry_id=?", id)
      .facts,
    null,
  );
  d = await seal(app, p, "2026-09-10");
  assert.equal(d.data.revision, 2);
  assert.match(d.data.version.facts, /明确更正/);
  assert.equal(
    (await app.call(`/diary/entries/${id}`, "DELETE", undefined, match(2)))
      .status,
    200,
  );
  assert.equal(
    (await seal(app, p, "2026-09-10")).data.error.code,
    "diary_period_forgotten",
  );
  const directory = app.dataDir;
  await app.close();
  app = await fixture({ dataDir: directory });
  await app.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  assert.equal((await seal(app, p, "2026-09-10")).status, 400);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_entry_versions WHERE facts IS NOT NULL AND entry_id=?",
      id,
    ).n,
    0,
  );
  assert.equal(
    (
      await app.call(
        `/diary/events/${e.id}`,
        "PATCH",
        { content: "不能用旧版本改", happened_at: instant("2026-09-10") },
        match(1),
      )
    ).status,
    409,
  );
});
test("empty past parents are marked unobserved, never call a model and private events are owner scoped", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const p = await self(app, true);
  const month = await seal(app, p, "2026-09-01", "month", true);
  assert.equal(month.status, 200, JSON.stringify(month.data));
  assert.equal(month.data.coverage.parents.length, 30);
  assert.equal(month.data.task.state, "empty");
  tick(app);
  assert.equal(remote.calls.length, 0);
  const first = app.store.get(
    "SELECT v.* FROM diary_entry_versions v JOIN diary_entries e ON e.id=v.entry_id WHERE e.period='day' ORDER BY e.period_start LIMIT 1",
  );
  assert.match(first.facts, /观测不完整/);
  assert.equal(first.observation, "unobserved");
  const e = await event(app, "2026-09-10", "仅甲账户的事件"),
    other = randomUUID();
  app.store.run(
    "INSERT INTO admins VALUES(?,'diary-other',?,?)",
    other,
    await passwordHash("Other-diary-password-2026!"),
    now(),
  );
  await app.call("/auth/login", "POST", {
    username: "diary-other",
    password: "Other-diary-password-2026!",
  });
  assert.equal(
    (await app.call(`/diary/events/${e.id}`, "DELETE", undefined, match(1)))
      .status,
    404,
  );
  assert.equal((await app.call(`/diary/entries/${month.data.id}`)).status, 404);
  assert.equal((await app.call("/diary/events")).data.total, 0);
  assert.equal(
    (
      await app.call(
        "/diary/events",
        "POST",
        {
          content: "共享需要明确发布",
          happened_at: instant("2026-09-10"),
          visibility: "shared",
        },
        { headers: { "idempotency-key": randomUUID() } },
      )
    ).status,
    400,
  );
});

test("successful bounded fragments survive restart and only a final reduction publishes model coverage", async (t) => {
  const remote = await provider(t);
  let app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const p = await self(app, true);
  for (let i = 0; i < 12; i++)
    await event(app, "2026-09-12", `${i}:` + "合成来源".repeat(550));
  const d = await seal(app, p, "2026-09-12", "day", true);
  assert.equal(d.status, 200, JSON.stringify(d.data));
  assert.equal(d.data.coverage.complete_raw, false);
  await until(
    () =>
      Promise.resolve(
        app.store.get(
          "SELECT count(*) AS n FROM diary_fragments WHERE entry_id=?",
          d.data.id,
        ).n,
      ),
    (n) => n === 1,
  );
  app.features.diary.canModel = () => false;
  const part = (await app.call(`/diary/entries/${d.data.id}`)).data;
  assert.equal(part.version.generator, "evidence_excerpt_v1");
  assert.equal(part.task.failures, 0);
  const firstDigest = app.store.get(
      "SELECT input_digest FROM diary_fragments WHERE entry_id=?",
      d.data.id,
    ).input_digest,
    directory = app.dataDir;
  await app.close();
  app = await fixture({ dataDir: directory });
  await app.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  const complete = await until(
    () => app.call(`/diary/entries/${d.data.id}`),
    (r) => r.data.task.state === "completed",
  );
  assert.equal(complete.data.version.generator, "model:synthetic-model");
  assert.equal(complete.data.coverage.complete_raw, true);
  assert.equal(complete.data.version.source_count, 12);
  assert.equal(
    app.store.get(
      "SELECT input_digest FROM diary_fragments WHERE entry_id=? ORDER BY level,ordinal LIMIT 1",
      d.data.id,
    ).input_digest,
    firstDigest,
  );
  assert.ok(remote.calls.length >= 3);
  for (const call of remote.calls) {
    assert.ok(call.bytes <= 48000);
    assert.equal(call.request.max_tokens, 2400);
  }
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_attempts WHERE config_json IS NOT NULL",
    ).n,
    0,
  );
});

test("closed JSON with a length finish remains failed, bounded retries preserve diagnostics and explicit retry succeeds", async (t) => {
  let finish = "length";
  const raw = JSON.stringify({
      facts: "不会因闭合JSON而接受截断的模型正文",
      feelings: "",
    }),
    remote = await provider(t, () => ({ content: raw, finish_reason: finish })),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const p = await self(app, true);
  await event(app, "2026-09-13");
  const d = (await seal(app, p, "2026-09-13", "day", true)).data;
  for (let i = 1; i <= 3; i++) {
    tick(app);
    await until(
      () =>
        Promise.resolve(
          app.store.get(
            "SELECT count(*) AS n FROM diary_attempts WHERE finished_at IS NOT NULL",
          ).n,
        ),
      (n) => n === i,
    );
    if (i < 3)
      app.store.run("UPDATE diary_tasks SET next_at=0 WHERE entry_id=?", d.id);
  }
  const failed = (await app.call(`/diary/entries/${d.id}`)).data;
  assert.equal(failed.task.state, "failed");
  assert.equal(failed.version.generator, "evidence_excerpt_v1");
  assert.equal(failed.attempts[0].output_metadata.finish_reason, "length");
  assert.equal(failed.attempts[0].output_metadata.complete_json, true);
  assert.equal(failed.attempts[0].error_code, "diary_output_truncated");
  assert.equal(JSON.stringify(failed.attempts).includes("不会因闭合"), false);
  finish = "stop";
  assert.equal(
    (await seal(app, p, "2026-09-13", "day", true, { retry_failed: true }))
      .status,
    200,
  );
  tick(app);
  await until(
    () => app.call(`/diary/entries/${d.id}`),
    (r) => r.data.task.state === "completed",
  );
  assert.equal(remote.calls.length, 4);
});

test("a month covers all 310 sources with fixed day versions and forgetting the final source clears every reduction and dependent", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const p = await self(app, true),
    events = [];
  for (let day = 1; day <= 31; day++) {
    const date = `2026-08-${String(day).padStart(2, "0")}`;
    for (let n = 0; n < 10; n++)
      events.push(
        await event(app, date, `有来源的${day}-${n} ` + "原始材料".repeat(12)),
      );
    const d = await seal(app, p, date);
    assert.equal(d.status, 200);
    assert.equal(d.data.coverage.complete_raw, true);
  }
  const made = await seal(app, p, "2026-08-01", "month", true);
  assert.equal(made.status, 200, JSON.stringify(made.data));
  assert.equal(made.data.coverage.parents.length, 31);
  assert.equal(made.data.sources.length, 310);
  const ready = await until(
    () => app.call(`/diary/entries/${made.data.id}`),
    (r) => r.data.task.state === "completed",
    12000,
  );
  assert.equal(ready.data.version.source_count, 310);
  assert.equal(ready.data.coverage.complete_raw, true);
  assert.ok(remote.calls.length >= 2);
  const firstParent = ready.data.coverage.parents[0],
    dayEntry = (await app.call(`/diary/entries/${firstParent.parent_id}`)).data;
  await seal(app, p, dayEntry.period_start, "day", true);
  await until(
    () => app.call(`/diary/entries/${firstParent.parent_id}`),
    (r) => r.data.task.state === "completed",
  );
  const again = (await seal(app, p, "2026-08-01", "month", true)).data;
  assert.equal(again.revision, ready.data.revision);
  assert.equal(
    again.coverage.parents[0].parent_revision,
    firstParent.parent_revision,
  );
  const legacyResponse = await app.call("/records/diary", "POST", {
    data: {
      title: "明确来源的派生记录",
      content: "需要随月来源撤回",
      date: "2026-08-31",
      sources: [made.data.id],
    },
  });
  assert.equal(legacyResponse.status, 201, JSON.stringify(legacyResponse.data));
  const legacy = legacyResponse.data;
  const grandchildResponse = await app.call("/records/diary", "POST", {
    data: {
      title: "更深的派生记录",
      content: "需要一并撤回",
      date: "2026-08-31",
      sources: [legacy.id],
    },
  });
  assert.equal(
    grandchildResponse.status,
    201,
    JSON.stringify(grandchildResponse.data),
  );
  const grandchild = grandchildResponse.data;
  const last = events.at(-1);
  assert.equal(
    (await app.call(`/diary/events/${last.id}`, "DELETE", undefined, match(1)))
      .status,
    200,
  );
  const gone = (await app.call(`/diary/entries/${made.data.id}`)).data;
  assert.equal(gone.state, "invalidated");
  assert.equal(gone.version.facts, null);
  assert.ok(
    gone.versions.every((v) => v.facts === null && v.feelings === null),
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_fragments WHERE entry_id=?",
      made.data.id,
    ).n,
    0,
  );
  assert.equal((await app.call(`/records/diary/${legacy.id}`)).status, 404);
  assert.equal((await app.call(`/records/diary/${grandchild.id}`)).status, 404);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_id IN (?,?) AND content IS NOT NULL",
      legacy.id,
      grandchild.id,
    ).n,
    0,
  );
});
