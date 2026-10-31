import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fixture, until } from "./helpers.mjs";
import { now, hash, privateDirectory, passwordHash } from "../dist/security.js";
import { splitTextPlan } from "../dist/daily-life-json.js";
const match = (n) => ({ headers: { "if-match": `"${n}"` } });
const offered = (body) => {
  const line = body.messages[0].content
    .split("\n")
    .find((s) => s.startsWith('{"available":'));
  return line ? JSON.parse(line).available : { slots: [] };
};
async function provider(t, handle) {
  const calls = [],
    server = createServer(async (req, res) => {
      let raw = "";
      for await (const c of req) raw += c;
      const body = JSON.parse(raw);
      calls.push({ body, res });
      const answer = await handle(body, calls.length, res);
      if (answer !== undefined && !res.destroyed) {
        const content = typeof answer === "string" ? answer : answer.content;
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          JSON.stringify({
            choices: [
              { message: { content }, finish_reason: answer.finish ?? "stop" },
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
  return { calls, url: `http://127.0.0.1:${server.address().port}` };
}
async function setup(t, model) {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  app.features.automation.onScheduleTick = () => {};
  await app.features.knowledge.stop();
  if (model) await app.configure(model.url);
  await app.call("/diary/policies");
  return app;
}
async function writingGrant(app, extra = {}) {
  const r = await app.call("/schedule/autonomy-grants", "POST", {
    confirmed: true,
    policy: {
      title: "自主文字的明确范围",
      allowed_types: ["writing"],
      resource_ids: [],
      time_zone: "Asia/Shanghai",
      valid_from: now() - 60,
      valid_until: now() + 2 * 86400,
      horizon_days: 2,
      window_start: "00:00",
      window_end: "23:59",
      max_daily_activities: 8,
      max_daily_minutes: 120,
      max_daily_changes: 12,
      max_daily_model_calls: 8,
      max_daily_tokens: 100000,
      max_output_tokens: 1200,
      ...extra,
    },
  });
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return r.data;
}
async function conversation(app) {
  const r = await app.call("/conversations", "POST", {
    title: "本次来源的独立聊天",
  });
  assert.equal(r.status, 201);
  return r.data;
}
async function send(
  app,
  c,
  input = "请自然回应这次交流。",
  key = randomUUID(),
) {
  const r = await app.call(`/conversations/${c.id}/messages`, "POST", {
    content: input,
    client_message_id: key,
  });
  assert.equal(r.status, 202, JSON.stringify(r.data));
  const run = await until(
    () =>
      app.runtime.getRun(
        r.data.run_id,
        c.owner_id ??
          app.store.get("SELECT owner_id FROM conversations WHERE id=?", c.id)
            .owner_id,
      ),
    (r) => !["queued", "running"].includes(r.status),
    10000,
  );
  return run;
}
const selection = (
  body,
  title = "合成文字安排",
  content = "完整的来源私有成果。",
) => ({
  reply: "这件事已经登记，成果会按实际状态保存。",
  self_activity: {
    slot_id: offered(body).slots[0]?.slot_id ?? "missing",
    title,
    content,
  },
});
const artifacts = async (app) =>
  (await app.call("/schedule/text-activities")).data.items;
function materialize(app, at) {
  app.store.run("UPDATE schedule_runtime SET last_tick=0 WHERE id=1");
  app.features.schedule.tick(at);
}

test("text reply and registered activity commit together; preparation, actual save, private index, expiry and forgetting remain distinct", async (t) => {
  const model = await provider(t, (body) =>
      JSON.stringify(
        selection(
          body,
          "TITLE_PRIVATE_CANARY",
          "BODY_PRIVATE_CANARY 完整文字成果。",
        ),
      ),
    ),
    app = await setup(t, model),
    g = await writingGrant(app),
    c = await conversation(app);
  const run = await send(app, c);
  assert.equal(run.status, "completed", JSON.stringify(run));
  assert.equal(model.calls.length, 1);
  assert.ok(!run.response.includes("self_activity"));
  let [a] = await artifacts(app);
  assert.equal(a.state, "prepared");
  assert.equal(a.saved_at, null);
  assert.equal(a.content, "BODY_PRIVATE_CANARY 完整文字成果。");
  assert.equal(a.delivery_status, "not_applicable");
  assert.equal(
    (await app.call("/schedule/activities/" + a.activity_id)).data
      .capability_block,
    null,
  );
  materialize(app, now());
  app.features.dailyLife.tick();
  assert.equal((await artifacts(app))[0].state, "prepared");
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='schedule_activity'",
    ).n,
    0,
  );
  // Only this isolated fixture advances its internal clock. Production has no clock-setting API.
  app.features.dailyLife.clock = () => a.start_at;
  materialize(app, a.start_at);
  app.features.dailyLife.tick();
  [a] = await artifacts(app);
  assert.equal(a.state, "saved");
  assert.equal(a.saved_at, a.start_at);
  const occurrence = app.store.get(
    "SELECT * FROM schedule_occurrences WHERE activity_id=?",
    a.activity_id,
  );
  assert.equal(occurrence.status, "completed");
  assert.equal(
    JSON.parse(occurrence.result_json).method,
    "saved_text_artifact",
  );
  assert.equal(occurrence.checkpoint, [...a.content].length);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='schedule_activity' AND visibility='private'",
    ).n,
    1,
  );
  const index = app.store.get(
    "SELECT * FROM diary_events WHERE origin_kind='schedule_activity'",
  );
  assert.equal(
    index.owner_id,
    c.owner_id ??
      app.store.get("SELECT owner_id FROM conversations WHERE id=?", c.id)
        .owner_id,
  );
  assert.ok(!index.content.includes("PRIVATE_CANARY"));
  app.features.dailyLife.tick();
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='schedule_activity'",
    ).n,
    1,
  );
  app.features.dailyLife.clock = () => g.policy.valid_until + 1;
  assert.equal((await artifacts(app))[0].state, "saved");
  assert.equal((await artifacts(app))[0].content, a.content);
  const forgotten = await app.call(
    `/schedule/text-activities/${a.activity_id}/forget`,
    "POST",
    {},
    match(a.revision),
  );
  assert.equal(forgotten.status, 200, JSON.stringify(forgotten.data));
  assert.equal(forgotten.data.state, "invalidated");
  assert.equal(forgotten.data.content, null);
  assert.equal(forgotten.data.saved_at, a.saved_at);
  assert.equal(
    app.store.get(
      "SELECT result_json FROM schedule_occurrences WHERE id=?",
      occurrence.id,
    ).result_json,
    null,
  );
  assert.equal(
    app.store.get("SELECT content FROM diary_events WHERE id=?", index.id)
      .content,
    null,
  );
  assert.equal(
    app.store.get(
      "SELECT original_reply FROM daily_reply_checks WHERE run_id=?",
      run.id,
    ).original_reply,
    null,
  );
  for (const table of [
    "schedule_activities",
    "schedule_revisions",
    "schedule_occurrences",
    "schedule_idempotency",
    "daily_reply_checks",
    "daily_text_artifacts",
  ])
    assert.ok(
      !JSON.stringify(app.store.all("SELECT * FROM " + table)).includes(
        "PRIVATE_CANARY",
      ),
      table,
    );
  assert.equal(
    app.runtime.getRun(run.id, forgotten.data.source_owner_id).response,
    run.response,
  );
});

