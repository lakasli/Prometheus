import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fixture, until } from "./helpers.mjs";
import {
  activity,
  expand,
  calendarRange,
  localInstant,
} from "../dist/schedule-time.js";
import { strictModelJson } from "../dist/schedule-json.js";
import { now, passwordHash, privateDirectory } from "../dist/security.js";
const match = (revision) => ({ headers: { "if-match": `"${revision}"` } });
const base = (extra = {}) => ({
  title: "合成活动",
  type: "video",
  purpose: "明确的测试安排",
  start_at: Date.parse("2027-01-31T09:00:00+08:00") / 1000,
  end_at: Date.parse("2027-01-31T10:00:00+08:00") / 1000,
  time_zone: "Asia/Shanghai",
  resource_id: null,
  target_ref: null,
  repeat: null,
  movable: false,
  grant_id: null,
  restart_reading: false,
  ...extra,
});
const create = (app, definition, key = randomUUID(), proposal = false) =>
  app.call(
    `/schedule/${proposal ? "proposals" : "activities"}`,
    "POST",
    { definition },
    { headers: { "idempotency-key": key } },
  );
const calendar = (app, start, end, extra = "") =>
  app.call(`/schedule/calendar?start=${start}&end=${end}${extra}`);
async function resource(app, content = "a".repeat(4600)) {
  const result = await app.call("/schedule/resources", "POST", {
    title: "已明确授权的合成文本",
    content,
    type: "text",
    confirmed: true,
  });
  assert.equal(result.status, 201);
  return result.data;
}
async function provider(t, handle = () => "合成片段笔记") {
  const calls = [],
    server = createServer(async (req, res) => {
      let raw = "";
      for await (const c of req) raw += c;
      const body = JSON.parse(raw),
        call = { body, req, res };
      calls.push(call);
      const content = await handle(call, calls.length);
      if (content !== undefined && !res.destroyed) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ choices: [{ message: { content } }] }));
      }
    });
  await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
  t.after(
    () =>
      new Promise((done) => {
        server.closeAllConnections();
        server.close(done);
      }),
  );
  return { calls, url: `http://127.0.0.1:${server.address().port}` };
}
function tick(app) {
  app.store.run("UPDATE schedule_runtime SET last_tick=0 WHERE id=1");
  app.features.schedule.tick();
}
function ongoing(id, extra = {}) {
  const at = now();
  return base({
    type: "reading",
    start_at: at - 1,
    end_at: at + 180,
    resource_id: id,
    ...extra,
  });
}
async function grant(app, resources, extra = {}) {
  const at = now(),
    result = await app.call("/schedule/autonomy-grants", "POST", {
      confirmed: true,
      policy: {
        title: "人工授予的阅读范围",
        allowed_types: ["reading"],
        resource_ids: resources,
        time_zone: "Asia/Shanghai",
        valid_from: at - 60,
        valid_until: at + 3 * 86400,
        horizon_days: 3,
        window_start: "00:00",
        window_end: "23:59",
        max_daily_activities: 4,
        max_daily_minutes: 120,
        max_daily_changes: 8,
        max_daily_model_calls: 8,
        max_daily_tokens: 100000,
        max_output_tokens: 1200,
        ...extra,
      },
    });
  assert.equal(result.status, 201);
  return result.data;
}

