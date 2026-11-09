import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { test } from "node:test";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { fixture, until } from "./helpers.mjs";

async function provider(t, handle) {
  const requests = [],
    server = createServer(async (req, res) => {
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      const data = JSON.parse(Buffer.concat(chunks));
      requests.push(data);
      await handle(data, res, req);
    });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(
    () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(resolve);
      }),
  );
  return { url: `http://127.0.0.1:${server.address().port}`, requests };
}
const respond = (res, content) => {
  if (!res.destroyed) {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ choices: [{ message: { content } }] }));
  }
};

test("channel expression requires a current grant and remains separate from the stored reply", async (t) => {
  const marker = "PROMETHEUS_STAGE_FACE_V1";
  let releaseReply;
  const held = new Promise((resolve) => {
    releaseReply = resolve;
  });
  const model = await provider(t, async (data, res) => {
    const input = data.messages.at(-1).content;
    if (input === "revoked") await held;
    respond(res, `reply:${input}\n${marker} blush`);
  });

  const sourceRoot = fileURLToPath(new URL("../../../", import.meta.url));
  const runtimeRoot = resolve(
    process.env.PROMETHEUS_RELEASE_ROOT ?? sourceRoot,
  );
  const runtimeModule = (name) =>
    import(pathToFileURL(resolve(runtimeRoot, `apps/server/dist/${name}.js`)));
  const [{ Runtime }, { Store }] = await Promise.all([
    runtimeModule("runtime"),
    runtimeModule("store"),
  ]);
  const store = new Store(
    resolve(sourceRoot, ".local/prometheus/tests", randomUUID()),
  );
  const owner = "qq:stage-fixture:c2c:peer";
  const config = store.config();
  config.config.model = {
    base_url: model.url,
    model: "synthetic-model",
    credential_ref: store.addSecret(
      "model_api_key",
      "synthetic-provider",
      owner,
    ).id,
  };
  store.applyConfig(config.revision, config.config, owner);
  const grants = new Map();
  const actions = [];
  const runtime = new Runtime(
    store,
    true,
    (id, action, grant) =>
      actions.push([id, action, grant, runtime.getRun(id, owner).status]),
    (id) => grants.get(id),
  );
  t.after(async () => {
    releaseReply();
    await runtime.stop();
    store.close();
  });
  await runtime.start();
  const conversation = runtime.createConversation(owner, "合成私聊");
  store.run(
    "UPDATE conversations SET source_key=? WHERE id=?",
    owner,
    conversation.id,
  );

  function queue(input, grant, target = true) {
    const { run_id: id } = runtime.enqueue(
      conversation.id,
      owner,
      input,
      randomUUID(),
      undefined,
      {
        deferDispatch: true,
      },
    );
    if (target)
      store.run(
        "INSERT INTO run_targets VALUES(?,?,?,?,?,?)",
        id,
        "qq",
        store.seal({ kind: "c2c" }),
        "synthetic",
        Math.floor(Date.now() / 1_000) + 120,
        "stage-fixture",
      );
    if (grant) grants.set(id, grant);
    return id;
  }
  const response = (id) =>
    until(
      () => runtime.getRun(id, owner),
      (run) => run.status === "completed",
    );
  const offered = () =>
    model.requests.at(-1).messages[0].content.includes(marker);

  const authorized = queue("authorized", "proof-1");
  assert.equal((await response(authorized)).response, "reply:authorized");
  assert.equal(offered(), true);
  assert.deepEqual(actions, [[authorized, "blush", "proof-1", "completed"]]);

  const revoked = queue("revoked", "proof-1");
  await until(
    () => model.requests.length,
    (length) => length === 2,
  );
  assert.equal(offered(), true);
  grants.set(revoked, "proof-2");
  releaseReply();
  assert.equal((await response(revoked)).response, "reply:revoked");

  assert.equal(
    (await response(queue("ungranted"))).response,
    "reply:ungranted",
  );
  assert.equal(offered(), false);
  assert.equal(
    (await response(queue("no-target", "proof-1", false))).response,
    "reply:no-target",
  );
  assert.equal(offered(), false);
  assert.deepEqual(actions, [[authorized, "blush", "proof-1", "completed"]]);
});