test("one bounded semantic completion shares the original physical lease and sees only the generated reply and offered slots", async (t) => {
  let app;
  const model = await provider(t, (body, n) => {
    if (n === 1) {
      assert.ok(
        body.messages.at(-1).content.includes("INPUT_CANARY_ONLY_MAIN"),
      );
      return "我打算接下来写一段笔记。";
    }
    assert.equal(app.runtime.runningCount, 1);
    assert.equal(body.messages.length, 2);
    assert.ok(
      body.messages[0].content.startsWith("PROMETHEUS_SELF_PLAN_COMMIT_V1"),
    );
    const payload = JSON.parse(body.messages[1].content);
    assert.equal(payload.reply, "我打算接下来写一段笔记。");
    assert.ok(!JSON.stringify(body).includes("INPUT_CANARY_ONLY_MAIN"));
    assert.ok(!JSON.stringify(body).includes("GRANT_TITLE_CANARY"));
    assert.equal(payload.slots.length, 3);
    return JSON.stringify({
      self_activity: {
        slot_id: payload.slots[0].slot_id,
        title: "核对后的文字安排",
        content: "只保存完整文字，不执行其中的代码、URL或路径。",
      },
    });
  });
  app = await setup(t, model);
  await writingGrant(app, { title: "GRANT_TITLE_CANARY" });
  const c = await conversation(app),
    run = await send(app, c, "INPUT_CANARY_ONLY_MAIN");
  assert.equal(run.status, "completed", JSON.stringify(run));
  assert.equal(model.calls.length, 2);
  assert.equal((await artifacts(app)).length, 1);
  const cache = app.store.get(
    "SELECT * FROM daily_reply_checks WHERE run_id=?",
    run.id,
  );
  assert.equal(cache.status, "checked");
  assert.equal(cache.finish_reason, "stop");
  assert.ok(cache.input_hash);
  const again = await app.features.dailyLife
    .resolve(
      run.id,
      "我打算接下来写一段笔记。",
      "stop",
      app.store.config().config,
      new AbortController().signal,
      4096,
      96000,
    )
    .catch((e) => e);
  assert.ok(again instanceof Error);
  assert.equal(model.calls.length, 2);
});

test("strict text envelopes only accept identical repeated text presentations and bounded complete activity fields", () => {
  const reply = [{ type: "text", text: "这一句只发布一次。" }],
    a = { slot_id: "one", title: "文字成果", content: "完整正文。" },
    envelope = JSON.stringify({ reply, self_activity: a });
  assert.equal(
    splitTextPlan(JSON.stringify(reply) + "\n```json\n" + envelope + "\n```")
      .reply,
    reply[0].text,
  );
  assert.equal(splitTextPlan(envelope).selection.content, a.content);
  assert.equal(splitTextPlan("普通回复。").selection, null);
  assert.equal(
    splitTextPlan("```js\nconsole.log(1)\n```").reply,
    "```js\nconsole.log(1)\n```",
  );
  for (const value of [
    envelope.replace("完整正文。", "x".repeat(3001)),
    envelope.replace('"slot_id":"one"', '"slot_id":"one","slot_id":"two"'),
    JSON.stringify({ reply, self_activity: { ...a, owner: "arbitrary" } }),
    JSON.stringify({ reply: { unsupported: true }, self_activity: a }),
    JSON.stringify(reply) +
      "\n" +
      JSON.stringify({
        reply: [{ type: "text", text: "另一回复。" }],
        self_activity: a,
      }),
    JSON.stringify({
      reply: [{ type: "voice", text: "不能绕过语音门禁" }],
      self_activity: a,
    }),
  ])
    assert.throws(() => splitTextPlan(value));
});

