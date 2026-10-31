import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import dns from "node:dns";
import { syncBuiltinESMExports } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve, join } from "node:path";
import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { randomUUID } from "node:crypto";
import { fixture, until } from "./helpers.mjs";
import { qqSignature } from "../dist/channels.js";
import { now, hash, privateDirectory } from "../dist/security.js";
import sharp from "sharp";
const image = await sharp({
  create: { width: 1, height: 1, channels: 4, background: "red" },
})
  .png()
  .toBuffer();
const runtimeRoot = resolve(
    process.env.PROMETHEUS_RELEASE_ROOT ??
      fileURLToPath(new URL("../../../", import.meta.url)),
  ),
  { qqCdnUrl, downloadQqImage } = await import(
    pathToFileURL(join(runtimeRoot, "apps/server/dist/qq-media-download.js"))
  );
async function http(t, handler) {
  const calls = [],
    server = createServer(async (req, res) => {
      let body = "";
      for await (const b of req) body += b;
      const call = {
        path: req.url,
        headers: req.headers,
        body: body ? JSON.parse(body) : null,
      };
      calls.push(call);
      const value = await handler(call, res);
      if (value !== undefined && !res.headersSent) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify(value));
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
  return { server, calls, url: "http://127.0.0.1:" + server.address().port };
}
async function setup(t, download, options = {}) {
  const cdn = await http(t, async (c, res) => {
      if (download) return download(c, res);
      res.writeHead(200, {
        "content-type": "image/png",
        "content-length": image.length,
      });
      res.end(image);
    }),
    remote = await http(t, (c, res) =>
      options.model && c.path.endsWith("/chat/completions")
        ? options.model(c, res)
        : c.path === "/app/getAppAccessToken"
          ? { access_token: "synthetic-access-token", expires_in: 3600 }
          : c.path.endsWith("/chat/completions")
            ? {
                choices: [
                  {
                    message: {
                      content:
                        c.body.model === "vision"
                          ? "合成图片中有红色像素。"
                          : "根据图片描述的合成回答。",
                    },
                    finish_reason: "stop",
                  },
                ],
              }
            : { id: "confirmed-synthetic-message" },
    ),
    app = await fixture({
      testChannelEndpoints: {
        qq: remote.url,
        weixin: remote.url,
        qqMedia: cdn.url,
      },
    });
  t.after(() => app.close());
  await app.bootstrap();
  if (options.native !== false) await app.configure(remote.url);
  const secret = (
      await app.call("/secrets", "POST", {
        kind: "qq_app_secret",
        value: "synthetic-qq-secret",
      })
    ).data,
    config = (await app.call("/config")).data;
  config.config.qq = {
    enabled: true,
    app_id: "123456",
    credential_ref: secret.id,
    transport: "webhook",
  };
  assert.equal(
    (
      await app.call(
        "/config",
        "PUT",
        { config: config.config },
        { headers: { "if-match": '"' + config.revision + '"' } },
      )
    ).status,
    200,
  );
  const policy = (await app.call("/media/policy")).data;
  assert.equal(
    (
      await app.call(
        "/media/policy",
        "PUT",
        {
          vision_enabled: options.vision !== false,
          vision_model: "vision",
          image_generation_enabled: false,
          image_model: "",
        },
        { headers: { "if-match": '"' + policy.revision + '"' } },
      )
    ).status,
    200,
  );
  return { app, cdn, remote, secret };
}
function event(cdn, message = "media-one", content = "这张图是什么？") {
  return {
    op: 0,
    id: "event-" + message,
    t: "C2C_MESSAGE_CREATE",
    d: {
      id: message,
      timestamp: new Date().toISOString(),
      author: { user_openid: "sender1" },
      content,
      attachments: [
        {
          url: cdn.url + "/image?rkey=synthetic-private-url",
          content_type: "image/png",
          size: image.length,
        },
      ],
    },
  };
}
async function post(app, payload) {
  const raw = JSON.stringify(payload),
    timestamp = String(now());
  return fetch(app.origin + "/webhooks/qq", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-signature-timestamp": timestamp,
      "x-signature-ed25519": qqSignature(
        "synthetic-qq-secret",
        timestamp,
        Buffer.from(raw),
      ),
    },
    body: raw,
  });
}
test("a signed QQ image is durably received, fetched without platform credentials, normalized and answered through actual Vision then Native", async (t) => {
  const { app, cdn, remote } = await setup(t);
  assert.equal((await post(app, event(cdn))).status, 200);
  await until(
    async () =>
      remote.calls.filter((c) => c.path === "/v2/users/sender1/messages")
        .length,
    (n) => n === 1,
  );
  assert.equal(cdn.calls.length, 1);
  assert.equal(cdn.calls[0].headers.authorization, undefined);
  assert.equal(cdn.calls[0].headers.cookie, undefined);
  const model = remote.calls.filter((c) =>
    c.path.endsWith("/chat/completions"),
  );
  assert.equal(model.length, 2);
  assert.equal(model[0].body.model, "vision");
  assert.ok(
    model[0].body.messages[1].content[1].image_url.url.startsWith(
      "data:image/png;base64,",
    ),
  );
  assert.ok(
    model[1].body.messages.some(
      (m) => m.role === "user" && m.content.includes("合成图片中有红色像素。"),
    ),
  );
  assert.equal(
    model[1].body.messages[0].content.includes("合成图片中有红色像素。"),
    false,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM channel_events WHERE status='completed' AND payload_json IS NULL",
    ).n,
    1,
  );
  assert.equal(app.store.get("SELECT image FROM vision_sources").image, null);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM received_media_assets WHERE length(bytes)>0",
    ).n,
    1,
  );
  assert.equal(
    JSON.stringify(app.store.all("SELECT * FROM audit_events")).includes(
      "rkey=",
    ),
    false,
  );
});

