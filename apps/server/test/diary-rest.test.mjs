import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { fixture, until } from "./helpers.mjs";
import { DatabaseSync } from "node:sqlite";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { privateDirectory } from "../dist/security.js";
import { diaryBounds, diaryDate, dateAdd } from "../dist/diary-time.js";
import { now } from "../dist/security.js";
const match = (n) => ({ headers: { "if-match": `"${n}"` } });
async function setup(t, handle) {
  const calls = [];
  const remote = createServer(async (req, res) => {
    let raw = "";
    for await (const part of req) raw += part;
    const body = JSON.parse(raw);
    calls.push({ body, res });
    const value = handle
      ? await handle(body, calls.length, res)
      : { facts: "本次冻结来源中的事实。", feelings: "" };
    if (value !== undefined && !res.destroyed) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          choices: [
            {
              message: {
                content:
                  typeof value === "string" ? value : JSON.stringify(value),
              },
              finish_reason: "stop",
            },
          ],
        }),
      );
    }
  });
  await new Promise((r) => remote.listen(0, "127.0.0.1", r));
  const app = await fixture();
  t.after(async () => {
    await app.close();
    await new Promise((r) => {
      remote.closeAllConnections();
      remote.close(r);
    });
  });
  await app.bootstrap();
  await app.configure("http://127.0.0.1:" + remote.address().port);
  await app.features.knowledge.stop();
  const policy = (await app.call("/rest/policy")).data;
  assert.equal(
    (
      await app.call(
        "/rest/policy",
        "PUT",
        { ...policy.value, drain_seconds: 0, consolidation_seconds: 120 },
        match(policy.revision),
      )
    ).status,
    200,
  );
  return { app, calls };
}
function seeded(app, c, owner, text = "获授权的冻结文字", options = {}) {
  const r = randomUUID(),
    u = randomUUID(),
    a = randomUUID(),
    at = now();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,?,?,?,?)",
    r,
    c,
    owner,
    randomUUID(),
    text,
    options.status ?? "completed",
    options.reply ?? "已保存的合成回复。",
    at,
    at,
  );
  app.store.run(
    "INSERT INTO messages VALUES(?,?,?,'user',?,?)",
    u,
    c,
    r,
    text,
    at,
  );
  if (options.status !== "running")
    app.store.run(
      "INSERT INTO messages VALUES(?,?,?,'assistant',?,?)",
      a,
      c,
      r,
      options.reply ?? "已保存的合成回复。",
      at,
    );
  return { r, u, a };
}
async function source(app, label, model = false, extra = {}) {
  const owner = (await app.call("/auth/me")).data.user.id,
    c = (await app.call("/conversations", "POST", { title: label })).data;
  const grant = await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: c.id,
      grant: {
        capture: true,
        allow_model: model,
        can_sleep: false,
        can_wake: false,
        expires_at: null,
        ...extra,
      },
    },
    match(0),
  );
  assert.equal(grant.status, 200);
  return { owner, c, grant: grant.data };
}
async function sleep(app) {
  const r = await app.call(
    "/rest/control",
    "POST",
    { action: "sleep", duration_seconds: 300, request_id: randomUUID() },
    match(0),
  );
  assert.equal(r.status, 200, JSON.stringify(r.data));
  return r.data.state.id;
}
const cycles = async (app) => (await app.call("/diary/rest-cycles")).data.items;
async function prepare(app, cycle) {
  await until(
    () => app.features.automation.restStatus().state.phase,
    (p) => p === "consolidating",
  );
  app.features.diary.prepareRest(cycle);
}
test("one sleep freezes and locally prepares all 33 granted conversations across bounded pages without granting model access", async (t) => {
  const { app, calls } = await setup(t);
  const expected = [];
  for (let i = 0; i < 33; i++) {
    const s = await source(app, "独立来源" + i);
    seeded(app, s.c.id, s.owner, "ONLY_SOURCE_" + i);
    expected.push(s.c.id);
  }
  const cycle = await sleep(app);
  await until(
    () => app.features.automation.restStatus().state.phase,
    (s) => s === "asleep",
    20000,
  );
  const report = (await cycles(app)).find((c) => c.cycle_id === cycle);
  assert.equal(report.status, "completed");
  assert.equal(report.sources.length, 34);
  assert.equal(
    report.sources.filter((s) => s.status === "prepared").length,
    34,
  );
  assert.equal(calls.length, 0);
  for (const key of expected) {
    const p = app.store.get(
      "SELECT id FROM diary_policies WHERE source_key=?",
      key,
    );
    const e = app.store.get(
      "SELECT * FROM diary_entries WHERE policy_id=? AND period='day' ORDER BY period_start DESC LIMIT 1",
      p.id,
    );
    const v = app.features.diary.entry(
      report.sources[0].manager_id ?? (await app.call("/auth/me")).data.user.id,
      e.id,
    );
    assert.equal(v.coverage.sources, 2);
    assert.equal(v.coverage.as_of, report.frozen_at);
    assert.equal(v.coverage.period_closed, false);
    assert.match(v.version.facts, /ONLY_SOURCE_/);
    assert.equal(
      app.store.get("SELECT id FROM records WHERE id=?", e.id),
      undefined,
    );
  }
  assert.equal(app.store.get("SELECT count(*) AS n FROM diary_attempts").n, 0);
});
test("events and completed messages arriving after the accepted sleep snapshot never enter its actual model request", async (t) => {
  const { app, calls } = await setup(t);
  const s = await source(app, "本次冻结的明确来源", true);
  const before = seeded(app, s.c.id, s.owner, "BEFORE_FREEZE_CANARY");
  const cycle = await sleep(app);
  const later = seeded(app, s.c.id, s.owner, "AFTER_FREEZE_CANARY");
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
    12000,
  );
  assert.equal(calls.length, 1);
  const prompt = JSON.stringify(calls[0].body);
  assert.match(prompt, /BEFORE_FREEZE_CANARY/);
  assert.doesNotMatch(prompt, /AFTER_FREEZE_CANARY/);
  const refs = app.store
    .all("SELECT message_id FROM rest_diary_messages WHERE cycle_id=?", cycle)
    .map((r) => r.message_id);
  assert.ok(refs.includes(before.u));
  assert.ok(!refs.includes(later.u));
  const entries = app.store.all(
    "SELECT e.* FROM diary_entries e JOIN diary_policies p ON p.id=e.policy_id WHERE p.source_key=? AND period='day' ORDER BY e.period_start DESC LIMIT 1",
    s.c.id,
  );
  const detail = app.features.diary.entry(s.owner, entries[0].id);
  assert.equal(detail.coverage.as_of, (await cycles(app))[0].frozen_at);
  assert.equal(detail.coverage.period_closed, false);
});