test("unknown slots, duplicate fields, unsupported actions and incomplete inline results cannot publish a reply or register work", async (t) => {
  let mode = "unknown";
  const model = await provider(t, (body) => {
      const plan = selection(body);
      if (mode === "unknown") plan.self_activity.slot_id = "not-offered";
      if (mode === "extra") plan.self_activity.command = "anything";
      let content = JSON.stringify(plan);
      if (mode === "duplicate")
        content = content.replace('"content":', '"content":"first","content":');
      return {
        content,
        finish:
          mode === "length"
            ? "length"
            : mode === "missing"
              ? "missing"
              : "stop",
      };
    }),
    app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app);
  for (mode of ["unknown", "extra", "duplicate", "length", "missing"]) {
    const run = await send(app, c, "本次边界 " + mode);
    assert.equal(run.status, "failed", mode + JSON.stringify(run));
    assert.equal(
      app.store.get(
        "SELECT count(*) AS n FROM messages WHERE run_id=? AND role='assistant'",
        run.id,
      ).n,
      0,
    );
    assert.equal((await artifacts(app)).length, 0);
  }
  assert.equal(model.calls.length, 5);
});

test("quoted, conditional and past self references remain ordinary replies, while an unfulfilled new physical commitment is withheld", async (t) => {
  let current = "引用“我准备写笔记”，这是一段过去的转述。";
  const model = await provider(t, (body) =>
      body.messages[0].content.startsWith("PROMETHEUS_SELF_PLAN_COMMIT_V1")
        ? JSON.stringify({
            self_activity: null,
            ...(current === "我现在去外面散步。" ? { unfulfilled: true } : {}),
          })
        : current,
    ),
    app = await setup(t, model),
    c = await conversation(app);
  for (current of [
    "引用“我准备写笔记”，这是一段过去的转述。",
    "如果我准备写，之后再决定。",
    "我之前读过这本书。",
  ]) {
    const r = await send(app, c);
    assert.equal(r.status, "completed", JSON.stringify(r));
    assert.equal(r.response, current);
    assert.equal((await artifacts(app)).length, 0);
  }
  current = "我现在去外面散步。";
  const r = await send(app, c);
  assert.equal(r.status, "failed");
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM messages WHERE run_id=? AND role='assistant'",
      r.id,
    ).n,
    0,
  );
  assert.equal(model.calls.length, 8);
});

test("revoking a writing grant during the original response invalidates the offered transaction before publication", async (t) => {
  let release;
  const gate = new Promise((r) => (release = r)),
    model = await provider(t, async (body) => {
      await gate;
      return JSON.stringify(selection(body));
    }),
    app = await setup(t, model),
    g = await writingGrant(app),
    c = await conversation(app),
    owner = app.store.get(
      "SELECT owner_id FROM conversations WHERE id=?",
      c.id,
    ).owner_id;
  const queued = app.runtime.enqueue(c.id, owner, "请回应本轮", randomUUID());
  await until(
    async () => model.calls.length,
    (n) => n === 1,
  );
  assert.equal(app.runtime.runningCount, 1);
  const revoked = await app.call(
    `/schedule/autonomy-grants/${g.id}/revoke`,
    "POST",
    {},
    match(g.revision),
  );
  assert.equal(revoked.status, 200);
  release();
  const ended = await until(
    () => app.runtime.getRun(queued.run_id, owner),
    (r) => !["queued", "running"].includes(r.status),
  );
  assert.notEqual(ended.status, "completed");
  await until(
    async () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.equal((await artifacts(app)).length, 0);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_activities").n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM messages WHERE run_id=? AND role='assistant'",
      ended.id,
    ).n,
    0,
  );
});

test("a cancelled semantic check retains its original slot, leaves no activity, and never replays on the same input receipt", async (t) => {
  let release;
  const gate = new Promise((r) => (release = r));
  let app;
  const model = await provider(t, async (body, n) => {
    if (n === 1) return "我准备接下来写点文字。";
    assert.equal(app.runtime.runningCount, 1);
    await gate;
    const p = JSON.parse(body.messages[1].content);
    return JSON.stringify({
      self_activity: {
        slot_id: p.slots[0].slot_id,
        title: "不应落库的安排",
        content: "不应落库的完整正文",
      },
    });
  });
  app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app),
    owner = app.store.get(
      "SELECT owner_id FROM conversations WHERE id=?",
      c.id,
    ).owner_id,
    key = randomUUID(),
    queued = app.runtime.enqueue(c.id, owner, "请回应", key);
  await until(
    async () => model.calls.length,
    (n) => n === 2,
  );
  assert.equal(app.runtime.getRun(queued.run_id, owner).status, "running");
  assert.equal(app.runtime.runningCount, 1);
  app.runtime.cancel(queued.run_id, owner);
  release();
  await until(
    async () => app.runtime.runningCount,
    (n) => n === 0,
  );
  assert.equal(app.runtime.getRun(queued.run_id, owner).status, "cancelled");
  assert.equal((await artifacts(app)).length, 0);
  const prior = app.runtime.enqueue(c.id, owner, "请回应", key);
  assert.equal(prior.run_id, queued.run_id);
  await new Promise((r) => setTimeout(r, 100));
  assert.equal(model.calls.length, 2);
  assert.notEqual(
    app.store.get(
      "SELECT status FROM daily_reply_checks WHERE run_id=?",
      queued.run_id,
    ).status,
    "checked",
  );
});