test("CDN policy accepts only the original HTTPS host family and never credentials, private literals or suffix spoofing", () => {
  for (const url of [
    "https://multimedia.nt.qq.com.cn/image?rkey=opaque",
    "//gchat.qpic.cn/a",
    "https://c2cpicdw.qpic.cn/a",
    "https://p.qpic.cn/a",
    "https://gchat-bucket.cos.ap-shanghai.myqcloud.com/a",
  ])
    assert.ok(qqCdnUrl(url).url);
  for (const url of [
    "http://multimedia.nt.qq.com.cn/a",
    "https://multimedia.nt.qq.com.cn.attacker.invalid/a",
    "https://127.0.0.1/a",
    "https://[::1]/a",
    "https://u:p@gchat.qpic.cn/a",
    "https://gchat.qpic.cn:8443/a",
    "https://gchat.qpic.cn/a#private",
    "file:///C:/secret",
    "https://gchat.qpic.cn\\@evil.invalid/a",
    " https://gchat.qpic.cn/a",
  ])
    assert.throws(() => qqCdnUrl(url));
});
test("the actual HTTPS socket lookup rejects a private or mixed DNS answer before connecting", async (t) => {
  const original = dns.lookup;
  t.after(() => {
    dns.lookup = original;
    syncBuiltinESMExports();
  });
  let calls = 0;
  for (const addresses of [
    [{ address: "127.0.0.1", family: 4 }],
    [
      { address: "8.8.8.8", family: 4 },
      { address: "10.0.0.1", family: 4 },
    ],
  ]) {
    dns.lookup = (_host, _options, callback) => {
      calls++;
      callback(null, addresses);
    };
    syncBuiltinESMExports();
    await assert.rejects(
      () =>
        downloadQqImage(
          "https://multimedia.nt.qq.com.cn/picture?private=synthetic",
          AbortSignal.timeout(1000),
        ),
      (e) => e.code === "media_dns_private" && !e.message.includes("private="),
    );
  }
  assert.equal(calls, 2);
});
test("different gateway IDs for one platform message ACK once, even while its durable download is active", async (t) => {
  let release,
    entered = false;
  const { app, cdn, remote } = await setup(t, async (_c, res) => {
    entered = true;
    await new Promise((r) => (release = r));
    if (!res.destroyed) {
      res.writeHead(200, { "content-type": "image/png" });
      res.end(image);
    }
  });
  t.after(() => release?.());
  const payload = event(cdn);
  assert.equal((await post(app, payload)).status, 200);
  await until(
    async () => entered,
    (x) => x,
  );
  assert.equal(
    (await post(app, { ...payload, id: "different-transport-event" })).status,
    200,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM channel_events").n, 1);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM qq_inbound_receipts").n,
    1,
  );
  assert.equal(cdn.calls.length, 1);
  release();
  await until(
    async () =>
      remote.calls.filter((c) => c.path.startsWith("/v2/users/")).length,
    (n) => n === 1,
  );
  assert.equal(
    remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    2,
  );
});
test("an attachment never executes memory or skill commands, and unsupported count, channel, MIME and size never trigger download or Native", async (t) => {
  const { app, cdn, remote } = await setup(t),
    cases = [
      event(cdn, "slash", "/memory remember 不应保存的事实"),
      event(cdn, "multi"),
      event(cdn, "group"),
      event(cdn, "audio"),
      event(cdn, "large"),
      event(cdn, "missing"),
    ];
  cases[1].d.attachments.push({ ...cases[1].d.attachments[0] });
  cases[2].t = "GROUP_AT_MESSAGE_CREATE";
  cases[2].d.group_openid = "group1";
  cases[3].d.attachments[0].content_type = "audio/silk";
  cases[4].d.attachments[0].size = 8 * 1024 * 1024 + 1;
  delete cases[5].d.attachments[0].url;
  for (const p of cases) assert.equal((await post(app, p)).status, 200);
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === cases.length,
    9000,
  );
  assert.equal(cdn.calls.length, 0);
  assert.equal(
    remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    0,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM records").n, 0);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM local_runs WHERE kind='channel_media_notice'",
    ).n,
    cases.length,
  );
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM received_media_assets").n,
    0,
  );
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM qq_inbound_receipts WHERE code IS NOT NULL",
    ).n,
    cases.length,
  );
});
test("missing vision or Native configuration produces a factual host notice without touching the CDN", async (t) => {
  for (const options of [{ vision: false }, { native: false }]) {
    const { app, cdn, remote } = await setup(t, undefined, options);
    assert.equal((await post(app, event(cdn))).status, 200);
    await until(
      async () =>
        app.store.get(
          "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
        ).n,
      (n) => n === 1,
    );
    assert.equal(cdn.calls.length, 0);
    assert.equal(
      remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
      0,
    );
    assert.match(
      app.store.get("SELECT content FROM delivery_parts").content,
      /没.*读取|没看到/,
    );
  }
});
test("expired event timestamps cannot gain a fresh reply window or cause a download", async (t) => {
  const { app, cdn, remote } = await setup(t),
    p = event(cdn);
  p.d.timestamp = new Date((now() - 3700) * 1000).toISOString();
  assert.equal((await post(app, p)).status, 200);
  await until(
    async () => app.store.get("SELECT status FROM channel_events").status,
    (s) => s === "expired",
  );
  assert.equal(cdn.calls.length, 0);
  assert.equal(remote.calls.length, 0);
  assert.equal(
    app.store.get("SELECT payload_json FROM channel_events").payload_json,
    null,
  );
});
test("redirects, compressed bodies, wrong MIME, length overruns and malformed pixels are refused with a host notice and no model calls", async (t) => {
  for (const fault of [
    "redirect",
    "encoding",
    "mime",
    "length",
    "stream",
    "pixels",
  ]) {
    const { app, cdn, remote } = await setup(t, (_c, res) => {
      if (fault === "redirect") {
        res.writeHead(302, { location: "http://127.0.0.1/private" });
        res.end();
        return;
      }
      const headers = {
        "content-type": fault === "mime" ? "application/json" : "image/png",
        ...(fault === "encoding" ? { "content-encoding": "gzip" } : {}),
        ...(fault === "length"
          ? { "content-length": 8 * 1024 * 1024 + 1 }
          : {}),
      };
      res.writeHead(200, headers);
      res.end(
        fault === "stream"
          ? Buffer.alloc(8 * 1024 * 1024 + 1)
          : fault === "pixels"
            ? Buffer.from("invalid image payload")
            : image,
      );
    });
    assert.equal((await post(app, event(cdn))).status, 200);
    await until(
      async () =>
        app.store.get(
          "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
        ).n,
      (n) => n === 1,
    );
    assert.equal(cdn.calls.length, 1);
    assert.equal(
      remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
      0,
    );
    assert.equal(app.store.get("SELECT count(*) AS n FROM run_media").n, 0);
    assert.equal(
      app.store.get("SELECT payload_json FROM channel_events").payload_json,
      null,
    );
  }
});
test("source invalidation aborts the actual download, erases the private URL and cannot publish a late image", async (t) => {
  let release,
    entered = false,
    closed = false;
  const { app, cdn, remote } = await setup(t, async (_c, res) => {
    entered = true;
    res.on("close", () => (closed = true));
    await new Promise((r) => (release = r));
    if (!res.destroyed) {
      res.writeHead(200, { "content-type": "image/png" });
      res.end(image);
    }
  });
  t.after(() => release?.());
  await post(app, event(cdn));
  await until(
    async () => entered,
    (x) => x,
  );
  const receiver = app.features.channels.incoming;
  assert.ok(receiver.active);
  app.runtime.invalidate("qq:123456:c2c:sender1");
  assert.ok(receiver.active);
  const receipt = app.store.get("SELECT * FROM qq_inbound_receipts");
  assert.ok(receipt.request_started_at);
  assert.equal(receipt.request_finished_at, null);
  assert.equal(
    app.store.get("SELECT payload_json FROM channel_events").payload_json,
    null,
  );
  await until(
    async () => !receiver.active && closed,
    (x) => x,
  );
  release();
  await new Promise((r) => setTimeout(r, 250));
  assert.equal(remote.calls.length, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 0);
  assert.ok(
    app.store.get("SELECT request_finished_at FROM qq_inbound_receipts")
      .request_finished_at,
  );
});
test("account and model-policy changes during download revoke the original receipt before it creates any Run", async (t) => {
  for (const change of ["account", "policy"]) {
    let entered = false,
      release;
    const { app, cdn, remote } = await setup(t, async (_c, res) => {
      entered = true;
      await new Promise((r) => (release = r));
      if (!res.destroyed) {
        res.writeHead(200, { "content-type": "image/png" });
        res.end(image);
      }
    });
    t.after(() => release?.());
    await post(app, event(cdn));
    await until(
      async () => entered,
      (x) => x,
    );
    if (change === "account") {
      const c = (await app.call("/config")).data;
      c.config.qq.enabled = false;
      assert.equal(
        (
          await app.call(
            "/config",
            "PUT",
            { config: c.config },
            { headers: { "if-match": '"' + c.revision + '"' } },
          )
        ).status,
        200,
      );
    } else {
      const p = (await app.call("/media/policy")).data;
      assert.equal(
        (
          await app.call(
            "/media/policy",
            "PUT",
            { ...p.value, vision_enabled: false },
            { headers: { "if-match": '"' + p.revision + '"' } },
          )
        ).status,
        200,
      );
    }
    await until(
      async () => !app.features.channels.incoming.active,
      (x) => x,
    );
    release();
    assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 0);
    assert.equal(
      app.store.get("SELECT payload_json FROM channel_events").payload_json,
      null,
    );
    assert.equal(remote.calls.length, 0);
  }
});
test("an authenticated administrator sees bounded receive status but never signed URLs, tokens or media bytes", async (t) => {
  const { app, cdn } = await setup(t);
  await post(app, event(cdn));
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM channel_events WHERE status='completed'",
      ).n,
    (n) => n === 1,
  );
  const r = await app.call("/channels/inbox");
  assert.equal(r.status, 200);
  assert.equal(r.data.items.length, 1);
  const raw = JSON.stringify(r.data);
  for (const secret of [
    "rkey=",
    "synthetic-private-url",
    "synthetic-access-token",
    "payload_json",
    "data:image",
    "bytes",
  ])
    assert.equal(raw.includes(secret), false);
  assert.equal(
    (await app.call("/channels/inbox", "GET", undefined, { auth: false }))
      .status,
    401,
  );
});
test("a valid long caption and 256-byte platform identifiers remain accepted while the internal Run key stays bounded", async (t) => {
  const { app, cdn, remote } = await setup(t),
    p = event(cdn, "m".repeat(256), "a".repeat(16000));
  p.id = "e".repeat(256);
  assert.equal((await post(app, p)).status, 200);
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 1,
  );
  const run = app.store.get("SELECT input,client_message_id FROM runs");
  assert.equal(run.input.length, 16000);
  assert.equal(run.client_message_id.length, 64);
  assert.equal(
    remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    2,
  );
});
test("vision HTTP failure sends a bounded truthful notice while retaining failed Run status and no fabricated assistant result", async (t) => {
  const { app, cdn, remote } = await setup(t, undefined, {
    model: (_c, res) => {
      res.writeHead(502, { "content-type": "application/json" });
      res.end("{}");
    },
  });
  await post(app, event(cdn));
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 1,
  );
  const run = app.store.get("SELECT * FROM runs");
  assert.equal(run.status, "failed");
  assert.equal(run.response, null);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM messages WHERE role='assistant'")
      .n,
    0,
  );
  assert.match(
    app.store.get("SELECT content FROM delivery_parts").content,
    /回复没能完成/,
  );
  assert.equal(
    remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    1,
  );
  assert.equal(
    app.store.get("SELECT image,description FROM vision_sources").image,
    null,
  );
  assert.equal(
    app.store.get("SELECT description FROM vision_sources").description,
    null,
  );
});
test("a commit failure rolls back Run, source and archive together, preserves the durable receipt and retries after storage recovers", async (t) => {
  const { app, cdn, remote } = await setup(t);
  app.store.database.exec(
    "CREATE TRIGGER deny_received_archive BEFORE INSERT ON received_media_assets BEGIN SELECT RAISE(ABORT,'synthetic archive failure'); END;",
  );
  await post(app, event(cdn));
  await until(
    async () => app.store.get("SELECT error FROM channel_events")?.error,
    (e) => String(e).includes("存储暂不可用"),
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM messages").n, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 0);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM received_media_assets").n,
    0,
  );
  assert.equal(remote.calls.length, 0);
  assert.ok(
    app.store.get("SELECT payload_json FROM channel_events").payload_json,
  );
  app.store.database.exec("DROP TRIGGER deny_received_archive");
  app.store.run("UPDATE channel_events SET ready_at=0");
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 1,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 1);
  assert.equal(
    remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    2,
  );
  assert.equal(cdn.calls.length, 2);
});

