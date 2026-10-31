import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID, createHash } from "node:crypto";
import { createServer } from "node:http";
import { DatabaseSync } from "node:sqlite";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { fixture, until } from "./helpers.mjs";
import { now, passwordHash, privateDirectory } from "../dist/security.js";
const match = (n) => ({ headers: { "if-match": '"' + n + '"' } });
async function setup(t, model = false, handler) {
  const calls = [];
  let remote;
  if (model) {
    remote = createServer(async (req, res) => {
      let raw = "";
      for await (const c of req) raw += c;
      const body = JSON.parse(raw);
      calls.push({ body, bytes: Buffer.byteLength(raw) });
      const answer = handler
        ? await handler(body, calls.length)
        : { facts: "已依据获授权资料完成本次归并。", feelings: "" };
      if (answer !== undefined && !res.destroyed) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            choices: [
              {
                message: { content: JSON.stringify(answer) },
                finish_reason: "stop",
              },
            ],
          }),
        );
      }
    });
    await new Promise((r) => remote.listen(0, "127.0.0.1", r));
  }
  const app = await fixture();
  t.after(async () => {
    await app.close();
    if (remote)
      await new Promise((r) => {
        remote.closeAllConnections();
        remote.close(r);
      });
  });
  await app.bootstrap();
  if (model) await app.configure("http://127.0.0.1:" + remote.address().port);
  await app.features.knowledge.stop();
  app.features.automation.onScheduleTick = () => {};
  app.features.diary.lastSweep = now();
  return { app, calls };
}
const data = (content, extra = {}) => ({
  title: "明确导入的历史日记",
  date: "2024-02-16",
  period: "day",
  content,
  feelings: "",
  ...extra,
});
async function record(app, content, extra = {}) {
  const r = await app.call("/records/diary", "POST", {
    data: data(content, extra),
  });
  assert.equal(r.status, 201, JSON.stringify(r.data.error));
  return r.data;
}
async function modelPolicy(app) {
  let p = (await app.call("/diary/policies")).data.items.find(
    (p) => p.source_key.startsWith("self:") && p.visibility === "private",
  );
  const r = await app.call(
    "/diary/policies/" + p.id,
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
  );
  assert.equal(r.status, 200);
  return r.data;
}
function sourceEvent(app, r) {
  return app.store.get(
    "SELECT * FROM diary_events WHERE origin_kind='record' AND origin_id=? AND deleted_at IS NULL",
    r.id,
  );
}
function run(app) {
  app.features.automation.onScheduleTick = () => app.features.diary.tick();
  app.features.diary.tick();
}
async function seal(app, p, record, model = true) {
  const r = await app.call("/diary/consolidate", "POST", {
    policy_id: p.id,
    date: record.data.date,
    period: "day",
    model,
  });
  assert.equal(r.status, 200, JSON.stringify(r.data));
  return r.data;
}
async function done(app, id, ms = 15000) {
  return until(
    () =>
      app.features.diary.entry(
        app.store.get("SELECT owner_id FROM diary_entries WHERE id=?", id)
          .owner_id,
        id,
      ),
    (e) => e.task.state === "completed",
    ms,
  );
}
async function sleep(app) {
  const p = (await app.call("/rest/policy")).data;
  assert.equal(
    (
      await app.call(
        "/rest/policy",
        "PUT",
        { ...p.value, drain_seconds: 0, consolidation_seconds: 60 },
        match(p.revision),
      )
    ).status,
    200,
  );
  const r = await app.call(
    "/rest/control",
    "POST",
    { action: "sleep", duration_seconds: 300, request_id: randomUUID() },
    match(0),
  );
  assert.equal(r.status, 200, JSON.stringify(r.data));
  return r.data.state.id;
}
function wake(app) {
  const state = app.features.automation.restStatus().state;
  app.features.automation.restControl(
    "test",
    { action: "wake", request_id: randomUUID() },
    state.revision,
  );
}