test("civil recurrence skips nonexistent and ambiguous times without changing local time or count semantics", () => {
  const ny = (
    start,
    repeat = { frequency: "daily", interval: 1, count: 4, until: null },
  ) =>
    activity(
      base({
        start_local: start,
        end_local: start.slice(0, 11) + "03:30:00",
        start_at: undefined,
        end_at: undefined,
        time_zone: "America/New_York",
        repeat,
      }),
    );
  const spring = ny("2026-03-07T02:30:00");
  const slots = expand(spring, 0, Date.parse("2026-03-12T00:00:00Z") / 1000);
  assert.equal(slots.length, 3);
  assert.deepEqual(
    slots.map((s) => s.ordinal),
    [0, 2, 3],
  );
  const fall = activity(
    base({
      start_at: undefined,
      end_at: undefined,
      start_local: "2026-10-31T01:30:00",
      end_local: "2026-10-31T02:00:00",
      time_zone: "America/New_York",
      repeat: { frequency: "daily", interval: 1, count: 3, until: null },
    }),
  );
  assert.deepEqual(
    expand(fall, 0, Date.parse("2026-11-04T00:00Z") / 1000).map(
      (s) => s.ordinal,
    ),
    [0, 2],
  );
  const monthly = activity(
    base({
      repeat: { frequency: "monthly", interval: 1, count: 4, until: null },
    }),
  );
  assert.deepEqual(
    expand(monthly, monthly.start_at, monthly.start_at + 125 * 86400).map(
      (s) => s.ordinal,
    ),
    [0, 2],
  );
  assert.equal(
    calendarRange("2026-03-08", "day", "America/New_York").end -
      calendarRange("2026-03-08", "day", "America/New_York").start,
    23 * 3600,
  );
  assert.equal(
    calendarRange("2026-11-01", "day", "America/New_York").end -
      calendarRange("2026-11-01", "day", "America/New_York").start,
    25 * 3600,
  );
  assert.throws(
    () => localInstant("2026-10-04T02:15:00", "Australia/Lord_Howe"),
    (error) => error.code === "civil_time_invalid",
  );
  assert.throws(
    () => strictModelJson('{"selection":null,"selection":{}}'),
    (error) => error.code === "planning_invalid_json",
  );
  assert.deepEqual(strictModelJson('```json\n{"selection":null}\n```'), {
    selection: null,
  });
});
test("series revisions, readonly previews and moved exceptions preserve their original opportunity key", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const a = base({
      repeat: { frequency: "monthly", interval: 1, count: 4, until: null },
    }),
    key = randomUUID();
  assert.equal(
    (await app.call("/schedule/activities", "POST", { definition: a })).status,
    400,
  );
  const saved = await create(app, a, key);
  assert.equal(saved.status, 201);
  assert.equal((await create(app, a, key)).data.id, saved.data.id);
  assert.equal(
    (await create(app, { ...a, title: "不同请求" }, key)).status,
    409,
  );
  const moved = {
    ...a,
    repeat: null,
    start_at: Date.parse("2027-02-08T09:00+08:00") / 1000,
    end_at: Date.parse("2027-02-08T10:00+08:00") / 1000,
  };
  const change = { scope: "single", nominal_at: a.start_at, definition: moved };
  const preview = await app.call(
    `/schedule/activities/${saved.data.id}/preview`,
    "POST",
    change,
    match(1),
  );
  assert.equal(preview.status, 200);
  assert.equal(preview.data.read_only, true);
  assert.equal(
    (await app.call(`/schedule/activities/${saved.data.id}`)).data.revision,
    1,
  );
  const changed = await app.call(
    `/schedule/activities/${saved.data.id}`,
    "PATCH",
    change,
    match(1),
  );
  assert.equal(changed.status, 200);
  assert.equal(changed.data.revision, 2);
  const feb = (
    await calendar(
      app,
      Date.parse("2027-02-01T00:00+08:00") / 1000,
      Date.parse("2027-03-01T00:00+08:00") / 1000,
    )
  ).data.items;
  assert.equal(feb.length, 1);
  assert.equal(feb[0].nominal_at, a.start_at);
  assert.equal(feb[0].start_at, moved.start_at);
  assert.equal(
    (
      await app.call(
        `/schedule/activities/${saved.data.id}`,
        "PATCH",
        change,
        match(1),
      )
    ).status,
    409,
  );
  const history = (
    await app.call(`/schedule/activities/${saved.data.id}/history`)
  ).data.items;
  assert.equal(history.length, 2);
  assert.equal(history[0].exceptions[0].nominal_at, a.start_at);
});
test("dragging creates a readonly scoped preview and rejects invalid civil target times", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const a = base({
      start_at: undefined,
      end_at: undefined,
      start_local: "2027-03-13T02:30:00",
      end_local: "2027-03-13T03:30:00",
      time_zone: "America/New_York",
    }),
    saved = (await create(app, a)).data,
    nominal = saved.definition.start_at;
  const invalid = await app.call(
    `/schedule/activities/${saved.id}/move-preview`,
    "POST",
    {
      nominal_at: nominal,
      target_date: "2027-03-14",
      time_zone: "America/New_York",
    },
    match(1),
  );
  assert.equal(invalid.status, 400);
  assert.equal(invalid.data.error.code, "civil_time_invalid");
  const preview = await app.call(
    `/schedule/activities/${saved.id}/move-preview`,
    "POST",
    {
      nominal_at: nominal,
      target_date: "2027-03-15",
      time_zone: "America/New_York",
    },
    match(1),
  );
  assert.equal(preview.status, 200);
  assert.equal(preview.data.read_only, true);
  assert.equal(preview.data.local_start, "2027-03-15T02:30:00");
  assert.equal(
    (await app.call(`/schedule/activities/${saved.id}`)).data.revision,
    1,
  );
  assert.equal(
    (
      await app.call(
        `/schedule/activities/${saved.id}`,
        "PATCH",
        preview.data.change,
        match(1),
      )
    ).status,
    200,
  );
});