test("native model runs persist ordered conversation history, explicit skills and idempotent submissions", async (t) => {
  const model = await provider(t, async (data, res) =>
    respond(res, `reply:${data.messages.at(-1).content}`),
  );
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(model.url);
  const skill = (
    await app.call("/records/skills", "POST", {
      data: {
        name: "简明回答",
        instructions: "synthetic-explicit-skill",
        enabled: true,
      },
    })
  ).data;
  const conversation = (
    await app.call("/conversations", "POST", { title: "model integration" })
  ).data;
  const input = {
    content: "first-turn",
    client_message_id: "client-1",
    skill_id: skill.id,
  };
  const first = await app.call(
    `/conversations/${conversation.id}/messages`,
    "POST",
    input,
  );
  assert.equal(first.status, 202);
  assert.equal(
    (
      await app.call(
        `/conversations/${conversation.id}/messages`,
        "POST",
        input,
      )
    ).data.run_id,
    first.data.run_id,
  );
  assert.equal(
    (
      await app.call(`/conversations/${conversation.id}/messages`, "POST", {
        ...input,
        content: "changed",
      })
    ).status,
    409,
  );
  const result = await until(
    () => app.call(`/runs/${first.data.run_id}`),
    (value) => value.data.status === "completed",
  );
  assert.equal(result.data.response, "reply:first-turn");
  assert.match(
    model.requests[0].messages[0].content,
    /synthetic-explicit-skill/,
  );
  const second = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "second-turn",
      client_message_id: "client-2",
    })
  ).data;
  await until(
    () => app.call(`/runs/${second.run_id}`),
    (value) => value.data.status === "completed",
  );
  assert.deepEqual(
    model.requests[1].messages.slice(1).map((item) => item.content),
    ["first-turn", "reply:first-turn", "second-turn"],
  );
  assert.equal(
    (await app.call(`/conversations/${conversation.id}/messages`)).data.items
      .length,
    4,
  );
});
test("cancellation retains actual request slots, limits concurrency and prevents stale publication", async (t) => {
  let active = 0,
    peak = 0;
  const model = await provider(t, async (_data, res) => {
    active++;
    peak = Math.max(peak, active);
    await new Promise((resolve) => setTimeout(resolve, 250));
    active--;
    respond(res, "delayed-result");
  });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(model.url);
  const conversations = await Promise.all(
    [1, 2, 3].map((index) =>
      app.call("/conversations", "POST", { title: `concurrency-${index}` }),
    ),
  );
  const runs = await Promise.all(
    conversations.map((value, index) =>
      app.call(`/conversations/${value.data.id}/messages`, "POST", {
        content: `request-${index}`,
        client_message_id: `parallel-${index}`,
      }),
    ),
  );
  await until(
    async () => model.requests.length,
    (value) => value >= 2,
  );
  assert.equal(app.runtime.runningCount, 2);
  const key = runs[0].data.run_id;
  assert.equal(
    (await app.call(`/runs/${key}`, "DELETE")).data.status,
    "cancelled",
  );
  await until(
    () => app.call(`/runs/${runs[2].data.run_id}`),
    (value) => value.data.status === "completed",
  );
  assert.equal((await app.call(`/runs/${key}`)).data.response, null);
  // Server-side work may continue after socket cancellation; client leases are what is bounded.
  assert.ok(peak >= 2);
  assert.ok(app.runtime.runningCount <= 2);
});
test("forgetting a source cancels an in-flight model and rejects its late result", async (t) => {
  const model = await provider(t, async (_data, res) => {
    await new Promise((resolve) => setTimeout(resolve, 300));
    respond(res, "must-never-publish");
  });
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(model.url);
  const memory = (
    await app.call("/records/memory", "POST", {
      data: { content: "source-must-be-forgotten" },
    })
  ).data;
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const run = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "test-forgetting",
      client_message_id: "forget-test",
    })
  ).data;
  await until(
    async () => model.requests.length,
    (value) => value === 1,
  );
  assert.match(
    model.requests[0].messages[0].content,
    /source-must-be-forgotten/,
  );
  await app.call(`/records/memory/${memory.id}`, "DELETE", undefined, {
    headers: { "if-match": '"1"' },
  });
  await until(
    async () => app.runtime.runningCount,
    (value) => value === 0,
  );
  const result = (await app.call(`/runs/${run.run_id}`)).data;
  assert.equal(result.status, "cancelled");
  assert.equal(result.response, null);
});
test("shutdown interrupts active work and leaves no live request lease", async (t) => {
  const model = await provider(t, async (_data, res) => {
    await new Promise((resolve) => setTimeout(resolve, 1000));
    respond(res, "late");
  });
  const app = await fixture();
  await app.bootstrap();
  await app.configure(model.url);
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const run = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "shutdown",
      client_message_id: "shutdown",
    })
  ).data;
  await until(
    async () => model.requests.length,
    (value) => value === 1,
  );
  await app.close();
  assert.equal(app.runtime.runningCount, 0);
  const restarted = await fixture({ dataDir: app.dataDir });
  t.after(() => restarted.close());
  assert.equal(
    restarted.store.get("SELECT status FROM runs WHERE id=?", run.run_id)
      .status,
    "interrupted",
  );
});