test("QQ source binding is explicit, does not borrow an administrator grant, and saves only a private generic diary index", async (t) => {
  const model = await provider(t, (body) =>
      offered(body).slots.length
        ? JSON.stringify(selection(body, "QQ_PRIVATE_TITLE", "QQ_PRIVATE_BODY"))
        : "当前没有新安排。",
    ),
    app = await setup(t, model),
    cid = randomUUID(),
    qq = "qq:fixture:c2c:writer",
    otherCid = randomUUID(),
    other = "weixin:fixture:writer";
  for (const [id, owner] of [
    [cid, qq],
    [otherCid, other],
  ]) {
    app.store.run(
      "INSERT INTO conversations(id,owner_id,source_key,title,context_epoch,created_at) VALUES(?,?,?,'合成私聊文字来源',0,?)",
      id,
      owner,
      owner,
      now(),
    );
    const grant = await app.call(
      "/knowledge/source/grant",
      "PUT",
      {
        source_key: owner,
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
  }
  const unbound = await writingGrant(app),
    channelSend = async (id, owner) => {
      const q = app.runtime.enqueue(
        id,
        owner,
        "来源私聊的普通输入",
        randomUUID(),
      );
      return until(
        () => app.runtime.getRun(q.run_id, owner),
        (r) => !["queued", "running"].includes(r.status),
      );
    };
  assert.equal((await channelSend(cid, qq)).status, "completed");
  assert.equal((await artifacts(app)).length, 0);
  assert.equal(offered(model.calls[0].body).slots.length, 0);
  const bound = await writingGrant(app, { source_conversation_id: cid });
  const policy = await app.call("/diary/policies", "POST", {
    source_key: qq,
    time_zone: "Asia/Shanghai",
    visibility: "private",
    enabled: true,
    allow_model: false,
    confirmed: true,
  });
  assert.equal(policy.status, 201, JSON.stringify(policy.data));
  const result = await channelSend(cid, qq);
  assert.equal(result.status, "completed", JSON.stringify(result));
  let [a] = await artifacts(app);
  assert.equal(a.source_owner_id, qq);
  assert.equal(a.conversation_id, cid);
  assert.notEqual(a.source_owner_id, unbound.owner_id);
  assert.equal((await channelSend(otherCid, other)).status, "completed");
  assert.equal((await artifacts(app)).length, 1);
  app.features.dailyLife.clock = () => a.start_at;
  materialize(app, a.start_at);
  app.features.dailyLife.tick();
  [a] = await artifacts(app);
  assert.equal(a.state, "saved");
  const index = app.store.get(
    "SELECT * FROM diary_events WHERE origin_kind='schedule_activity'",
  );
  assert.equal(index.owner_id, qq);
  assert.equal(index.visibility, "private");
  assert.ok(!index.content.includes("QQ_PRIVATE"));
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM diary_events WHERE origin_kind='schedule_activity' AND visibility='shared'",
    ).n,
    0,
  );
  const revoked = await app.call(
    "/knowledge/source/grant",
    "PUT",
    {
      source_key: qq,
      grant: {
        capture: false,
        allow_model: false,
        can_sleep: false,
        can_wake: false,
        expires_at: null,
      },
    },
    match(1),
  );
  assert.equal(revoked.status, 200);
  assert.equal((await artifacts(app))[0].state, "invalidated");
  assert.equal(
    app.store.get("SELECT content FROM diary_events WHERE id=?", index.id)
      .content,
    null,
  );
  assert.equal(
    app.store.get(
      "SELECT content FROM daily_text_artifacts WHERE activity_id=?",
      a.activity_id,
    ).content,
    null,
  );
});

test("same-content pending work is not duplicated, cancellation does not refund allocations, and source memory forgetting erases all related caches", async (t) => {
  const model = await provider(t, (body) =>
      offered(body).slots.length
        ? JSON.stringify(
            selection(body, "DEDUP_PRIVATE_TITLE", "DEDUP_PRIVATE_BODY"),
          )
        : "没有增加新的安排。",
    ),
    app = await setup(t, model);
  await writingGrant(app, { max_daily_activities: 2 });
  const c = await conversation(app);
  const first = await send(app, c),
    second = await send(app, c);
  assert.equal(first.status, "completed");
  assert.equal(second.status, "completed");
  assert.equal((await artifacts(app)).length, 1);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_allocations").n,
    1,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM daily_reply_checks WHERE selection_json IS NOT NULL",
    ).n,
    2,
  );
  let [a] = await artifacts(app);
  const forgotten = await app.call(
    `/schedule/text-activities/${a.activity_id}/forget`,
    "POST",
    {},
    match(a.revision),
  );
  assert.equal(forgotten.status, 200);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_allocations").n,
    1,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM daily_reply_checks WHERE original_reply IS NOT NULL OR selection_json IS NOT NULL",
    ).n,
    0,
  );
  const third = await send(app, c);
  assert.equal(third.status, "completed");
  assert.equal(
    (await artifacts(app)).filter((a) => a.state === "prepared").length,
    1,
  );
  const memory = (
    await app.call("/records/memory", "POST", {
      data: { content: "要明确遗忘的旧来源" },
    })
  ).data;
  assert.equal(
    (
      await app.call(
        `/records/memory/${memory.id}`,
        "DELETE",
        undefined,
        match(memory.revision),
      )
    ).status,
    200,
  );
  assert.ok((await artifacts(app)).every((a) => a.state === "invalidated"));
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM daily_reply_checks WHERE original_reply IS NOT NULL OR selection_json IS NOT NULL",
    ).n,
    0,
  );
  assert.ok(
    !JSON.stringify(app.store.all("SELECT * FROM schedule_revisions")).includes(
      "DEDUP_PRIVATE",
    ),
  );
});

