import assert from "node:assert/strict";
import { test } from "node:test";
import { DatabaseSync } from "node:sqlite";
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { resolve, join } from "node:path";
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { Store } from "../dist/store.js";
import { privateDirectory, now } from "../dist/security.js";
import { fixture, until } from "./helpers.mjs";
const root = fileURLToPath(new URL("../../../", import.meta.url));
test("stopped-instance snapshot preserves the secret master key and can restore into a separate directory", async () => {
  const app = await fixture();
  await app.bootstrap();
  const secret = (
    await app.call("/secrets", "POST", {
      kind: "model_api_key",
      value: "snapshot-synthetic-key",
    })
  ).data;
  const identity = app.store.assistantId;
  await app.close();
  const report = JSON.parse(
    execFileSync(
      process.execPath,
      [
        resolve(root, "deploy/prometheus/backup-instance.mjs"),
        "--data-dir",
        app.dataDir,
        "--source-commit",
        "49c3ed46b9f6737c8913ad2eff78ee7bc20a8049",
        "--backup-root",
        resolve(root, ".local/prometheus/tests/snapshots"),
      ],
      { encoding: "utf8" },
    ),
  );
  assert.equal(report.integrity_check, "ok");
  assert.equal(existsSync(join(app.dataDir, ".prometheus.lock")), false);
  const restored = new Store(report.backup);
  try {
    assert.equal(restored.assistantId, identity);
    assert.equal(
      restored.secret(secret.id, "model_api_key"),
      "snapshot-synthetic-key",
    );
  } finally {
    restored.close();
  }
});
test("schema1 upgrade takes an integrity-checked restorable snapshot and rejects future downgrades", () => {
  const path = privateDirectory(
    resolve(root, `.local/prometheus/tests/upgrade-${randomUUID()}`),
  );
  const original = new DatabaseSync(join(path, "prometheus.db"));
  original.exec(
    readFileSync(new URL("./schema1.sql", import.meta.url), "utf8"),
  );
  original
    .prepare("INSERT INTO metadata VALUES(?,?)")
    .run("prometheus.schema", "1");
  original
    .prepare("INSERT INTO metadata VALUES(?,?)")
    .run("assistant.identity", "stable-before-upgrade");
  original.close();
  const upgraded = new Store(path);
  assert.equal(upgraded.assistantId, "stable-before-upgrade");
  assert.equal(
    upgraded.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "10",
  );
  const backup = join(
    path,
    "backups",
    readdirSync(join(path, "backups")).find((name) => name.endsWith(".db")),
  );
  assert.ok(existsSync(backup));
  const restored = new DatabaseSync(backup, { readOnly: true });
  assert.equal(
    restored.prepare("PRAGMA integrity_check").get().integrity_check,
    "ok",
  );
  assert.equal(
    restored
      .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .get().value,
    "1",
  );
  restored.close();
  upgraded.run("UPDATE metadata SET value='999' WHERE key='prometheus.schema'");
  upgraded.close();
  assert.throws(() => new Store(path), /Unsupported Prometheus schema/);
});
test("explicit private correction cancels the previous generation and suppresses its late publication", async (t) => {
  const calls = [];
  const provider = createServer(async (req, res) => {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const input = JSON.parse(raw);
    calls.push(input);
    const index = calls.length;
    await new Promise((r) => setTimeout(r, index === 1 ? 250 : 10));
    if (!res.destroyed) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          choices: [
            { message: { content: index === 1 ? "过时回答" : "更正后的回答" } },
          ],
        }),
      );
    }
  });
  await new Promise((r) => provider.listen(0, "127.0.0.1", r));
  t.after(
    () =>
      new Promise((r) => {
        provider.closeAllConnections();
        provider.close(r);
      }),
  );
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(`http://127.0.0.1:${provider.address().port}`);
  const conv = (await app.call("/conversations", "POST", {})).data;
  const first = (
    await app.call(`/conversations/${conv.id}/messages`, "POST", {
      content: "把明天的安排写成阅读。",
      client_message_id: "old",
    })
  ).data;
  await until(
    () => Promise.resolve(calls.length),
    (n) => n === 1,
  );
  const correction = (
    await app.call(`/conversations/${conv.id}/messages`, "POST", {
      content: "更正：后天阅读。",
      client_message_id: "correct",
    })
  ).data;
  const final = await until(
    () => app.call(`/runs/${correction.run_id}`),
    (v) => v.data.status === "completed",
  );
  assert.equal(final.data.response, "更正后的回答");
  assert.equal(
    (await app.call(`/runs/${first.run_id}`)).data.status,
    "cancelled",
  );
  assert.equal(
    app.store.get(
      "SELECT state FROM run_publications WHERE run_id=?",
      first.run_id,
    ).state,
    "suppressed",
  );
  assert.match(calls[1].messages[0].content, /上一条输入/);
  assert.equal(
    calls[1].messages.some((item) => item.content === "过时回答"),
    false,
  );
});
test("reviewed affinity stays scoped and never grants contact; merge leaves unrelated relationships untouched", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const people = [];
  for (const name of ["甲", "乙", "丙", "丁"])
    people.push(
      (await app.call("/records/people", "POST", { data: { name } })).data,
    );
  const edge = (
    await app.call("/records/relationships", "POST", {
      data: {
        from_id: people[2].id,
        to_id: people[3].id,
        description: "无关关系",
      },
    })
  ).data;
  assert.equal(
    (
      await app.call(
        `/relationships/people/${people[0].id}/event`,
        "POST",
        {
          delta: 5,
          reason: "合成依据",
          reviewed: false,
          event_key: "unreviewed",
          kind: "manual_adjustment",
        },
        { headers: { "if-match": '"1"' } },
      )
    ).status,
    400,
  );
  const event = await app.call(
    `/relationships/people/${people[0].id}/event`,
    "POST",
    {
      delta: 5,
      reason: "已核对的合成依据",
      reviewed: true,
      event_key: "reviewed",
      kind: "manual_adjustment",
    },
    { headers: { "if-match": '"1"' } },
  );
  assert.equal(event.data.affinity, 55);
  assert.equal(event.data.contact_permission, false);
  assert.equal(event.data.autonomous_sending, false);
  const merged = await app.call(
    `/relationships/people/${people[0].id}/merge`,
    "POST",
    {
      into_id: people[1].id,
      expected_from_revision: event.data.revision,
      expected_into_revision: 1,
      confirmed: true,
      reason: "明确核对同一人物的合成档案",
    },
  );
  assert.equal(merged.status, 200);
  assert.equal(
    (await app.call(`/records/relationships/${edge.id}`)).data.revision,
    1,
  );
  const target = (await app.call("/relationships/people")).data.items.find(
    (p) => p.id === people[1].id,
  );
  assert.equal(target.events.length, 0);
  assert.equal((await app.call("/knowledge/sources")).data.items.length, 0);
});
test("sleep context reset never resurrects a completed reply whose publication was suppressed", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const conv = (await app.call("/conversations", "POST", {})).data,
    owner = (await app.call("/auth/me")).data.user.id,
    run = randomUUID();
  app.store.run(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',0,'completed','旧候选',?,?)",
    run,
    conv.id,
    owner,
    "superseded",
    "旧输入",
    now(),
    now(),
  );
  app.store.run(
    "INSERT INTO run_targets VALUES(?,'qq',?,'old-credential',?,'123')",
    run,
    app.store.seal({
      kind: "c2c",
      target: "synthetic",
      message_id: "old",
      sender: "synthetic",
    }),
    now() + 60,
  );
  app.store.run(
    "INSERT INTO run_publications VALUES(?,'suppressed','明确更正',?)",
    run,
    now(),
  );
  app.runtime.resetConversationContexts();
  await new Promise((resolve) => setTimeout(resolve, 600));
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM delivery_parts WHERE run_id=?",
      run,
    ).n,
    0,
  );
});
