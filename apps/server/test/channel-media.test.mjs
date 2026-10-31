import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import {
  createCipheriv,
  createDecipheriv,
  randomUUID,
  createHash,
} from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import dns from "node:dns";
import { syncBuiltinESMExports } from "node:module";
import { DatabaseSync } from "node:sqlite";
import sharp from "sharp";
import { fixture, until } from "./helpers.mjs";
import { qqSignature } from "../dist/channels.js";
import { now, hash, privateDirectory } from "../dist/security.js";
const root = fileURLToPath(new URL("../../../", import.meta.url)),
  runtime = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? root);
const { weixinCdn, weixinKey, downloadWeixin } = await import(
  pathToFileURL(join(runtime, "apps/server/dist/weixin-media.js"))
);
const { visualSourceAllowed } = await import(
  pathToFileURL(join(runtime, "apps/server/dist/vision.js"))
);
const { Store } = await import(
  pathToFileURL(join(runtime, "apps/server/dist/store.js"))
);
const image = await sharp({
    create: { width: 2, height: 2, channels: 4, background: "red" },
  })
    .png()
    .toBuffer(),
  video = readFileSync(
    new URL("./fixtures/synthetic-h264.mp4", import.meta.url),
  ),
  voice = Buffer.from("#!SILK_V3 synthetic transport only"),
  file = Buffer.from("opaque attachment, never interpreted or executed"),
  aes = Buffer.from("0123456789abcdef");