test("a captured original message correction immediately erases pending text provenance without waiting for a background sweep", async (t) => {
  const model = await provider(t, (body) =>
      JSON.stringify(selection(body, "RAW_PRIVATE_TITLE", "RAW_PRIVATE_BODY")),
    ),
    app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app),
    owner = app.store.get(
      "SELECT owner_id FROM conversations WHERE id=?",
      c.id,
    ).owner_id;
  const allowed = await app.call(
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
  assert.equal(allowed.status, 200);
  const policy = await app.call("/diary/policies", "POST", {
    source_key: c.id,
    time_zone: "Asia/Shanghai",
    visibility: "private",
    enabled: true,
    allow_model: false,
    confirmed: true,
  });
  assert.equal(policy.status, 201);
  const run = await send(app, c);
  assert.equal(run.status, "completed");
  app.features.diary.lastSweep = 0;
  app.features.diary.tick();
  const message = app.store.get(
    "SELECT m.id FROM messages m WHERE m.run_id=? AND m.role='user'",
    run.id,
  );
  const event = await until(
    async () =>
      app.store.get(
        "SELECT * FROM diary_events WHERE origin_kind='message' AND origin_id=?",
        message.id,
      ),
    (r) => !!r,
  );
  const corrected = await app.call(
    `/diary/events/${event.id}`,
    "PATCH",
    {
      content: "已明确更正的原文",
      happened_at: now(),
      statement_type: "user_statement",
    },
    match(event.revision),
  );
  assert.equal(corrected.status, 200, JSON.stringify(corrected.data));
  assert.equal(
    app.store.get(
      "SELECT state FROM daily_text_artifacts WHERE source_run_id=?",
      run.id,
    ).state,
    "invalidated",
  );
  assert.equal(
    app.store.get(
      "SELECT original_reply FROM daily_reply_checks WHERE run_id=?",
      run.id,
    ).original_reply,
    null,
  );
  assert.equal(app.runtime.getRun(run.id, owner).status, "completed");
});

test("pause/resume and real sleep boundaries preserve legitimate future preparation but prevent saving while asleep", async (t) => {
  const model = await provider(t, (body) => JSON.stringify(selection(body))),
    app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app);
  assert.equal((await send(app, c)).status, "completed");
  let [a] = await artifacts(app),
    activity = (await app.call("/schedule/activities/" + a.activity_id)).data;
  const paused = await app.call(
    `/schedule/activities/${a.activity_id}/pause`,
    "POST",
    {},
    match(activity.revision),
  );
  assert.equal(paused.status, 200, JSON.stringify(paused.data));
  assert.equal((await artifacts(app))[0].state, "prepared");
  const resumed = await app.call(
    `/schedule/activities/${a.activity_id}/resume`,
    "POST",
    {},
    match(paused.data.revision),
  );
  assert.equal(resumed.status, 200, JSON.stringify(resumed.data));
  assert.equal((await artifacts(app))[0].state, "prepared");
  const sleep = await app.call(
    "/rest/control",
    "POST",
    { action: "sleep", request_id: randomUUID(), duration_seconds: 60 },
    match(0),
  );
  assert.equal(sleep.status, 200);
  app.features.dailyLife.clock = () => a.start_at;
  materialize(app, a.start_at);
  app.features.dailyLife.tick();
  assert.equal((await artifacts(app))[0].state, "prepared");
  const state = (await app.call("/rest/status")).data.state;
  const wake = await app.call(
    "/rest/control",
    "POST",
    { action: "wake", request_id: randomUUID() },
    match(state.revision),
  );
  assert.equal(wake.status, 200);
  materialize(app, a.start_at);
  app.features.dailyLife.tick();
  assert.equal((await artifacts(app))[0].state, "saved");
});

test("an actual elapsed Windows execution window saves once; the fixture offers a short future slot while production buffer is checked separately", async (t) => {
  const model = await provider(t, (body) =>
      JSON.stringify(selection(body, "实际时钟窗口", "实际时钟下保存的正文。")),
    ),
    app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app),
    original = app.features.schedule.writingOffer.bind(app.features.schedule);
  app.features.schedule.writingOffer = (manager, id, at) => {
    const real = original(manager, id, at);
    assert.ok(real.slots[0].start_at >= now() + 7 * 60 - 1);
    const start = now() + 4;
    return {
      ...real,
      slots: [
        {
          id: "fixture-real-elapsed-window",
          start_at: start,
          end_at: start + 60,
        },
      ],
    };
  };
  const run = await send(app, c);
  assert.equal(run.status, "completed", JSON.stringify(run));
  let [a] = await artifacts(app);
  assert.ok(a.prepared_at < a.start_at);
  assert.equal(a.state, "prepared");
  const ended = await until(
    async () => {
      materialize(app, now());
      app.features.dailyLife.tick();
      return (await artifacts(app))[0];
    },
    (r) => r.state === "saved",
    8000,
  );
  assert.ok(ended.saved_at >= a.start_at);
  assert.ok(ended.saved_at <= now());
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM schedule_occurrences WHERE activity_id=?",
      a.activity_id,
    ).n,
    1,
  );
  assert.equal(model.calls.length, 1);
});