test("following edits keep remaining count and exceptions while same-series collisions roll back", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const a = base({
      repeat: { frequency: "daily", interval: 1, count: 5, until: null },
    }),
    saved = (await create(app, a)).data;
  const clash = {
    ...a,
    repeat: null,
    start_at: a.start_at + 86400,
    end_at: a.end_at + 86400,
  };
  assert.equal(
    (
      await app.call(
        `/schedule/activities/${saved.id}`,
        "PATCH",
        { scope: "single", nominal_at: a.start_at, definition: clash },
        match(1),
      )
    ).status,
    409,
  );
  assert.equal(
    (await app.call(`/schedule/activities/${saved.id}`)).data.revision,
    1,
  );
  const paused = await app.call(
    `/schedule/activities/${saved.id}/pause`,
    "POST",
    { scope: "following", nominal_at: a.start_at + 2 * 86400 },
    match(1),
  );
  assert.equal(paused.status, 200);
  assert.ok(paused.data.following_id);
  const items = (await calendar(app, a.start_at, a.start_at + 6 * 86400)).data
    .items;
  assert.equal(items.length, 5);
  assert.equal(items.filter((item) => item.state === "enabled").length, 2);
  assert.equal(items.filter((item) => item.state === "paused").length, 3);
  assert.equal(
    (await app.call(`/schedule/activities/${paused.data.following_id}`)).data
      .definition.repeat.count,
    3,
  );
});
test("calendar filters sources and conflicts reveal only anonymous busy intervals across accounts", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const marker = "private-calendar-title-never-share",
    saved = (await create(app, base({ title: marker }))).data;
  app.store.run(
    "INSERT INTO admins VALUES(?,'other-schedule-user',?,?)",
    randomUUID(),
    await passwordHash("Other-schedule-user-2026!"),
    now(),
  );
  await app.call("/auth/login", "POST", {
    username: "other-schedule-user",
    password: "Other-schedule-user-2026!",
  });
  assert.equal(
    (await app.call(`/schedule/activities/${saved.id}`)).status,
    404,
  );
  const preview = await app.call("/schedule/activities/preview", "POST", {
    definition: base(),
  });
  assert.equal(preview.status, 200);
  assert.equal(preview.data.conflicts[0].origin, "private_busy");
  assert.equal(JSON.stringify(preview.data).includes(marker), false);
  assert.equal(JSON.stringify(preview.data).includes(saved.id), false);
  assert.deepEqual(
    (await calendar(app, base().start_at, base().end_at)).data.items,
    [],
  );
  assert.equal((await create(app, base())).status, 409);
  assert.equal(
    (
      await app.call(
        "/schedule/calendar?date=2026-03-08&view=day&time_zone=America/New_York",
        "GET",
        undefined,
        { auth: false },
      )
    ).status,
    401,
  );
});
test("trigger calendar projections keep persisted anchors and never dispatch a second job", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const owner = (await app.call("/auth/me")).data.user.id,
    start = base().start_at,
    id = randomUUID();
  app.store.run(
    "INSERT INTO trigger_rules VALUES(?,?,1,1,?,?,?,NULL,?,?)",
    id,
    owner,
    JSON.stringify({ type: "interval", seconds: 3600 }),
    JSON.stringify({ type: "text", title: "仅投影的提醒", content: "测试" }),
    start,
    start - 100,
    start - 100,
  );
  const response = await calendar(app, start + 1800, start + 3 * 3600);
  assert.equal(response.status, 200);
  assert.deepEqual(
    response.data.items.map((item) => item.start_at),
    [start + 3600, start + 7200],
  );
  assert.equal(response.data.items[0].end_at, null);
  assert.equal(
    app.store.get("SELECT next_at FROM trigger_rules WHERE id=?", id).next_at,
    start,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM trigger_firings").n, 0);
});
test("actual reading persists bounded unicode coverage and resumes a different occurrence after restart", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const text = "a".repeat(4500) + "🙂".repeat(100),
    r = await resource(app, text),
    created = await create(app, ongoing(r.id));
  assert.equal(created.status, 201);
  tick(app);
  await until(
    () =>
      Promise.resolve(
        app.store.get(
          "SELECT checkpoint FROM schedule_reading_progress WHERE resource_id=?",
          r.id,
        )?.checkpoint,
      ),
    (value) => value === 4096,
  );
  await app.close();
  const restarted = await fixture({ dataDir: app.dataDir });
  t.after(() => restarted.close());
  await restarted.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  await until(
    () =>
      Promise.resolve(
        restarted.store.get(
          "SELECT checkpoint FROM schedule_reading_progress WHERE resource_id=?",
          r.id,
        )?.checkpoint,
      ),
    (value) => value === 4600,
  );
  const detail = (await restarted.call(`/schedule/resources/${r.id}`)).data;
  assert.equal(detail.notes.length, 2);
  assert.equal(detail.progress.checkpoint, 4600);
  assert.equal(remote.calls.length, 2);
  const first = JSON.parse(remote.calls[0].body.messages[1].content),
    second = JSON.parse(remote.calls[1].body.messages[1].content);
  assert.equal(first.resource.start_char, 0);
  assert.equal(second.resource.start_char, 4096);
  assert.equal(second.resource.end_char, 4600);
  assert.equal([...second.resource.text].length, 504);
  assert.equal(remote.calls[0].body.messages.length, 2);
  assert.equal(
    restarted.store.get(
      "SELECT count(*) AS n FROM schedule_attempts WHERE config_json IS NOT NULL",
    ).n,
    0,
  );
});
test("resource revocation cancels the real request and cannot commit late notes or reuse the physical slot early", async (t) => {
  const remote = await provider(t, () => undefined),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const r = await resource(app, "独立的合成正文"),
    created = await create(app, ongoing(r.id));
  assert.equal(created.status, 201);
  tick(app);
  await until(
    () => Promise.resolve(remote.calls.length),
    (value) => value === 1,
  );
  const owner = (await app.call("/auth/me")).data.user.id;
  app.features.schedule.revokeResource(owner, r.id, r.revision);
  assert.equal(app.runtime.runningCount, 1);
  await until(
    () => Promise.resolve(app.runtime.runningCount),
    (value) => value === 0,
  );
  const row = app.store.get(
    "SELECT status,config_json FROM schedule_attempts LIMIT 1",
  );
  assert.equal(row.status, "invalidated");
  assert.equal(row.config_json, null);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_reading_notes").n,
    0,
  );
  assert.equal(
    app.store.get("SELECT content FROM schedule_resources WHERE id=?", r.id)
      .content,
    null,
  );
  const budget = app.store.get(
    "SELECT calls FROM schedule_budgets WHERE owner_id=?",
    owner,
  );
  assert.equal(budget.calls, 1);
});
test("out-of-scope proposals remain pending and approval cannot be replaced by resume", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const r = await resource(app, "合成资料"),
    g = await grant(app, [r.id]),
    at = now() + 600;
  const definition = base({
    type: "video",
    start_at: at,
    end_at: at + 600,
    grant_id: g.id,
  });
  const proposal = await create(app, definition, randomUUID(), true);
  assert.equal(proposal.status, 201);
  assert.equal(proposal.data.state, "pending_approval");
  assert.equal(
    (
      await app.call(
        `/schedule/activities/${proposal.data.id}/resume`,
        "POST",
        {},
        match(1),
      )
    ).status,
    400,
  );
  const approved = await app.call(
    `/schedule/activities/${proposal.data.id}/approve`,
    "POST",
    { confirmed: true, scope: "series" },
    match(1),
  );
  assert.equal(approved.status, 200);
  assert.equal(approved.data.state, "enabled");
  assert.ok(approved.data.admin_approval);
  assert.equal(approved.data.capability_block, "video_provider_unavailable");
  const changed = await app.call(
    `/schedule/activities/${proposal.data.id}`,
    "PATCH",
    { definition: { ...approved.data.definition, title: "修改后重新待审" } },
    match(2),
  );
  assert.equal(changed.status, 200);
  assert.equal(changed.data.state, "pending_approval");
  assert.equal(changed.data.admin_approval, null);
});
test("bounded planning respects grants and persists one allocation without refunding cancelled activities", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const r = await resource(app),
    g = await grant(app, [r.id], { max_daily_activities: 1 });
  const plan = await app.call("/schedule/plans", "POST", {
    definition: {
      goal: "完成合成资料阅读",
      grant_id: g.id,
      resource_ids: [r.id],
      duration_minutes: 15,
      planning_mode: "bounded_rule",
    },
  });
  assert.equal(plan.status, 201);
  app.store.run(
    "UPDATE schedule_runtime SET last_plan_tick=0,last_tick=0 WHERE id=1",
  );
  app.features.schedule.tick();
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_activities").n,
    1,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_allocations").n,
    1,
  );
  const scheduled = app.store.get("SELECT id FROM schedule_activities");
  assert.equal(
    (
      await app.call(
        `/schedule/activities/${scheduled.id}/cancel`,
        "POST",
        {},
        match(1),
      )
    ).status,
    200,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_allocations").n,
    1,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_attempts").n,
    0,
  );
});
test("successful chunks reset only the retry count and explicit rereading advances its generation once while slots are busy", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const r = await resource(app, "a".repeat(13000)),
    g = await grant(app, [r.id]),
    scheduled = await create(app, ongoing(r.id, { grant_id: g.id }));
  assert.equal(scheduled.status, 201);
  for (let n = 1; n <= 4; n++) {
    tick(app);
    await until(
      () =>
        Promise.resolve(
          app.store.get(
            "SELECT checkpoint FROM schedule_reading_progress WHERE resource_id=?",
            r.id,
          )?.checkpoint,
        ),
      (value) => value === Math.min(n * 4096, 13000),
    );
    await until(
      () => Promise.resolve(app.runtime.runningCount),
      (value) => value === 0,
    );
  }
  assert.equal(remote.calls.length, 4);
  assert.equal(
    app.store.get(
      "SELECT attempts FROM schedule_occurrences WHERE activity_id=?",
      scheduled.data.id,
    ).attempts,
    0,
  );
  const old = (await app.call(`/schedule/activities/${scheduled.data.id}`))
    .data;
  await app.call(
    `/schedule/activities/${old.id}/cancel`,
    "POST",
    {},
    match(old.revision),
  );
  const reread = await create(
    app,
    ongoing(r.id, { restart_reading: true, grant_id: g.id }),
  );
  assert.equal(reread.status, 201);
  const release = app.runtime.beginCheck(true);
  try {
    tick(app);
    tick(app);
    assert.equal(
      app.store.get(
        "SELECT generation FROM schedule_reading_progress WHERE resource_id=?",
        r.id,
      ).generation,
      2,
    );
    assert.equal(remote.calls.length, 4);
  } finally {
    release();
  }
  tick(app);
  await until(
    () => Promise.resolve(remote.calls.length),
    (value) => value === 5,
  );
});

