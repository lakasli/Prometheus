import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { fixture, until } from "./helpers.mjs";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const runtimeRoot = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? root);
const { qqSignature } = await import(
  pathToFileURL(resolve(runtimeRoot, "apps/server/dist/channels.js"))
);
const marker = "PROMETHEUS_STAGE_FACE_V1";
const stageList = "/stage/conversations";
const stageLink = "/stage/qq-link";
const feed = (id) => `${stageList}/${id}/feed`;

async function service(t, handler) {
  const calls = [];
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const call = {
      path: req.url,
      headers: req.headers,
      body: chunks.length ? JSON.parse(Buffer.concat(chunks)) : null,
    };
    calls.push(call);
    const value = await handler(call);
    if (!res.destroyed) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(value));
    }
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  t.after(
    () =>
      new Promise((done) => {
        server.closeAllConnections();
        server.close(done);
      }),
  );
  return { calls, url: `http://127.0.0.1:${server.address().port}` };
}

async function stageFixture(t, url) {
  const scratch = join(root, ".local/prometheus/tests", randomUUID());
  const stageDir = join(scratch, "stage");
  const stageModelDir = join(scratch, "model");
  const stageCoreDir = join(scratch, "core");
  await Promise.all(
    [stageDir, stageModelDir, stageCoreDir].map((path) =>
      mkdir(path, { recursive: true }),
    ),
  );
  await Promise.all([
    writeFile(
      join(stageDir, "index.html"),
      "<!doctype html><title>Stage</title>",
    ),
    writeFile(
      join(stageDir, "build.json"),
      await readFile(join(runtimeRoot, "apps/admin/dist/build.json")),
    ),
  ]);
  const app = await fixture({
    stageDir,
    stageModelDir,
    stageCoreDir,
    testChannelEndpoints: { qq: url, weixin: url },
  });
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(url);
  return app;
}

async function configureChannel(app, channel, config) {
  const current = (await app.call("/config")).data;
  current.config[channel] = config;
  const result = await app.call(
    "/config",
    "PUT",
    { config: current.config },
    {
      headers: { "if-match": `"${current.revision}"` },
    },
  );
  assert.equal(result.status, 200, JSON.stringify(result.data));
}

const answer = (content) => ({
  choices: [{ message: { content }, finish_reason: "stop" }],
});
const qqSecret = "synthetic-stage-qq-secret";
const qqOwner = "stage_owner";

function sendQq(
  app,
  messageId,
  content,
  { sender = qqOwner, signed = true, group = false } = {},
) {
  const body = Buffer.from(
    JSON.stringify({
      op: 0,
      id: `event_${messageId}`,
      t: group ? "GROUP_AT_MESSAGE_CREATE" : "C2C_MESSAGE_CREATE",
      d: {
        id: messageId,
        author: { user_openid: sender },
        content,
        ...(group ? { group_openid: "stage_group" } : {}),
      },
    }),
  );
  const timestamp = String(Math.floor(Date.now() / 1_000));
  return fetch(`${app.origin}/webhooks/qq`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-signature-timestamp": timestamp,
      "x-signature-ed25519": signed
        ? qqSignature(qqSecret, timestamp, body)
        : "0".repeat(128),
    },
    body,
  });
}