test("the originally accepted 13000-byte diary is synchronously linked and a maximum-size manual record preserves all fields and reference-period semantics", async (t) => {
  const { app, calls } = await setup(t);
  const r = await record(app, "L".repeat(13000));
  assert.equal(r.diary_ingestion.state, "imported");
  assert.equal(sourceEvent(app, r).origin_revision, 1);
  assert.match(sourceEvent(app, r).content, /L{13000}/);
  const maximum = await record(app, "C".repeat(16000), {
    title: "T".repeat(200),
    feelings: "F".repeat(4000),
    period: "week",
  });
  const e = sourceEvent(app, maximum);
  assert.ok(Buffer.byteLength(e.content) > 20000);
  assert.ok(Buffer.byteLength(e.content) <= 24000);
  assert.match(e.content, /历史明确周记导入/);
  assert.match(e.content, /原参考日期2024-02-16/);
  assert.match(e.content, /不表示当时已观测全天/);
  assert.match(e.content, /F{4000}/);
  assert.deepEqual(
    (await app.call("/records/diary/" + maximum.id)).data.data,
    maximum.data,
  );
  assert.equal(
    (await app.call("/records/diary?limit=10")).data.items.find(
      (i) => i.id === r.id,
    ).diary_ingestion.state,
    "imported",
  );
  assert.equal(calls.length, 0);
});

test("a long-record correction and forgetting retain one source identity, clear all old versions and atomically invalidate canonical coverage", async (t) => {
  const { app } = await setup(t),
    owner = (await app.call("/auth/me")).data.user.id,
    p = (await app.call("/diary/policies")).data.items[0],
    r = await record(app, "OLD_LONG_CANARY_" + "A".repeat(13000)),
    event = sourceEvent(app, r),
    entry = await seal(app, p, r, false);
  const corrected = await app.call(
    "/records/diary/" + r.id,
    "PUT",
    {
      data: data(("NEW_LONG_CANARY_" + "B".repeat(16000)).slice(0, 16000), {
        feelings: "F".repeat(4000),
      }),
    },
    match(1),
  );
  assert.equal(corrected.status, 200, JSON.stringify(corrected.data));
  const updated = sourceEvent(app, r);
  assert.equal(updated.id, event.id);
  assert.equal(updated.revision, 2);
  assert.equal(updated.origin_revision, 2);
  assert.equal(corrected.data.diary_ingestion.record_revision, 2);
  assert.equal(app.features.diary.entry(owner, entry.id).state, "invalidated");
  assert.equal(
    app.store.get(
      "SELECT content FROM diary_event_versions WHERE event_id=? AND revision=1",
      event.id,
    ).content,
    null,
  );
  assert.equal(
    (
      await app.call(
        "/records/diary/" + r.id,
        "PUT",
        { data: data("不允许旧版本覆盖") },
        match(1),
      )
    ).status,
    409,
  );
  assert.equal(
    (await app.call("/records/diary/" + r.id, "DELETE", undefined, match(2)))
      .status,
    200,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_event_versions WHERE event_id=? AND content IS NOT NULL",
      event.id,
    ).n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT state FROM diary_record_imports WHERE record_id=?",
      r.id,
    ).state,
    "forgotten",
  );
  app.features.diary.tick();
  assert.equal(sourceEvent(app, r), undefined);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='record' AND origin_id=?",
      r.id,
    ).n,
    1,
  );
});

test("database failure in the import-status write rolls back the original save and its canonical event instead of returning a false success", async (t) => {
  const { app } = await setup(t);
  app.store.database.exec(
    "CREATE TRIGGER fail_record_ingestion BEFORE INSERT ON diary_record_imports BEGIN SELECT RAISE(ABORT,'synthetic ingestion write failure'); END",
  );
  const failed = await app.call("/records/diary", "POST", {
    data: data("L".repeat(13000)),
  });
  assert.equal(failed.status, 500);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM records WHERE kind='diary'").n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='record'",
    ).n,
    0,
  );
  app.store.database.exec("DROP TRIGGER fail_record_ingestion");
  const r = await record(app, "L".repeat(13000));
  assert.equal(r.diary_ingestion.state, "imported");
});