test("series and following changes explicitly reset future exceptions and keep completed historical snapshots", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const a = base({
      repeat: { frequency: "daily", interval: 1, count: 4, until: null },
    }),
    saved = (await create(app, a)).data;
  const shifted = {
    ...a,
    repeat: null,
    start_at: a.start_at + 86400 + 7200,
    end_at: a.end_at + 86400 + 7200,
  };
  const one = await app.call(
    `/schedule/activities/${saved.id}`,
    "PATCH",
    { scope: "single", nominal_at: a.start_at + 86400, definition: shifted },
    match(1),
  );
  assert.equal(one.status, 200);
  const changed = {
    ...a,
    start_at: a.start_at + 3600,
    end_at: a.end_at + 3600,
  };
  assert.equal(
    (
      await app.call(
        `/schedule/activities/${saved.id}`,
        "PATCH",
        { definition: changed },
        match(2),
      )
    ).data.error.code,
    "future_exceptions_confirmation_required",
  );
  assert.equal(
    (
      await app.call(
        `/schedule/activities/${saved.id}`,
        "PATCH",
        { definition: changed, reset_future_exceptions: true },
        match(2),
      )
    ).status,
    200,
  );
  assert.equal(
    (await app.call(`/schedule/activities/${saved.id}`)).data.exceptions.length,
    0,
  );
  const past = now() - 2 * 86400,
    snapshot = {
      ...a,
      repeat: null,
      start_at: past,
      end_at: past + 600,
      title: "历史真实结果的合成快照",
    },
    owner = (await app.call("/auth/me")).data.user.id;
  app.store.run(
    "INSERT INTO schedule_occurrences(id,activity_id,owner_id,activity_revision,nominal_at,start_at,end_at,definition_json,status,updated_at) VALUES(?,?,?,1,?,?,?,?,'completed',?)",
    `${saved.id}:${past}`,
    saved.id,
    owner,
    past,
    past,
    past + 600,
    JSON.stringify(snapshot),
    now(),
  );
  const historical = (await calendar(app, past - 1, past + 601)).data.items;
  assert.equal(historical.length, 1);
  assert.equal(historical[0].title, snapshot.title);
  assert.equal(historical[0].status, "completed");
  assert.equal(historical[0].read_only, true);
});