test("each allowed source has its own actual provider envelope and management history stays actor scoped", async (t) => {
  const { app, calls } = await setup(t);
  const a = await source(app, "来源甲", true),
    b = await source(app, "来源乙", true);
  seeded(app, a.c.id, a.owner, "PRIVATE_A_ONLY");
  seeded(app, b.c.id, b.owner, "PRIVATE_B_ONLY");
  await sleep(app);
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
    12000,
  );
  assert.equal(calls.length, 2);
  for (const call of calls) {
    const body = JSON.stringify(call.body);
    assert.notEqual(
      body.includes("PRIVATE_A_ONLY"),
      body.includes("PRIVATE_B_ONLY"),
    );
  }
  assert.deepEqual(app.features.diary.rest.summaries(randomUUID()).items, []);
  assert.equal(
    (await app.call("/diary/rest-cycles", "GET", undefined, { auth: false }))
      .status,
    401,
  );
});
test("frozen delivery includes only already-confirmed text parts and never adopts a late receipt", async (t) => {
  const { app, calls } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const s = await source(app, "确认部分的来源", true),
    r = seeded(app, s.c.id, s.owner, "DELIVERY_USER"),
    first = randomUUID(),
    later = randomUUID();
  app.store.run(
    "INSERT INTO run_targets VALUES(?,'qq','{}',?, ?, ?)",
    r.r,
    "synthetic-msg",
    now() + 600,
    "synthetic-context",
  );
  app.store.run(
    "INSERT INTO delivery_parts VALUES(?,?,1,'text',?,'sent',NULL,'synthetic-confirmed',?,?)",
    first,
    r.r,
    "CONFIRMED_BEFORE_FREEZE",
    Date.now(),
    now(),
  );
  app.store.run(
    "INSERT INTO delivery_parts VALUES(?,?,2,'text',?,'unknown',NULL,NULL,?,?)",
    later,
    r.r,
    "CONFIRMED_AFTER_FREEZE",
    Date.now(),
    now(),
  );
  const cycle = await sleep(app);
  app.store.run(
    "UPDATE delivery_parts SET status='sent',platform_id='late' WHERE id=?",
    later,
  );
  await prepare(app, cycle);
  app.features.automation.onScheduleTick = () => app.features.diary.tick();
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
    12000,
  );
  assert.equal(calls.length, 1);
  const body = JSON.stringify(calls[0].body);
  assert.match(body, /CONFIRMED_BEFORE_FREEZE/);
  assert.doesNotMatch(body, /CONFIRMED_AFTER_FREEZE/);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM rest_diary_parts WHERE cycle_id=?",
      cycle,
    ).n,
    1,
  );
});
test("a corrected frozen event blocks its targets, leaves the correction intact and never resurrects the original", async (t) => {
  const { app, calls } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const actor = (await app.call("/auth/me")).data.user.id,
    e = (
      await app.call(
        "/diary/events",
        "POST",
        { content: "ORIGINAL_TO_FORGET", happened_at: now() - 10 },
        { headers: { "idempotency-key": randomUUID() } },
      )
    ).data;
  const cycle = await sleep(app);
  assert.equal(
    (
      await app.call(
        "/diary/events/" + e.id,
        "PATCH",
        {
          content: "人工明确替换的事实",
          statement_type: "user_statement",
          happened_at: e.happened_at,
        },
        match(e.revision),
      )
    ).status,
    200,
  );
  await prepare(app, cycle);
  const history = app.features.diary.rest
    .summaries(actor)
    .items.find((c) => c.cycle_id === cycle);
  assert.equal(history.status, "partial");
  assert.ok(
    history.targets.some(
      (t) => t.error_code === "rest_diary_reference_changed",
    ),
  );
  assert.equal(calls.length, 0);
  assert.ok(
    app.features.diary
      .eventDetail(actor, e.id)
      .content.includes("人工明确替换"),
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_entry_versions WHERE instr(coalesce(facts,''),?)>0",
      "ORIGINAL_TO_FORGET",
    ).n,
    0,
  );
});
test("a manual diary revision after sleep acceptance is preserved rather than overwritten by the frozen target", async (t) => {
  const { app } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const actor = (await app.call("/auth/me")).data.user.id;
  const p = app.features.diary
    .policies(actor)
    .items.find((p) => p.source_key === "self:" + actor);
  await app.call(
    "/diary/events",
    "POST",
    { content: "原始明确事实", happened_at: now() - 10 },
    { headers: { "idempotency-key": randomUUID() } },
  );
  const date = new Date(Date.now() + 8 * 3600000).toISOString().slice(0, 10),
    entry = app.features.diary.consolidate(actor, {
      policy_id: p.id,
      date,
      period: "day",
      model: false,
    });
  const cycle = await sleep(app);
  app.features.diary.changeEntry(actor, entry.id, entry.revision, {
    facts: "HUMAN_AFTER_FREEZE_KEEP",
    feelings: "",
  });
  await prepare(app, cycle);
  const result = app.features.diary.entry(actor, entry.id);
  assert.equal(result.version.facts, "HUMAN_AFTER_FREEZE_KEEP");
  assert.equal(result.locked, 1);
  assert.ok(
    (await cycles(app))[0].targets.some(
      (t) => t.error_code === "rest_diary_target_protected",
    ),
  );
});
test("revoking a source during the actual diary request cancels publication and leaves no reusable model body", async (t) => {
  let release;
  const { app, calls } = await setup(t, async () => {
    await new Promise((r) => (release = r));
    return { facts: "LATE_MUST_NOT_SAVE", feelings: "" };
  });
  const s = await source(app, "在途来源撤销", true);
  seeded(app, s.c.id, s.owner, "已授权后才采集的来源");
  await sleep(app);
  await until(
    () => calls.length,
    (n) => n === 1,
    10000,
  );
  const revision = app.store.get(
    "SELECT revision FROM source_grants WHERE source_key=?",
    s.c.id,
  ).revision;
  app.features.knowledge.setGrant(s.owner, s.c.id, revision, {
    capture: false,
    allow_model: false,
    can_sleep: false,
    can_wake: false,
    expires_at: null,
  });
  assert.equal(app.runtime.runningCount, 1);
  release();
  await until(
    () => app.runtime.runningCount,
    (n) => n === 0,
  );
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
    10000,
  );
  assert.equal((await cycles(app))[0].status, "partial");
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_entry_versions WHERE instr(coalesce(facts,''),'LATE_MUST_NOT_SAVE')>0",
    ).n,
    0,
  );
});
test("a stopped cycle records partial progress without extending the original wake deadline or replaying pending messages", async (t) => {
  const { app, calls } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const s = await source(app, "停止前的来源", false);
  for (let i = 0; i < 40; i++) seeded(app, s.c.id, s.owner, "有界资料" + i);
  const cycle = await sleep(app),
    before = app.store.get("SELECT * FROM rest_cycles WHERE id=?", cycle);
  await prepare(app, cycle);
  assert.equal(
    app.store.get(
      "SELECT processed FROM rest_diary_sources WHERE cycle_id=? AND source_key=?",
      cycle,
      s.c.id,
    ).processed,
    64,
  );
  const state = app.features.automation.restStatus().state;
  app.features.automation.restControl(
    "test",
    { action: "wake", request_id: randomUUID() },
    state.revision,
  );
  const history = (await cycles(app))[0];
  assert.equal(history.status, "partial");
  assert.ok(history.sources.some((s) => s.status === "interrupted"));
  assert.equal(
    app.store.get("SELECT wake_at FROM rest_cycles WHERE id=?", cycle).wake_at,
    before.wake_at,
  );
  assert.equal(calls.length, 0);
});
test("restarting during preparation resumes the frozen source pages and excludes messages received after the old cutoff", async (t) => {
  const { app, calls } = await setup(t);
  app.features.automation.stop();
  const s = await source(app, "重启前来源", false);
  for (let i = 0; i < 40; i++)
    seeded(app, s.c.id, s.owner, "PERSISTED_BEFORE_RESTART_" + i);
  const cycle = await sleep(app),
    cutoff = app.store.get(
      "SELECT message_cutoff FROM rest_diary_cycles WHERE cycle_id=?",
      cycle,
    ).message_cutoff,
    dir = app.dataDir;
  await app.close();
  const restored = await fixture({ dataDir: dir });
  t.after(() => restored.close());
  seeded(restored, s.c.id, s.owner, "POST_RESTART_NOT_IN_THIS_CYCLE");
  await until(
    () => restored.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
    12000,
  );
  assert.equal(
    restored.store.get(
      "SELECT message_cutoff FROM rest_diary_cycles WHERE cycle_id=?",
      cycle,
    ).message_cutoff,
    cutoff,
  );
  const rows = restored.store.all(
    "SELECT sequence FROM rest_diary_messages WHERE cycle_id=?",
    cycle,
  );
  assert.equal(rows.length, 80);
  assert.ok(rows.every((r) => r.sequence <= cutoff));
  assert.equal(
    restored.features.diary.rest.summaries(s.owner).items[0].status,
    "completed",
  );
  assert.equal(calls.length, 0);
});