const crypt = (b, k = aes, decrypt = false) => {
  const c = decrypt
    ? createDecipheriv("aes-128-ecb", k, null)
    : createCipheriv("aes-128-ecb", k, null);
  return Buffer.concat([c.update(b), c.final()]);
};
async function server(t, handler) {
  const calls = [],
    s = createServer(async (req, res) => {
      const chunks = [];
      for await (const b of req) chunks.push(b);
      const call = {
        path: req.url,
        method: req.method,
        headers: req.headers,
        raw: Buffer.concat(chunks),
      };
      if (call.raw.length && req.headers["content-type"]?.includes("json"))
        call.body = JSON.parse(call.raw);
      calls.push(call);
      const value = await handler(call, res);
      if (value !== undefined && !res.headersSent) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify(value));
      }
    });
  await new Promise((r) => s.listen(0, "127.0.0.1", r));
  t.after(
    () =>
      new Promise((r) => {
        s.closeAllConnections();
        s.close(r);
      }),
  );
  return { calls, url: "http://127.0.0.1:" + s.address().port };
}
async function setup(t, options = {}) {
  const blobs = new Map(),
    updates = [],
    uploads = [];
  let base = "",
    cursor = 0;
  const cdn = await server(t, async (c, res) => {
    if (options.cdn) return options.cdn(c, res, blobs);
    if (c.method === "POST") {
      uploads.push(c);
      res.writeHead(200, { "x-encrypted-param": "synthetic-private-download" });
      res.end();
    } else {
      const b = blobs.get(new URL(c.path, "http://localhost").pathname);
      res.writeHead(b ? 200 : 404, {
        "content-type": "application/octet-stream",
      });
      res.end(b ?? "");
    }
  });
  const remote = await server(t, async (c, res) => {
    if (c.path.startsWith("/ilink/bot/get_bot_qrcode"))
      return {
        qrcode: "synthetic-private-qr",
        qrcode_img_content: "https://example.invalid/qr",
      };
    if (c.path.startsWith("/ilink/bot/get_qrcode_status"))
      return {
        status: "confirmed",
        ilink_bot_id: "bot@im.bot",
        ilink_user_id: "owner@im.wechat",
        bot_token: "synthetic-private-token",
        baseurl: base,
      };
    if (c.path === "/ilink/bot/getupdates")
      return {
        ret: 0,
        get_updates_buf: "cursor-" + ++cursor,
        msgs: updates.splice(0),
      };
    if (c.path === "/ilink/bot/getuploadurl") {
      if (options.prepare) return options.prepare(c, res);
      return { upload_full_url: cdn.url + "/upload" };
    }
    if (c.path === "/ilink/bot/sendmessage") {
      if (c.body.msg.item_list[0].type !== 1 && options.send)
        return options.send(c, res);
      return { ret: 0 };
    }
    if (c.path === "/app/getAppAccessToken")
      return { access_token: "synthetic-qq-token", expires_in: 3600 };
    if (c.path.startsWith("/v2/"))
      return c.path.endsWith("/files")
        ? { file_info: "synthetic-file-info" }
        : { id: "qq-sent" };
    if (options.model) return options.model(c, res);
    return {
      choices: [
        {
          message: {
            content:
              c.body?.model === "vision"
                ? "合成图片有红色像素。"
                : "合成文字回复。",
          },
          finish_reason: "stop",
        },
      ],
    };
  });
  base = remote.url;
  const app = await fixture({
    testLocalMediaRunner: options.localRunner,
    testChannelEndpoints: {
      qq: base,
      weixin: base,
      weixinCdn: cdn.url,
      qqMedia: cdn.url,
    },
  });
  t.after(() => app.close());
  await app.bootstrap();
  if (options.native !== false) await app.configure(base);
  const login = await app.call("/channels/weixin/login", "POST", {
    expected_revision: 0,
  });
  assert.equal(login.status, 202);
  await until(
    () => app.call("/channels/weixin/login/" + login.data.id),
    (v) => v.data.status === "confirmed",
  );
  const secret = (
    await app.call("/secrets", "POST", {
      kind: "qq_app_secret",
      value: "synthetic-qq-secret",
    })
  ).data;
  const c = (await app.call("/config")).data;
  c.config.weixin.enabled = true;
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
  if (options.vision !== false) {
    const p = (await app.call("/media/policy")).data;
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
          { headers: { "if-match": '"' + p.revision + '"' } },
        )
      ).status,
      200,
    );
  }
  function message(kind, bytes, extra = {}) {
    const n = randomUUID(),
      path = "/blob-" + n;
    blobs.set(path, crypt(bytes));
    const type = { image: 2, voice: 3, file: 4, video: 5 }[kind],
      key = kind + "_item";
    return {
      message_id: n,
      from_user_id: "owner@im.wechat",
      to_user_id: "bot@im.bot",
      message_type: 1,
      message_state: 2,
      create_time_ms: Date.now(),
      context_token: "synthetic-private-context",
      item_list: [
        {
          type,
          [key]: {
            media: {
              full_url: cdn.url + path,
              aes_key: aes.toString("base64"),
              encrypt_type: 1,
            },
            ...(kind === "voice"
              ? { text: "FAKE_ASR_NEVER_TRUST", encode_type: 6 }
              : {}),
            ...(kind === "file" ? { len: String(bytes.length) } : {}),
          },
        },
      ],
      ...extra,
    };
  }
  async function receive(m) {
    updates.push(m);
    return until(
      async () =>
        app.store.get(
          "SELECT * FROM channel_events WHERE event_key=?",
          "weixin:bot@im.bot:" + m.message_id,
        ),
      (v) => v?.status === "completed",
    );
  }
  async function sent(run) {
    return until(
      async () =>
        app.store.get(
          "SELECT * FROM delivery_parts WHERE run_id=? AND sequence=1",
          run,
        ),
      (v) => v?.status === "sent",
    );
  }
  async function queue(run, asset, text = "") {
    await sent(run);
    const r = await app.call("/qq/media/deliveries", "POST", {
      run_id: run,
      asset_id: asset,
      text,
      request_id: randomUUID(),
      acknowledge_effects: true,
    });
    assert.equal(r.status, 202, JSON.stringify(r.data));
    return r.data;
  }
  return {
    app,
    remote,
    cdn,
    blobs,
    updates,
    uploads,
    message,
    receive,
    sent,
    queue,
  };
}
for (const kind of ["voice", "video"])
  test(`Weixin ${kind} uses the local understanding pipeline and voice replies upload MP3 with original account context`, async (t) => {
    const localCalls = [],
      mp3 = Buffer.from("ID3 synthetic MP3, engine tested separately");
    const x = await setup(t, {
      localRunner: async (operation, input, options) => {
        localCalls.push({ operation, input, options });
        return {
          receipt: {
            result:
              operation === "asr"
                ? {
                    provider: "synthetic-SenseVoice",
                    text: "微信音频中的合成资料。",
                    seconds: 1,
                    confidence: null,
                    timestamps: null,
                  }
                : operation === "video"
                  ? {
                      provider: "synthetic-PyAV",
                      frames_at: [0],
                      decoded_until: 1.933,
                      frame_sampling_seconds: 10,
                      max_frames: 6,
                      audio: { state: "none" },
                    }
                  : { provider: "synthetic-Piper" },
          },
          files: new Map(
            operation === "video"
              ? [["frame-0.png", image]]
              : operation === "tts"
                ? [["speech.mp3", mp3]]
                : [],
          ),
          isolation: { active_processes: 0 },
          elapsed_ms: 1,
        };
      },
      model: (c) => ({
        choices: [
          {
            message: {
              content:
                c.body.model === "vision"
                  ? "采样画面含红色像素。"
                  : JSON.stringify([{ type: "voice", text: "微信合成语音。" }]),
            },
            finish_reason: "stop",
          },
        ],
      }),
    });
    const config = (await x.app.call("/config")).data;
    config.config.speech.enabled = true;
    assert.equal(
      (
        await x.app.call(
          "/config",
          "PUT",
          { config: config.config },
          { headers: { "if-match": `"${config.revision}"` } },
        )
      ).status,
      200,
    );
    const policy = (await x.app.call("/media/policy")).data;
    assert.equal(
      (
        await x.app.call(
          "/media/policy",
          "PUT",
          { ...policy.value, asr_enabled: true, video_enabled: true },
          { headers: { "if-match": `"${policy.revision}"` } },
        )
      ).status,
      200,
    );
    const input = kind === "voice" ? voice : video,
      e = await x.receive(x.message(kind, input));
    await x.sent(e.run_id);
    assert.equal(localCalls[0].operation, kind === "voice" ? "asr" : "video");
    assert.equal(hash(localCalls[0].input), hash(input));
    assert.equal(localCalls.at(-1).operation, "tts");
    assert.equal(localCalls.at(-1).options.encoding, "mp3");
    const native = x.remote.calls.find(
      (c) => c.body?.model === "synthetic-model",
    );
    assert.ok(
      native.body.messages.some(
        (m) =>
          typeof m.content === "string" &&
          m.content.startsWith("PROMETHEUS_LOCAL_MEDIA_SOURCE_V1\n"),
      ),
    );
    assert.ok(
      !JSON.stringify(native.body.messages).includes("FAKE_ASR_NEVER_TRUST"),
    );
    const prep = x.remote.calls.find(
        (c) => c.path === "/ilink/bot/getuploadurl",
      ),
      upload = x.uploads[0];
    assert.deepEqual(
      crypt(upload.raw, Buffer.from(prep.body.aeskey, "hex"), true),
      mp3,
    );
    const sent = x.remote.calls.find(
      (c) =>
        c.path === "/ilink/bot/sendmessage" &&
        c.body.msg.item_list[0].type === 3,
    );
    assert.equal(sent.body.msg.to_user_id, "owner@im.wechat");
    assert.equal(sent.body.msg.context_token, "synthetic-private-context");
    assert.equal(
      x.app.store.get(
        "SELECT status FROM understanding_sources WHERE run_id=?",
        e.run_id,
      ).status,
      "described",
    );
    assert.equal(
      x.app.store.get("SELECT response FROM runs WHERE id=?", e.run_id)
        .response,
      "[语音朗读]微信合成语音。",
    );
  });