test("QQ stage accepts only signed private binding and presents text after confirmed delivery", async (t) => {
  const ack = Promise.withResolvers();
  t.after(() => ack.resolve());
  const reply = "已收到你的舞台消息";
  const input = "给舞台发一条消息";
  const remote = await service(t, async (call) => {
    if (call.path === "/app/getAppAccessToken")
      return { access_token: "synthetic-stage-qq-token", expires_in: 7200 };
    if (call.path.startsWith("/v2/users/")) {
      if (call.body.content === reply) await ack.promise;
      return { id: `synthetic-${call.body.msg_id}` };
    }
    const latest = call.body.messages.at(-1).content;
    return answer(
      latest === input ? `${reply}\n${marker} blush` : "已收到预热消息",
    );
  });
  const app = await stageFixture(t, remote.url);
  const credential = await app.call("/secrets", "POST", {
    kind: "qq_app_secret",
    value: qqSecret,
  });
  await configureChannel(app, "qq", {
    enabled: true,
    app_id: "123456",
    credential_ref: credential.data.id,
    transport: "webhook",
  });

  const warmup = "预热".repeat(190);
  assert.equal((await sendQq(app, "warmup_owner", warmup)).status, 200);
  assert.equal(
    (await sendQq(app, "warmup_other", warmup, { sender: "other_peer" }))
      .status,
    200,
  );
  await until(
    () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 2,
    10_000,
  );
  assert.deepEqual((await app.call(stageList)).data.items, []);
  const prelinkModels = remote.calls.filter(
    ({ path }) => path === "/chat/completions",
  );
  assert.equal(prelinkModels.length, 2);
  assert.ok(
    prelinkModels.every(({ body }) => !JSON.stringify(body).includes(marker)),
  );

  assert.equal(
    (await app.call(stageLink, "POST", {}, { auth: false })).status,
    401,
  );
  const challenge = await app.call(stageLink, "POST", {});
  assert.equal(challenge.status, 201);
  const command = challenge.data.command;
  assert.match(command, /^\/舞台绑定 [0-9a-f]{20}$/);
  assert.equal((await app.call(stageLink)).data.pending, true);
  assert.equal(
    (await sendQq(app, "forged_link", command, { signed: false })).status,
    403,
  );
  assert.equal(
    (await sendQq(app, "group_link", command, { group: true })).status,
    200,
  );
  assert.equal((await app.call(stageLink)).data.linked, false);
  assert.equal((await sendQq(app, "owner_link", command)).status, 200);
  assert.deepEqual((await app.call(stageLink)).data, {
    configured: true,
    linked: true,
    pending: false,
  });
  assert.equal(
    (await sendQq(app, "used_link", command, { sender: "other_peer" })).status,
    200,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM channel_events").n, 2);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM messages WHERE instr(content,?)>0",
      command,
    ).n,
    0,
  );

  const listing = (await app.call(stageList)).data.items;
  assert.equal(listing.length, 1);
  assert.equal(listing[0].channel, "qq");
  const selected = listing[0].id;
  const unrelated = app.store.get(
    "SELECT id FROM conversations WHERE source_key=?",
    "qq:123456:c2c:other_peer",
  );
  assert.equal((await app.call(feed(unrelated.id))).status, 404);
  assert.equal(
    (await app.call(feed(selected), "GET", undefined, { auth: false })).status,
    401,
  );

  assert.equal((await sendQq(app, "stage_turn", input)).status, 200);
  const pending = await until(
    async () =>
      (await app.call(feed(selected))).data.items.find(
        (item) => item.role === "user" && item.text === input,
      ),
    Boolean,
  );
  assert.equal(
    app.store.get("SELECT status FROM channel_events WHERE id=?", pending.id)
      .status,
    "queued",
  );
  const outbound = await until(
    () =>
      remote.calls.find(
        ({ path, body }) =>
          path.startsWith("/v2/users/") && body.content === reply,
      ),
    Boolean,
    10_000,
  );
  assert.equal(outbound.body.content, reply);
  const before = (await app.call(`${feed(selected)}?replay=1`)).data.items;
  assert.equal(before.find((item) => item.text === input).id, pending.id);
  assert.ok(before.every((item) => item.text !== reply && !item.expression));
  const prompt = remote.calls.find(
    ({ path, body }) =>
      path === "/chat/completions" && body.messages.at(-1).content === input,
  );
  assert.ok(prompt.body.messages[0].content.includes(marker));
  assert.ok(!JSON.stringify(prompt.body).includes(command));

  ack.resolve();
  const confirmed = await until(
    async () =>
      (await app.call(feed(selected))).data.items.find(
        (item) => item.text === reply,
      ),
    Boolean,
  );
  assert.equal(confirmed.role, "assistant");
  const sent = app.store.get(
    "SELECT status,content,updated_at FROM delivery_parts WHERE id=?",
    confirmed.id,
  );
  assert.deepEqual(confirmed.expression, {
    id: "blush",
    expiresAt: sent.updated_at * 1_000 + 5_000,
  });
  assert.equal(
    (await app.call(feed(selected))).data.items.find(
      (item) => item.text === input,
    ).id,
    pending.id,
  );
  assert.equal(sent.status, "sent");
  assert.equal(sent.content, reply);
  assert.equal((await app.call(stageLink, "DELETE")).status, 200);
  assert.equal((await app.call(feed(selected))).status, 404);
  assert.deepEqual((await app.call(stageList)).data.items, []);
});