async function selfPolicy(app, model = false) {
  let p = (await app.call("/diary/policies")).data.items.find((p) =>
    p.source_key.startsWith("self:"),
  );
  if (model)
    p = (
      await app.call(
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
      )
    ).data;
  return p;
}
async function explicitEvent(app, at, content) {
  const r = await app.call(
    "/diary/events",
    "POST",
    { content, happened_at: at },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return r.data;
}

test("frozen period checks use the original event date when a correction moves it to another day", async (t) => {
  const { app } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const e = await explicitEvent(app, now() - 5, "原日期事件");
  const cycle = await sleep(app);
  const changed = await app.call(
    "/diary/events/" + e.id,
    "PATCH",
    {
      content: "改为昨天的事实",
      happened_at: now() - 86400,
      statement_type: "user_statement",
    },
    match(e.revision),
  );
  assert.equal(changed.status, 200);
  await prepare(app, cycle);
  const targets = (await cycles(app))[0].targets;
  assert.ok(
    targets.some(
      (t) =>
        t.period === "day" && t.error_code === "rest_diary_reference_changed",
    ),
  );
});

test("grant renewal between snapshot and capture cannot backfill the earlier authorized range", async (t) => {
  const { app, calls } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const s = await source(app, "撤销后重新许可", true);
  seeded(app, s.c.id, s.owner, "OLD_GRANT_BODY");
  const cycle = await sleep(app);
  for (const capture of [false, true]) {
    const g = app.store.get(
      "SELECT revision FROM source_grants WHERE source_key=?",
      s.c.id,
    );
    const r = await app.call(
      "/knowledge/source/grant",
      "PUT",
      {
        source_key: s.c.id,
        grant: {
          capture,
          allow_model: capture,
          can_sleep: false,
          can_wake: false,
          expires_at: null,
        },
      },
      match(g.revision),
    );
    assert.equal(r.status, 200);
  }
  await prepare(app, cycle);
  const h = (await cycles(app))[0];
  assert.equal(h.status, "partial");
  assert.ok(
    h.sources.some((s) => s.error_code === "rest_diary_source_changed"),
  );
  assert.equal(calls.length, 0);
});

test("pending, local control, automatic and suppressed outputs are excluded while sensitive completed inputs are kept out of diary bodies", async (t) => {
  const { app, calls } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const s = await source(app, "排除的输入", false);
  const pending = seeded(app, s.c.id, s.owner, "NOT_COMPLETED", {
      status: "running",
    }),
    local = seeded(app, s.c.id, s.owner, "LOCAL_CONTROL"),
    auto = seeded(app, s.c.id, s.owner, "AUTO_RUN"),
    suppressed = seeded(app, s.c.id, s.owner, "可见用户语句", {
      reply: "SUPPRESSED_BODY",
    }),
    sensitive = seeded(
      app,
      s.c.id,
      s.owner,
      "api_key=sk-test-sensitive-diary-exclusion-1234567890",
    );
  app.store.run("INSERT INTO local_runs VALUES(?,?)", local.r, "control");
  app.store.run("INSERT INTO automatic_runs VALUES(?,?)", auto.r, "trigger");
  app.store.run(
    "INSERT INTO run_publications VALUES(?,'suppressed','synthetic',?)",
    suppressed.r,
    now(),
  );
  const cycle = await sleep(app);
  const refs = app.store
    .all("SELECT message_id FROM rest_diary_messages WHERE cycle_id=?", cycle)
    .map((r) => r.message_id);
  for (const id of [pending.u, local.u, auto.u, suppressed.a])
    assert.ok(!refs.includes(id));
  assert.ok(refs.includes(suppressed.u));
  // The synthetic pending run has no live network request; finish it only after the cutoff.
  app.store.run("UPDATE runs SET status='completed' WHERE id=?", pending.r);
  await prepare(app, cycle);
  const sourceRow = (await cycles(app))[0].sources.find(
    (x) => x.source_key === s.c.id,
  );
  assert.ok(sourceRow.excluded >= 1);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_id=?",
      sensitive.u,
    ).n,
    0,
  );
  assert.equal(calls.length, 0);
});