test("a graceful restart closes the active socket before retaining the original receipt and resumes it exactly once within its original deadline", async (t) => {
  let entered = false,
    closed = false,
    release;
  const { app, cdn, remote, secret } = await setup(t, async (_c, res) => {
    if (cdn.calls.length === 1) {
      entered = true;
      res.on("close", () => (closed = true));
      await new Promise((r) => (release = r));
    }
    if (!res.destroyed) {
      res.writeHead(200, { "content-type": "image/png" });
      res.end(image);
    }
  });
  t.after(() => release?.());
  const payload = event(cdn);
  await post(app, payload);
  await until(async () => entered, Boolean);
  const original = app.store.get("SELECT id,deadline FROM channel_events");
  assert.equal((await app.call("/secrets/" + secret.id, "DELETE")).status, 409);
  await app.close();
  await until(async () => closed, Boolean);
  release();
  const db = new DatabaseSync(join(app.dataDir, "prometheus.db"), {
    readOnly: true,
  });
  assert.equal(
    db.prepare("SELECT status FROM channel_events").get().status,
    "queued",
  );
  assert.ok(
    db.prepare("SELECT request_finished_at FROM qq_inbound_receipts").get()
      .request_finished_at,
  );
  assert.equal(db.prepare("SELECT count(*) AS n FROM runs").get().n, 0);
  db.close();
  const resumed = await fixture({
    dataDir: app.dataDir,
    testChannelEndpoints: {
      qq: remote.url,
      weixin: remote.url,
      qqMedia: cdn.url,
    },
  });
  t.after(() => resumed.close());
  await until(
    async () =>
      resumed.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 1,
  );
  assert.equal(
    (await post(resumed, { ...payload, id: "redelivery-after-restart" }))
      .status,
    200,
  );
  assert.deepEqual(
    resumed.store.get("SELECT id,deadline FROM channel_events"),
    original,
  );
  assert.equal(resumed.store.get("SELECT count(*) AS n FROM runs").n, 1);
  assert.equal(cdn.calls.length, 2);
  assert.equal(
    remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    2,
  );
});

