import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer, get as requestGet } from "node:http";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { fixture, until } from "./helpers.mjs";

const conversations = "/stage/conversations";
const marker = "PROMETHEUS_STAGE_FACE_V1";
const feedPath = (id) => `${conversations}/${id}/feed`;

async function stageFixture(t) {
  const root = fileURLToPath(new URL("../../../", import.meta.url));
  const runtime = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? root);
  const scratch = join(root, ".local/prometheus/tests", randomUUID());
  const stageDir = join(scratch, "stage");
  const stageModelDir = join(scratch, "model");
  const stageCoreDir = join(scratch, "core");
  await Promise.all(
    [stageDir, stageModelDir, stageCoreDir].map((dir) =>
      mkdir(dir, { recursive: true }),
    ),
  );
  await Promise.all([
    writeFile(
      join(stageDir, "index.html"),
      "<!doctype html><title>Stage</title>",
    ),
    writeFile(
      join(stageDir, "build.json"),
      await readFile(join(runtime, "apps/admin/dist/build.json")),
    ),
  ]);
  const app = await fixture({ stageDir, stageModelDir, stageCoreDir });
  t.after(() => app.close());
  return app;
}

test("stage feed requires direct loopback authentication and a native conversation owned by the session", async (t) => {
  const app = await stageFixture(t);
  assert.equal((await app.call(conversations)).status, 401);
  const { data } = await app.bootstrap();
  const owner = data.user.id;
  const own = app.runtime.createConversation(owner, "私有会话");
  const foreign = app.runtime.createConversation(randomUUID(), "其他账号");
  const channel = app.runtime.createConversation(owner, "渠道会话");
  app.store.run(
    "UPDATE conversations SET source_key=? WHERE id=?",
    "qq:synthetic:c2c:peer",
    channel.id,
  );

  const result = await app.call(conversations);
  assert.equal(result.status, 200);
  assert.equal(result.headers.get("cache-control"), "no-store");
  assert.deepEqual(result.data, { items: [{ id: own.id, title: own.title }] });
  assert.equal(
    (await app.call(feedPath(own.id), "GET", undefined, { auth: false }))
      .status,
    401,
  );
  assert.equal((await app.call(feedPath(own.id))).status, 200);
  assert.equal((await app.call(feedPath(foreign.id))).status, 404);
  assert.equal((await app.call(feedPath(channel.id))).status, 404);

  for (const [headers, expected] of [
    [{ host: "public.example" }, 403],
    [{ forwarded: "for=203.0.113.1" }, 404],
    [{ "x-forwarded-host": "public.example" }, 404],
    [{ "sec-fetch-site": "cross-site" }, 403],
  ]) {
    const status = await new Promise((resolve, reject) => {
      requestGet(
        new URL(`/api/admin/v1${feedPath(own.id)}`, app.origin),
        { headers: { cookie: app.cookie, origin: app.origin, ...headers } },
        (response) => {
          response.resume();
          response.once("end", () => resolve(response.statusCode));
        },
      ).once("error", reject);
    });
    assert.equal(status, expected, JSON.stringify(headers));
  }
});

test("stage displays recent native replies and only replays live actions", async (t) => {
  const app = await stageFixture(t);
  const requests = [];
  const turns = ["第一句", "第二句", "第三句", "第四句", "第五句"];
  const longReply = "🙂".repeat(401);
  const reply = (text) => (text === "第三句" ? longReply : `答复：${text}`);
  const model = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks));
    requests.push(input);
    const user = input.messages.at(-1).content;
    const content =
      user === "第五句" ? `${reply(user)}\n${marker} blush` : reply(user);
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ choices: [{ message: { content } }] }));
  });
  await new Promise((resolve) => model.listen(0, "127.0.0.1", resolve));
  t.after(
    () =>
      new Promise((resolve) => {
        model.closeAllConnections();
        model.close(resolve);
      }),
  );
  const { data } = await app.bootstrap();
  await app.configure(`http://127.0.0.1:${model.address().port}`);
  const own = app.runtime.createConversation(data.user.id, "表情会话");
  const messages = `/conversations/${own.id}/messages`;
  for (const text of turns) {
    const queued = await app.call(messages, "POST", {
      content: text,
      client_message_id: randomUUID(),
    });
    assert.equal(queued.status, 202);
    const run = await until(
      () => app.call(`/runs/${queued.data.run_id}`),
      ({ data }) => !["queued", "running"].includes(data.status),
    );
    assert.equal(run.data.status, "completed");
    assert.equal(run.data.response, reply(text));
  }
  assert.ok(
    requests
      .at(-1)
      .messages.some(
        ({ role, content }) => role === "system" && content.includes(marker),
      ),
  );
  const stored = (await app.call(messages)).data.items;
  assert.deepEqual(
    stored.map(({ content }) => content),
    turns.flatMap((text) => [text, reply(text)]),
  );
  const captions = stored.map(({ id, role, content, created_at }) => ({
    id,
    role,
    text: content === longReply ? `${"🙂".repeat(400)}…` : content,
    createdAt: created_at * 1_000,
    expiresAt: created_at * 1_000 + 12_000,
  }));
  const live = await app.call(feedPath(own.id));
  assert.equal(live.status, 200);
  assert.equal(live.headers.get("cache-control"), "no-store");
  assert.ok(Number.isSafeInteger(live.data.now));
  const { expression, ...last } = live.data.items.at(-1);
  assert.deepEqual([...live.data.items.slice(0, -1), last], captions.slice(-8));
  assert.equal(expression.id, "blush");
  assert.ok(expression.expiresAt > live.data.now);
  assert.ok(expression.expiresAt - live.data.now <= 5_000);

  const replay = () => app.call(`${feedPath(own.id)}?replay=1`);
  const replayCaptions = (now) =>
    captions
      .slice(-4)
      .map((caption) => ({ ...caption, expiresAt: now + 12_000 }));
  const recentReplay = await replay();
  assert.equal(recentReplay.status, 200);
  const replayed = replayCaptions(recentReplay.data.now);
  replayed.at(-1).expression = expression;
  assert.deepEqual(recentReplay.data.items, replayed);

  t.mock.timers.enable({ apis: ["Date"], now: expression.expiresAt + 1 });
  assert.ok(
    (await app.call(feedPath(own.id))).data.items.every(
      (item) => !item.expression,
    ),
  );
  const expiredReplay = await replay();
  assert.deepEqual(
    expiredReplay.data.items,
    replayCaptions(expiredReplay.data.now),
  );
  t.mock.timers.setTime(captions.at(-1).expiresAt + 1);
  assert.deepEqual((await app.call(feedPath(own.id))).data.items, []);
  const oldReplay = await replay();
  assert.deepEqual(oldReplay.data.items, replayCaptions(oldReplay.data.now));
  t.mock.timers.reset();
});