test("closed week and month keep complete daily parent chains and recursive forgetting invalidates both aggregates", async (t) => {
  const { app, calls } = await setup(t);
  const p = await selfPolicy(app, true),
    today = diaryDate(now(), p.time_zone),
    week = dateAdd(diaryBounds("week", today, p.time_zone).date, -7),
    month = diaryBounds(
      "month",
      dateAdd(diaryBounds("month", today, p.time_zone).date, -1),
      p.time_zone,
    ).date;
  const e = await explicitEvent(
    app,
    diaryBounds("day", week, p.time_zone).start_at + 3600,
    "CLOSED_WEEK_SOURCE",
  );
  await explicitEvent(
    app,
    diaryBounds("day", month, p.time_zone).start_at + 3600,
    "CLOSED_MONTH_SOURCE",
  );
  const cycle = await sleep(app);
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
    20000,
  );
  const report = (await cycles(app)).find((c) => c.cycle_id === cycle);
  assert.equal(report.status, "completed");
  for (const [period, date, days] of [
    ["week", week, 7],
    ["month", month, diaryBounds("month", month, p.time_zone).days],
  ]) {
    const target = report.targets.find(
      (x) => x.policy_id === p.id && x.period === period && x.date === date,
    );
    assert.equal(target.state, "completed");
    const entry = app.features.diary.entry(p.manager_id, target.entry_id);
    assert.equal(entry.coverage.parents.length, days);
    assert.equal(entry.coverage.period_closed, true);
    assert.equal(entry.coverage.complete_raw, true);
    assert.ok(entry.version.generator.startsWith("model:"));
  }
  assert.ok(calls.length >= 4);
  assert.ok(
    calls.every((c) => {
      const b = JSON.parse(c.body.messages[1].content);
      return b.period === "day" || b.period_closed;
    }),
  );
  const forgotten = await app.call(
    "/diary/events/" + e.id,
    "DELETE",
    undefined,
    match(e.revision),
  );
  assert.equal(forgotten.status, 200);
  const target = report.targets.find(
    (x) => x.policy_id === p.id && x.period === "week",
  );
  const invalid = app.features.diary.entry(p.manager_id, target.entry_id);
  assert.equal(invalid.state, "invalidated");
  assert.equal(invalid.version.facts, null);
});

