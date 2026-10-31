import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import http from "node:http";
import dns from "node:dns";
import { syncBuiltinESMExports } from "node:module";
import { createHash, randomUUID } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fixture, until } from "./helpers.mjs";
import { qqSignature } from "../dist/channels.js";
import { now } from "../dist/security.js";
import { signedUploadUrl, qqWire } from "../dist/qq-upload.js";
import sharp from "sharp";
const smallVideo = readFileSync(
  new URL("./fixtures/synthetic-h264.mp4", import.meta.url),
);
const png = await sharp({
  create: { width: 2, height: 2, channels: 4, background: "red" },
})
  .png()
  .toBuffer();
const digest = (name, b) => createHash(name).update(b).digest("hex");
function video(size = smallVideo.length) {
  if (size === smallVideo.length) return smallVideo;
  const free = Buffer.alloc(size - smallVideo.length);
  free.writeUInt32BE(free.length);
  free.write("free", 4);
  return Buffer.concat([smallVideo, free]);
}
function wav() {
  const b = Buffer.alloc(44 + 16000);
  b.write("RIFF");
  b.writeUInt32LE(b.length - 8, 4);
  b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16);
  b.writeUInt16LE(1, 20);
  b.writeUInt16LE(1, 22);
  b.writeUInt32LE(8000, 24);
  b.writeUInt32LE(16000, 28);
  b.writeUInt16LE(2, 32);
  b.writeUInt16LE(16, 34);
  b.write("data", 36);
  b.writeUInt32LE(16000, 40);
  return b;
}
async function setup(t, options = {}) {
  const calls = [];
  let base;
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const b of req) chunks.push(b);
    const bytes = Buffer.concat(chunks),
      path = req.url,
      body =
        req.method === "PUT" ? null : bytes.length ? JSON.parse(bytes) : null;
    const call = {
      path,
      method: req.method,
      headers: req.headers,
      body,
      bytes: req.method === "PUT" ? bytes : undefined,
    };
    calls.push(call);
    let value;
    if (path === "/app/getAppAccessToken")
      value = { access_token: "synthetic-token", expires_in: 3600 };
    else if (path.endsWith("/chat/completions"))
      value = {
        choices: [
          {
            message: {
              content: options.reply ? options.reply(body) : "合成文字回复。",
            },
            finish_reason: "stop",
          },
        ],
      };
    else if (path.startsWith("/image")) {
      res.writeHead(200, { "content-type": "image/png" });
      res.end(png);
      return;
    } else if (path.endsWith("/upload_prepare")) {
      const block = options.block ?? 2 * 1024 * 1024,
        first = options.first ?? 0;
      value = {
        upload_id: "synthetic-upload",
        block_size: String(block),
        parts: Array.from(
          { length: Math.ceil(Number(body.file_size) / block) },
          (_, i) => ({
            index: first + i,
            presigned_url: base + "/put/" + i + "?private=synthetic-signed",
          }),
        ),
      };
      if (options.prepare) value = options.prepare(value, call);
    } else if (path.startsWith("/put/")) {
      if (options.put) {
        await options.put(call, res);
        return;
      }
      res.writeHead(200);
      res.end();
      return;
    } else if (path.endsWith("/upload_part_finish")) {
      if (options.finish) {
        await options.finish(call, res);
        return;
      }
      value = {};
    } else if (path.endsWith("/files")) {
      if (options.files) {
        await options.files(call, res);
        return;
      }
      value = { file_info: "SYNTHETIC_PRIVATE_FILE_INFO" };
    } else if (path.endsWith("/messages")) {
      if (body.msg_type === 7 && options.send) {
        await options.send(call, res);
        return;
      }
      value = { id: "sent-" + calls.length };
    } else throw Error("Unexpected fixture path " + path);
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(value));
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  base = "http://127.0.0.1:" + server.address().port;
  t.after(
    () =>
      new Promise((r) => {
        server.closeAllConnections();
        server.close(r);
      }),
  );
  const endpoints = { qq: base, weixin: base, qqMedia: base, qqUpload: base };
  let app = await fixture({ testChannelEndpoints: endpoints });
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(base);
  const secret = (
      await app.call("/secrets", "POST", {
        kind: "qq_app_secret",
        value: "synthetic-secret",
      })
    ).data,
    c = (await app.call("/config")).data;
  c.config.qq = {
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
        { config: c.config },
        { headers: { "if-match": '"' + c.revision + '"' } },
      )
    ).status,
    200,
  );
  async function receive(
    id = "source",
    attachments,
    content = "T".repeat(360),
    sender = "sender1",
  ) {
    const payload = {
        op: 0,
        id: "e-" + id,
        t: "C2C_MESSAGE_CREATE",
        d: {
          id,
          timestamp: new Date().toISOString(),
          content,
          author: { user_openid: sender },
          ...(attachments ? { attachments } : {}),
        },
      },
      raw = JSON.stringify(payload),
      ts = String(now());
    const r = await fetch(app.origin + "/webhooks/qq", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-signature-timestamp": ts,
        "x-signature-ed25519": qqSignature(
          "synthetic-secret",
          ts,
          Buffer.from(raw),
        ),
      },
      body: raw,
    });
    assert.equal(r.status, 200);
    return payload;
  }
  await receive();
  await until(
    async () =>
      app.store.get(
        "SELECT count(*) AS n FROM delivery_parts WHERE status='sent'",
      ).n,
    (n) => n === 1,
  );
  const run = String(app.store.get("SELECT id FROM runs").id);
  const upload = async (
    bytes,
    kind = "video",
    id = randomUUID(),
    source = run,
  ) =>
    app.call("/qq/media/assets", "POST", {
      run_id: source,
      kind,
      data_base64: bytes.toString("base64"),
      request_id: id,
    });
  const queue = async (asset, id = randomUUID(), source = run) =>
    app.call("/qq/media/deliveries", "POST", {
      run_id: source,
      asset_id: asset,
      text: "合成媒体说明",
      request_id: id,
      acknowledge_effects: true,
    });
  return { app, calls, base, run, upload, queue, receive, endpoints, server };
}
async function terminal(app, id) {
  return until(
    async () => app.store.get("SELECT * FROM delivery_parts WHERE id=?", id),
    (p) => p && !["queued", "sending"].includes(p.status),
    20000,
  );
}
test("direct image and WAV transport upload private bytes and send only through the fixed passive C2C target with distinct sequence numbers", async (t) => {
  const { app, calls, upload, queue } = await setup(t);
  for (const [bytes, kind, type] of [
    [png, "image", 1],
    [wav(), "voice", 3],
  ]) {
    const a = await upload(bytes, kind);
    assert.equal(a.status, 201);
    const q = await queue(a.data.id);
    assert.equal(q.status, 202);
    assert.equal((await terminal(app, q.data.part_id)).status, "sent");
    const file = calls.filter((c) => c.path.endsWith("/files")).at(-1);
    assert.equal(file.body.file_type, type);
    assert.equal(file.body.srv_send_msg, false);
    assert.ok(file.body.file_data);
    assert.equal(file.headers.authorization, "QQBot synthetic-token");
  }
  const messages = calls.filter((c) => c.body?.msg_type === 7);
  const actor = String(app.store.get("SELECT id FROM admins").id),
    sourceRun = String(
      app.store.get("SELECT id FROM runs ORDER BY rowid LIMIT 1").id,
    ),
    answer = String(
      app.store.get(
        "SELECT id FROM messages WHERE run_id=? AND role='assistant'",
        sourceRun,
      ).id,
    );
  app.features.knowledge.setGrant(actor, "qq:123456:c2c:sender1", 0, {
    capture: false,
    allow_model: true,
    can_sleep: false,
    can_wake: false,
  });
  assert.throws(() =>
    app.features.knowledge.enqueue(actor, {
      kind: "memory_candidates",
      sources: [{ type: "message", id: answer }],
    }),
  );
  assert.deepEqual(
    messages.map((c) => c.body.msg_seq),
    [2, 3],
  );
  assert.ok(
    messages.every(
      (c) =>
        c.path === "/v2/users/sender1/messages" &&
        c.body.msg_id === "source" &&
        c.body.media.file_info === "SYNTHETIC_PRIVATE_FILE_INFO",
    ),
  );
  const status = JSON.stringify((await app.call("/qq/media/deliveries")).data);
  for (const secret of [
    "SYNTHETIC_PRIVATE_FILE_INFO",
    "synthetic-token",
    "file_data",
    "private=",
  ])
    assert.equal(status.includes(secret), false);
});
for (const first of [0, 1])
  test(
    "multipart video preserves " +
      first +
      "-based integer indexes, exact ranges and all digest fields without leaking QQ headers to PUT",
    async (t) => {
      const { app, calls, upload, queue } = await setup(t, { first }),
        bytes = video(6_300_000),
        a = await upload(bytes);
      assert.equal(a.status, 201);
      const q = await queue(a.data.id);
      assert.equal((await terminal(app, q.data.part_id)).status, "sent");
      const prepare = calls.find((c) => c.path.endsWith("/upload_prepare"));
      assert.equal(prepare.body.file_size, String(bytes.length));
      assert.equal(prepare.body.md5, digest("md5", bytes));
      assert.equal(prepare.body.sha1, digest("sha1", bytes));
      assert.equal(
        prepare.body.md5_10m,
        digest("md5", bytes.subarray(0, 10_002_432)),
      );
      const puts = calls.filter((c) => c.method === "PUT"),
        finishes = calls.filter((c) => c.path.endsWith("/upload_part_finish"));
      assert.equal(puts.length, 4);
      assert.equal(finishes.length, 4);
      assert.deepEqual(Buffer.concat(puts.map((c) => c.bytes)), bytes);
      for (let i = 0; i < puts.length; i++) {
        assert.equal(puts[i].headers.authorization, undefined);
        assert.equal(puts[i].headers["x-union-appid"], undefined);
        assert.equal(puts[i].headers.cookie, undefined);
        assert.equal(finishes[i].body.part_index, i + first);
        assert.equal(finishes[i].body.block_size, String(puts[i].bytes.length));
        assert.equal(finishes[i].body.md5, digest("md5", puts[i].bytes));
      }
      const files = calls.find((c) => c.path.endsWith("/files"));
      assert.equal(files.body.upload_id, "synthetic-upload");
      assert.equal(files.body.file_data, undefined);
      assert.equal(files.body.srv_send_msg, false);
      const stages = app.store.get("SELECT * FROM qq_media_transfers");
      assert.equal(stages.completed_parts, 4);
      assert.equal(stages.stage, "sent");
      assert.ok(stages.request_finished_at);
    },
  );