test("text before and after an image keeps arrival order and never coalesces across the attachment", async (t) => {
  const { app, cdn, remote } = await setup(t);
  const first = event(cdn, "before", "BEFORE_IMAGE"),
    last = event(cdn, "after", "AFTER_IMAGE");
  delete first.d.attachments;
  delete last.d.attachments;
  await post(app, first);
  await post(app, event(cdn, "middle", "IMAGE_CAPTION"));
  await post(app, last);
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 3,
    12000,
  );
  assert.deepEqual(
    app.store.all("SELECT input FROM runs ORDER BY rowid").map((r) => r.input),
    ["BEFORE_IMAGE", "IMAGE_CAPTION", "AFTER_IMAGE"],
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM run_media").n, 1);
  assert.equal(cdn.calls.length, 1);
  assert.equal(
    remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    4,
  );
});

test("per-conversation invalidation during download suppresses the late attachment without invalidating another owner", async (t) => {
  let entered = false,
    release;
  const { app, cdn, remote } = await setup(t, async (_c, res) => {
    entered = true;
    await new Promise((r) => (release = r));
    if (!res.destroyed) {
      res.writeHead(200, { "content-type": "image/png" });
      res.end(image);
    }
  });
  t.after(() => release?.());
  const owner = "qq:123456:c2c:sender1",
    c = app.runtime.createConversation(owner, "existing");
  app.store.run(
    "UPDATE conversations SET source_key=? WHERE id=?",
    owner,
    c.id,
  );
  await post(app, event(cdn));
  await until(async () => entered, Boolean);
  app.runtime.invalidateConversation(c.id);
  await until(async () => !app.features.channels.incoming.active, Boolean);
  release();
  assert.equal(
    app.store.get("SELECT status FROM channel_events").status,
    "cancelled",
  );
  assert.equal(
    app.store.get("SELECT payload_json FROM channel_events").payload_json,
    null,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 0);
  assert.equal(remote.calls.length, 0);
});