test("prepared artifacts survive a service restart, but an unconfirmed semantic check becomes interrupted and is never replayed", async (t) => {
  let pending = false,
    release;
  const gate = new Promise((r) => (release = r)),
    model = await provider(t, async (body) => {
      if (
        body.messages[0].content.startsWith("PROMETHEUS_SELF_PLAN_COMMIT_V1")
      ) {
        await gate;
        return JSON.stringify({ self_activity: null });
      }
      return pending
        ? "我准备接下来写点文字。"
        : JSON.stringify(selection(body));
    }),
    app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app);
  const first = await send(app, c);
  assert.equal(first.status, "completed");
  const [a] = await artifacts(app);
  pending = true;
  const c2 = await conversation(app),
    owner = app.store.get(
      "SELECT owner_id FROM conversations WHERE id=?",
      c2.id,
    ).owner_id,
    waiting = app.runtime.enqueue(
      c2.id,
      owner,
      "触发一次尚未确认核对",
      randomUUID(),
    );
  await until(
    async () => model.calls.length,
    (n) => n === 3,
  );
  await app.close();
  release();
  const directory = app.dataDir,
    reopened = await fixture({ dataDir: directory });
  t.after(() => reopened.close());
  reopened.features.automation.onScheduleTick = () => {};
  await reopened.features.knowledge.stop();
  await reopened.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  const current = (
    await reopened.call(`/schedule/text-activities/${a.activity_id}`)
  ).data;
  assert.equal(current.state, "prepared");
  assert.equal(current.content, a.content);
  assert.equal(
    reopened.runtime.getRun(waiting.run_id, owner).status,
    "interrupted",
  );
  assert.notEqual(
    reopened.store.get(
      "SELECT status FROM daily_reply_checks WHERE run_id=?",
      waiting.run_id,
    ).status,
    "checking",
  );
  await new Promise((r) => setTimeout(r, 200));
  assert.equal(model.calls.length, 3);
  reopened.features.dailyLife.clock = () => a.start_at;
  materialize(reopened, a.start_at);
  reopened.features.dailyLife.tick();
  assert.equal(
    (await reopened.call(`/schedule/text-activities/${a.activity_id}`)).data
      .state,
    "saved",
  );
});

test("private artifact access, explicit forgetting, exact artifact revisions and CSRF remain scoped to the manager", async (t) => {
  const model = await provider(t, (body) => JSON.stringify(selection(body))),
    app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app);
  assert.equal((await send(app, c)).status, "completed");
  const [a] = await artifacts(app);
  assert.equal(
    (
      await app.call(
        `/schedule/text-activities/${a.activity_id}/forget`,
        "POST",
        {},
        match(a.revision + 1),
      )
    ).status,
    409,
  );
  assert.equal(
    (
      await app.call(
        `/schedule/text-activities/${a.activity_id}/forget`,
        "POST",
        {},
        { headers: { "if-match": '"' + a.revision + '"', "x-csrf-token": "" } },
      )
    ).status,
    403,
  );
  assert.equal(
    (
      await app.call(
        `/schedule/text-activities/${a.activity_id}`,
        "GET",
        undefined,
        { auth: false },
      )
    ).status,
    401,
  );
  app.store.run(
    "INSERT INTO admins VALUES(?,?,?,?)",
    randomUUID(),
    "daily-other",
    await passwordHash("Synthetic-daily-other!"),
    now(),
  );
  await app.call("/auth/login", "POST", {
    username: "daily-other",
    password: "Synthetic-daily-other!",
  });
  assert.equal((await artifacts(app)).length, 0);
  assert.equal(
    (await app.call(`/schedule/text-activities/${a.activity_id}`)).status,
    404,
  );
  assert.equal(
    (
      await app.call(
        `/schedule/text-activities/${a.activity_id}/forget`,
        "POST",
        {},
        match(a.revision),
      )
    ).status,
    404,
  );
  assert.equal(
    app.store.get(
      "SELECT state FROM daily_text_artifacts WHERE activity_id=?",
      a.activity_id,
    ).state,
    "prepared",
  );
});

test("a newly occupied offered slot rolls back both response and activity instead of silently choosing a different time", async (t) => {
  let app;
  const model = await provider(t, (body) => {
    const plan = selection(body),
      slot = offered(body).slots[0];
    const create = app.features.schedule.create(
      app.store.get("SELECT id FROM admins LIMIT 1").id,
      {
        definition: {
          title: "后来人工占用",
          type: "video",
          purpose: "不可挪动的手工事项",
          start_at: slot.start_at,
          end_at: slot.end_at,
          time_zone: "Asia/Shanghai",
          repeat: null,
          movable: false,
        },
      },
      randomUUID(),
    );
    assert.equal(create.state, "enabled");
    return JSON.stringify(plan);
  });
  app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app),
    run = await send(app, c);
  assert.equal(run.status, "failed");
  assert.equal((await artifacts(app)).length, 0);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_activities").n,
    1,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM messages WHERE run_id=? AND role='assistant'",
      run.id,
    ).n,
    0,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_allocations").n,
    0,
  );
});