test("late model plans recheck newly occupied slots and retain failure evidence without activity publication", async (t) => {
  let release;
  const barrier = new Promise((resolve) => (release = resolve));
  const remote = await provider(t, async (call) => {
    const data = JSON.parse(call.body.messages[1].content);
    await barrier;
    return JSON.stringify({
      selection: {
        slot_id: data.slots[0].id,
        resource_id: data.resources[0].id,
      },
      reason: "原来提供的候选",
    });
  });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const r = await resource(app),
    g = await grant(app, [r.id]);
  await app.call("/schedule/plans", "POST", {
    definition: {
      goal: "等待屏障的合成规划",
      grant_id: g.id,
      resource_ids: [r.id],
      duration_minutes: 15,
      planning_mode: "model",
    },
  });
  app.store.run(
    "UPDATE schedule_runtime SET last_plan_tick=0,last_tick=0 WHERE id=1",
  );
  app.features.schedule.tick();
  await until(
    () => Promise.resolve(remote.calls.length),
    (value) => value === 1,
  );
  const selected = JSON.parse(remote.calls[0].body.messages[1].content)
    .slots[0];
  assert.equal(
    (
      await app.call("/records/schedule", "POST", {
        data: {
          title: "在途期间新增占用",
          content: "本人手工修改",
          start_at: selected.start_at,
          end_at: selected.end_at,
          status: "planned",
        },
      })
    ).status,
    201,
  );
  release();
  await until(
    () =>
      Promise.resolve(
        app.store.get("SELECT status FROM schedule_attempts LIMIT 1")?.status,
      ),
    (value) => value === "failed",
  );
  assert.equal(
    app.store.get("SELECT error_code FROM schedule_attempts LIMIT 1")
      .error_code,
    "schedule_conflict",
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_activities").n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT calls FROM schedule_budgets WHERE owner_id=?",
      (await app.call("/auth/me")).data.user.id,
    ).calls,
    1,
  );
});

