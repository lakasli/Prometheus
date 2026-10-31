import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { DatabaseSync } from "node:sqlite";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
import { fixture, until } from "./helpers.mjs";
import { defaultConfig } from "../dist/config.js";
import { Store } from "../dist/store.js";
import { now, passwordHash, privateDirectory } from "../dist/security.js";
const match = (revision) => ({ headers: { "if-match": `"${revision}"` } });
async function provider(
  t,
  handler = () => ({ choices: [{ message: { content: "OK" } }] }),
) {
  const calls = [];
  const server = createServer(async (request, response) => {
    let text = "";
    for await (const chunk of request) text += chunk;
    const call = {
      path: request.url,
      body: text ? JSON.parse(text) : null,
      request,
      response,
    };
    calls.push(call);
    const result = await handler(call);
    if (result !== undefined && !response.destroyed) {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify(result));
    }
  });
  await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
  t.after(
    () =>
      new Promise((closed) => {
        server.closeAllConnections();
        server.close(closed);
      }),
  );
  return { calls, url: `http://127.0.0.1:${server.address().port}` };
}
async function draft(app, config) {
  const current = (await app.call("/config")).data;
  const result = await app.call("/config/drafts", "POST", {
    base_revision: current.revision,
    config: config ?? current.config,
  });
  assert.equal(result.status, 201);
  return result.data;
}

test("configuration drafts persist inactive fields and enforce both draft and live configuration revisions", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const initial = (await app.call("/config")).data;
  const value = structuredClone(initial.config);
  value.runtime.timeout_seconds = 45;
  const saved = await draft(app, value);
  assert.equal(saved.applied, false);
  assert.equal((await app.call("/config")).data.revision, initial.revision);
  value.runtime.timeout_seconds = 50;
  const edited = await app.call(
    `/config/drafts/${saved.id}`,
    "PUT",
    { config: value },
    match(1),
  );
  assert.equal(edited.data.revision, 2);
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}`,
        "PUT",
        { config: value },
        match(1),
      )
    ).status,
    409,
  );
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}/validate`,
        "POST",
        {},
        match(2),
      )
    ).data.valid,
    true,
  );
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}/apply`,
        "POST",
        { base_revision: initial.revision, confirmed: false },
        match(2),
      )
    ).status,
    400,
  );
  await app.call(
    "/config",
    "PUT",
    { config: initial.config },
    match(initial.revision),
  );
  const latest = (await app.call("/config")).data;
  assert.equal(
    (await app.call(`/config/drafts/${saved.id}`)).data.stale_base,
    true,
  );
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}/apply`,
        "POST",
        { base_revision: initial.revision, confirmed: true },
        match(2),
      )
    ).status,
    409,
  );
  const rebased = await app.call(
    `/config/drafts/${saved.id}/rebase`,
    "POST",
    { base_revision: latest.revision, confirmed: true },
    match(2),
  );
  assert.equal(rebased.status, 200);
  assert.equal(rebased.data.revision, 3);
  assert.equal(rebased.data.config.runtime.timeout_seconds, 50);
  const body = { base_revision: latest.revision, confirmed: true };
  const applied = await app.call(
    `/config/drafts/${saved.id}/apply`,
    "POST",
    body,
    match(3),
  );
  assert.equal(applied.status, 202);
  assert.equal(applied.data.status, "completed");
  assert.equal(applied.data.result.applied_revision, latest.revision + 1);
  const repeated = await app.call(
    `/config/drafts/${saved.id}/apply`,
    "POST",
    body,
    match(3),
  );
  assert.equal(repeated.data.id, applied.data.id);
  const current = (await app.call("/config")).data;
  assert.equal(current.config.runtime.timeout_seconds, 50);
  assert.equal(current.revision, latest.revision + 1);
  assert.equal(
    (await app.call(`/config/drafts/${saved.id}`)).data.is_current_revision,
    true,
  );
  await app.call(
    "/config",
    "PUT",
    { config: current.config },
    match(current.revision),
  );
  assert.equal(
    (await app.call(`/config/drafts/${saved.id}`)).data.is_current_revision,
    false,
  );
  assert.equal(
    (await app.call(`/config/drafts/${saved.id}/apply`, "POST", body, match(3)))
      .data.id,
    applied.data.id,
  );
  assert.equal((await app.call("/config")).data.revision, current.revision + 1);
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}`,
        "PUT",
        { config: value },
        match(4),
      )
    ).status,
    400,
  );
});

test("drafts preserve unfinished fields without accepting inline credentials, unsafe transports or enabling gated execution", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const value = defaultConfig();
  value.model.base_url = "";
  value.model.model = "";
  value.runtime.max_concurrent_runs = 999;
  const saved = await draft(app, value);
  const checked = await app.call(
    `/config/drafts/${saved.id}/validate`,
    "POST",
    {},
    match(saved.revision),
  );
  assert.equal(checked.status, 200);
  assert.equal(checked.data.valid, false);
  assert.equal(
    (await app.call("/config")).data.config.runtime.max_concurrent_runs,
    2,
  );
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}/apply`,
        "POST",
        { base_revision: saved.base_revision, confirmed: true },
        match(saved.revision),
      )
    ).status,
    400,
  );
  for (const change of [
    (config) => {
      config.model.api_key = "synthetic-inline-secret";
    },
    (config) => {
      config.model.base_url = "https://user:synthetic-password@example.com";
    },
    (config) => {
      config.model.credential_ref = "synthetic-inline-secret";
    },
    (config) => {
      config.qq.transport = "shell";
    },
  ]) {
    const config = defaultConfig();
    change(config);
    assert.equal(
      (await app.call("/config/drafts", "POST", { base_revision: 1, config }))
        .status,
      400,
    );
  }
  const speech = defaultConfig();
  speech.speech.enabled = true;
  const gated = await draft(app, speech);
  assert.equal(
    (
      await app.call(
        `/config/drafts/${gated.id}/validate`,
        "POST",
        {},
        match(1),
      )
    ).data.valid,
    false,
  );
  assert.equal(
    (
      await app.call(
        `/config/drafts/${gated.id}/apply`,
        "POST",
        { base_revision: 1, confirmed: true },
        match(1),
      )
    ).status,
    503,
  );
  assert.equal((await app.call("/config")).data.config.speech.enabled, false);
  await app.close();
  const restarted = await fixture({ dataDir: app.dataDir });
  t.after(() => restarted.close());
  await restarted.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  const restored = await restarted.call(`/config/drafts/${saved.id}`);
  assert.equal(restored.data.config.model.base_url, "");
  assert.equal(restored.data.config.runtime.max_concurrent_runs, 999);
});

