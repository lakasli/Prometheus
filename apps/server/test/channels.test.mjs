import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { WebSocketServer } from "ws";
import { randomUUID } from "node:crypto";
import { fixture, until } from "./helpers.mjs";
import { qqSignature } from "../dist/channels.js";
import { now } from "../dist/security.js";

async function service(t, handler) {
  const calls = [];
  const server = createServer(async (req, res) => {
    let text = "";
    for await (const chunk of req) text += chunk;
    const call = {
      path: req.url,
      method: req.method,
      body: text ? JSON.parse(text) : null,
      headers: req.headers,
    };
    calls.push(call);
    const value = await handler(call, res);
    if (value !== undefined && !res.headersSent) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(value));
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(
    () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(resolve);
      }),
  );
  return { server, calls, url: `http://127.0.0.1:${server.address().port}` };
}
async function qqConfig(app, base, transport = "webhook") {
  await app.configure(base);
  const key = (
    await app.call("/secrets", "POST", {
      kind: "qq_app_secret",
      value: "synthetic-qq-secret",
    })
  ).data;
  const current = (await app.call("/config")).data;
  current.config.qq = {
    enabled: true,
    app_id: "123456",
    credential_ref: key.id,
    transport,
  };
  assert.equal(
    (
      await app.call(
        "/config",
        "PUT",
        { config: current.config },
        { headers: { "if-match": `"${current.revision}"` } },
      )
    ).status,
    200,
  );
}
test("every catalog entry builds a bounded dry-run without platform dispatch and rejects unlisted fields", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const catalog = (await app.call("/qq/api/catalog")).data;
  assert.equal(catalog.operations.length, 77);
  for (const operation of catalog.operations) {
    const parameters = Object.fromEntries(
        operation.parameters.map((key) => [key, "synthetic_1"]),
      ),
      query = Object.fromEntries(
        (operation.required_query_fields ?? []).map((key) => [key, "1"]),
      ),
      body = Object.fromEntries(
        (operation.required_body_fields ?? []).map((key) => [key, "synthetic"]),
      );
    const result = await app.call(
      `/qq/api/operations/${operation.id}`,
      "POST",
      { parameters, query, body, dry_run: true },
    );
    assert.equal(result.status, 200, operation.id);
    assert.equal(result.data.dispatched, false);
    assert.equal(result.data.platform_verified, false);
    assert.equal(
      (
        await app.call(`/qq/api/operations/${operation.id}`, "POST", {
          parameters: { ...parameters, unexpected: "x" },
          query,
          body,
          dry_run: true,
        })
      ).status,
      400,
    );
  }
  assert.equal(app.store.get("SELECT count(*) AS n FROM operator_calls").n, 0);
});
const sample = (message = "msg1", content = "合成渠道文字") => ({
  op: 0,
  id: `event_${message}`,
  t: "C2C_MESSAGE_CREATE",
  d: { id: message, author: { user_openid: "sender1" }, content },
});
async function post(app, payload, valid = true) {
  const raw = JSON.stringify(payload),
    timestamp = String(now());
  return fetch(`${app.origin}/webhooks/qq`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-signature-timestamp": timestamp,
      "x-signature-ed25519": valid
        ? qqSignature("synthetic-qq-secret", timestamp, Buffer.from(raw))
        : "0".repeat(128),
    },
    body: raw,
  });
}
test("signed QQ webhook persists, deduplicates, batches texts and records only confirmed delivery parts", async (t) => {
  const remote = await service(t, (call) =>
    call.path === "/app/getAppAccessToken"
      ? { access_token: "synthetic-qq-token", expires_in: 7200 }
      : call.path.startsWith("/v2/")
        ? { id: "platform-confirmed" }
        : { choices: [{ message: { content: "合成回复。" } }] },
  );
  const app = await fixture({
    testChannelEndpoints: { qq: remote.url, weixin: remote.url },
  });
  t.after(() => app.close());
  await app.bootstrap();
  await qqConfig(app, remote.url);
  assert.equal((await post(app, sample(), false)).status, 403);
  assert.equal((await post(app, sample())).status, 200);
  assert.equal((await post(app, sample())).status, 200);
  assert.equal((await post(app, sample("msg2", "第二句"))).status, 200);
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (value) => value === 1,
    9000,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 1);
  assert.match(
    String(app.store.get("SELECT input FROM runs").input),
    /合成渠道文字\n第二句/,
  );
  assert.equal(
    remote.calls.filter((call) => call.path.startsWith("/v2/users/")).length,
    1,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM channel_events").n, 2);
  assert.equal(
    (await app.call("/channels/status")).data.connection.qq,
    "webhook",
  );
});
for (const revoke of ["deadline", "grant", "sleep"])
  test(`QQ rechecks ${revoke} after token acquisition before dispatching a passive reply`, async (t) => {
    let tokenStarted = false,
      releaseToken;
    const remote = await service(t, async (call) => {
      if (call.path === "/app/getAppAccessToken") {
        tokenStarted = true;
        await new Promise((resolve) => {
          releaseToken = resolve;
        });
        return { access_token: "synthetic-delayed-token", expires_in: 7200 };
      }
      return { id: "synthetic-after-wake" };
    });
    const app = await fixture({
      testChannelEndpoints: { qq: remote.url, weixin: remote.url },
    });
    t.after(() => releaseToken?.());
    t.after(() => app.close());
    await app.bootstrap();
    await qqConfig(app, remote.url);
    const owner = "qq:123456:c2c:revocation-test",
      conversation = app.runtime.createConversation(owner, "待发回复复核");
    app.store.run(
      "UPDATE conversations SET source_key=? WHERE id=?",
      owner,
      conversation.id,
    );
    const expiration = now() + 2;
    if (revoke === "grant") {
      const actor = (await app.call("/auth/me")).data.user.id;
      app.features.knowledge.setGrant(actor, owner, 0, {
        capture: true,
        allow_model: true,
        can_sleep: false,
        can_wake: false,
        expires_at: expiration,
      });
      await app.features.knowledge.stop();
    }
    const config = app.store.config().config;
    const run = randomUUID(),
      epoch = app.store.get(
        "SELECT context_epoch FROM conversations WHERE id=?",
        conversation.id,
      ).context_epoch;
    app.store.run(
      "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,?,?,'{}',?,'completed','不得过期发送',?,?)",
      run,
      conversation.id,
      owner,
      randomUUID(),
      "source",
      epoch,
      now(),
      now(),
    );
    app.store.run(
      "INSERT INTO run_targets VALUES(?,'qq',?,?,?,?)",
      run,
      app.store.seal({
        kind: "c2c",
        target: "synthetic",
        message_id: "source",
        sender: "synthetic",
      }),
      config.qq.credential_ref,
      revoke === "deadline" ? expiration : now() + 60,
      config.qq.app_id,
    );
    await until(() => Promise.resolve(tokenStarted), Boolean);
    if (revoke === "sleep") {
      const actor = (await app.call("/auth/me")).data.user.id;
      app.features.automation.restControl(
        actor,
        { action: "sleep", duration_seconds: 60, request_id: randomUUID() },
        Number(app.features.automation.restStatus().state.revision),
      );
      await until(
        () => Promise.resolve(app.features.automation.restStatus().state.phase),
        (phase) => phase === "asleep",
      );
      releaseToken();
      await until(
        () =>
          Promise.resolve(
            app.store.get(
              "SELECT status FROM delivery_parts WHERE run_id=?",
              run,
            )?.status,
          ),
        (status) => status === "queued",
      );
      assert.equal(
        remote.calls.filter((call) => call.path.startsWith("/v2/")).length,
        0,
      );
      app.features.automation.restControl(
        actor,
        { action: "wake", request_id: randomUUID() },
        Number(app.features.automation.restStatus().state.revision),
      );
      await until(
        () =>
          Promise.resolve(
            app.store.get(
              "SELECT status FROM delivery_parts WHERE run_id=?",
              run,
            )?.status,
          ),
        (status) => status === "sent",
      );
      assert.equal(
        remote.calls.filter((call) => call.path.startsWith("/v2/")).length,
        1,
      );
      return;
    }
    await until(
      () => Promise.resolve(now()),
      (value) => value >= expiration,
      4000,
    );
    releaseToken();
    await until(
      () =>
        Promise.resolve(
          app.store.get("SELECT status FROM delivery_parts WHERE run_id=?", run)
            ?.status,
        ),
      (status) => status === "failed",
    );
    assert.equal(
      remote.calls.filter((call) => call.path.startsWith("/v2/")).length,
      0,
    );
  });