test("autonomous single exceptions cannot escape the granted resource scope", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const allowed = await resource(app, "允许的资料"),
    other = await resource(app, "不在本次范围内的资料"),
    g = await grant(app, [allowed.id]),
    from = now() + 600,
    definition = base({
      type: "reading",
      resource_id: allowed.id,
      grant_id: g.id,
      start_at: from,
      end_at: from + 600,
      movable: true,
    });
  const proposed = await create(app, definition, randomUUID(), true);
  assert.equal(proposed.data.state, "enabled");
  const changed = await app.call(
    `/schedule/activities/${proposed.data.id}`,
    "PATCH",
    {
      scope: "single",
      nominal_at: from,
      definition: { ...definition, resource_id: other.id },
    },
    match(1),
  );
  assert.equal(changed.status, 200);
  assert.equal(changed.data.state, "pending_approval");
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_attempts").n,
    0,
  );
});

test("grant revision changes invalidate an in-flight manual reading and removed goal resources never become completed", async (t) => {
  const remote = await provider(t, () => undefined),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const r = await resource(app, "尚未读取的授权文本"),
    g = await grant(app, [r.id]);
  const saved = await create(app, ongoing(r.id, { grant_id: g.id }));
  assert.equal(saved.status, 201);
  tick(app);
  await until(
    () => Promise.resolve(remote.calls.length),
    (v) => v === 1,
  );
  const p = (
    await app.call("/schedule/plans", "POST", {
      definition: {
        goal: "不能把失去范围的资料记作已读",
        grant_id: g.id,
        resource_ids: [r.id],
        duration_minutes: 15,
        planning_mode: "bounded_rule",
      },
    })
  ).data;
  const changed = await app.call(
    `/schedule/autonomy-grants/${g.id}`,
    "PATCH",
    { confirmed: true, policy: { ...g.policy, resource_ids: [] } },
    match(g.revision),
  );
  assert.equal(changed.status, 200);
  await until(
    () =>
      Promise.resolve(
        app.store.get("SELECT status FROM schedule_attempts LIMIT 1")?.status,
      ),
    (v) => v === "invalidated",
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_reading_notes").n,
    0,
  );
  app.store.run(
    "UPDATE schedule_runtime SET last_tick=0,last_plan_tick=0 WHERE id=1",
  );
  app.features.schedule.tick();
  assert.equal(
    (await app.call(`/schedule/plans/${p.id}`)).data.state,
    "blocked",
  );
  assert.equal(
    (await app.call(`/schedule/resources/${r.id}`)).data.progress.checkpoint,
    0,
  );
  assert.equal(remote.calls.length, 1);
});