test("a synthetic cross-midnight slice stays explicitly partial in entry, Native projection and actual model envelope", async (t) => {
  const { app, calls } = await setup(t);
  app.features.automation.onScheduleTick = () => {};
  app.features.automation.onRestConsolidate = () => {};
  const p = await selfPolicy(app, true),
    yesterday = dateAdd(diaryDate(now(), p.time_zone), -1),
    asOf = diaryBounds("day", yesterday, p.time_zone).start_at + 3600;
  await explicitEvent(app, asOf - 10, "CROSS_MIDNIGHT_SOURCE");
  const cycle = await sleep(app);
  // Exercise persisted old-day evidence without changing the host clock or using any product time-injection API.
  app.store.run(
    "UPDATE rest_diary_cycles SET frozen_at=? WHERE cycle_id=?",
    asOf,
    cycle,
  );
  await prepare(app, cycle);
  app.features.automation.onScheduleTick = () => app.features.diary.tick();
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
    12000,
  );
  const entry = app.store.get(
      "SELECT * FROM diary_entries WHERE policy_id=? AND period='day' AND period_start=?",
      p.id,
      yesterday,
    ),
    view = app.features.diary.entry(p.manager_id, entry.id);
  assert.equal(view.coverage.period_closed, false);
  assert.equal(view.coverage.as_of, asOf);
  const payload = JSON.parse(
    app.store.get("SELECT payload FROM records WHERE id=?", entry.id).payload,
  );
  assert.equal(payload.period_closed, false);
  assert.match(payload.content, /不代表全天完成/);
  const envelope = JSON.parse(calls[0].body.messages[1].content);
  assert.equal(envelope.observed_through, asOf);
  assert.equal(envelope.period_closed, false);
});