test("connection operations are persistent, scoped, secret-free and do not apply a checked draft", async (t) => {
  const remote = await provider(t),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const current = (await app.call("/config")).data;
  const value = structuredClone(current.config);
  value.runtime.timeout_seconds = 40;
  const saved = await draft(app, value);
  const started = await app.call(
    `/config/drafts/${saved.id}/checks`,
    "POST",
    {},
    match(1),
  );
  assert.equal(started.status, 202);
  const done = await until(
    () => app.call(`/config/operations/${started.data.id}`),
    (response) => response.data.status === "completed",
  );
  assert.equal(done.data.result.ok, true);
  assert.equal((await app.call("/config")).data.revision, current.revision);
  assert.equal(done.data.config_json, undefined);
  assert.equal(
    JSON.stringify(done.data).includes("synthetic-provider-credential"),
    false,
  );
  assert.equal(
    app.store.get(
      "SELECT config_json FROM config_operations WHERE id=?",
      started.data.id,
    ).config_json,
    null,
  );
  assert.equal(
    (await app.call("/config/drafts", "GET", undefined, { auth: false }))
      .status,
    401,
  );
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}/checks`,
        "POST",
        {},
        { headers: { "if-match": '"1"', "x-csrf-token": "wrong" } },
      )
    ).status,
    403,
  );
  app.store.run(
    "INSERT INTO admins VALUES(?,?,?,?)",
    randomUUID(),
    "other-config-admin",
    await passwordHash("Other-synthetic-password-2026!"),
    now(),
  );
  await app.call("/auth/login", "POST", {
    username: "other-config-admin",
    password: "Other-synthetic-password-2026!",
  });
  assert.equal((await app.call(`/config/drafts/${saved.id}`)).status, 404);
  assert.equal(
    (await app.call(`/config/operations/${started.data.id}`)).status,
    404,
  );
  assert.equal(
    (
      await app.call(
        `/config/drafts/${saved.id}/apply`,
        "POST",
        { base_revision: saved.base_revision, confirmed: true },
        match(1),
      )
    ).status,
    404,
  );
});

test("editing a checked draft retains its physical lease and credential reference until cancellation closes", async (t) => {
  let arrived = false;
  const remote = await provider(t, () => {
    arrived = true;
    return undefined;
  });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const owner = (await app.call("/auth/me")).data.user.id;
  const key = (
    await app.call("/secrets", "POST", {
      kind: "model_api_key",
      value: "draft-only-synthetic-key",
    })
  ).data;
  const value = (await app.call("/config")).data.config;
  value.model.credential_ref = key.id;
  const saved = await draft(app, value);
  const started = await app.call(
    `/config/drafts/${saved.id}/checks`,
    "POST",
    {},
    match(1),
  );
  await until(() => Promise.resolve(arrived), Boolean);
  assert.equal(
    (await app.call("/config/check", "POST", { config: value })).status,
    429,
  );
  value.model.credential_ref = null;
  app.configurations.save(owner, { config: value }, saved.id, 1);
  app.configurations.remove(owner, saved.id, 2);
  assert.equal(app.runtime.runningCount, 1);
  assert.throws(
    () => app.store.removeSecret(key.id, owner),
    (error) => error.code === "credential_in_use",
  );
  await until(
    () => Promise.resolve(app.runtime.runningCount),
    (count) => count === 0,
  );
  const operation = (await app.call(`/config/operations/${started.data.id}`))
    .data;
  assert.equal(operation.status, "invalidated");
  assert.equal(operation.result.ok, false);
  assert.equal((await app.call(`/secrets/${key.id}`, "DELETE")).status, 200);
});

test("draft QQ checks obtain a token without activating the account, dispatching messages or exposing response secrets", async (t) => {
  let reject = false;
  const remote = await provider(t, (call) =>
    call.path === "/app/getAppAccessToken"
      ? reject
        ? { code: 1, message: "ephemeral-qq-secret-not-for-storage" }
        : {
            access_token: "ephemeral-qq-secret-not-for-storage",
            expires_in: 7200,
          }
      : { choices: [{ message: { content: "OK" } }] },
  );
  const app = await fixture({
    testChannelEndpoints: { qq: remote.url, weixin: remote.url },
  });
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const key = (
    await app.call("/secrets", "POST", {
      kind: "qq_app_secret",
      value: "draft-qq-app-secret",
    })
  ).data;
  const value = (await app.call("/config")).data.config;
  value.qq = {
    enabled: true,
    app_id: "123456",
    credential_ref: key.id,
    transport: "webhook",
  };
  const saved = await draft(app, value);
  const started = await app.call(
    `/config/drafts/${saved.id}/checks`,
    "POST",
    {},
    match(1),
  );
  const done = await until(
    () => app.call(`/config/operations/${started.data.id}`),
    (response) => response.data.status === "completed",
  );
  assert.deepEqual(
    done.data.result.checks.map((item) => item.name),
    ["model", "qq"],
  );
  assert.equal((await app.call("/config")).data.config.qq.enabled, false);
  assert.equal(
    remote.calls.filter((call) => call.path.startsWith("/v2/")).length,
    0,
  );
  assert.equal(
    JSON.stringify(done.data).includes("ephemeral-qq-secret-not-for-storage"),
    false,
  );
  reject = true;
  const failed = await app.call(
    `/config/drafts/${saved.id}/checks`,
    "POST",
    {},
    match(1),
  );
  const result = await until(
    () => app.call(`/config/operations/${failed.data.id}`),
    (response) => response.data.status === "failed",
  );
  assert.equal(
    JSON.stringify(result.data).includes("ephemeral-qq-secret-not-for-storage"),
    false,
  );
});

test("shutdown interrupts background configuration checks and restart never replays them", async (t) => {
  const remote = await provider(t, () => undefined),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(remote.url);
  const current = (await app.call("/config")).data;
  current.config.runtime.max_concurrent_runs = 1;
  await app.call(
    "/config",
    "PUT",
    { config: current.config },
    match(current.revision),
  );
  const saved = await draft(app);
  const started = await app.call(
    `/config/drafts/${saved.id}/checks`,
    "POST",
    {},
    match(1),
  );
  await until(
    () => Promise.resolve(remote.calls.length),
    (count) => count === 1,
  );
  const conversation = (
    await app.call("/conversations", "POST", {
      title: "queued across check shutdown",
    })
  ).data;
  const queued = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "synthetic pending input",
      client_message_id: "pending-on-shutdown",
    })
  ).data;
  assert.equal(
    (await app.call(`/runs/${queued.run_id}`)).data.status,
    "queued",
  );
  await app.close();
  assert.equal(app.runtime.runningCount, 0);
  assert.equal(
    remote.calls.length,
    1,
    "closing a check must not dispatch the queued conversation",
  );
  const restarted = await fixture({ dataDir: app.dataDir });
  t.after(() => restarted.close());
  await restarted.call("/auth/login", "POST", {
    username: "tester",
    password: "Synthetic-password-2026!",
  });
  assert.equal(
    (await restarted.call(`/config/operations/${started.data.id}`)).data.status,
    "interrupted",
  );
  // The ordinary Native queue resumes on restart; the interrupted check does not.
  await until(
    () => Promise.resolve(remote.calls.length),
    (count) => count === 2,
  );
  assert.equal(
    remote.calls.filter(
      (call) =>
        call.body.messages.length === 1 &&
        call.body.messages[0].content === "请只回复 OK。",
    ).length,
    1,
  );
});

test("the direct form checker also protects pending credentials and persists its cancellation receipt", async (t) => {
  const remote = await provider(t, () => undefined),
    app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const key = (
    await app.call("/secrets", "POST", {
      kind: "model_api_key",
      value: "legacy-check-only-synthetic-key",
    })
  ).data;
  const value = (await app.call("/config")).data.config;
  value.model.base_url = remote.url;
  value.model.credential_ref = key.id;
  const pending = app.call("/config/check", "POST", { config: value });
  await until(
    () => Promise.resolve(remote.calls.length),
    (count) => count === 1,
  );
  const owner = (await app.call("/auth/me")).data.user.id;
  assert.throws(
    () => app.store.removeSecret(key.id, owner),
    (error) => error.code === "credential_in_use",
  );
  const operation = (await app.call("/config/operations")).data.items[0];
  assert.equal(operation.status, "running");
  assert.equal(
    (await app.call(`/config/operations/${operation.id}`, "DELETE")).status,
    200,
  );
  assert.equal((await pending).status, 408);
  const receipt = (await app.call(`/config/operations/${operation.id}`)).data;
  assert.equal(receipt.status, "cancelled");
  assert.equal(app.runtime.runningCount, 0);
  assert.equal((await app.call(`/secrets/${key.id}`, "DELETE")).status, 200);
});

test("schema3 upgrade preserves pre-existing inactive drafts and snapshots the old schema", () => {
  const root = fileURLToPath(new URL("../../../", import.meta.url));
  const directory = privateDirectory(
    resolve(root, `.local/prometheus/tests/config-upgrade-${randomUUID()}`),
  );
  const old = new DatabaseSync(join(directory, "prometheus.db"));
  old.exec(readFileSync(new URL("./schema3.sql", import.meta.url), "utf8"));
  old.prepare("INSERT INTO metadata VALUES('prometheus.schema','3')").run();
  old
    .prepare(
      "INSERT INTO metadata VALUES('assistant.identity','draft-upgrade-identity')",
    )
    .run();
  const owner = randomUUID(),
    id = randomUUID(),
    timestamp = now() - 100;
  old
    .prepare(
      "INSERT INTO admins VALUES(?,'previous-draft-owner','synthetic-not-used-for-login',?)",
    )
    .run(owner, timestamp);
  old
    .prepare("INSERT INTO config_drafts VALUES(?,?,1,?,?)")
    .run(id, owner, JSON.stringify(defaultConfig()), timestamp);
  old.close();
  const upgraded = new Store(directory);
  assert.equal(
    upgraded.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "19",
  );
  const saved = upgraded.get("SELECT * FROM config_drafts WHERE id=?", id);
  assert.equal(saved.revision, 1);
  assert.equal(saved.updated_at, timestamp);
  assert.equal(saved.state, "draft");
  upgraded.close();
  const snapshot = new DatabaseSync(
    join(
      directory,
      "backups",
      readdirSync(join(directory, "backups")).find((name) =>
        name.startsWith("schema3-"),
      ),
    ),
    { readOnly: true },
  );
  assert.equal(
    snapshot
      .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .get().value,
    "3",
  );
  snapshot.close();
});