test("sensitive and derived records have explicit exclusion reasons, stay distinct from raw evidence, and keep actor authorization", async (t) => {
  const { app } = await setup(t);
  const sensitive = await record(app, "password=synthetic-only-secret");
  assert.equal(sensitive.diary_ingestion.state, "excluded");
  assert.equal(sensitive.diary_ingestion.reason_code, "sensitive_content");
  assert.equal(sourceEvent(app, sensitive), undefined);
  const memory = (
    await app.call("/records/memory", "POST", {
      data: { content: "明确的合成来源" },
    })
  ).data;
  const derived = await app.call("/records/diary", "POST", {
    data: { ...data("来自明确来源的派生视图"), sources: [memory.id] },
  });
  assert.equal(derived.status, 201);
  assert.equal(derived.data.diary_ingestion.reason_code, "derived_view");
  assert.equal(sourceEvent(app, derived.data), undefined);
  assert.equal(
    (
      await app.call("/records/diary/" + sensitive.id, "GET", undefined, {
        auth: false,
      })
    ).status,
    401,
  );
});

test("the actual model receives the entire maximum-size explicit record and publishes only after complete coverage", async (t) => {
  const { app, calls } = await setup(t, true),
    p = await modelPolicy(app),
    r = await record(app, "C".repeat(16000), {
      title: "T".repeat(200),
      feelings: "F".repeat(4000),
    }),
    entry = await seal(app, p, r);
  assert.equal(entry.coverage.complete_raw, false);
  run(app);
  const final = await done(app, entry.id);
  assert.equal(final.coverage.complete_raw, true);
  assert.equal(final.coverage.sources, 1);
  assert.ok(calls.length >= 1);
  const prompt = JSON.stringify(calls[0].body);
  assert.ok(prompt.includes("C".repeat(16000)));
  assert.ok(prompt.includes("F".repeat(4000)));
  assert.ok(calls.every((c) => c.bytes <= 48000));
  assert.ok(final.version.generator.startsWith("model:"));
});

test("a heavily escaped valid record is reduced in bounded source-unit requests without inflating source counts or publishing partial work", async (t) => {
  const { app, calls } = await setup(t, true),
    p = await modelPolicy(app),
    r = await record(app, '"'.repeat(13000)),
    entry = await seal(app, p, r);
  run(app);
  const final = await done(app, entry.id, 20000);
  assert.equal(final.coverage.sources, 1);
  assert.equal(final.coverage.complete_raw, true);
  assert.ok(final.unit_fragments_count > 0);
  assert.ok(calls.length > 1);
  assert.ok(calls.length <= 8);
  assert.ok(calls.every((c) => c.bytes <= 48000));
  const inner = calls.filter(
    (c) => JSON.parse(c.body.messages[1].content).source_unit_reduction,
  );
  assert.ok(inner.length);
  assert.ok(
    inner.every(
      (c) =>
        JSON.parse(c.body.messages[1].content).reduction.final_batch === false,
    ),
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_entry_sources WHERE entry_id=?",
      entry.id,
    ).n,
    1,
  );
});

test("an in-flight source-unit request keeps its physical lease on correction and never publishes a late body or reusable unit cache", async (t) => {
  let release;
  const { app, calls } = await setup(t, true, async () => {
    await new Promise((r) => (release = r));
    return { facts: "LATE_UNIT_MUST_NOT_SAVE", feelings: "" };
  });
  t.after(() => release?.());
  const p = await modelPolicy(app),
    r = await record(app, '"'.repeat(13000)),
    entry = await seal(app, p, r);
  run(app);
  await until(
    () => calls.length,
    (n) => n === 1,
  );
  assert.ok(
    JSON.parse(calls[0].body.messages[1].content).source_unit_reduction,
  );
  const changed = await app.call(
    "/records/diary/" + r.id,
    "PUT",
    { data: data("人工更正后的完整事实") },
    match(1),
  );
  assert.equal(changed.status, 200);
  assert.equal(app.runtime.runningCount, 1);
  release();
  await until(
    () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_unit_fragments WHERE entry_id=?",
      entry.id,
    ).n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_entry_versions WHERE instr(coalesce(facts,''),'LATE_UNIT_MUST_NOT_SAVE')>0",
    ).n,
    0,
  );
  assert.equal(
    app.features.diary.entry(p.manager_id, entry.id).state,
    "invalidated",
  );
});