test("daily allocation caps are not refunded after a cancelled preparation and arbitrary writing remains blocked", async (t) => {
  const model = await provider(t, (body) =>
      offered(body).slots.length
        ? JSON.stringify(selection(body))
        : "今天没有新增安排。",
    ),
    app = await setup(t, model);
  await writingGrant(app, { max_daily_activities: 1, horizon_days: 1 });
  const c = await conversation(app);
  assert.equal((await send(app, c)).status, "completed");
  const [a] = await artifacts(app),
    before = (await app.call("/schedule/activities/" + a.activity_id)).data;
  const cancel = await app.call(
    `/schedule/activities/${a.activity_id}/cancel`,
    "POST",
    {},
    match(before.revision),
  );
  assert.equal(cancel.status, 200);
  assert.equal((await artifacts(app))[0].state, "invalidated");
  assert.equal((await send(app, c)).status, "completed");
  assert.equal(offered(model.calls[1].body).slots.length, 0);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM schedule_allocations").n,
    1,
  );
  const free = await app.call(
    "/schedule/activities",
    "POST",
    {
      definition: {
        title: "未注册文字",
        type: "writing",
        purpose: "不能执行任意文本",
        start_at: now() + 7200,
        end_at: now() + 7800,
        time_zone: "Asia/Shanghai",
        movable: false,
      },
    },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(free.status, 201);
  assert.equal(
    (await app.call("/schedule/activities/" + free.data.id)).data
      .capability_block,
    "registered_text_activity_required",
  );
  assert.equal((await artifacts(app)).length, 1);
});

test("typed repeated text replies persist their pacing plan and seed one ordered Outbox without exposing artifact content", async (t) => {
  const reply = [
    { type: "text", text: "第一句只发送一次。", delay_ms: 0 },
    { type: "text", text: "第二句遵守间隔。", delay_ms: 7500 },
  ];
  const model = await provider(t, (body) => {
      const v = selection(
        body,
        "不应发送的成果标题",
        "不能进入Outbox的完整成果",
      );
      v.reply = reply;
      return JSON.stringify(reply) + "\n" + JSON.stringify(v);
    }),
    app = await setup(t, model);
  await writingGrant(app);
  const c = await conversation(app),
    run = await send(app, c);
  assert.equal(run.status, "completed", JSON.stringify(run));
  assert.equal(run.response, reply.map((r) => r.text).join("\n"));
  const presentation = app.store.get(
    "SELECT items_json FROM native_text_presentations WHERE run_id=?",
    run.id,
  );
  assert.ok(presentation);
  assert.equal(JSON.parse(presentation.items_json)[1].delay_ms, 7500);
  await app.features.channels.stop();
  app.store.run(
    "INSERT INTO run_targets VALUES(?,?,?,?,?,?)",
    run.id,
    "qq",
    JSON.stringify({
      openid: "synthetic-only",
      message_id: "synthetic-message",
    }),
    "synthetic-credential-ref",
    now() + 60,
    "fixture-account",
  );
  app.features.channels.seedDeliveries();
  app.features.channels.seedDeliveries();
  const parts = app.store.all(
    "SELECT * FROM delivery_parts WHERE run_id=? ORDER BY sequence",
    run.id,
  );
  assert.equal(parts.length, 2);
  assert.equal(parts[0].content, reply[0].text);
  assert.equal(parts[1].content, reply[1].text);
  assert.equal(parts[1].not_before - parts[0].not_before, 7500);
  assert.ok(!JSON.stringify(parts).includes("完整成果"));
  assert.equal(model.calls.length, 1);
});

test("model reading marked length cannot advance coverage even when its visible text is syntactically complete", async (t) => {
  const model = await provider(t, () => ({
      content: "看似完整但被服务标记截断的笔记。",
      finish: "length",
    })),
    app = await setup(t, model);
  const book = await app.call("/schedule/resources", "POST", {
    title: "截断保护资料",
    content: "全文需要真实处理。",
    type: "text",
    confirmed: true,
  });
  assert.equal(book.status, 201);
  const a = await app.call(
    "/schedule/activities",
    "POST",
    {
      definition: {
        title: "截断输出的阅读",
        type: "reading",
        purpose: "不可推进水位",
        start_at: now() - 1,
        end_at: now() + 90,
        time_zone: "Asia/Shanghai",
        resource_id: book.data.id,
        movable: false,
      },
    },
    { headers: { "idempotency-key": randomUUID() } },
  );
  assert.equal(a.status, 201);
  materialize(app, now());
  await until(
    async () =>
      app.store.get(
        "SELECT status FROM schedule_attempts ORDER BY rowid DESC LIMIT 1",
      ),
    (r) => r?.status === "failed",
  );
  const progress = app.store.get(
    "SELECT checkpoint FROM schedule_reading_progress WHERE resource_id=?",
    book.data.id,
  );
  assert.equal(progress.checkpoint, 0);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM schedule_reading_notes WHERE resource_id=?",
      book.data.id,
    ).n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT error_code FROM schedule_attempts ORDER BY rowid DESC LIMIT 1",
    ).error_code,
    "schedule_model_incomplete_output",
  );
});