for (const kind of ["video", "voice", "file"])
  test(
    "Weixin " +
      kind +
      " is decrypted, source bound, truthfully unread and sent through encrypted CDN without model or ASR",
    async (t) => {
      const x = await setup(t, { native: false, vision: false }),
        b = { video, voice, file }[kind],
        m = x.message(kind, b),
        event = await x.receive(m);
      await x.sent(event.run_id);
      const asset = x.app.store.get(
        "SELECT id,kind,digest FROM qq_media_assets WHERE source_run_id=?",
        event.run_id,
      );
      assert.equal(asset.kind, kind);
      assert.equal(asset.digest, hash(b));
      assert.equal(event.payload_json, null);
      assert.equal(
        x.remote.calls.filter((c) => c.path.endsWith("/chat/completions"))
          .length,
        0,
      );
      assert.equal(
        x.app.features.runtime.getRun(event.run_id, String(event.owner_id))
          .basis,
        "channel_media_transport",
      );
      const source = x.app.store.get(
        "SELECT * FROM messages WHERE run_id=? AND role='user'",
        event.run_id,
      );
      assert.equal(visualSourceAllowed(x.app.store, source, "summary"), false);
      const queued = await x.queue(event.run_id, asset.id);
      await until(
        async () =>
          x.app.store.get(
            "SELECT status FROM delivery_parts WHERE id=?",
            queued.part_id,
          ),
        (v) => v.status === "sent",
      );
      const prepare = x.remote.calls.find(
          (c) => c.path === "/ilink/bot/getuploadurl",
        ),
        send = x.remote.calls.find(
          (c) =>
            c.path === "/ilink/bot/sendmessage" &&
            c.body.msg.item_list[0].type !== 1,
        ),
        item = send.body.msg.item_list[0][kind + "_item"];
      assert.equal(
        prepare.body.media_type,
        { video: 2, voice: 4, file: 3 }[kind],
      );
      assert.equal(prepare.body.rawsize, b.length);
      assert.equal(prepare.body.filesize, Math.ceil((b.length + 1) / 16) * 16);
      assert.deepEqual(
        crypt(x.uploads[0].raw, Buffer.from(prepare.body.aeskey, "hex"), true),
        b,
      );
      assert.deepEqual(
        weixinKey(item.media.aes_key),
        Buffer.from(prepare.body.aeskey, "hex"),
      );
      assert.equal(send.body.msg.to_user_id, "owner@im.wechat");
      assert.equal(send.body.msg.context_token, "synthetic-private-context");
      assert.equal(send.body.msg.client_id, queued.part_id);
      assert.equal(x.uploads[0].headers.authorization, undefined);
      assert.equal(x.cdn.calls[0].headers.authorization, undefined);
      assert.equal(
        send.headers.authorization,
        "Bearer synthetic-private-token",
      );
      const dto = JSON.stringify([
        (await x.app.call("/channels/inbox")).data,
        (await x.app.call("/qq/media/deliveries")).data,
        (await x.app.call("/export")).data,
      ]);
      for (const secret of [
        "synthetic-private-token",
        "synthetic-private-context",
        aes.toString("base64"),
        m.item_list[0][kind + "_item"].media.full_url,
        "FAKE_ASR_NEVER_TRUST",
      ])
        assert.equal(dto.includes(secret), false, secret);
    },
  );