test("frozen explicit record versions include old pending originals but never accept an edit or a newly created record after the cutoff", async (t) => {
  const { app } = await setup(t);
  app.features.automation.onRestConsolidate = () => {};
  const r = await record(app, "BEFORE_FROZEN_RECORD_" + "L".repeat(13000)),
    cycle = await sleep(app);
  const later = await record(app, "AFTER_ACCEPT_RECORD");
  assert.ok(
    !app.store.get(
      "SELECT record_id FROM rest_diary_records WHERE cycle_id=? AND record_id=?",
      cycle,
      later.id,
    ),
  );
  assert.equal(
    (
      await app.call(
        "/records/diary/" + r.id,
        "PUT",
        { data: data("EDIT_AFTER_RECORD_SNAPSHOT") },
        match(1),
      )
    ).status,
    200,
  );
  await until(
    () => app.features.automation.restStatus().state.phase,
    (s) => s === "consolidating",
  );
  app.features.diary.prepareRest(cycle);
  app.features.automation.onScheduleTick = () => app.features.diary.tick();
  await until(
    () => app.features.automation.restStatus().state.phase,
    (s) => s === "asleep",
    10000,
  );
  assert.equal(
    app.store.get(
      "SELECT status FROM rest_diary_cycles WHERE cycle_id=?",
      cycle,
    ).status,
    "partial",
  );
  assert.equal(
    app.store.get(
      "SELECT status FROM rest_diary_records WHERE cycle_id=? AND record_id=?",
      cycle,
      r.id,
    ).status,
    "invalidated",
  );
  assert.equal(sourceEvent(app, r).origin_revision, 2);
});

test("schema14 records skipped behind the old cursor are repaired across pages and a sleep snapshots still-pending originals", async (t) => {
  const dir = privateDirectory(
      resolve(".local/prometheus/tests/record14-repair-" + randomUUID()),
    ),
    db = new DatabaseSync(join(dir, "prometheus.db")),
    owner = randomUUID(),
    ids = [],
    at = now();
  db.exec(readFileSync(new URL("./schema14.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES(?,?)").run("prometheus.schema", "14");
  db.prepare("INSERT INTO metadata VALUES(?,?)").run(
    "assistant.identity",
    "record-repair-stable",
  );
  db.prepare("INSERT INTO metadata VALUES(?,?)").run(
    "diary.record_cursor",
    "1000000",
  );
  db.prepare("INSERT INTO admins VALUES(?,'tester',?,?)").run(
    owner,
    await passwordHash("Synthetic-password-2026!"),
    at,
  );
  const insert = db.prepare(
    "INSERT INTO records VALUES(?,'diary',?,'private',1,?,?,?,NULL)",
  );
  for (let i = 0; i < 95; i++) {
    const id = randomUUID();
    ids.push(id);
    insert.run(
      id,
      owner,
      JSON.stringify({
        ...data("LEGACY_" + i + "_" + "L".repeat(13000)),
        generator: "manual",
        sources: [],
        timezone: "Asia/Shanghai",
      }),
      at - 20,
      at - 20,
    );
  }
  db.close();
  const app = await fixture({ dataDir: dir });
  t.after(() => app.close());
  app.features.automation.onScheduleTick = () => {};
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "15",
  );
  assert.equal(app.store.assistantId, "record-repair-stable");
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM diary_record_imports").n,
    64,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='record'",
    ).n,
    64,
  );
  assert.ok(
    Number(
      app.store.get(
        "SELECT value FROM metadata WHERE key='diary.record_cursor'",
      ).value,
    ) < 95,
  );
  await app.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  const cycle = await sleep(app);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM rest_diary_records WHERE cycle_id=?",
      cycle,
    ).n,
    95,
  );
  run(app);
  await until(
    () => app.features.automation.restStatus().state.phase,
    (s) => s === "asleep",
    15000,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_record_imports WHERE state='imported'",
    ).n,
    95,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM rest_diary_records WHERE cycle_id=? AND status='captured'",
      cycle,
    ).n,
    95,
  );
  for (const id of ids)
    assert.equal(
      app.store.get(
        "SELECT count(*) AS n FROM diary_events WHERE origin_id=? AND content IS NOT NULL",
        id,
      ).n,
      1,
    );
  const snapshot = join(
      dir,
      "backups",
      readdirSync(join(dir, "backups")).find((n) => n.startsWith("schema14-")),
    ),
    old = new DatabaseSync(snapshot, { readOnly: true });
  assert.equal(
    old.prepare("PRAGMA integrity_check").get().integrity_check,
    "ok",
  );
  assert.equal(
    old
      .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .get().value,
    "14",
  );
  assert.equal(
    old
      .prepare("SELECT value FROM metadata WHERE key='diary.record_cursor'")
      .get().value,
    "1000000",
  );
  assert.equal(
    old
      .prepare(
        "SELECT count(*) AS n FROM diary_events WHERE origin_kind='record'",
      )
      .get().n,
    0,
  );
  old.close();
});

