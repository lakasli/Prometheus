import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fixture, until } from "./helpers.mjs";
import { now, privateDirectory, passwordHash } from "../dist/security.js";
import { scheduleChoice } from "../dist/schedule-notices.js";
const match = (n) => ({ headers: { "if-match": `"${n}"` } });
async function setup(
  t,
  handle = () => ({ decision: "execute", delay_minutes: 0 }),
) {
  const calls = [];
  const remote = createServer(async (req, res) => {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw);
    calls.push({ body, res });
    const result = await handle(body, calls.length, res);
    if (result !== undefined && !res.destroyed) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          choices: [
            {
              message: {
                content:
                  typeof result === "string" ? result : JSON.stringify(result),
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
  await app.configure(`http://127.0.0.1:${remote.address().port}`);
  app.features.automation.onRoutineTick = () => {};
  app.features.automation.onScheduleTick = () => {};
  await app.features.knowledge.stop();
  const p = app.features.automation.restPolicy();
  app.features.automation.saveRestPolicy(p.revision, {
    ...p.value,
    anticipation_enabled: true,
  });
  return { app, calls, remote };
}
async function activity(app, extra = {}, grantExtra = {}) {
  const at = now(),
    resource = (
      await app.call("/schedule/resources", "POST", {
        title: "私有阅读资料",
        type: "text",
        content: "PRIVATE_READING_BODY_CANARY",
        confirmed: true,
      })
    ).data;
  const grant = (
    await app.call("/schedule/autonomy-grants", "POST", {
      confirmed: true,
      policy: {
        title: "PRIVATE_GRANT_TITLE_CANARY",
        allowed_types: ["reading"],
        resource_ids: [resource.id],
        time_zone: "Asia/Shanghai",
        valid_from: at - 60,
        valid_until: at + 86400,
        horizon_days: 1,
        window_start: "00:00",
        window_end: "23:59",
        max_daily_activities: 8,
        max_daily_minutes: 240,
        max_daily_changes: 10,
        max_daily_model_calls: 24,
        max_daily_tokens: 250000,
        max_output_tokens: 1200,
        ...grantExtra,
      },
    })
  ).data;
  const definition = {
    title: "本次公开活动标题",
    purpose: "明确授权的阅读目的",
    type: "reading",
    resource_id: resource.id,
    target_ref: null,
    start_at: at + 50,
    end_at: at + 650,
    time_zone: "Asia/Shanghai",
    repeat: null,
    movable: true,
    grant_id: grant.id,
    restart_reading: false,
    ...extra,
  };
  const owner = (await app.call("/auth/me")).data.user.id;
  const a = app.features.schedule.create(
    owner,
    { definition },
    randomUUID(),
    true,
  );
  assert.equal(a.state, "enabled");
  return { a, grant, resource, owner, definition };
}
const notice = (app) =>
  app.store.get(
    "SELECT * FROM schedule_notices ORDER BY created_at,rowid DESC LIMIT 1",
  );
const settle = (app) =>
  until(
    () => notice(app),
    (r) =>
      r && r.decision_status !== "pending" && r.decision_status !== "running",
    10000,
  );

test("strict schedule choices reject duplicate fields, unknown keys, out-of-range and incoherent delay", () => {
  assert.deepEqual(scheduleChoice('{"decision":"execute","delay_minutes":0}'), {
    decision: "execute",
    delay_minutes: 0,
  });
  for (const raw of [
    '{"decision":"cancel","decision":"execute"}',
    '{"decision":"postpone","delay_minutes":0}',
    '{"decision":"execute","delay_minutes":1}',
    '{"decision":"postpone","delay_minutes":31}',
    '{"decision":"execute","shell":"x"}',
  ])
    assert.throws(() => scheduleChoice(raw));
});
test("one actual minute notice uses metadata only, shares the automatic physical slot, preserves evidence and never sends", async (t) => {
  const { app, calls } = await setup(t);
  const { a, owner } = await activity(app);
  app.features.scheduleNotices.tick();
  const n = await settle(app);
  assert.equal(calls.length, 1);
  assert.equal(n.decision, "execute");
  assert.equal(n.decision_status, "completed");
  assert.equal(n.attempts, 1);
  const prompt = JSON.stringify(calls[0].body);
  assert.match(prompt, /PROMETHEUS_SCHEDULE_NOTICE_V1/);
  assert.match(prompt, /本次公开活动标题/);
  assert.doesNotMatch(
    prompt,
    /PRIVATE_READING_BODY_CANARY|PRIVATE_GRANT_TITLE_CANARY/,
  );
  for (let i = 0; i < 3; i++) app.features.scheduleNotices.tick();
  assert.equal(calls.length, 1);
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM delivery_parts").n, 0);
  const list = (await app.call("/schedule/decisions")).data;
  assert.equal(list.items[0].attempts.length, 1);
  assert.equal(list.items[0].attempts[0].status, "completed");
  assert.equal(list.items[0].activity_id, a.id);
  assert.equal(list.items[0].owner_id, owner);
});
test("postpone preserves the original opportunity and allows at most two choices across successive deadlines", async (t) => {
  const { app, calls } = await setup(t, () => ({
    decision: "postpone",
    delay_minutes: 1,
  }));
  const { a, owner } = await activity(app);
  const service = app.features.scheduleNotices;
  let simulated = now();
  service.clock = () => simulated;
  service.tick();
  await until(
    () => notice(app),
    (n) => n?.postponements === 1,
  );
  let n = notice(app);
  assert.equal(n.due_at, n.nominal_at + 60);
  let detail = app.features.schedule.detail(owner, a.id);
  assert.equal(detail.exceptions[0].nominal_at, n.nominal_at);
  assert.equal(detail.definition.start_at, n.nominal_at);
  simulated = n.due_at - 45;
  service.tick();
  await until(
    () => notice(app),
    (n) => n?.postponements === 2,
  );
  n = notice(app);
  assert.equal(n.due_at, n.nominal_at + 120);
  simulated = n.due_at - 45;
  service.tick();
  await settle(app);
  n = notice(app);
  assert.equal(n.postponements, 2);
  assert.equal(n.decision_status, "failed");
  assert.equal(n.error_code, "notice_postpone_limit");
  assert.equal(calls.length, 3);
  assert.equal(
    app.features.schedule.detail(owner, a.id).exceptions[0].definition.start_at,
    n.nominal_at + 120,
  );
});
test("cancel changes one opportunity without cancelling the base activity or refunding allocations", async (t) => {
  const { app } = await setup(t, () => ({
    decision: "cancel",
    delay_minutes: 0,
  }));
  const { a, owner } = await activity(app);
  const before = app.store.get(
    "SELECT * FROM schedule_allocations WHERE activity_id=?",
    a.id,
  );
  app.features.scheduleNotices.tick();
  const n = await settle(app);
  assert.equal(n.status, "cancelled");
  const detail = app.features.schedule.detail(owner, a.id);
  assert.equal(detail.state, "enabled");
  assert.equal(detail.exceptions[0].state, "cancelled");
  assert.deepEqual(
    {
      ...app.store.get(
        "SELECT * FROM schedule_allocations WHERE activity_id=?",
        a.id,
      ),
    },
    { ...before },
  );
});
test("manual, locked and unregistered writing never acquire model modification rights", async (t) => {
  const { app, calls } = await setup(t);
  const { owner, grant, definition } = await activity(app, { movable: false });
  const at = now();
  const manual = await app.call(
    "/schedule/activities",
    "POST",
    {
      definition: {
        ...definition,
        start_at: at + 10,
        end_at: at + 20,
        movable: true,
      },
    },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(manual.status, 201);
  const updated = app.features.schedule.saveGrant(
    owner,
    {
      confirmed: true,
      policy: { ...grant.policy, allowed_types: ["reading", "writing"] },
    },
    grant.id,
    grant.revision,
  );
  const unregistered = app.features.schedule.create(
    owner,
    {
      definition: {
        ...definition,
        type: "writing",
        resource_id: null,
        start_at: at + 30,
        end_at: at + 40,
      },
    },
    randomUUID(),
    true,
  );
  assert.equal(unregistered.state, "enabled");
  assert.ok(updated.revision > grant.revision);
  app.features.scheduleNotices.tick();
  assert.equal(notice(app), undefined);
  assert.equal(calls.length, 0);
});
test("invalid complete JSON is retained as a failed attempt and defaults to the unchanged opportunity", async (t) => {
  const { app, calls } = await setup(
    t,
    () => '{"decision":"postpone","delay_minutes":0}',
  );
  const { a, owner } = await activity(app);
  app.features.scheduleNotices.tick();
  const n = await settle(app);
  assert.equal(n.decision, "execute");
  assert.equal(n.decision_status, "failed");
  assert.equal(app.features.schedule.detail(owner, a.id).exceptions.length, 0);
  app.features.scheduleNotices.tick();
  assert.equal(calls.length, 1);
});
test("a late model result cannot overwrite an actual manual revision and retains the physical slot until close", async (t) => {
  let release;
  const { app, calls } = await setup(t, async () => {
    await new Promise((r) => (release = r));
    return { decision: "postpone", delay_minutes: 1 };
  });
  const { a, owner } = await activity(app);
  app.features.scheduleNotices.tick();
  await until(
    () => calls.length,
    (n) => n === 1,
  );
  assert.equal(app.runtime.runningCount, 1);
  app.features.schedule.change(owner, a.id, 1, {
    scope: "series",
    state: "paused",
  });
  assert.equal(app.runtime.runningCount, 1);
  release();
  await until(
    () => app.runtime.runningCount,
    (n) => n === 0,
  );
  app.features.scheduleNotices.tick();
  assert.equal(notice(app).status, "invalidated");
  assert.equal(app.features.schedule.detail(owner, a.id).state, "paused");
  assert.equal(app.features.schedule.detail(owner, a.id).exceptions.length, 0);
});
test("budget refusal records a reason without a request and cannot create a model permission", async (t) => {
  const { app, calls } = await setup(t);
  const { owner } = await activity(app);
  const day = new Date(Date.now() + 8 * 3600000).toISOString().slice(0, 10);
  app.store.run(
    "INSERT INTO schedule_notice_budgets VALUES(?,?,24,250000)",
    owner,
    day,
  );
  app.features.scheduleNotices.tick();
  assert.equal(notice(app).error_code, "notice_budget_exhausted");
  assert.equal(notice(app).attempts, 1);
  assert.equal(calls.length, 0);
});
test("a cancelled wake notice keeps an absolute deadline, works while ordinary chat is asleep, and never resets the limit", async (t) => {
  const { app, calls } = await setup(t, () => ({
    decision: "cancel",
    delay_minutes: 0,
  }));
  let p = app.features.automation.restPolicy();
  app.features.automation.saveRestPolicy(p.revision, {
    ...p.value,
    schedule_enabled: true,
    drain_seconds: 0,
    consolidation_seconds: 1,
    max_sleep_seconds: 180,
    default_sleep_seconds: 40,
  });
  app.features.automation.restControl(
    "test",
    { action: "sleep", duration_seconds: 40, request_id: randomUUID() },
    0,
  );
  await until(
    () => app.features.automation.restStatus().state.phase,
    (x) => x === "asleep",
  );
  const before = app.store.get(
    "SELECT * FROM rest_cycles WHERE completed_at IS NULL",
  );
  app.features.scheduleNotices.tick();
  const n = await settle(app);
  const after = app.store.get(
    "SELECT * FROM rest_cycles WHERE id=?",
    before.id,
  );
  assert.equal(n.kind, "wake");
  assert.equal(n.status, "cancelled");
  assert.equal(after.wake_at, before.absolute_wake_at);
  assert.equal(after.absolute_wake_at, before.absolute_wake_at);
  assert.equal(calls[0].body.messages.length, 2);
  assert.match(calls[0].body.messages[1].content, /asleep/);
  app.features.scheduleNotices.tick();
  assert.equal(calls.length, 1);
});
test("GET decision history and routine calendar remain authenticated, owner scoped and read only", async (t) => {
  const { app } = await setup(t);
  const p = app.features.automation.restPolicy();
  app.features.automation.saveRestPolicy(p.revision, {
    ...p.value,
    schedule_enabled: true,
  });
  const owner = (await app.call("/auth/me")).data.user.id;
  const before = app.store.get("SELECT count(*) AS n FROM schedule_notices").n;
  const start = now(),
    end = start + 2 * 86400;
  const r = await app.call(`/schedule/calendar?start=${start}&end=${end}`);
  assert.equal(r.status, 200);
  assert.ok(r.data.items.some((i) => i.origin === "routine"));
  assert.ok(
    r.data.items
      .filter((i) => i.origin === "routine")
      .every((i) => i.read_only),
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_notices").n,
    before,
  );
  assert.equal(
    (await app.call("/schedule/decisions", "GET", undefined, { auth: false }))
      .status,
    401,
  );
  assert.deepEqual(
    app.features.scheduleNotices.list(randomUUID(), new URLSearchParams())
      .items,
    [],
  );
  assert.ok(owner);
});

test("registered text survives an authorized single postponement and saves only at the changed opportunity", async (t) => {
  const { app, calls } = await setup(t, (body) => {
    if (body.messages[0].content.includes("PROMETHEUS_SCHEDULE_NOTICE_V1"))
      return { decision: "postpone", delay_minutes: 1 };
    const line = body.messages[0].content
      .split("\n")
      .find((s) => s.startsWith('{"available":'));
    return {
      reply: "这份文字已经登记。",
      self_activity: {
        slot_id: JSON.parse(line).available.slots[0].slot_id,
        title: "延期后仍完整的文字",
        content: "TEXT_MOVE_BODY_CANARY 完整正文。",
      },
    };
  });
  const { a, owner, grant } = await activity(app);
  app.features.schedule.change(owner, a.id, 1, {
    scope: "series",
    state: "paused",
  });
  app.features.schedule.saveGrant(
    owner,
    {
      confirmed: true,
      policy: { ...grant.policy, allowed_types: ["reading", "writing"] },
    },
    grant.id,
    grant.revision,
  );
  const original = app.features.schedule.writingOffer.bind(
    app.features.schedule,
  );
  app.features.schedule.writingOffer = (...args) => {
    const value = original(...args);
    return {
      ...value,
      slots: [
        {
          id: "fixture-text-notice-slot",
          start_at: now() + 50,
          end_at: now() + 650,
        },
      ],
    };
  };
  const c = app.runtime.createConversation(owner, "本次登记来源");
  const r = app.runtime.enqueue(c.id, owner, "请安排完整文字。", randomUUID());
  await until(
    () => app.runtime.getRun(r.run_id, owner),
    (r) => r.status === "completed",
  );
  let result = (await app.call("/schedule/text-activities")).data.items[0];
  assert.equal(result.state, "prepared");
  const before = result.start_at;
  app.features.scheduleNotices.tick();
  await until(
    () => notice(app),
    (n) => n?.postponements === 1,
  );
  result = (await app.call("/schedule/text-activities")).data.items[0];
  assert.equal(result.state, "prepared");
  assert.equal(result.start_at, before + 60);
  assert.match(result.content, /TEXT_MOVE_BODY_CANARY/);
  assert.equal(
    app.features.schedule.detail(owner, result.activity_id).capability_block,
    null,
  );
  const at = result.start_at + 1;
  app.features.dailyLife.clock = () => at;
  app.store.run("UPDATE schedule_runtime SET last_tick=0");
  app.features.schedule.tick(at);
  app.features.dailyLife.tick(at);
  result = (await app.call("/schedule/text-activities")).data.items[0];
  assert.equal(result.state, "saved");
  assert.equal(result.saved_at, at);
  assert.equal(calls.length, 2);
});
test("a future unconfirmed decision survives restart as interrupted and is not requested again", async (t) => {
  let release;
  const { app, calls } = await setup(t, async () => {
    await new Promise((r) => (release = r));
    return { decision: "cancel", delay_minutes: 0 };
  });
  const { a, owner } = await activity(app);
  app.features.scheduleNotices.tick();
  await until(
    () => calls.length,
    (n) => n === 1,
  );
  const directory = app.store.dataDir;
  await app.close();
  release();
  const restored = await fixture({ dataDir: directory });
  t.after(() => restored.close());
  restored.features.automation.onRoutineTick = () => {};
  restored.features.automation.onScheduleTick = () => {};
  assert.equal(notice(restored).decision_status, "interrupted");
  restored.features.scheduleNotices.tick();
  assert.equal(calls.length, 1);
  assert.equal(
    restored.features.schedule.detail(owner, a.id).exceptions.length,
    0,
  );
});
test("an actual future civil-minute wake receives one bounded choice while asleep and wakes without injected time or event rows", async (t) => {
  const { app, calls } = await setup(t);
  const p = app.features.automation.restPolicy();
  app.features.automation.saveRestPolicy(p.revision, {
    ...p.value,
    schedule_enabled: true,
    drain_seconds: 0,
    consolidation_seconds: 1,
    max_sleep_seconds: 180,
    default_sleep_seconds: 40,
  });
  let due = Math.floor(now() / 60) * 60 + 60;
  if (due - now() <= 5) due += 60;
  app.features.automation.restControl(
    "test",
    {
      action: "sleep",
      duration_seconds: due - now(),
      request_id: randomUUID(),
    },
    0,
  );
  await until(
    () => app.features.automation.restStatus().state.phase,
    (phase) => phase === "asleep",
  );
  app.features.automation.onRoutineTick = () =>
    app.features.scheduleNotices.tick();
  await until(
    () => calls.length,
    (n) => n === 1,
    10000,
  );
  const payload = JSON.parse(calls[0].body.messages[1].content);
  assert.equal(payload.due_at, due);
  assert.ok(payload.seconds_until > 0 && payload.seconds_until <= 60);
  await until(
    () => app.features.automation.restStatus().state.phase,
    (n) => n === "awake",
    70000,
  );
  assert.equal(calls.length, 1);
  assert.ok(now() >= due);
  assert.equal(notice(app).decision, "execute");
  assert.equal(app.store.get("SELECT count(*) AS n FROM delivery_parts").n, 0);
});

test("schema11 keeps its original wake cap and missing anticipation remains disabled, with an untouched old snapshot", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/notices-old11-" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db")),
    owner = randomUUID(),
    cycle = randomUUID(),
    at = now();
  db.exec(readFileSync(new URL("./schema11.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','11')").run();
  db.prepare(
    "INSERT INTO metadata VALUES('assistant.identity','notice-stable-before12')",
  ).run();
  db.prepare("INSERT INTO admins VALUES(?,?,?,?)").run(
    owner,
    "notice-old11",
    await passwordHash("Synthetic-notice-upgrade!"),
    at,
  );
  const policy = {
    schedule_enabled: false,
    natural_enabled: false,
    sleep_local: "23:00",
    wake_local: "07:00",
    default_sleep_seconds: 28800,
    max_sleep_seconds: 86400,
    drain_seconds: 30,
    consolidation_seconds: 300,
    reset_context_on_sleep: true,
  };
  db.prepare("INSERT INTO feature_policies VALUES('rest',3,?,?)").run(
    JSON.stringify(policy),
    at,
  );
  db.prepare(
    "INSERT INTO rest_cycles VALUES(?,'asleep',4,?,?,?,NULL,'admin',NULL)",
  ).run(cycle, at - 10, at + 30, at - 5);
  db.close();
  const app = await fixture({ dataDir: directory });
  t.after(() => app.close());
  assert.equal(
    (
      await app.call("/auth/login", "POST", {
        username: "notice-old11",
        password: "Synthetic-notice-upgrade!",
      })
    ).status,
    200,
  );
  assert.equal((await app.call("/export")).data.schema, 15);
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "15",
  );
  assert.equal(app.store.assistantId, "notice-stable-before12");
  assert.equal(
    app.store.get("SELECT absolute_wake_at FROM rest_cycles WHERE id=?", cycle)
      .absolute_wake_at,
    at + 30,
  );
  assert.equal(
    app.features.automation.restPolicy().value.anticipation_enabled,
    undefined,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_notices").n,
    0,
  );
  const snapshot = new DatabaseSync(
    join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((n) =>
        n.startsWith("schema11-"),
      ),
    ),
    { readOnly: true },
  );
  try {
    assert.equal(
      snapshot
        .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
        .get().value,
      "11",
    );
    assert.equal(
      snapshot.prepare("PRAGMA integrity_check").get().integrity_check,
      "ok",
    );
    assert.ok(
      !snapshot
        .prepare("PRAGMA table_info(rest_cycles)")
        .all()
        .some((c) => c.name === "absolute_wake_at"),
    );
  } finally {
    snapshot.close();
  }
});
test("a request whose original deadline passed cannot apply a late postponement or keep a running decision receipt", async (t) => {
  let release;
  const { app, calls } = await setup(t, async () => {
    await new Promise((r) => (release = r));
    return { decision: "postpone", delay_minutes: 1 };
  });
  const { a, owner } = await activity(app);
  app.features.scheduleNotices.tick();
  await until(
    () => calls.length,
    (n) => n === 1,
  );
  const due = notice(app).due_at;
  app.features.scheduleNotices.clock = () => due + 1;
  app.features.scheduleNotices.tick();
  assert.equal(notice(app).status, "elapsed");
  assert.equal(notice(app).decision_status, "failed");
  assert.equal(app.runtime.runningCount, 1);
  release();
  await until(
    () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.equal(app.features.schedule.detail(owner, a.id).exceptions.length, 0);
  assert.equal(notice(app).error_code, "notice_deadline_reached");
});
test("revoked grants abort pending choices and source authorization is checked independently of the routine switch", async (t) => {
  let release;
  const { app, calls } = await setup(t, async () => {
    await new Promise((r) => (release = r));
    return { decision: "postpone", delay_minutes: 1 };
  });
  const { a, owner, grant } = await activity(app);
  app.features.scheduleNotices.tick();
  await until(
    () => calls.length,
    (n) => n === 1,
  );
  app.features.schedule.revokeGrant(owner, grant.id, grant.revision);
  release();
  await until(
    () => app.runtime.runningCount,
    (n) => n === 0,
  );
  app.features.scheduleNotices.tick();
  assert.equal(notice(app).status, "invalidated");
  assert.equal(app.features.schedule.detail(owner, a.id).exceptions.length, 0);
});
test("a busy automatic slot keeps an unattempted notice pending and only dispatches after the original owner releases it", async (t) => {
  const { app, calls } = await setup(t);
  await activity(app);
  const release = app.runtime.beginCheck(true);
  app.features.scheduleNotices.tick();
  assert.equal(notice(app).decision_status, "pending");
  assert.equal(notice(app).attempts, 0);
  assert.equal(calls.length, 0);
  release();
  app.features.scheduleNotices.tick();
  await settle(app);
  assert.equal(calls.length, 1);
});
test("missing or length completion metadata cannot authorize a schedule choice even with closed JSON", async (t) => {
  for (const finish of ["length", undefined]) {
    const { app } = await setup(t, (_body, _n, res) => {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          choices: [
            {
              message: { content: '{"decision":"cancel","delay_minutes":0}' },
              ...(finish ? { finish_reason: finish } : {}),
            },
          ],
        }),
      );
    });
    const { a, owner } = await activity(app);
    app.features.scheduleNotices.tick();
    const n = await settle(app);
    assert.equal(n.decision_status, "failed");
    assert.equal(n.decision, "execute");
    assert.equal(
      app.features.schedule.detail(owner, a.id).exceptions.length,
      0,
    );
  }
});
test("a newly occupied postponed tail rejects the entire schedule change while retaining the attempt and charged budget", async (t) => {
  let release;
  const { app, calls } = await setup(t, async () => {
    await new Promise((r) => (release = r));
    return { decision: "postpone", delay_minutes: 1 };
  });
  const { a, owner, definition } = await activity(app);
  app.features.scheduleNotices.tick();
  await until(
    () => calls.length,
    (n) => n === 1,
  );
  const inserted = await app.call(
    "/schedule/activities",
    "POST",
    {
      definition: {
        ...definition,
        title: "后加入的人工事项",
        start_at: definition.end_at,
        end_at: definition.end_at + 120,
      },
    },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(inserted.status, 201);
  release();
  const n = await settle(app);
  assert.equal(n.error_code, "schedule_conflict");
  assert.equal(app.features.schedule.detail(owner, a.id).exceptions.length, 0);
  assert.equal(
    app.store.get(
      "SELECT sum(calls) AS n FROM schedule_notice_budgets WHERE owner_id=?",
      owner,
    ).n,
    1,
  );
});
test("cancelling a routine occurrence leaves the fixed civil time unchanged and a later civil day is a distinct notice", async (t) => {
  const { app, calls } = await setup(t, () => ({
    decision: "cancel",
    delay_minutes: 0,
  }));
  let p = app.features.automation.restPolicy();
  const target = new Date(Date.now() + 8 * 3600000 + 3600000)
    .toISOString()
    .slice(11, 16);
  p = app.features.automation.saveRestPolicy(p.revision, {
    ...p.value,
    schedule_enabled: true,
    sleep_local: target,
    wake_local: target === "07:00" ? "08:00" : "07:00",
  });
  const day = new Date(Date.now() + 8 * 3600000).toISOString().slice(0, 10),
    due = Date.parse(`${day}T${target}:00+08:00`) / 1000;
  let simulated = due - 30;
  app.features.scheduleNotices.clock = () => simulated;
  app.features.scheduleNotices.tick();
  await settle(app);
  simulated = due + 1;
  app.features.scheduleNotices.tick();
  assert.equal(app.features.automation.restStatus().state.phase, "awake");
  assert.equal(app.features.automation.restPolicy().value.sleep_local, target);
  simulated = due + 86400 - 30;
  app.features.scheduleNotices.tick();
  await until(
    () => calls.length,
    (n) => n === 2,
  );
  await until(
    () =>
      app.store.get(
        "SELECT count(*) AS n FROM schedule_notices WHERE status='cancelled'",
      ).n,
    (n) => n === 2,
  );
  assert.equal(app.features.automation.restPolicy().value.sleep_local, target);
});

test("a second notice can really postpone after the original start passed, while preserving the original nominal key", async (t) => {
  const { app, calls } = await setup(t, () => ({
    decision: "postpone",
    delay_minutes: 1,
  }));
  const at = now();
  const { a, owner } = await activity(app, {
    start_at: at + 4,
    end_at: at + 604,
  });
  app.features.scheduleNotices.tick();
  await until(
    () => notice(app),
    (n) => n?.postponements === 1,
  );
  await until(
    () => now(),
    (n) => n > at + 4,
    10000,
  );
  app.features.scheduleNotices.tick();
  await until(
    () => notice(app),
    (n) => n?.postponements === 2,
  );
  assert.equal(calls.length, 2);
  const detail = app.features.schedule.detail(owner, a.id);
  assert.equal(detail.state, "enabled");
  assert.equal(detail.exceptions[0].nominal_at, at + 4);
  assert.equal(detail.exceptions[0].definition.start_at, at + 124);
});

test("a postponement crossing a civil day charges the new opportunity date and retains the original allocation", async (t) => {
  const { app } = await setup(t, () => ({
    decision: "postpone",
    delay_minutes: 30,
  }));
  const day = new Date(Date.now() + 8 * 3600000).toISOString().slice(0, 10);
  let start = Date.parse(`${day}T23:40:00+08:00`) / 1000;
  if (start <= now() + 60) start += 86400;
  const { a, owner } = await activity(
    app,
    { start_at: start, end_at: start + 600 },
    { valid_until: start + 7200, horizon_days: 2 },
  );
  const original = {
    ...app.store.get(
      "SELECT * FROM schedule_allocations WHERE activity_id=?",
      a.id,
    ),
  };
  app.features.scheduleNotices.clock = () => start - 30;
  app.features.scheduleNotices.tick();
  await until(
    () => notice(app),
    (n) => n?.postponements === 1,
  );
  const rows = app.store.all(
    "SELECT * FROM schedule_allocations WHERE activity_id=? ORDER BY day",
    a.id,
  );
  assert.equal(rows.length, 2);
  assert.deepEqual({ ...rows[0] }, original);
  assert.equal(rows[1].seconds, 600);
  assert.notEqual(rows[1].day, original.day);
  assert.equal(
    app.store.get(
      "SELECT changes FROM schedule_budgets WHERE owner_id=? AND day=?",
      owner,
      rows[1].day,
    ).changes,
    1,
  );
});