test("an exhausted model budget keeps a durable pending target and waking preserves its original deadline", async (t) => {
  const { app, calls } = await setup(t);
  const s = await source(app, "预算等待", true);
  seeded(app, s.c.id, s.owner, "BUDGET_WAIT_SOURCE");
  const date = new Date().toISOString().slice(0, 10);
  app.features.knowledge.setPolicy("budget:" + date, 0, { calls: 1000 });
  const cycle = await sleep(app),
    original = app.store.get(
      "SELECT wake_at FROM rest_cycles WHERE id=?",
      cycle,
    ).wake_at;
  await until(
    () =>
      app.store.get(
        "SELECT error_code FROM diary_tasks WHERE cycle_id=? AND error_code='daily_budget_wait'",
        cycle,
      ),
    Boolean,
    10000,
  );
  const h = (await cycles(app))[0];
  assert.equal(h.status, "model_wait");
  assert.ok(h.targets.some((t) => t.state === "pending"));
  const state = app.features.automation.restStatus().state;
  app.features.automation.restControl(
    "test",
    { action: "wake", request_id: randomUUID() },
    state.revision,
  );
  assert.equal((await cycles(app))[0].status, "partial");
  assert.equal(
    app.store.get("SELECT wake_at FROM rest_cycles WHERE id=?", cycle).wake_at,
    original,
  );
  assert.equal(calls.length, 0);
});

