import assert from "node:assert/strict";
import { test } from "node:test";
import { randomUUID } from "node:crypto";
import { fixture } from "./helpers.mjs";
import { passwordHash, now } from "../dist/security.js";

test("records validate domain rules, publication consent and optimistic revisions", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  assert.equal(
    (
      await app.call("/records/memory", "POST", {
        data: { content: "公开须确认" },
        visibility: "shared",
      })
    ).status,
    400,
  );
  const saved = await app.call("/records/memory", "POST", {
    data: { content: "我喜欢冬天的薄荷" },
    visibility: "private",
  });
  assert.equal(saved.status, 201);
  assert.equal(saved.data.revision, 1);
  assert.equal((await app.call("/records/memory?q=薄荷")).data.total, 1);
  const updated = await app.call(
    `/records/memory/${saved.data.id}`,
    "PUT",
    { data: { content: "更正为夏天的薄荷" } },
    { headers: { "if-match": '"1"' } },
  );
  assert.equal(updated.data.revision, 2);
  assert.equal(
    (
      await app.call(
        `/records/memory/${saved.data.id}`,
        "PUT",
        { data: { content: "旧版本" } },
        { headers: { "if-match": '"1"' } },
      )
    ).status,
    409,
  );
  assert.equal(
    (
      await app.call("/records/schedule", "POST", {
        data: { title: "错误日程", start_at: 200, end_at: 100 },
      })
    ).status,
    400,
  );
  assert.equal(
    (
      await app.call("/records/diary", "POST", {
        data: { title: "无效日期", content: "...", date: "2026-02-31" },
      })
    ).status,
    400,
  );
  assert.equal(
    (
      await app.call("/records/skills", "POST", {
        data: {
          name: "脚本",
          instructions: "...",
          enabled: true,
          shell: "cmd.exe",
        },
      })
    ).status,
    400,
  );
});
test("forgetting clears live contents, search entries and dependent diary records", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const memory = (
    await app.call("/records/memory", "POST", {
      data: { content: "unique-source-to-forget" },
    })
  ).data;
  const diary = (
    await app.call("/records/diary", "POST", {
      data: {
        title: "来源派生日记",
        content: "unique-derived-content",
        date: "2026-10-30",
        sources: [memory.id],
      },
    })
  ).data;
  const monthly = (
    await app.call("/records/diary", "POST", {
      data: {
        title: "二级派生月记",
        content: "must-also-be-forgotten",
        date: "2026-10-30",
        period: "month",
        sources: [diary.id],
      },
    })
  ).data;
  assert.equal(diary.revision, 1);
  assert.equal(
    (
      await app.call(`/records/memory/${memory.id}`, "DELETE", undefined, {
        headers: { "if-match": '"1"' },
      })
    ).status,
    200,
  );
  assert.equal((await app.call(`/records/memory/${memory.id}`)).status, 404);
  assert.equal((await app.call(`/records/diary/${diary.id}`)).status, 404);
  assert.equal((await app.call(`/records/diary/${monthly.id}`)).status, 404);
  assert.equal(
    app.store.get("SELECT payload FROM records WHERE id=?", memory.id).payload,
    null,
  );
  assert.equal(
    app.store.get("SELECT payload FROM records WHERE id=?", diary.id).payload,
    null,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS count FROM record_fts WHERE record_fts MATCH '" +
        '"unique-source-to-forget"' +
        "'",
    ).count,
    0,
  );
});
test("people and directed relationships invalidate together on forgetting", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const alice = (
    await app.call("/records/people", "POST", {
      data: { name: "同名甲", notes: "来源甲" },
    })
  ).data;
  const bob = (
    await app.call("/records/people", "POST", {
      data: { name: "同名甲", notes: "来源乙" },
    })
  ).data;
  assert.notEqual(alice.id, bob.id);
  const edge = await app.call("/records/relationships", "POST", {
    data: { from_id: alice.id, to_id: bob.id, description: "同事" },
  });
  assert.equal(edge.status, 201);
  await app.call(`/records/people/${alice.id}`, "DELETE", undefined, {
    headers: { "if-match": '"1"' },
  });
  assert.equal(
    (await app.call(`/records/relationships/${edge.data.id}`)).status,
    404,
  );
});
test("accounts cannot read or mutate each other’s conversations, runs or private records", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const record = (
    await app.call("/records/memory", "POST", {
      data: { content: "first-account-only" },
    })
  ).data;
  const conversation = (
    await app.call("/conversations", "POST", { title: "私有对话" })
  ).data;
  const second = randomUUID();
  app.store.run(
    "INSERT INTO admins VALUES(?,?,?,?)",
    second,
    "second",
    await passwordHash("Second-synthetic-password!"),
    now(),
  );
  await app.call("/auth/login", "POST", {
    username: "second",
    password: "Second-synthetic-password!",
  });
  assert.equal((await app.call(`/records/memory/${record.id}`)).status, 404);
  assert.equal((await app.call("/records/memory")).data.total, 0);
  assert.equal(
    (await app.call(`/conversations/${conversation.id}/messages`)).status,
    404,
  );
  assert.equal(
    (
      await app.call(`/records/memory/${record.id}`, "DELETE", undefined, {
        headers: { "if-match": '"1"' },
      })
    ).status,
    404,
  );
});
test("personality and stable identity survive restart without resetting configuration", async () => {
  const app = await fixture();
  await app.bootstrap();
  const identity = app.store.assistantId,
    state = (await app.call("/assistant/personality")).data;
  state.personality.name = "测试名字";
  assert.equal(
    (
      await app.call(
        "/assistant/personality",
        "PUT",
        { personality: state.personality },
        { headers: { "if-match": '"1"' } },
      )
    ).status,
    200,
  );
  const directory = app.dataDir;
  await app.close();
  const restarted = await fixture({ dataDir: directory });
  try {
    assert.equal(restarted.store.assistantId, identity);
    assert.equal(restarted.store.personality().personality.name, "测试名字");
    assert.equal(restarted.store.personality().revision, 2);
  } finally {
    await restarted.close();
  }
});