test("unmaterialized elapsed opportunities are readonly calendar projections, never executed or written by GET", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const old = base({
    start_at: now() - 3 * 86400,
    end_at: now() - 3 * 86400 + 60,
  });
  const saved = await create(app, old);
  assert.equal(saved.status, 201);
  const response = await calendar(app, old.start_at - 1, old.end_at + 1);
  assert.equal(response.status, 200);
  assert.equal(response.data.items[0].status, "skipped");
  assert.equal(response.data.items[0].execution_record, false);
  assert.equal(response.data.items[0].block_reason, "missed_execution_window");
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_occurrences").n,
    0,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_attempts").n,
    0,
  );
});

test("schema5 upgrade retains current affinity ledger, drafts and simple schedules and keeps a readable snapshot", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/schema5-schedule-" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db"));
  db.exec(readFileSync(new URL("./schema5.sql", import.meta.url), "utf8"));
  const owner = randomUUID(),
    person = randomUUID(),
    record = randomUUID(),
    draft = randomUUID(),
    at = now();
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','5')").run();
  db.prepare("INSERT INTO admins VALUES(?,'schema5-schedule-owner',?,?)").run(
    owner,
    await passwordHash("Synthetic-schedule-upgrade-2026!"),
    at,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',1,?,?,?,NULL)",
  ).run(
    person,
    owner,
    JSON.stringify({ name: "升级前人物", notes: "保留" }),
    at,
    at,
  );
  db.prepare(
    "INSERT INTO records VALUES(?,'schedule',?,'private',1,?,?,?,NULL)",
  ).run(
    record,
    owner,
    JSON.stringify({
      title: "升级前人工安排",
      start_at: at + 86400,
      end_at: at + 86500,
      status: "planned",
    }),
    at,
    at,
  );
  db.prepare(
    "INSERT INTO config_drafts(id,owner_id,base_revision,config_json,created_at) VALUES(?,?,?,?,?)",
  ).run(draft, owner, 1, "{}", at);
  const ledger = {
    person_id: person,
    owner_id: owner,
    score_micro: 48250000,
    remainder: 123,
    settled_at: at,
    last_direct_at: at - 100,
    policy_revision: 1,
  };
  const columns = Object.keys(ledger);
  db.prepare(
    `INSERT INTO affinity_states(${columns.join(",")}) VALUES(${columns.map(() => "?").join(",")})`,
  ).run(...Object.values(ledger));
  db.close();
  const app = await fixture({ dataDir: directory });
  t.after(() => app.close());
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "12",
  );
  assert.deepEqual(
    {
      ...app.store.get(
        "SELECT * FROM affinity_states WHERE person_id=?",
        person,
      ),
    },
    ledger,
  );
  assert.equal(
    app.store.get("SELECT config_json FROM config_drafts WHERE id=?", draft)
      .config_json,
    "{}",
  );
  assert.equal(
    JSON.parse(
      app.store.get("SELECT payload FROM records WHERE id=?", record).payload,
    ).title,
    "升级前人工安排",
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_activities").n,
    0,
  );
  const snapshot = new DatabaseSync(
    join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((name) =>
        name.startsWith("schema5-"),
      ),
    ),
    { readOnly: true },
  );
  try {
    assert.equal(
      snapshot
        .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
        .get().value,
      "5",
    );
    assert.equal(
      snapshot.prepare("PRAGMA integrity_check").get().integrity_check,
      "ok",
    );
  } finally {
    snapshot.close();
  }
});