test("successful inner unit requests survive a restart and an explicit retry resumes only unsaved ranges", async (t) => {
  let release;
  const { app, calls } = await setup(t, true, async (_b, n) => {
    if (n === 2) await new Promise((r) => (release = r));
    return { facts: "单来源成功片段" + n, feelings: "" };
  });
  t.after(() => release?.());
  const p = await modelPolicy(app),
    r = await record(app, '"'.repeat(16000), {
      feelings: "\u0001".repeat(4000),
    }),
    entry = await seal(app, p, r);
  run(app);
  await until(
    () => calls.length,
    (n) => n === 2,
    10000,
  );
  const saved = app.store.get(
    "SELECT * FROM diary_unit_fragments WHERE entry_id=? ORDER BY sub_level,ordinal LIMIT 1",
    entry.id,
  );
  assert.ok(saved);
  const dir = app.dataDir;
  const closing = app.close();
  release();
  await closing;
  const next = await fixture({ dataDir: dir });
  t.after(() => next.close());
  await next.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  const retry = await next.call("/diary/consolidate", "POST", {
    policy_id: p.id,
    date: r.data.date,
    period: "day",
    model: true,
    retry_failed: true,
  });
  assert.equal(retry.status, 200);
  next.store.run("UPDATE diary_tasks SET next_at=0 WHERE entry_id=?", entry.id);
  run(next);
  await until(
    () => calls.length,
    (n) => n >= 3,
    10000,
  );
  const resumed = JSON.parse(calls[2].body.messages[1].content);
  assert.equal(resumed.source_unit_reduction.unit_start, saved.unit_end);
  assert.deepEqual(
    {
      ...next.store.get(
        "SELECT * FROM diary_unit_fragments WHERE entry_id=? AND entry_revision=? AND sub_level=? AND ordinal=?",
        entry.id,
        saved.entry_revision,
        saved.sub_level,
        saved.ordinal,
      ),
    },
    { ...saved },
  );
  await until(
    () => next.runtime.runningCount,
    (n) => n === 0,
  );
  assert.ok(calls.every((c) => c.bytes <= 48000));
});

test("a small configured context budget is applied while planning fragments, without waiting forever on a larger already-built request", async (t) => {
  const { app, calls } = await setup(t, true),
    p = await modelPolicy(app),
    policy = (await app.call("/context/policy")).data;
  // Use the real saved budget interface; this is not a clock or request injection.
  const saved = await app.call(
    "/context/policy",
    "PUT",
    { ...policy.value, max_input_bytes: 12000, estimated_input_limit: 12000 },
    match(policy.revision),
  );
  assert.equal(saved.status, 200, JSON.stringify(saved.data));
  const r = await record(app, "L".repeat(13000)),
    entry = await seal(app, p, r);
  run(app);
  const final = await done(app, entry.id, 15000);
  assert.equal(final.coverage.complete_raw, true);
  assert.ok(final.unit_fragments_count > 0);
  assert.ok(calls.every((c) => c.bytes <= 12000));
});