test("schema10 upgrade preserves old writing grants, notes and affinity without inventing registered text or execution evidence", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/daily-schema10-" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db")),
    owner = randomUUID(),
    gid = randomUUID(),
    aid = randomUUID(),
    pid = randomUUID(),
    at = now();
  db.exec(readFileSync(new URL("./schema10.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES('prometheus.schema','10')").run();
  db.prepare(
    "INSERT INTO metadata VALUES('assistant.identity','daily-stable-before-11')",
  ).run();
  db.prepare(
    "INSERT INTO metadata VALUES('affinity.ledger.initialized','1')",
  ).run();
  db.prepare("INSERT INTO metadata VALUES('affinity.heartbeat',?)").run(
    String(at),
  );
  db.prepare("INSERT INTO admins VALUES(?,?,?,?)").run(
    owner,
    "daily-schema10",
    await passwordHash("Synthetic-daily-schema10!"),
    at,
  );
  const policy = {
    title: "旧文字授权",
    allowed_types: ["writing"],
    resource_ids: [],
    time_zone: "Asia/Shanghai",
    valid_from: at - 60,
    valid_until: at + 86400,
    horizon_days: 1,
    window_start: "00:00",
    window_end: "23:59",
    max_daily_activities: 2,
    max_daily_minutes: 30,
    max_daily_changes: 4,
    max_daily_model_calls: 8,
    max_daily_tokens: 24000,
    max_output_tokens: 1200,
  };
  db.prepare("INSERT INTO schedule_grants VALUES(?,?,4,?,NULL,?,?)").run(
    gid,
    owner,
    JSON.stringify(policy),
    at,
    at,
  );
  const definition = {
    title: "旧占位文字事项",
    type: "writing",
    purpose: "原来没有注册的正文不能推测补造",
    start_at: at + 3600,
    end_at: at + 4200,
    time_zone: "Asia/Shanghai",
    repeat: null,
    resource_id: null,
    target_ref: null,
    grant_id: null,
    movable: false,
    restart_reading: false,
  };
  db.prepare(
    "INSERT INTO schedule_activities VALUES(?,?,?,1,'manual','enabled',?,NULL,NULL,?,?)",
  ).run(
    aid,
    owner,
    "daily-stable-before-11",
    JSON.stringify(definition),
    at,
    at,
  );
  db.prepare(
    "INSERT INTO schedule_revisions VALUES(?,1,?,'[]','enabled',?)",
  ).run(aid, JSON.stringify(definition), at);
  db.prepare(
    "INSERT INTO records VALUES(?,'people',?,'private',2,?,?,?,NULL)",
  ).run(
    pid,
    owner,
    JSON.stringify({ name: "旧账本人", notes: "原有人物备注" }),
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
  await app.call("/auth/login", "POST", {
    username: "daily-schema10",
    password: "Synthetic-daily-schema10!",
  });
  assert.equal(app.store.assistantId, "daily-stable-before-11");
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "11",
  );
  assert.equal(
    app.store.get("SELECT policy_json FROM schedule_grants WHERE id=?", gid)
      .policy_json,
    JSON.stringify(policy),
  );
  assert.equal(
    (await app.call("/schedule/activities/" + aid)).data.capability_block,
    "registered_text_activity_required",
  );
  assert.equal((await artifacts(app)).length, 0);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM native_text_presentations").n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT score_micro FROM affinity_states WHERE person_id=?",
      pid,
    ).score_micro,
    49125000,
  );
  assert.equal(
    app.store.get(
      "SELECT remainder FROM affinity_states WHERE person_id=?",
      pid,
    ).remainder,
    321,
  );
  const old = new DatabaseSync(
    join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((n) =>
        n.startsWith("schema10-"),
      ),
    ),
    { readOnly: true },
  );
  try {
    assert.equal(
      old
        .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
        .get().value,
      "10",
    );
    assert.equal(
      old.prepare("PRAGMA integrity_check").get().integrity_check,
      "ok",
    );
  } finally {
    old.close();
  }
});

test("an isolated automatic read rejects length output and leaves its persistent trigger cursor unchanged", async (t) => {
  const model = await provider(t, () => ({
      content: "这段文字看起来闭合，但服务已标记截断。",
      finish: "length",
    })),
    app = await setup(t, model),
    rule = await app.call("/automation/triggers", "POST", {
      rule: { type: "once", after_seconds: 1 },
      action: {
        type: "read_text",
        title: "截断不能推进",
        content: "有限授权阅读资料。".repeat(80),
        batch_chars: 200,
        cursor: 0,
      },
    });
  assert.equal(rule.status, 201);
  const preview = (
    await app.call(`/automation/triggers/${rule.data.id}/preview`)
  ).data;
  const enabled = await app.call(
    `/automation/triggers/${rule.data.id}/enable`,
    "PUT",
    { enabled: true, preview_digest: preview.digest },
    match(rule.data.revision),
  );
  assert.equal(enabled.status, 200);
  const result = await until(
    () => app.call("/automation/results"),
    (r) => r.data.items.some((v) => v.status === "failed"),
    6000,
  );
  assert.equal(result.data.items[0].status, "failed");
  assert.equal(
    JSON.parse(
      app.store.get(
        "SELECT action_json FROM trigger_rules WHERE id=?",
        rule.data.id,
      ).action_json,
    ).cursor,
    0,
  );
  assert.equal((await artifacts(app)).length, 0);
  assert.equal(model.calls.length, 1);
});