test("Weixin stage follows the confirmed QR owner and excludes history after credential rotation", async (t) => {
  const bot = "stage-bot@im.bot";
  const owner = "stage-owner@im.wechat";
  const stranger = "stage-stranger@im.wechat";
  const messages = new Set();
  let base;
  let generation = 0;
  const remote = await service(t, (call) => {
    if (call.path.startsWith("/ilink/bot/get_bot_qrcode"))
      return {
        qrcode: "synthetic-stage-qr",
        qrcode_img_content: "https://example.invalid/stage-qr",
      };
    if (call.path.startsWith("/ilink/bot/get_qrcode_status")) {
      generation++;
      return {
        status: "confirmed",
        ilink_bot_id: bot,
        ilink_user_id: owner,
        bot_token: `stage-wx-token-${generation}`,
        baseurl: base,
      };
    }
    if (call.path === "/ilink/bot/getupdates") {
      const token = call.headers.authorization;
      if (messages.has(token)) return { ret: 0, msgs: [] };
      messages.add(token);
      const id = token.endsWith("-1") ? 1 : 2;
      const incoming = (sender, messageId, text) => ({
        message_id: String(messageId),
        from_user_id: sender,
        to_user_id: bot,
        message_type: 1,
        message_state: 2,
        context_token: `private-stage-context-${id}`,
        item_list: [{ type: 1, text_item: { text } }],
      });
      return {
        ret: 0,
        get_updates_buf: `private-stage-cursor-${id}`,
        msgs: [
          incoming(owner, id, `微信舞台消息 ${id}`),
          incoming(stranger, id + 10, "外部账号消息"),
        ],
      };
    }
    if (call.path === "/ilink/bot/sendmessage")
      return { ret: 0, message_id: `confirmed-${call.body.msg.client_id}` };
    const index = call.body.messages.at(-1).content.endsWith("1") ? 1 : 2;
    return answer(`微信舞台回复 ${index}\n${marker} sweat`);
  });
  base = remote.url;
  const app = await stageFixture(t, base);
  async function login(revision) {
    const job = await app.call("/channels/weixin/login", "POST", {
      expected_revision: revision,
    });
    assert.equal(job.status, 202);
    await until(
      () => app.call(`/channels/weixin/login/${job.data.id}`),
      ({ data }) => data.status === "confirmed",
    );
  }
  await login(0);
  assert.deepEqual((await app.call(stageList)).data.items, []);
  await configureChannel(app, "weixin", { enabled: true });
  const conversation = await until(
    async () => (await app.call(stageList)).data.items[0],
    Boolean,
  );
  assert.equal(conversation.channel, "weixin");
  const read = () => app.call(`${feed(conversation.id)}?replay=1`);
  async function verifyGeneration(index) {
    const result = await until(
      read,
      ({ data }) =>
        data.items?.some(({ text }) => text === `微信舞台回复 ${index}`),
      10_000,
    );
    assert.equal(result.status, 200);
    assert.deepEqual(
      result.data.items.map(({ role, text }) => [role, text]),
      [
        ["user", `微信舞台消息 ${index}`],
        ["assistant", `微信舞台回复 ${index}`],
      ],
    );
    assert.equal(result.data.items.at(-1).expression.id, "sweat");
    const model = remote.calls.find(
      ({ path, body }) =>
        path === "/chat/completions" &&
        body.messages.at(-1).content === `微信舞台消息 ${index}`,
    );
    assert.ok(model.body.messages[0].content.includes(marker));
    const outbound = remote.calls.find(
      ({ path, body }) =>
        path === "/ilink/bot/sendmessage" &&
        body.msg.item_list[0].text_item.text === `微信舞台回复 ${index}`,
    );
    assert.equal(
      outbound.headers.authorization,
      `Bearer stage-wx-token-${index}`,
    );
    const visible = JSON.stringify([conversation, result.data]);
    for (const secret of [
      owner,
      bot,
      stranger,
      "stage-wx-token",
      "private-stage-context",
      "private-stage-cursor",
      marker,
    ])
      assert.ok(!visible.includes(secret));
  }
  await verifyGeneration(1);
  await login(1);
  await verifyGeneration(2);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM channel_events WHERE channel='weixin'",
    ).n,
    2,
  );
  await configureChannel(app, "weixin", { enabled: false });
  assert.equal((await read()).status, 404);
  assert.deepEqual((await app.call(stageList)).data.items, []);
});