test("QQ WebSocket performs Windows identify, receives events, and exits on configuration revocation", async (t) => {
  let wsUrl = "",
    identified;
  const remote = await service(t, (call) =>
    call.path === "/app/getAppAccessToken"
      ? { access_token: "synthetic-token", expires_in: 3600 }
      : call.path === "/gateway"
        ? { url: wsUrl }
        : call.path.startsWith("/v2/")
          ? { id: randomUUID() }
          : { choices: [{ message: { content: "网关回复" } }] },
  );
  const ws = new WebSocketServer({ server: remote.server });
  t.after(
    () =>
      new Promise((resolve) => {
        for (const client of ws.clients) client.terminate();
        ws.close(resolve);
      }),
  );
  wsUrl = `ws://127.0.0.1:${remote.server.address().port}`;
  ws.on("connection", (socket) => {
    socket.send(JSON.stringify({ op: 10, d: { heartbeat_interval: 1000 } }));
    socket.on("message", (data) => {
      const frame = JSON.parse(data);
      if (frame.op === 2) {
        identified = frame;
        socket.send(
          JSON.stringify({
            op: 0,
            t: "READY",
            s: 1,
            d: { session_id: "session1" },
          }),
        );
        socket.send(JSON.stringify(sample("gatewaymsg", "/not-a-command")));
      }
      if (frame.op === 1) socket.send(JSON.stringify({ op: 11 }));
    });
  });
  const app = await fixture({
    testChannelEndpoints: { qq: remote.url, weixin: remote.url },
  });
  t.after(() => app.close());
  await app.bootstrap();
  await qqConfig(app, remote.url, "websocket");
  await until(
    async () => identified,
    (value) => !!value,
  );
  assert.equal(identified.d.properties.$os, "windows");
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (value) => value === 1,
  );
  const config = (await app.call("/config")).data;
  config.config.qq.enabled = false;
  await app.call(
    "/config",
    "PUT",
    { config: config.config },
    { headers: { "if-match": `"${config.revision}"` } },
  );
  await until(
    async () => app.features.channels.connection.qq,
    (value) => value === "disabled",
  );
});
test("iLink QR binds one owner; cursor and inbox commit atomically; reply context and tokens never enter DTOs", async (t) => {
  let base = "",
    updates = 0;
  const remote = await service(t, (call) => {
    if (call.path.startsWith("/ilink/bot/get_bot_qrcode"))
      return {
        qrcode: "synthetic-private-qr",
        qrcode_img_content: "https://example.invalid/synthetic-qr",
      };
    if (call.path.startsWith("/ilink/bot/get_qrcode_status"))
      return {
        status: "confirmed",
        ilink_bot_id: "bot@im.bot",
        ilink_user_id: "owner@im.wechat",
        bot_token: "synthetic-wx-token",
        baseurl: base,
      };
    if (call.path === "/ilink/bot/getupdates")
      return {
        ret: 0,
        get_updates_buf: "cursor2",
        msgs: updates++
          ? []
          : [
              {
                message_id: "18446744073709551615",
                from_user_id: "owner@im.wechat",
                to_user_id: "bot@im.bot",
                message_type: 1,
                message_state: 2,
                context_token: "synthetic-private-context",
                item_list: [{ type: 1, text_item: { text: "微信合成文字" } }],
              },
              {
                message_id: "2",
                from_user_id: "other@im.wechat",
                to_user_id: "bot@im.bot",
                message_type: 1,
                message_state: 2,
                context_token: "other-context",
                item_list: [{ type: 1, text_item: { text: "不可接受" } }],
              },
            ],
      };
    if (call.path === "/ilink/bot/sendmessage")
      return { ret: 0, message_id: "wx-sent" };
    return { choices: [{ message: { content: "微信合成回复" } }] };
  });
  base = remote.url;
  const app = await fixture({
    testChannelEndpoints: { qq: base, weixin: base },
  });
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(base);
  const login = await app.call("/channels/weixin/login", "POST", {
    expected_revision: 0,
  });
  assert.equal(login.status, 202);
  await until(
    () => app.call(`/channels/weixin/login/${login.data.id}`),
    (value) => value.data.status === "confirmed",
  );
  const config = (await app.call("/config")).data;
  config.config.weixin.enabled = true;
  assert.equal(
    (
      await app.call(
        "/config",
        "PUT",
        { config: config.config },
        { headers: { "if-match": `"${config.revision}"` } },
      )
    ).status,
    200,
  );
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (value) => value === 1,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM channel_events").n, 1);
  const send = remote.calls.find(
    (call) => call.path === "/ilink/bot/sendmessage",
  );
  assert.equal(send.body.msg.context_token, "synthetic-private-context");
  assert.equal(send.headers.authorization, "Bearer synthetic-wx-token");
  assert.equal(
    JSON.stringify((await app.call("/channels/status")).data).includes(
      "synthetic-private-context",
    ),
    false,
  );
  assert.equal(
    JSON.stringify((await app.call("/export")).data).includes(
      "synthetic-wx-token",
    ),
    false,
  );
});