test("received normalized bytes expire and source forgetting erases them while preserving the factual chat record", async (t) => {
  const { app, cdn } = await setup(t);
  await post(app, event(cdn));
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 1,
  );
  const source = app.store.get("SELECT * FROM received_media_assets");
  app.store.run("UPDATE received_media_assets SET expires_at=0");
  app.features.media.prune();
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM received_media_assets").n,
    0,
  );
  await post(app, event(cdn, "second"));
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 2,
  );
  const floor = app.store.get("SELECT max(rowid) AS n FROM messages").n;
  app.store.run(
    "INSERT INTO metadata VALUES(?,?)",
    "source_cutoff:" + hash(source.owner_id),
    String(floor),
  );
  app.runtime.invalidate(source.owner_id);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM received_media_assets").n,
    0,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM messages").n, 4);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM run_publications WHERE state='suppressed'",
    ).n,
    2,
  );
});

test("the durable byte quota rejects unacknowledged new work but still ACKs a previously received message", async (t) => {
  const { app, cdn } = await setup(t);
  app.features.channels.processEvents = () => {};
  const payload = event(cdn);
  assert.equal((await post(app, payload)).status, 200);
  const id = app.store.get("SELECT id FROM channel_events").id;
  app.store.run(
    "UPDATE channel_events SET payload_json=? WHERE id=?",
    "x".repeat(32 * 1024 * 1024),
    id,
  );
  assert.equal(
    (await post(app, { ...payload, id: "duplicate-at-quota" })).status,
    200,
  );
  assert.equal((await post(app, event(cdn, "not-accepted"))).status, 503);
  assert.equal(app.store.get("SELECT count(*) AS n FROM channel_events").n, 1);
  assert.equal(cdn.calls.length, 0);
});