test("model planning validates exact offered slots, rejects duplicate JSON and retains failed reservations", async (t) => {
  let valid = false;
  const remote = await provider(t, (call) => {
    const data = JSON.parse(call.body.messages[1].content);
    return valid
      ? JSON.stringify({
          selection: {
            slot_id: data.slots[0].id,
            resource_id: data.resources[0].id,
          },
          reason: "已提供的合成候选",
        })
      : '{"selection":null,"selection":null}';
  });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const r = await resource(app),
    g = await grant(app, [r.id]);
  const body = {
      definition: {
        goal: "模型只选择既有合成时段",
        grant_id: g.id,
        resource_ids: [r.id],
        duration_minutes: 15,
        planning_mode: "model",
      },
    },
    p = (await app.call("/schedule/plans", "POST", body)).data;
  app.store.run(
    "UPDATE schedule_runtime SET last_plan_tick=0,last_tick=0 WHERE id=1",
  );
  app.features.schedule.tick();
  await until(
    () =>
      Promise.resolve(
        app.store.get("SELECT status FROM schedule_attempts LIMIT 1")?.status,
      ),
    (value) => value === "failed",
  );
  assert.equal(
    app.store.get("SELECT error_code FROM schedule_attempts LIMIT 1")
      .error_code,
    "planning_invalid_json",
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_activities").n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT calls FROM schedule_budgets WHERE owner_id=?",
      g.policy ? (await app.call("/auth/me")).data.user.id : "invalid",
    ).calls,
    1,
  );
  valid = true;
  assert.equal(
    (await app.call(`/schedule/plans/${p.id}`, "PATCH", body, match(1))).status,
    200,
  );
  app.store.run(
    "UPDATE schedule_runtime SET last_plan_tick=0,last_tick=0 WHERE id=1",
  );
  app.features.schedule.tick();
  await until(
    () =>
      Promise.resolve(
        app.store.get("SELECT count(*) AS n FROM schedule_activities").n,
      ),
    (value) => value === 1,
  );
  assert.equal(remote.calls.length, 2);
  const attempts = (await app.call("/schedule/plans")).data.items[0]
    .planning_diagnostics.attempts;
  assert.equal(attempts.length, 2);
  assert.ok(attempts[0].input_hash);
  assert.equal(
    JSON.stringify(attempts).includes("模型只选择既有合成时段"),
    false,
  );
});