test("a schema12 active rest cycle is preserved on upgrade without inventing a source snapshot or silently claiming success", async (t) => {
  const dir = privateDirectory(
      resolve(".local/prometheus/tests/rest-upgrade-" + randomUUID()),
    ),
    db = new DatabaseSync(join(dir, "prometheus.db")),
    cycle = randomUUID(),
    wake = now() + 300;
  db.exec(readFileSync(new URL("./schema12.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES(?,?)").run("prometheus.schema", "12");
  db.prepare("INSERT INTO metadata VALUES(?,?)").run(
    "assistant.identity",
    "rest-upgrade-identity",
  );
  db.prepare(
    "INSERT INTO rest_cycles VALUES(?,'consolidating',2,?,?,?,NULL,'test',NULL,?)",
  ).run(cycle, now() - 5, wake, now() + 60, wake);
  db.close();
  const app = await fixture({ dataDir: dir });
  t.after(() => app.close());
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "13",
  );
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
  );
  const report = app.store.get(
    "SELECT * FROM rest_diary_cycles WHERE cycle_id=?",
    cycle,
  );
  assert.equal(report.status, "interrupted");
  assert.equal(report.error_code, "legacy_rest_snapshot_missing");
  assert.equal(
    app.store.get("SELECT wake_at FROM rest_cycles WHERE id=?", cycle).wake_at,
    wake,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM rest_diary_sources").n,
    0,
  );
  const snapshot = join(
      dir,
      "backups",
      readdirSync(join(dir, "backups")).find((n) => n.startsWith("schema12-")),
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
    "12",
  );
  old.close();
});