test("legacy diary views obey explicit model revocation in the SQL query and credential-like memory never falls back into Native context", async (t) => {
  const { app } = await setup(t),
    p = await modelPolicy(app),
    r = await record(app, "MODEL_SCOPE_CANARY"),
    owner = p.manager_id;
  assert.ok(
    app.runtime.records
      .nativeContext(owner, "MODEL_SCOPE_CANARY")
      .refs.some((i) => i.id === r.id),
  );
  const disabled = await app.call(
    "/diary/policies/" + p.id,
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
  assert.equal(disabled.status, 200);
  assert.ok(
    !app.runtime.records
      .nativeContext(owner, "MODEL_SCOPE_CANARY")
      .refs.some((i) => i.id === r.id),
  );
  const secret = (
    await app.call("/records/memory", "POST", {
      data: { content: "password=synthetic-private-only" },
    })
  ).data;
  assert.equal(secret.diary_ingestion.reason_code, "sensitive_content");
  const explicit = (
    await app.call("/records/memory", "POST", {
      data: { content: "正常的明确记忆仍可用于后续对话" },
    })
  ).data;
  const context = app.runtime.records.nativeContext(owner, "");
  assert.ok(!context.refs.some((i) => i.id === secret.id));
  assert.ok(context.refs.some((i) => i.id === explicit.id));
  assert.ok(!context.text.includes("synthetic-private-only"));
});

test("forgetting after a successful inner checkpoint clears every derived unit row while keeping cancellation tied to the actual HTTP lease", async (t) => {
  let release;
  const { app, calls } = await setup(t, true, async (_b, n) => {
    if (n === 2) await new Promise((r) => (release = r));
    return {
      facts:
        n === 1 ? "SAVED_INNER_BODY_TO_FORGET" : "LATE_INNER_BODY_TO_REJECT",
      feelings: "",
    };
  });
  t.after(() => release?.());
  const p = await modelPolicy(app),
    r = await record(app, '"'.repeat(13000)),
    entry = await seal(app, p, r);
  run(app);
  await until(
    () => calls.length,
    (n) => n === 2,
    10000,
  );
  assert.ok(
    app.store.get(
      "SELECT count(*) AS n FROM diary_unit_fragments WHERE entry_id=?",
      entry.id,
    ).n > 0,
  );
  const forgotten = app.runtime.records.forget("diary", r.id, p.manager_id, 1);
  assert.equal(forgotten.deleted, true);
  assert.equal(app.runtime.runningCount, 1);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_unit_fragments WHERE entry_id=?",
      entry.id,
    ).n,
    0,
  );
  release();
  await until(
    () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_entry_versions WHERE instr(coalesce(facts,''),'INNER_BODY')>0",
    ).n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_event_versions WHERE event_id=? AND content IS NOT NULL",
      r.diary_ingestion.event_id,
    ).n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT state FROM diary_record_imports WHERE record_id=?",
      r.id,
    ).state,
    "forgotten",
  );
});