test("Weixin image uses actual Vision then Native; normalized image is offered for a subsequent source-bound media reply", async (t) => {
  const x = await setup(t),
    m = x.message("image", image);
  m.item_list[0].image_item.aeskey = aes.toString("hex");
  m.item_list[0].image_item.media.aes_key =
    "invalid-ignored-outer-key-preferred";
  const event = await x.receive(m);
  await x.sent(event.run_id);
  assert.equal(
    x.app.store.get(
      "SELECT status FROM vision_sources WHERE run_id=?",
      event.run_id,
    ).status,
    "described",
  );
  assert.equal(
    x.remote.calls.filter((c) => c.path.endsWith("/chat/completions")).length,
    2,
  );
  const context = x.app.features.channels.outgoing.context(event.run_id);
  assert.match(context, /制品目录/);
  const asset = x.app.store.get(
      "SELECT id FROM qq_media_assets WHERE source_run_id=?",
      event.run_id,
    ),
    p = await x.queue(event.run_id, asset.id, "单独文字说明");
  await until(
    async () =>
      x.app.store.get(
        "SELECT status FROM delivery_parts WHERE id=?",
        p.part_id,
      ),
    (v) => v.status === "sent",
  );
  const sends = x.remote.calls.filter(
    (c) => c.path === "/ilink/bot/sendmessage",
  );
  assert.equal(sends.length, 3);
  assert.deepEqual(sends[1].body.msg.item_list, [
    { type: 1, text_item: { text: "单独文字说明" } },
  ]);
  assert.equal(sends[2].body.msg.item_list[0].type, 2);
  assert.equal(
    x.app.store.get("SELECT max(sequence) AS n FROM delivery_parts").n,
    3,
  );
});
for (const mime of ["video/mp4", "application/octet-stream"])
  test(
    "QQ inbound " +
      mime +
      " 6.3MB MP4 is validated and archived without claiming video understanding",
    async (t) => {
      const x = await setup(t, { native: false, vision: false }),
        padding = Buffer.alloc(6_300_000 - video.length);
      padding.writeUInt32BE(padding.length);
      padding.write("free", 4);
      const b = Buffer.concat([video, padding]);
      x.blobs.set("/qq-video", b);
      const m = {
        op: 0,
        id: "video-event",
        t: "C2C_MESSAGE_CREATE",
        d: {
          id: "video-message",
          timestamp: new Date().toISOString(),
          author: { user_openid: "sender1" },
          content: "请看看视频",
          attachments: [
            {
              url: x.cdn.url + "/qq-video",
              content_type: mime,
              size: b.length,
            },
          ],
        },
      };
      const raw = JSON.stringify(m),
        timestamp = String(now());
      const post = () =>
        fetch(x.app.origin + "/webhooks/qq", {
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
      assert.equal((await post()).status, 200);
      assert.equal((await post()).status, 200);
      const event = await until(
        async () =>
          x.app.store.get("SELECT * FROM channel_events WHERE channel='qq'"),
        (v) => v?.status === "completed",
      );
      await x.sent(event.run_id);
      assert.equal(
        x.app.store.get("SELECT kind FROM channel_media_sources").kind,
        "video",
      );
      assert.equal(
        x.app.store.get("SELECT count(*) AS n FROM qq_media_assets").n,
        1,
      );
      assert.equal(
        x.app.store.get("SELECT digest FROM qq_media_assets").digest,
        hash(b),
      );
      assert.match(
        x.app.store.get("SELECT response FROM runs WHERE id=?", event.run_id)
          .response,
        /不能读取/,
      );
      assert.equal(
        x.remote.calls.some((c) => c.path.endsWith("/chat/completions")),
        false,
      );
      assert.equal(x.cdn.calls.length, 1);
    },
  );
for (const [name, change, code] of [
  [
    "invalid AES key",
    (m) => (m.item_list[0].file_item.media.aes_key = "bad"),
    "wx_media_key_invalid",
  ],
  [
    "wrong plaintext length",
    (m) => (m.item_list[0].file_item.len = "999"),
    "wx_media_length_mismatch",
  ],
  [
    "wrong digest",
    (m) => (m.item_list[0].file_item.md5 = "0".repeat(32)),
    "wx_media_digest_mismatch",
  ],
  [
    "forbidden URL",
    (m) =>
      (m.item_list[0].file_item.media.full_url = "http://127.0.0.1/private"),
    "wx_media_url_forbidden",
  ],
  [
    "control command",
    (m) =>
      m.item_list.unshift({
        type: 1,
        text_item: { text: "/memory remember do not execute" },
      }),
    "media_control_command",
  ],
  [
    "multiple attachments",
    (m) => m.item_list.push(m.item_list[0]),
    "media_count_unsupported",
  ],
])
  test(
    "Weixin fails closed on " + name + " and clears encrypted payload",
    async (t) => {
      const x = await setup(t, { native: false, vision: false }),
        m = x.message("file", file);
      change(m);
      const e = await x.receive(m);
      await x.sent(e.run_id);
      assert.equal(
        x.app.store.get(
          "SELECT code FROM qq_inbound_receipts WHERE event_id=?",
          e.id,
        ).code,
        code,
      );
      assert.equal(
        x.app.store.get("SELECT count(*) AS n FROM qq_media_assets").n,
        0,
      );
      assert.equal(e.payload_json, null);
      assert.equal(
        x.remote.calls.some((c) => c.path.endsWith("/chat/completions")),
        false,
      );
    },
  );
test("Weixin generation change aborts an admitted download and never commits stale media", async (t) => {
  let begun;
  const started = new Promise((r) => (begun = r));
  const x = await setup(t, {
      native: false,
      vision: false,
      cdn: (c, res) => {
        res.writeHead(200);
        res.write(Buffer.alloc(16));
        begun();
      },
    }),
    m = x.message("file", file);
  x.updates.push(m);
  await started;
  const row = x.app.store.get(
    "SELECT * FROM channel_state WHERE channel='weixin'",
  );
  x.app.store.run(
    "UPDATE channel_state SET generation=? WHERE channel='weixin'",
    randomUUID(),
  );
  await until(
    async () => x.app.store.get("SELECT status FROM channel_events"),
    (v) => v?.status === "cancelled",
  );
  assert.equal(x.app.store.get("SELECT count(*) AS n FROM runs").n, 0);
  assert.equal(x.app.features.channels.incoming.active, undefined);
  assert.ok(row.generation);
});
test("Weixin context revocation after CDN upload prevents final message POST", async (t) => {
  let app, run;
  const x = await setup(t, {
    native: false,
    vision: false,
    cdn: (c, res, blobs) => {
      if (c.method === "POST") {
        app.features.channels.invalidateIncoming(
          "weixin:bot@im.bot:owner@im.wechat",
        );
        res.writeHead(200, { "x-encrypted-param": "synthetic-download" });
        res.end();
      } else {
        res.writeHead(200);
        res.end(blobs.get(c.path));
      }
    },
  });
  app = x.app;
  const e = await x.receive(x.message("file", file));
  run = e.run_id;
  const a = app.store.get("SELECT id FROM qq_media_assets"),
    p = await x.queue(run, a.id);
  await until(
    async () =>
      app.store.get("SELECT status FROM delivery_parts WHERE id=?", p.part_id),
    (v) => ["failed", "cancelled"].includes(v.status),
  );
  assert.equal(
    x.remote.calls.filter(
      (c) =>
        c.path === "/ilink/bot/sendmessage" &&
        c.body.msg.item_list[0].type !== 1,
    ).length,
    0,
  );
  assert.equal(
    app.store.get("SELECT message_started_at FROM qq_media_transfers")
      .message_started_at,
    null,
  );
});
for (const status of [202, 500])
  test(
    "Weixin final HTTP " +
      status +
      " stays unknown without retry after restart",
    async (t) => {
      const x = await setup(t, {
        native: false,
        vision: false,
        send: (c, res) => {
          res.writeHead(status, { "content-type": "application/json" });
          res.end('{"ret":0}');
        },
      });
      const e = await x.receive(x.message("file", file)),
        a = x.app.store.get("SELECT id FROM qq_media_assets"),
        p = await x.queue(e.run_id, a.id);
      await until(
        async () =>
          x.app.store.get(
            "SELECT status FROM delivery_parts WHERE id=?",
            p.part_id,
          ),
        (v) => v.status === "unknown",
      );
      await new Promise((r) => setTimeout(r, 400));
      assert.equal(
        x.remote.calls.filter(
          (c) =>
            c.path === "/ilink/bot/sendmessage" &&
            c.body.msg.item_list[0].type !== 1,
        ).length,
        1,
      );
      assert.ok(
        x.app.store.get("SELECT request_finished_at FROM qq_media_transfers")
          .request_finished_at,
      );
      await x.app.close();
      const reopened = new Store(x.app.dataDir);
      try {
        assert.equal(
          reopened.get(
            "SELECT status FROM delivery_parts WHERE id=?",
            p.part_id,
          ).status,
          "unknown",
        );
      } finally {
        reopened.close();
      }
    },
  );
test("Weixin platform business rejection is failed and upload rejection never starts a message", async (t) => {
  const x = await setup(t, {
    native: false,
    vision: false,
    send: () => ({ ret: 123 }),
  });
  const e = await x.receive(x.message("file", file)),
    a = x.app.store.get("SELECT id FROM qq_media_assets"),
    p = await x.queue(e.run_id, a.id);
  await until(
    async () =>
      x.app.store.get(
        "SELECT status FROM delivery_parts WHERE id=?",
        p.part_id,
      ),
    (v) => v.status === "failed",
  );
  assert.equal(
    x.app.store.get("SELECT error_code FROM qq_media_transfers").error_code,
    "channel_rejected",
  );
});
test("Weixin expired media is not fetched and duplicate IDs preserve original deadline", async (t) => {
  const x = await setup(t, { native: false, vision: false }),
    m = x.message("file", file, { create_time_ms: Date.now() - 3_700_000 });
  x.updates.push(m);
  const e = await until(
    async () => x.app.store.get("SELECT * FROM channel_events"),
    (v) => v?.status === "expired",
  );
  x.updates.push({ ...m, create_time_ms: Date.now() });
  await new Promise((r) => setTimeout(r, 500));
  assert.equal(
    x.app.store.get("SELECT count(*) AS n FROM channel_events").n,
    1,
  );
  assert.equal(
    x.app.store.get("SELECT deadline FROM channel_events").deadline,
    e.deadline,
  );
  assert.equal(x.cdn.calls.length, 0);
});
test("Weixin byte quota rolls back event and cursor; duplicate is accepted even at queue capacity", async (t) => {
  const x = await setup(t, { native: false, vision: false }),
    e = await x.receive(x.message("file", file));
  await x.sent(e.run_id);
  const state = x.app.store.get(
      "SELECT * FROM channel_state WHERE channel='weixin'",
    ),
    account = x.app.store.unseal(state.value_json),
    m = x.message("file", file);
  x.app.store.run(
    "UPDATE channel_events SET status='queued',ready_at=?,payload_json=? WHERE id=?",
    Date.now() + 60000,
    "x".repeat(32 * 1024 * 1024),
    e.id,
  );
  const old = x.app.store
    .get("SELECT event_key FROM channel_events")
    .event_key.split(":")
    .at(-1);
  assert.doesNotThrow(() =>
    x.app.features.channels.ingestWeixin(account, { ...m, message_id: old }),
  );
  assert.throws(
    () => x.app.features.channels.ingestWeixin(account, m),
    /队列已满/,
  );
  assert.equal(
    x.app.store.get("SELECT count(*) AS n FROM channel_events").n,
    1,
  );
  assert.equal(
    x.app.store.get(
      "SELECT value_json FROM channel_state WHERE channel='weixin'",
    ).value_json,
    state.value_json,
  );
  x.app.store.run(
    "UPDATE channel_events SET status='completed',payload_json=NULL WHERE id=?",
    e.id,
  );
});
test("Weixin source catalog can produce a later Native media reply but cannot fabricate asset or recipient", async (t) => {
  let offered;
  const x = await setup(t, {
      vision: false,
      model: (c) => {
        offered = c.body.messages[0].content;
        const assets = JSON.parse(
          offered.split("制品目录：")[1].split("\n")[0],
        );
        return {
          choices: [
            {
              message: {
                content: JSON.stringify([
                  { type: "file", text: "", asset_id: assets[0].id },
                ]),
              },
              finish_reason: "stop",
            },
          ],
        };
      },
    }),
    first = await x.receive(x.message("file", file));
  await x.sent(first.run_id);
  const second = x.message("file", file);
  second.item_list = [
    { type: 1, text_item: { text: "请把刚才文件回传给我。" } },
  ];
  const e = await x.receive(second);
  await x.sent(e.run_id);
  assert.match(offered, /未读取帧、音轨或正文/);
  assert.equal(
    x.app.store.get("SELECT kind FROM delivery_parts WHERE run_id=?", e.run_id)
      .kind,
    "file",
  );
  assert.throws(
    () =>
      x.app.features.channels.outgoing.resolve(
        e.run_id,
        JSON.stringify([
          {
            type: "file",
            text: "",
            asset_id: randomUUID(),
            target: "attacker",
          },
        ]),
      ),
    /字段|unknown|未允许/,
  );
});
for (const raw of [
  "http://novac2c.cdn.weixin.qq.com/c2c",
  "https://cdn.weixin.qq.com.evil.invalid/",
  "https://novac2c.cdn.weixin.qq.com:444/",
  "https://u:p@novac2c.cdn.weixin.qq.com/",
  "https://127.0.0.1/",
  "https://novac2c.cdn.weixin.qq.com/x#fragment",
])
  test("Weixin CDN rejects " + raw, () =>
    assert.throws(() => weixinCdn(raw), /微信媒体/),
  );
test("Weixin accepts raw/base64-hex AES keys, plaintext image, rejects bad padding, redirects, length overflow and private DNS", async (t) => {
  assert.deepEqual(weixinKey(aes.toString("base64")), aes);
  assert.deepEqual(
    weixinKey(Buffer.from(aes.toString("hex")).toString("base64")),
    aes,
  );
  for (const k of ["a", "!!!!", "a".repeat(128)])
    assert.throws(() => weixinKey(k));
  const cdn = await server(t, (c, res) => {
    if (c.path === "/redirect") {
      res.writeHead(302, { location: "/plain" });
      res.end();
    } else if (c.path === "/large") {
      res.writeHead(200, { "content-length": 30 * 1024 * 1024 });
      res.end();
    } else {
      res.writeHead(200);
      res.end(c.path === "/plain" ? image : Buffer.alloc(17));
    }
  });
  const load = (p) =>
    downloadWeixin(
      { kind: "image", reference: { full_url: cdn.url + p } },
      new AbortController().signal,
      cdn.url,
    );
  assert.deepEqual(await load("/plain"), image);
  await assert.rejects(load("/redirect"));
  await assert.rejects(load("/large"));
  await assert.rejects(
    downloadWeixin(
      {
        kind: "file",
        reference: {
          full_url: cdn.url + "/bad",
          aes_key: aes.toString("base64"),
        },
      },
      new AbortController().signal,
      cdn.url,
    ),
  );
  const original = dns.lookup;
  dns.lookup = (_host, _opts, cb) =>
    cb(null, [
      { address: "127.0.0.1", family: 4 },
      { address: "8.8.8.8", family: 4 },
    ]);
  syncBuiltinESMExports();
  try {
    await assert.rejects(
      downloadWeixin(
        {
          kind: "image",
          reference: {
            full_url: "https://novac2c.cdn.weixin.qq.com/c2c/download?p=x",
          },
        },
        new AbortController().signal,
      ),
      (e) => e.code === "wx_media_dns_private",
    );
  } finally {
    dns.lookup = original;
    syncBuiltinESMExports();
  }
});
test("schema19 snapshot upgrade preserves identity and an intact schema19 backup", () => {
  const dir = privateDirectory(
      resolve(root, ".local/prometheus/tests/wx-upgrade-" + randomUUID()),
    ),
    db = new DatabaseSync(join(dir, "prometheus.db"));
  db.exec(readFileSync(new URL("./schema19.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES(?,?)").run("prometheus.schema", "19");
  db.prepare("INSERT INTO metadata VALUES(?,?)").run(
    "assistant.identity",
    "schema19-stable",
  );
  db.close();
  const store = new Store(dir);
  try {
    assert.equal(store.assistantId, "schema19-stable");
    assert.equal(
      store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
        .value,
      "21",
    );
    const backup = new DatabaseSync(
      join(
        dir,
        "backups",
        readdirSync(join(dir, "backups")).find((n) => n.endsWith(".db")),
      ),
      { readOnly: true },
    );
    try {
      assert.equal(
        backup
          .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
          .get().value,
        "19",
      );
      assert.equal(
        backup.prepare("PRAGMA integrity_check").get().integrity_check,
        "ok",
      );
    } finally {
      backup.close();
    }
  } finally {
    store.close();
  }
});
test("Weixin final receipt preserves uint64 message ID while missing business receipt stays unknown", async (t) => {
  let confirmed = true;
  const x = await setup(t, {
      native: false,
      vision: false,
      send: (_c, res) => {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(
          confirmed
            ? '{"ret":0,"message_id":18446744073709551615}'
            : '{"message_id":"unconfirmed"}',
        );
      },
    }),
    e = await x.receive(x.message("file", file)),
    a = x.app.store.get("SELECT id FROM qq_media_assets"),
    p = await x.queue(e.run_id, a.id);
  const receipt = await until(
    async () =>
      x.app.store.get("SELECT * FROM delivery_parts WHERE id=?", p.part_id),
    (v) => v.status === "sent",
  );
  assert.equal(receipt.platform_id, "18446744073709551615");
  confirmed = false;
  const q = await x.queue(e.run_id, a.id);
  await until(
    async () =>
      x.app.store.get(
        "SELECT status FROM delivery_parts WHERE id=?",
        q.part_id,
      ),
    (v) => v.status === "unknown",
  );
  assert.equal(
    x.remote.calls.filter(
      (c) =>
        c.path === "/ilink/bot/sendmessage" &&
        c.body.msg.item_list[0].type !== 1,
    ).length,
    2,
  );
});
test("Weixin exact 20MiB file is encrypted and sent; an extra byte is rejected before upload", async (t) => {
  const x = await setup(t, { native: false, vision: false }),
    e = await x.receive(x.message("file", file));
  await x.sent(e.run_id);
  const bytes = Buffer.alloc(20 * 1024 * 1024, 42),
    value = {
      run_id: e.run_id,
      kind: "file",
      data_base64: bytes.toString("base64"),
      request_id: randomUUID(),
    };
  const accepted = await x.app.call("/qq/media/assets", "POST", value);
  assert.equal(accepted.status, 201, JSON.stringify(accepted.data));
  const p = await x.queue(e.run_id, accepted.data.id);
  await until(
    async () =>
      x.app.store.get(
        "SELECT status FROM delivery_parts WHERE id=?",
        p.part_id,
      ),
    (v) => v.status === "sent",
  );
  assert.equal(x.uploads[0].raw.length, bytes.length + 16);
  const rejected = await x.app.call("/qq/media/assets", "POST", {
    ...value,
    request_id: randomUUID(),
    data_base64: Buffer.alloc(bytes.length + 1).toString("base64"),
  });
  assert.equal(rejected.status, 400);
  assert.equal(
    x.remote.calls.filter((c) => c.path === "/ilink/bot/getuploadurl").length,
    1,
  );
});
test("old Weixin contexts without generation retain chat history but cannot acquire media authority", async (t) => {
  const x = await setup(t, { native: false, vision: false }),
    e = await x.receive(x.message("file", file));
  await x.sent(e.run_id);
  const row = x.app.store.get(
      "SELECT target_json FROM run_targets WHERE run_id=?",
      e.run_id,
    ),
    target = x.app.store.unseal(row.target_json);
  delete target.generation;
  x.app.store.run(
    "UPDATE run_targets SET target_json=? WHERE run_id=?",
    x.app.store.seal(target),
    e.run_id,
  );
  assert.equal(x.app.features.channels.outgoing.contexts().length, 0);
  assert.throws(() => x.app.features.channels.outgoing.live(e.run_id), /来源/);
  assert.ok(
    x.app.store.get("SELECT response FROM runs WHERE id=?", e.run_id).response,
  );
});
test("Weixin upload rejection stays failed before message fence; opaque received bytes are erased on scope revocation", async (t) => {
  const x = await setup(t, {
      native: false,
      vision: false,
      prepare: () => ({ ret: 13 }),
    }),
    e = await x.receive(x.message("file", file)),
    a = x.app.store.get("SELECT id FROM qq_media_assets"),
    p = await x.queue(e.run_id, a.id);
  await until(
    async () =>
      x.app.store.get(
        "SELECT status FROM delivery_parts WHERE id=?",
        p.part_id,
      ),
    (v) => v.status === "failed",
  );
  assert.equal(x.cdn.calls.filter((c) => c.method === "POST").length, 0);
  assert.equal(
    x.app.store.get("SELECT message_started_at FROM qq_media_transfers")
      .message_started_at,
    null,
  );
  x.app.features.channels.invalidateIncoming(String(e.owner_id));
  assert.equal(
    x.app.store.get("SELECT length(bytes) AS n FROM qq_media_assets").n,
    0,
  );
  assert.equal(x.app.store.get("SELECT count(*) AS n FROM messages").n, 2);
});