test("five MiB stays direct, one extra byte uses prepare, and the exact twenty MiB boundary uploads while an extra byte is refused", async (t) => {
  const { app, calls, upload, queue } = await setup(t, {
    block: 8 * 1024 * 1024,
  });
  for (const size of [5 * 1024 * 1024, 5 * 1024 * 1024 + 1, 20 * 1024 * 1024]) {
    const a = await upload(video(size));
    assert.equal(a.status, 201);
    const q = await queue(a.data.id);
    const completed = await terminal(app, q.data.part_id);
    assert.equal(
      completed.status,
      "sent",
      JSON.stringify({
        size,
        status: completed.status,
        transfer: app.store.get(
          "SELECT stage,error_code FROM qq_media_transfers WHERE part_id=?",
          q.data.part_id,
        ),
      }),
    );
  }
  assert.equal(
    calls.filter((c) => c.path.endsWith("/upload_prepare")).length,
    2,
  );
  assert.equal((await upload(video(20 * 1024 * 1024 + 1))).status, 400);
});
test("a file above ten million bytes hashes exactly the historical 10002432-byte prefix", async (t) => {
  const { app, calls, upload, queue } = await setup(t),
    bytes = video(11 * 1024 * 1024),
    a = await upload(bytes),
    q = await queue(a.data.id);
  assert.equal((await terminal(app, q.data.part_id)).status, "sent");
  const p = calls.find((c) => c.path.endsWith("/upload_prepare")).body;
  assert.equal(p.md5_10m, digest("md5", bytes.subarray(0, 10_002_432)));
  assert.notEqual(
    p.md5_10m,
    digest("md5", bytes.subarray(0, 10 * 1024 * 1024)),
  );
});
test("prepare validates every index, part count, block size and signed URL before any byte PUT", async (t) => {
  for (const fault of ["index", "string-index", "count", "block", "url"]) {
    const { app, calls, upload, queue } = await setup(t, {
        prepare: (p) => {
          if (fault === "index") p.parts[1].index = 7;
          if (fault === "string-index") p.parts[0].index = "0";
          if (fault === "count") p.parts.pop();
          if (fault === "block") p.block_size = "0";
          if (fault === "url")
            p.parts.at(-1).presigned_url = "http://127.0.0.1/private";
          return p;
        },
      }),
      a = await upload(video(6_300_000)),
      q = await queue(a.data.id);
    assert.equal((await terminal(app, q.data.part_id)).status, "failed");
    assert.equal(calls.filter((c) => c.method === "PUT").length, 0);
    assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 0);
  }
});
test("signed upload URLs require the original HTTPS COS family and reject private literals, credentials, redirects and suffix spoofing", () => {
  for (const u of [
    "https://bucket.cos.ap-shanghai.myqcloud.com/a?signature=opaque",
    "https://upload.qq.com/a",
    "https://upload.qcloud.com/a",
  ])
    assert.ok(signedUploadUrl(u).url);
  for (const u of [
    "http://upload.qq.com/a",
    "https://qq.com.evil.invalid/a",
    "https://127.0.0.1/a",
    "https://u:p@upload.qq.com/a",
    "https://upload.qq.com:8443/a",
    "https://upload.qq.com/a#fragment",
    "https://upload.qq.com\\@evil.invalid/a",
  ])
    assert.throws(() => signedUploadUrl(u));
});
test("PUT redirects and rejected part receipts never dispatch a media message or continue later parts", async (t) => {
  for (const fault of ["redirect", "finish"]) {
    const { app, calls, upload, queue } = await setup(
        t,
        fault === "redirect"
          ? {
              put: (_c, res) => {
                res.writeHead(302, { location: "http://127.0.0.1/private" });
                res.end();
              },
            }
          : {
              finish: (_c, res) => {
                res.writeHead(200, { "content-type": "application/json" });
                res.end('{"err_code":42}');
              },
            },
      ),
      a = await upload(video(6_300_000)),
      q = await queue(a.data.id);
    assert.equal((await terminal(app, q.data.part_id)).status, "failed");
    assert.equal(calls.filter((c) => c.method === "PUT").length, 1);
    assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 0);
  }
});
test("asynchronous final acceptance is unknown, a definite platform rejection is failed, and neither is retried", async (t) => {
  for (const [http, body, status] of [
    [202, "{}", "unknown"],
    [200, '{"err_code":304023}', "unknown"],
    [200, '{"err_code":42}', "failed"],
    [200, "{}", "unknown"],
  ]) {
    const { app, calls, upload, queue } = await setup(t, {
        send: (_c, res) => {
          res.writeHead(http, { "content-type": "application/json" });
          res.end(body);
        },
      }),
      a = await upload(png, "image"),
      id = randomUUID(),
      q = await queue(a.data.id, id);
    assert.equal((await terminal(app, q.data.part_id)).status, status);
    assert.equal((await queue(a.data.id, id)).data.part_id, q.data.part_id);
    await new Promise((r) => setTimeout(r, 250));
    assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 1);
  }
});
test("revocation during a real PUT closes the physical socket before release and cannot reach final message dispatch", async (t) => {
  let entered = false,
    closed = false,
    release;
  const { app, calls, upload, queue } = await setup(t, {
    put: async (_c, res) => {
      entered = true;
      res.on("close", () => (closed = true));
      await new Promise((r) => (release = r));
      if (!res.destroyed) {
        res.writeHead(200);
        res.end();
      }
    },
  });
  t.after(() => release?.());
  const a = await upload(video(6_300_000)),
    q = await queue(a.data.id);
  await until(async () => entered, Boolean);
  const outgoing = app.features.channels.outgoing;
  assert.ok(outgoing.active);
  app.runtime.invalidate("qq:123456:c2c:sender1");
  assert.ok(outgoing.active);
  assert.equal(
    app.store.get("SELECT request_finished_at FROM qq_media_transfers")
      .request_finished_at,
    null,
  );
  await until(async () => closed && !outgoing.active, Boolean);
  release();
  assert.equal((await terminal(app, q.data.part_id)).status, "failed");
  assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 0);
});
test("asset and delivery requests are idempotent, owner/conversation scoped, authenticated, and reject paths, URLs and arbitrary recipients", async (t) => {
  const { app, upload, queue, receive } = await setup(t),
    id = randomUUID(),
    a = await upload(png, "image", id);
  assert.equal((await upload(png, "image", id)).data.id, a.data.id);
  assert.equal((await upload(wav(), "voice", id)).status, 409);
  assert.equal(
    (await app.call("/qq/media/deliveries", "GET", undefined, { auth: false }))
      .status,
    401,
  );
  assert.equal(
    (
      await app.call(
        "/qq/media/assets",
        "POST",
        { run_id: "invalid" },
        { auth: false },
      )
    ).status,
    401,
  );
  const base = {
    run_id: a.data.run_id,
    asset_id: a.data.id,
    request_id: randomUUID(),
    acknowledge_effects: true,
  };
  for (const key of ["url", "path", "target", "recipient"])
    assert.equal(
      (
        await app.call("/qq/media/deliveries", "POST", {
          ...base,
          [key]: "forbidden",
        })
      ).status,
      400,
    );
  await receive("foreign", undefined, "X".repeat(360), "sender2");
  await until(
    async () => app.store.get("SELECT count(*) AS n FROM runs").n,
    (n) => n === 2,
  );
  await until(
    async () =>
      app.store.get("SELECT count(*) AS n FROM runs WHERE status='completed'")
        .n,
    (n) => n === 2,
  );
  const other = String(
    app.store.get("SELECT id FROM runs ORDER BY rowid DESC LIMIT 1").id,
  );
  assert.equal((await queue(a.data.id, randomUUID(), other)).status, 404);
});
test("MP4 structure, external data references, overlong duration, WAV truncation and malformed base64 are rejected without persisting assets", async (t) => {
  const { app, upload } = await setup(t);
  const duration = Buffer.from(smallVideo),
    at = duration.indexOf(Buffer.from("mvhd"));
  duration.writeUInt32BE(61000, at + 20);
  const reference = Buffer.from(smallVideo),
    url = reference.indexOf(Buffer.from("url "));
  reference[url + 7] = 0;
  for (const bytes of [
    Buffer.from("fake ftyp"),
    smallVideo.subarray(0, -1),
    duration,
    reference,
  ])
    assert.equal((await upload(bytes)).status, 400);
  assert.equal((await upload(wav().subarray(0, -1), "voice")).status, 400);
  assert.equal(app.store.get("SELECT count(*) AS n FROM qq_media_assets").n, 0);
});
test("Native may return an actually offered received image, but no transport token or arbitrary media URL enters its prompt", async (t) => {
  const { app, calls, base, receive } = await setup(t, {
    reply: (body) => {
      const raw = body.messages[0]?.content ?? "",
        match = raw.match(/制品目录：(\[[^\n]+\])/);
      if (body.model === "vision") return "合成红色图片。";
      return match
        ? JSON.stringify([
            { type: "text", text: "这是规范化后的来源图片。", delay_ms: 0 },
            {
              type: "image",
              asset_id: JSON.parse(match[1])[0].id,
              text: "来源图片",
              delay_ms: 1500,
            },
          ])
        : "合成文字回复。";
    },
  });
  const policy = (await app.call("/media/policy")).data;
  assert.equal(
    (
      await app.call(
        "/media/policy",
        "PUT",
        {
          vision_enabled: true,
          vision_model: "vision",
          image_generation_enabled: false,
          image_model: "",
        },
        { headers: { "if-match": '"' + policy.revision + '"' } },
      )
    ).status,
    200,
  );
  await receive(
    "picture",
    [{ url: base + "/image", content_type: "image/png", size: png.length }],
    "请把这张图片回给我。",
  );
  await until(
    async () => calls.filter((c) => c.body?.msg_type === 7).length,
    (n) => n === 1,
    10000,
  );
  const sent = app.store.get(
    "SELECT p.status FROM delivery_parts p JOIN qq_media_transfers t ON t.part_id=p.id",
  );
  assert.equal(sent.status, "sent");
  const model = calls
    .filter((c) => c.path.endsWith("/chat/completions"))
    .at(-1);
  assert.ok(model.body.messages[0].content.includes("制品目录："));
  assert.equal(JSON.stringify(model.body).includes("synthetic-token"), false);
  assert.equal(
    JSON.stringify(model.body).includes("SYNTHETIC_PRIVATE_FILE_INFO"),
    false,
  );
});