test("maximum escaped fields retain a completed unit checkpoint across sleep budget exhaustion and resume without recharging its completed request", async (t) => {
  const { app, calls } = await setup(t, true),
    p = await modelPolicy(app),
    r = await record(app, '"'.repeat(16000), {
      title: "T".repeat(200),
      feelings: "\u0001".repeat(4000),
    }),
    date = new Intl.DateTimeFormat("en-CA", {
      timeZone: "Asia/Shanghai",
    }).format(new Date());
  app.features.knowledge.setPolicy("budget:" + date, 0, { calls: 31 });
  const first = await sleep(app);
  run(app);
  await until(
    () =>
      app.store.get(
        "SELECT entry_id FROM diary_tasks WHERE cycle_id=? AND error_code='daily_budget_wait'",
        first,
      ),
    Boolean,
    10000,
  );
  assert.equal(calls.length, 1);
  const event = sourceEvent(app, r),
    entry = app.store.get(
      "SELECT entry_id FROM diary_entry_sources WHERE event_id=? ORDER BY rowid LIMIT 1",
      event.id,
    ),
    saved = app.store.get(
      "SELECT * FROM diary_unit_fragments WHERE entry_id=?",
      entry.entry_id,
    );
  assert.ok(saved);
  wake(app);
  assert.equal(
    app.store.get(
      "SELECT status FROM rest_diary_cycles WHERE cycle_id=?",
      first,
    ).status,
    "partial",
  );
  // Advance only synthetic accounting evidence. No production clock or HTTP budget-reset interface exists.
  app.store.run(
    "UPDATE diary_attempts SET started_at=started_at-86401 WHERE entry_id=?",
    entry.entry_id,
  );
  const budget = app.features.knowledge.policy("budget:" + date, { calls: 0 });
  app.features.knowledge.setPolicy("budget:" + date, budget.revision, {
    calls: 0,
  });
  app.store.run(
    "UPDATE diary_tasks SET next_at=0 WHERE entry_id=?",
    entry.entry_id,
  );
  const next = await sleep(app);
  await until(
    () => app.features.automation.restStatus().state.phase,
    (s) => s === "asleep",
    20000,
  ).catch((error) => {
    throw new Error(
      JSON.stringify({
        calls: calls.length,
        rest: app.features.automation.restStatus(),
        tasks: app.store.all(
          "SELECT entry_id,state,next_at,error_code FROM diary_tasks WHERE cycle_id=?",
          next,
        ),
        targets: app.store.all(
          "SELECT period,state,error_code FROM rest_diary_targets WHERE cycle_id=?",
          next,
        ),
      }),
      { cause: error },
    );
  });
  const resumed = JSON.parse(calls[1].body.messages[1].content);
  assert.equal(resumed.source_unit_reduction.unit_start, saved.unit_end);
  assert.deepEqual(
    {
      ...app.store.get(
        "SELECT * FROM diary_unit_fragments WHERE entry_id=? AND entry_revision=? AND sub_level=? AND ordinal=?",
        saved.entry_id,
        saved.entry_revision,
        saved.sub_level,
        saved.ordinal,
      ),
    },
    { ...saved },
  );
  const final = app.features.diary.entry(p.manager_id, entry.entry_id);
  assert.equal(final.coverage.sources, 1);
  assert.equal(final.coverage.complete_raw, true);
  assert.equal(
    app.store.get("SELECT status FROM rest_diary_cycles WHERE cycle_id=?", next)
      .status,
    "completed",
  );
  assert.ok(calls.every((c) => c.bytes <= 48000));
});

test("record metadata preserves its Shanghai reference calendar when the receiving diary policy uses a different civil zone", async (t) => {
  const { app, calls } = await setup(t, true);
  let p = await modelPolicy(app);
  p = (
    await app.call(
      "/diary/policies/" + p.id,
      "PATCH",
      {
        source_key: p.source_key,
        visibility: p.visibility,
        time_zone: "America/New_York",
        enabled: true,
        allow_model: true,
        confirmed: true,
      },
      match(p.revision),
    )
  ).data;
  const r = await record(app, "NY_SOURCE_REFERENCE", { period: "week" }),
    event = sourceEvent(app, r);
  assert.match(event.content, /Asia\/Shanghai/);
  const e = await app.call("/diary/consolidate", "POST", {
    policy_id: p.id,
    date: "2024-02-15",
    period: "day",
    model: true,
  });
  assert.equal(e.status, 200);
  run(app);
  await done(app, e.data.id);
  const body = JSON.parse(calls[0].body.messages[1].content);
  assert.equal(body.time_zone, "America/New_York");
  assert.equal(body.source_records[0].reference_date, "2024-02-16");
  assert.equal(body.source_records[0].reference_period, "week");
  assert.equal(body.source_records[0].reference_time_zone, "Asia/Shanghai");
  assert.equal(body.source_records[0].source_event_id, event.id);
});

test("record ingestion state and canonical content remain inaccessible to a different authenticated administrator", async (t) => {
  const { app } = await setup(t),
    r = await record(app, "PRIVATE_RECORD_METADATA_CANARY"),
    actor = randomUUID();
  app.store.run(
    "INSERT INTO admins VALUES(?,'second-record-reader',?,?)",
    actor,
    await passwordHash("Synthetic-second-reader-2026!"),
    now(),
  );
  assert.equal(
    (
      await app.call("/auth/login", "POST", {
        username: "second-record-reader",
        password: "Synthetic-second-reader-2026!",
      })
    ).status,
    200,
  );
  assert.equal((await app.call("/records/diary/" + r.id)).status, 404);
  assert.equal(
    (await app.call("/diary/events/" + r.diary_ingestion.event_id)).status,
    404,
  );
  const list = await app.call("/records/diary?limit=10");
  assert.equal(list.data.total, 0);
  assert.ok(!JSON.stringify(list.data).includes(r.id));
});