test("schema17 upgrade backs up the real prior schema, preserves identity and repair cursor, and quarantines unsupported legacy media without inventing a download", async (t) => {
  const directory = privateDirectory(
      resolve(".local/prometheus/tests/" + randomUUID()),
    ),
    db = new DatabaseSync(join(directory, "prometheus.db"));
  db.exec(readFileSync(new URL("./schema17.sql", import.meta.url), "utf8"));
  for (const [key, value] of [
    ["prometheus.schema", "17"],
    ["assistant.identity", "qq-upgrade-stable"],
    ["diary.record_cursor", "12345"],
  ])
    db.prepare("INSERT INTO metadata VALUES(?,?)").run(key, value);
  const c = randomUUID(),
    run = randomUUID(),
    user = randomUUID(),
    answer = randomUUID(),
    eventId = randomUUID(),
    job = randomUUID(),
    memory = randomUUID(),
    child = randomUUID(),
    at = now(),
    owner = "qq:123456:c2c:sender1";
  const caption =
    "旧图片文字\n[宿主：本条包含媒体，媒体理解尚未完成时不能声称看到了画面或听到了声音。]";
  db.prepare("INSERT INTO conversations VALUES(?,?,?,'old QQ',0,?)").run(
    c,
    owner,
    owner,
    at,
  );
  db.prepare(
    "INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,context_epoch,status,response,created_at,updated_at) VALUES(?,?,?,'old-client',?,'{}',0,'completed','旧模型未经识图的文字',?,?)",
  ).run(run, c, owner, caption, at, at);
  db.prepare("INSERT INTO messages VALUES(?,?,?,'user',?,?)").run(
    user,
    c,
    run,
    caption,
    at,
  );
  db.prepare(
    "INSERT INTO messages VALUES(?,?,?,'assistant','旧模型未经识图的文字',?)",
  ).run(answer, c, run, at);
  db.prepare(
    "INSERT INTO run_targets VALUES(?,'qq','unavailable-encrypted-target','lost-credential',?,'123456')",
  ).run(run, at + 300);
  db.prepare(
    "INSERT INTO channel_events VALUES(?,'qq','123456','legacy-event',?,?,NULL,'lost-credential','completed',?,?,?,?,NULL)",
  ).run(eventId, owner, owner, run, Date.now(), Date.now(), at + 300);
  db.prepare("INSERT INTO summaries VALUES(?,?,1,'旧未核实摘要','[]',0,?)").run(
    c,
    owner,
    at,
  );
  db.prepare(
    "INSERT INTO model_jobs(id,owner_id,kind,status,input_json,output_json,source_json,config_json,created_at,updated_at) VALUES(?,?,'memory_candidates','review_pending','{}','{}',?,'{}',?,?)",
  ).run(job, owner, JSON.stringify([{ type: "message", id: answer }]), at, at);
  for (const record of [memory, child])
    db.prepare(
      "INSERT INTO records VALUES(?,'memory',?,'private',1,?,?,?,NULL)",
    ).run(
      record,
      owner,
      JSON.stringify({ content: "LEGACY_UNVERIFIED_VISUAL_MEMORY" }),
      at,
      at,
    );
  db.prepare("INSERT INTO derivations VALUES(?,'message',?,0)").run(
    memory,
    answer,
  );
  db.prepare("INSERT INTO derivations VALUES(?,'record',?,1)").run(
    child,
    memory,
  );
  db.prepare("INSERT INTO derivations VALUES(?,'record',?,1)").run(
    memory,
    child,
  );
  db.close();
  let app = await fixture({ dataDir: directory });
  t.after(() => app.close());
  assert.equal(app.store.assistantId, "qq-upgrade-stable");
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "20",
  );
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='diary.record_cursor'")
      .value,
    "12345",
  );
  assert.equal(
    app.store.get("SELECT kind FROM channel_media_sources").kind,
    "legacy_unverified",
  );
  assert.equal(
    app.store.get("SELECT status FROM model_jobs WHERE id=?", job).status,
    "invalidated",
  );
  assert.equal(
    app.store.get("SELECT output_json FROM model_jobs WHERE id=?", job)
      .output_json,
    null,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM summaries").n, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM messages").n, 2);
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM records WHERE deleted_at IS NULL")
      .n,
    2,
  );
  assert.equal(
    app.runtime.records
      .nativeContext(owner, "")
      .text.includes("LEGACY_UNVERIFIED_VISUAL_MEMORY"),
    false,
  );
  await app.bootstrap();
  const refused = await app.call("/knowledge/jobs", "POST", {
    kind: "memory_candidates",
    sources: [{ type: "record", id: child }],
  });
  assert.equal(refused.status, 400);
  assert.equal(refused.data.error.code, "visual_source_excluded");
  const snapshot = readdirSync(join(directory, "backups"));
  assert.equal(snapshot.length, 1);
  const old = new DatabaseSync(join(directory, "backups", snapshot[0]), {
    readOnly: true,
  });
  assert.equal(
    old
      .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .get().value,
    "17",
  );
  assert.equal(
    old
      .prepare(
        "SELECT count(*) AS n FROM sqlite_master WHERE name='qq_inbound_receipts'",
      )
      .get().n,
    0,
  );
  assert.equal(
    old.prepare("PRAGMA integrity_check").get().integrity_check,
    "ok",
  );
  old.close();
  await app.close();
  app = await fixture({ dataDir: directory });
  assert.equal(
    app.store.get("SELECT count(*) AS n FROM channel_media_sources").n,
    1,
  );
  assert.equal(readdirSync(join(directory, "backups")).length, 1);
  assert.equal(app.store.get("PRAGMA integrity_check").integrity_check, "ok");
});