test("signed PUT socket lookup rejects private and mixed DNS answers before connecting and does not expose URL query text", async (t) => {
  const original = dns.lookup;
  t.after(() => {
    dns.lookup = original;
    syncBuiltinESMExports();
  });
  for (const addresses of [
    [{ address: "127.0.0.1", family: 4 }],
    [
      { address: "8.8.8.8", family: 4 },
      { address: "10.0.0.1", family: 4 },
    ],
  ]) {
    dns.lookup = (_h, _o, cb) => cb(null, addresses);
    syncBuiltinESMExports();
    await assert.rejects(
      () =>
        qqWire(
          new URL("https://upload.qq.com/part?private=opaque"),
          Buffer.from("x"),
          {},
          AbortSignal.timeout(1000),
          "PUT",
        ),
      (e) => e.code === "upload_dns_private" && !e.message.includes("opaque"),
    );
  }
});
test("bulk upload admission is acquired before reading the large authenticated request body", async (t) => {
  const { app, upload } = await setup(t),
    req = http.request(
      app.origin + "/api/admin/v1/qq/media/assets",
      {
        method: "POST",
        headers: {
          origin: app.origin,
          cookie: app.cookie,
          "x-csrf-token": app.csrf,
          "content-type": "application/json",
          "content-length": "100",
        },
      },
      (res) => res.resume(),
    );
  req.on("error", () => {});
  t.after(() => req.destroy());
  req.write("{");
  await until(async () => app.features.channels.outgoing.accepting, Boolean);
  assert.equal((await upload(png, "image")).status, 429);
  req.destroy();
  await until(async () => !app.features.channels.outgoing.accepting, Boolean);
  assert.equal((await upload(png, "image")).status, 201);
});
test("a pending media reply survives a real graceful restart and is dispatched once with its original target and deadline", async (t) => {
  const { app, calls, upload, queue, endpoints } = await setup(t),
    a = await upload(png, "image"),
    q = await queue(a.data.id),
    deadline = app.store.get("SELECT deadline FROM run_targets").deadline;
  await app.close();
  const resumed = await fixture({
    dataDir: app.dataDir,
    testChannelEndpoints: endpoints,
  });
  t.after(() => resumed.close());
  assert.equal((await terminal(resumed, q.data.part_id)).status, "sent");
  assert.equal(
    resumed.store.get("SELECT deadline FROM run_targets").deadline,
    deadline,
  );
  assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 1);
});
test("graceful shutdown after a real final POST keeps an unknown delivery across restart and never repeats it", async (t) => {
  let entered = false,
    release,
    closed = false;
  const { app, calls, upload, queue, endpoints } = await setup(t, {
    send: async (_c, res) => {
      entered = true;
      res.on("close", () => (closed = true));
      await new Promise((r) => (release = r));
      if (!res.destroyed) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end('{"id":"late"}');
      }
    },
  });
  t.after(() => release?.());
  const a = await upload(png, "image"),
    q = await queue(a.data.id);
  await until(async () => entered, Boolean);
  await app.close();
  await until(async () => closed, Boolean);
  release();
  const resumed = await fixture({
    dataDir: app.dataDir,
    testChannelEndpoints: endpoints,
  });
  t.after(() => resumed.close());
  assert.equal((await terminal(resumed, q.data.part_id)).status, "unknown");
  await new Promise((r) => setTimeout(r, 350));
  assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 1);
});
test("persisted upload-only and final-send checkpoints recover as failed and unknown respectively without fabricating a receipt", async (t) => {
  for (const started of [false, true]) {
    const { app, calls, upload, queue, endpoints } = await setup(t),
      a = await upload(png, "image"),
      q = await queue(a.data.id);
    await app.close();
    const db = new DatabaseSync(join(app.dataDir, "prometheus.db"));
    db.prepare("UPDATE delivery_parts SET status='sending' WHERE id=?").run(
      q.data.part_id,
    );
    db.prepare(
      "UPDATE qq_media_transfers SET stage=?,request_started_at=?,message_started_at=? WHERE part_id=?",
    ).run(
      started ? "sending" : "uploading",
      now(),
      started ? now() : null,
      q.data.part_id,
    );
    db.close();
    const resumed = await fixture({
      dataDir: app.dataDir,
      testChannelEndpoints: endpoints,
    });
    t.after(() => resumed.close());
    assert.equal(
      (await terminal(resumed, q.data.part_id)).status,
      started ? "unknown" : "failed",
    );
    assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 0);
  }
});
test("expiry and source revocation erase live asset bytes and block queued media before a message POST", async (t) => {
  for (const revoke of [false, true]) {
    const { app, calls, upload, queue } = await setup(t),
      a = await upload(png, "image"),
      q = await queue(a.data.id);
    if (revoke) app.runtime.invalidate("qq:123456:c2c:sender1");
    else {
      app.store.run("UPDATE qq_media_assets SET expires_at=0");
      app.features.channels.outgoing.prune();
    }
    await until(
      async () =>
        app.store.get(
          "SELECT status FROM delivery_parts WHERE id=?",
          q.data.part_id,
        ).status,
      (s) => !["queued", "sending"].includes(s),
    );
    assert.equal(
      app.store.get("SELECT length(bytes) AS n FROM qq_media_assets").n,
      0,
    );
    assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 0);
  }
});
test("a model cannot use a manually uploaded but unoffered asset or choose a recipient even in the same conversation", async (t) => {
  let forged;
  const { app, calls, upload, receive } = await setup(t, {
      reply: () =>
        forged
          ? JSON.stringify([
              { type: "image", text: "forged", asset_id: forged, delay_ms: 0 },
            ])
          : "合成文字回复。",
    }),
    a = await upload(png, "image");
  forged = a.data.id;
  await receive("forged", undefined, "X".repeat(360));
  await until(
    async () =>
      app.store.get("SELECT status FROM runs ORDER BY rowid DESC LIMIT 1")
        .status,
    (s) => s === "failed",
  );
  assert.equal(calls.filter((c) => c.body?.msg_type === 7).length, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM qq_media_plans").n, 0);
});
