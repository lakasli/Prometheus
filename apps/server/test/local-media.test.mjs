import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import sharp from "sharp";
import { fixture, until } from "./helpers.mjs";
import { resolve, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
const runtimeRoot = resolve(
  process.env.PROMETHEUS_RELEASE_ROOT ??
    fileURLToPath(new URL("../../../", import.meta.url)),
);
const { qqSignature } = await import(
  pathToFileURL(join(runtimeRoot, "apps/server/dist/channels.js"))
);
const { now, hash, passwordHash } = await import(
  pathToFileURL(join(runtimeRoot, "apps/server/dist/security.js"))
);
const { visualSourceAllowed } = await import(
  pathToFileURL(join(runtimeRoot, "apps/server/dist/vision.js"))
);
const image = await sharp({
    create: { width: 2, height: 2, channels: 4, background: "red" },
  })
    .png()
    .toBuffer(),
  video = readFileSync(
    new URL("./fixtures/synthetic-h264.mp4", import.meta.url),
  );
function wav() {
  const b = Buffer.alloc(6444);
  b.write("RIFF");
  b.writeUInt32LE(b.length - 8, 4);
  b.write("WAVEfmt ", 8);
  b.writeUInt32LE(16, 16);
  b.writeUInt16LE(1, 20);
  b.writeUInt16LE(1, 22);
  b.writeUInt32LE(16000, 24);
  b.writeUInt32LE(32000, 28);
  b.writeUInt16LE(2, 32);
  b.writeUInt16LE(16, 34);
  b.write("data", 36);
  b.writeUInt32LE(b.length - 44, 40);
  return b;
}
async function setup(t, options = {}) {
  const calls = [],
    localCalls = [],
    blobs = new Map();
  const remote = createServer(async (req, res) => {
    const chunks = [];
    for await (const b of req) chunks.push(b);
    const raw = Buffer.concat(chunks),
      body = raw.length ? JSON.parse(raw) : null,
      call = { path: req.url, body, headers: req.headers };
    calls.push(call);
    if (req.url.startsWith("/blob/")) {
      const b = blobs.get(req.url);
      res.writeHead(200, { "content-type": "application/octet-stream" });
      res.end(b);
      return;
    }
    let result;
    if (req.url === "/app/getAppAccessToken")
      result = { access_token: "synthetic-access-token", expires_in: 3600 };
    else if (req.url.endsWith("/files"))
      result = { file_info: "synthetic-private-file-info" };
    else if (req.url.endsWith("/messages"))
      result = { id: "confirmed-" + randomUUID() };
    else
      result = {
        choices: [
          {
            message: {
              content:
                body.model === "vision"
                  ? "0秒采样画面有红色块。"
                  : typeof options.reply === "function"
                    ? options.reply(body)
                    : (options.reply ?? "根据媒体资料的合成回复。"),
            },
            finish_reason: "stop",
          },
        ],
      };
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(result));
  });
  await new Promise((r) => remote.listen(0, "127.0.0.1", r));
  t.after(
    () =>
      new Promise((r) => {
        remote.closeAllConnections();
        remote.close(r);
      }),
  );
  const url = "http://127.0.0.1:" + remote.address().port;
  const runner = async (kind, input, opts, signal) => {
    localCalls.push({ kind, input, opts, signal });
    if (options.runner) return options.runner(kind, input, opts, signal);
    return {
      receipt: {
        status: "completed",
        kind,
        isolation: { app_container: 1, capabilities: 0, in_job: 1 },
        result:
          kind === "asr"
            ? {
                provider: "synthetic-ASR",
                text: options.transcript ?? "这段音频提到今天阳光很好。",
                seconds: 0.2,
                confidence: options.confidence ?? null,
                timestamps: null,
              }
            : kind === "video"
              ? {
                  provider: "synthetic-video-decoder",
                  frames_at: [0],
                  decoded_until: 1.933,
                  frame_sampling_seconds: 10,
                  max_frames: 6,
                  audio: options.videoAudio ?? { state: "none" },
                }
              : { provider: "synthetic-Piper" },
      },
      files: new Map(
        kind === "tts"
          ? [
              [
                opts.encoding === "mp3" ? "speech.mp3" : "speech.wav",
                opts.encoding === "mp3"
                  ? Buffer.from("ID3 synthetic mp3 transport")
                  : wav(),
              ],
            ]
          : kind === "video"
            ? [["frame-0.png", image]]
            : [],
      ),
      isolation: {
        active_processes: 0,
        peak_memory_bytes: 1048576,
        cpu_100ns: 100000,
      },
      elapsed_ms: 1,
    };
  };
  const app = await fixture({
    testLocalMediaRunner: runner,
    testChannelEndpoints: { qq: url, weixin: url, qqMedia: url },
  });
  t.after(() => {
    options.cleanup?.();
    return app.close();
  });
  await app.bootstrap();
  await app.configure(url);
  const secret = (
      await app.call("/secrets", "POST", {
        kind: "qq_app_secret",
        value: "synthetic-local-media-secret",
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
  async function policy(asr = true, vid = false) {
    const p = (await app.call("/media/policy")).data;
    const result = await app.call(
      "/media/policy",
      "PUT",
      {
        vision_enabled: true,
        vision_model: "vision",
        image_generation_enabled: false,
        image_model: "",
        asr_enabled: asr,
        video_enabled: vid,
      },
      { headers: { "if-match": '"' + p.revision + '"' } },
    );
    assert.equal(result.status, 200, JSON.stringify(result.data));
  }
  await policy(options.asr !== false, !!options.video);
  async function speech(enabled = true) {
    const c = (await app.call("/config")).data;
    c.config.speech.enabled = enabled;
    const r = await app.call(
      "/config",
      "PUT",
      { config: c.config },
      { headers: { "if-match": '"' + c.revision + '"' } },
    );
    assert.equal(r.status, 200, JSON.stringify(r.data));
  }
  async function receive(kind = "voice", text = "请听听这段音频。") {
    const id = randomUUID(),
      path = "/blob/" + id,
      b = kind === "video" ? video : wav();
    blobs.set(path, b);
    const value = {
        op: 0,
        id: "event-" + id,
        t: "C2C_MESSAGE_CREATE",
        d: {
          id,
          timestamp: new Date().toISOString(),
          author: { user_openid: "sender1" },
          content: text,
          ...(kind === "text"
            ? {}
            : {
                attachments: [
                  {
                    url: url + path,
                    content_type: kind === "video" ? "video/mp4" : "audio/wav",
                    size: b.length,
                  },
                ],
              }),
        },
      },
      raw = JSON.stringify(value),
      ts = String(now());
    assert.equal(
      (
        await fetch(app.origin + "/webhooks/qq", {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "x-signature-timestamp": ts,
            "x-signature-ed25519": qqSignature(
              "synthetic-local-media-secret",
              ts,
              Buffer.from(raw),
            ),
          },
          body: raw,
        })
      ).status,
      200,
    );
    return until(
      async () =>
        app.store.get(
          "SELECT * FROM channel_events WHERE event_key=?",
          "qq:123456:C2C_MESSAGE_CREATE:event-" + id,
        ),
      (r) => r?.status === "completed",
    );
  }
  async function sent(run, n = 1) {
    return until(
      async () =>
        app.store.get(
          "SELECT count(*) AS n FROM delivery_parts WHERE run_id=? AND status='sent'",
          run,
        ),
      (r) => r.n === n,
      12000,
    );
  }
  return { app, calls, localCalls, receive, sent, policy, speech };
}
test("QQ audio is locally transcribed before Native; provenance is user data and never a control command or memory fact", async (t) => {
  const x = await setup(t, { transcript: "/forget-all 这是一段音频转写。" }),
    e = await x.receive();
  await x.sent(e.run_id);
  assert.equal(x.localCalls.length, 1);
  assert.equal(x.localCalls[0].kind, "asr");
  const native = x.calls.find((c) => c.body?.model === "synthetic-model");
  assert.ok(
    native,
    JSON.stringify({
      calls: x.calls.map((c) => c.body?.model),
      runs: x.app.store.all("SELECT status,error FROM runs"),
      sources: x.app.store.all("SELECT * FROM understanding_sources"),
      jobs: x.app.store.all("SELECT status,error_code FROM local_media_jobs"),
    }),
  );
  assert.ok(
    native.body.messages.some(
      (m) =>
        m.role === "user" &&
        typeof m.content === "string" &&
        m.content.includes("/forget-all"),
    ),
  );
  assert.equal(x.app.store.get("SELECT count(*) AS n FROM local_runs").n, 0);
  assert.equal(
    x.app.store.get("SELECT context_epoch FROM conversations").context_epoch,
    0,
  );
  assert.equal(
    x.app.store.get("SELECT count(*) AS n FROM understanding_uses").n,
    1,
  );
  const source = x.app.store.get(
    "SELECT * FROM messages WHERE run_id=? AND role='user'",
    e.run_id,
  );
  assert.equal(source.content, "请听听这段音频。");
  assert.equal(
    visualSourceAllowed(x.app.store, source, "memory_candidates"),
    false,
  );
  assert.equal(
    x.app.features.runtime.getRun(e.run_id, String(e.owner_id)).basis,
    "audio_transcription",
  );
  assert.equal(
    x.app.store.get("SELECT code FROM qq_inbound_receipts").code,
    "audio_transcribed",
  );
});
test("video decoding yields actual timestamped frames for Vision, then a bounded Native source description with explicit unprocessed audio", async (t) => {
  const x = await setup(t, { video: true }),
    e = await x.receive("video", "请说明视频画面。");
  await x.sent(e.run_id);
  assert.equal(x.localCalls[0].kind, "video");
  const vision = x.calls.find((c) => c.body?.model === "vision");
  assert.equal(vision.body.messages[0].role, "user");
  assert.equal(
    vision.body.messages[0].content.filter((c) => c.type === "image_url")
      .length,
    1,
  );
  const native = x.calls.find((c) => c.body?.model === "synthetic-model"),
    context = native.body.messages.find(
      (m) =>
        typeof m.content === "string" && m.content.includes("frame_coverage"),
    );
  assert.equal(context.role, "user");
  assert.match(context.content, /sampled/);
  assert.match(context.content, /none/);
  assert.equal(
    x.app.features.runtime.getRun(e.run_id, String(e.owner_id)).basis,
    "video_sample_description",
  );
  assert.equal(
    x.app.store.get("SELECT code FROM qq_inbound_receipts").code,
    "video_frames_described",
  );
});
test("unconfigured ASR keeps transport truthful and does not create local jobs or a model request", async (t) => {
  const x = await setup(t, { asr: false }),
    e = await x.receive();
  await x.sent(e.run_id);
  assert.equal(x.localCalls.length, 0);
  assert.equal(
    x.calls.some((c) => c.body?.messages),
    false,
  );
  assert.equal(
    x.app.store.get("SELECT code FROM qq_inbound_receipts").code,
    "media_voice_unread",
  );
});
for (const options of [
  { transcript: "password: private-transcribed-secret" },
  { confidence: 0.99 },
])
  test("invalid or sensitive ASR output is withheld, retains a failed Run and sends a truthful text notice", async (t) => {
    const x = await setup(t, options),
      e = await x.receive();
    await x.sent(e.run_id);
    assert.equal(x.app.store.get("SELECT status FROM runs").status, "failed");
    assert.equal(
      x.calls.some((c) => c.body?.messages),
      false,
    );
    assert.equal(
      x.app.store.get("SELECT result_json FROM understanding_sources")
        .result_json,
      null,
    );
    const message = x.calls.find((c) => c.path.endsWith("/messages"));
    assert.match(message.body.content, /没有完成/);
  });
test("source revocation retains the local physical slot until the worker actually closes and suppresses its late transcript", async (t) => {
  let release, started;
  const begun = new Promise((r) => (started = r));
  let abortObserved = false;
  const x = await setup(t, {
    cleanup: () => release?.(),
    runner: async (kind, _b, _o, signal) => {
      signal.addEventListener("abort", () => (abortObserved = true));
      started();
      await new Promise((r) => (release = r));
      return {
        receipt: {
          status: "completed",
          kind,
          isolation: { app_container: 1, capabilities: 0, in_job: 1 },
          result: {
            text: "LATE_PRIVATE_TRANSCRIPT",
            provider: "synthetic",
            seconds: 0.2,
            confidence: null,
            timestamps: null,
          },
        },
        files: new Map(),
        isolation: { active_processes: 0 },
        elapsed_ms: 1,
      };
    },
  });
  const e = await x.receive();
  await begun;
  try {
    x.app.features.runtime.invalidate(String(e.owner_id));
    assert.equal(abortObserved, true);
    assert.equal(x.app.features.localMedia.worker.running, true);
    assert.equal(x.app.features.runtime.runningCount, 1);
  } finally {
    release();
  }
  await until(
    async () => x.app.features.localMedia.worker.running,
    (v) => v === false,
  );
  assert.equal(
    x.calls.some((c) => c.body?.messages),
    false,
  );
  assert.equal(
    x.app.store.get("SELECT result_json FROM understanding_sources")
      .result_json,
    null,
  );
});
test("an actually used audio source is excluded from summary and cannot re-enter Native history after media policy revocation", async (t) => {
  const x = await setup(t),
    first = await x.receive();
  await x.sent(first.run_id);
  const next = await x.receive("text", "请继续说明刚才的音频。".repeat(30));
  await x.sent(next.run_id);
  const m = x.app.store.get(
    "SELECT * FROM messages WHERE run_id=? AND role='assistant'",
    next.run_id,
  );
  assert.equal(visualSourceAllowed(x.app.store, m, "summary"), false);
  await x.policy(false, false);
  const third = await x.receive("text", "换个话题，继续聊天。".repeat(30));
  await x.sent(third.run_id);
  const last = x.calls.filter((c) => c.body?.messages).at(-1);
  assert.equal(
    JSON.stringify(last.body).includes("这段音频提到今天阳光很好"),
    false,
  );
});
test("evicting the original audio turn preserves transitive provenance, summary exclusion and later revocation", async (t) => {
  const marker = "AUDIO_DERIVED_SHOULD_NOT_SURVIVE_REVOCATION";
  const x = await setup(t, { reply: marker });
  const config = (await x.app.call("/config")).data;
  config.config.runtime.max_history_messages = 2;
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
  const first = await x.receive();
  await x.sent(first.run_id);
  for (let n = 0; n < 3; n++) {
    const next = await x.receive(
      "text",
      ("继续讨论音频第" + n + "次。").repeat(30),
    );
    await x.sent(next.run_id);
    assert.equal(
      x.app.store.get(
        "SELECT source_run_id FROM understanding_uses WHERE run_id=?",
        next.run_id,
      ).source_run_id,
      first.run_id,
    );
    const message = x.app.store.get(
      "SELECT * FROM messages WHERE run_id=? AND role='assistant'",
      next.run_id,
    );
    assert.equal(visualSourceAllowed(x.app.store, message, "summary"), false);
    assert.ok(
      JSON.stringify(
        x.calls.filter((c) => c.body?.model === "synthetic-model").at(-1).body
          .messages,
      ).includes("这段音频提到今天阳光很好"),
    );
  }
  await x.policy(false, false);
  const next = await x.receive("text", "现在讨论一个无关的新话题。".repeat(30));
  await x.sent(next.run_id);
  const body = JSON.stringify(
    x.calls.filter((c) => c.body?.model === "synthetic-model").at(-1).body
      .messages,
  );
  assert.equal(body.includes(marker), false);
  assert.equal(body.includes("这段音频提到今天阳光很好"), false);
});
test("Native voice text becomes a source-bound, proven audio asset and QQ sends it only after synthesis succeeds", async (t) => {
  const x = await setup(t, {
    reply: JSON.stringify([{ type: "voice", text: "你好，今天阳光很好。" }]),
  });
  await x.speech();
  const e = await x.receive("text", "请用语音回复这句话。".repeat(30));
  await x.sent(e.run_id);
  assert.equal(x.localCalls[0].kind, "tts");
  const asset = x.app.store.get(
    "SELECT source_kind,source_id,digest FROM qq_media_assets",
  );
  assert.equal(asset.source_kind, "generated");
  assert.equal(asset.digest, hash(wav()));
  assert.equal(
    x.app.store.get(
      "SELECT status FROM local_media_jobs WHERE id=?",
      asset.source_id,
    ).status,
    "completed",
  );
  assert.equal(
    x.calls.find((c) => c.path.endsWith("/files")).body.file_type,
    3,
  );
  assert.equal(
    x.calls.find((c) => c.path.endsWith("/messages")).body.msg_type,
    7,
  );
});
test("TTS failure preserves a readable text reply and never uploads fabricated audio", async (t) => {
  const x = await setup(t, {
    reply: JSON.stringify([
      { type: "voice", text: "你好，这是应当保留的文字。" },
    ]),
    runner: async () => {
      throw Error("synthetic engine failure");
    },
  });
  await x.speech();
  const e = await x.receive("text", "请用语音回复。".repeat(40));
  await x.sent(e.run_id);
  assert.equal(x.calls.filter((c) => c.path.endsWith("/files")).length, 0);
  assert.equal(
    x.calls.find((c) => c.path.endsWith("/messages")).body.content,
    "你好，这是应当保留的文字。",
  );
  assert.equal(
    x.app.store.get("SELECT status FROM local_media_jobs").status,
    "failed",
  );
});
test("authenticated synthesis is queued and idempotent, private WAV is owner scoped, and explicit deletion releases it", async (t) => {
  const x = await setup(t);
  await x.speech();
  const body = { text: "这是一段本地试听文字。", request_id: randomUUID() };
  assert.equal(
    (await x.app.call("/speech/syntheses", "POST", body, { auth: false }))
      .status,
    401,
  );
  const a = await x.app.call("/speech/syntheses", "POST", body);
  assert.equal(a.status, 202);
  assert.equal(
    (await x.app.call("/speech/syntheses", "POST", body)).data.id,
    a.data.id,
  );
  const job = await until(
    async () =>
      (await x.app.call("/speech")).data.jobs.find((j) => j.id === a.data.id),
    (j) => j?.status === "completed",
  );
  assert.ok(job.result.asset_id);
  const r = await fetch(
    x.app.origin + "/api/admin/v1/speech/assets/" + job.result.asset_id,
    { headers: { cookie: x.app.cookie } },
  );
  assert.equal(r.status, 200);
  assert.equal(r.headers.get("content-type"), "audio/wav");
  assert.equal(hash(Buffer.from(await r.arrayBuffer())), hash(wav()));
  assert.throws(() =>
    x.app.features.localMedia.asset("foreign-owner", job.result.asset_id),
  );
  await x.app.call("/speech/assets/" + job.result.asset_id, "DELETE", {});
  assert.equal(
    (
      await fetch(
        x.app.origin + "/api/admin/v1/speech/assets/" + job.result.asset_id,
        { headers: { cookie: x.app.cookie } },
      )
    ).status,
    404,
  );
  assert.equal(x.localCalls.length, 1);
});
test("queued preview cancellation uses revisions and discards the original text without running an engine", async (t) => {
  const x = await setup(t);
  await x.speech();
  const a = await x.app.call("/speech/syntheses", "POST", {
    text: "取消这段尚未开始的试听。",
    request_id: randomUUID(),
  });
  assert.equal(
    (
      await x.app.call("/speech/jobs/" + a.data.id, "DELETE", undefined, {
        headers: { "if-match": '"99"' },
      })
    ).status,
    409,
  );
  assert.equal(
    (
      await x.app.call("/speech/jobs/" + a.data.id, "DELETE", undefined, {
        headers: { "if-match": '"1"' },
      })
    ).status,
    200,
  );
  assert.equal(
    x.app.store.get("SELECT status,input_json FROM local_media_jobs").status,
    "cancelled",
  );
  assert.equal(
    x.app.store.get("SELECT input_json FROM local_media_jobs").input_json,
    null,
  );
  assert.equal(x.localCalls.length, 0);
});
test("disabled TTS converts a requested voice sentence to actual plain text without an engine or JSON leakage", async (t) => {
  const x = await setup(t, {
      reply: '[{"type":"voice","text":"保留这句可读的文字。"}]',
    }),
    e = await x.receive("text", "请朗读。".repeat(90));
  await x.sent(e.run_id);
  assert.equal(
    x.calls.find((c) => c.path.endsWith("/messages")).body.content,
    "保留这句可读的文字。",
  );
  assert.equal(x.localCalls.length, 0);
});
test("multiple generated voice parts keep their readable transcript, ordered fixed target, individual proofs and original four-part budget", async (t) => {
  const x = await setup(t, {
    reply: JSON.stringify([
      { type: "voice", text: "第一句你好。", delay_ms: 0 },
      { type: "voice", text: "第二句再见。", delay_ms: 1500 },
    ]),
  });
  await x.speech();
  const e = await x.receive("text", "请分两句朗读。".repeat(60));
  await x.sent(e.run_id, 2);
  const response = x.app.store.get(
    "SELECT response FROM runs WHERE id=?",
    e.run_id,
  ).response;
  assert.match(response, /第一句你好/);
  assert.match(response, /第二句再见/);
  assert.equal(x.localCalls.length, 2);
  assert.deepEqual(
    x.calls.filter((c) => c.body?.msg_type === 7).map((c) => c.body.msg_seq),
    [1, 2],
  );
  assert.equal(
    x.app.store.get(
      "SELECT count(*) AS n FROM local_media_jobs WHERE status='completed'",
    ).n,
    2,
  );
  assert.match(
    x.app.store.get(
      "SELECT content FROM delivery_parts ORDER BY sequence LIMIT 1",
    ).content,
    /语音朗读/,
  );
});
test("preview limits reject oversized text, unknown executable parameters and idempotency conflicts before a worker is started", async (t) => {
  const x = await setup(t);
  await x.speech();
  for (const body of [
    { text: "字".repeat(301), request_id: randomUUID() },
    {
      text: "正常文字",
      request_id: randomUUID(),
      executable: "C:/Windows/System32/cmd.exe",
    },
  ])
    assert.equal(
      (await x.app.call("/speech/syntheses", "POST", body)).status,
      400,
    );
  const id = randomUUID();
  await x.app.call("/speech/syntheses", "POST", {
    text: "第一份文字",
    request_id: id,
  });
  assert.equal(
    (
      await x.app.call("/speech/syntheses", "POST", {
        text: "其他文字",
        request_id: id,
      })
    ).status,
    400,
  );
  assert.equal(x.localCalls.length, 0);
});
test("an active preview is only marked cancelled after its physical worker closes; no late WAV becomes readable", async (t) => {
  let release, entered;
  const started = new Promise((r) => (entered = r)),
    x = await setup(t, {
      cleanup: () => release?.(),
      runner: async (kind, _input, _options, signal) => {
        entered();
        await new Promise((r) => (release = r));
        return {
          receipt: {
            status: "completed",
            kind,
            isolation: { app_container: 1, capabilities: 0, in_job: 1 },
            result: { provider: "synthetic" },
          },
          files: new Map([["speech.wav", wav()]]),
          isolation: { active_processes: 0 },
          elapsed_ms: 1,
        };
      },
    });
  await x.speech();
  const job = (
    await x.app.call("/speech/syntheses", "POST", {
      text: "应取消的试听。",
      request_id: randomUUID(),
    })
  ).data;
  await started;
  try {
    const running = (await x.app.call("/speech")).data.jobs.find(
      (j) => j.id === job.id,
    );
    assert.equal(
      (
        await x.app.call("/speech/jobs/" + job.id, "DELETE", undefined, {
          headers: { "if-match": '"' + running.revision + '"' },
        })
      ).data.status,
      "cancelling",
    );
    assert.equal(x.app.features.localMedia.worker.running, true);
    assert.equal(
      x.app.store.get("SELECT count(*) AS n FROM local_audio_assets").n,
      0,
    );
  } finally {
    release();
  }
  await until(
    async () =>
      x.app.store.get("SELECT status FROM local_media_jobs WHERE id=?", job.id)
        .status,
    (s) => s === "cancelled",
  );
  assert.equal(x.app.features.localMedia.worker.running, false);
  assert.equal(
    x.app.store.get("SELECT count(*) AS n FROM local_audio_assets").n,
    0,
  );
});
test("owner invalidation and asset TTL erase generated preview bytes without inventing a new synthesis", async (t) => {
  const x = await setup(t);
  await x.speech();
  const owner = (await x.app.call("/auth/me")).data.user.id,
    a = (
      await x.app.call("/speech/syntheses", "POST", {
        text: "短期试听。",
        request_id: randomUUID(),
      })
    ).data;
  await until(
    async () =>
      x.app.store.get("SELECT status FROM local_media_jobs WHERE id=?", a.id)
        .status,
    (s) => s === "completed",
  );
  const asset = x.app.store.get("SELECT id FROM local_audio_assets");
  x.app.store.run("UPDATE local_audio_assets SET expires_at=?", now() - 1);
  assert.throws(() => x.app.features.localMedia.asset(owner, asset.id));
  assert.equal(
    (await x.app.call("/speech")).data.jobs[0].asset_available,
    false,
  );
  const second = (
    await x.app.call("/speech/syntheses", "POST", {
      text: "另一段短期试听。",
      request_id: randomUUID(),
    })
  ).data;
  await until(
    async () =>
      x.app.store.get(
        "SELECT status FROM local_media_jobs WHERE id=?",
        second.id,
      ).status,
    (s) => s === "completed",
  );
  x.app.features.runtime.invalidate(owner);
  assert.equal(
    x.app.store.get("SELECT count(*) AS n FROM local_audio_assets").n,
    0,
  );
  assert.equal(x.localCalls.length, 2);
});
test("video without ASR consent cannot accept an engine-supplied transcript as authorized audio understanding", async (t) => {
  const x = await setup(t, {
      asr: false,
      video: true,
      videoAudio: {
        state: "transcribed",
        text: "UNAUTHORIZED_AUDIO_TRANSCRIPT",
        seconds: 0.2,
        confidence: null,
        timestamps: null,
        provider: "synthetic",
      },
    }),
    e = await x.receive("video", "只描述画面。");
  await x.sent(e.run_id);
  assert.equal(x.localCalls[0].opts.transcribe_audio, false);
  assert.equal(x.app.store.get("SELECT status FROM runs").status, "failed");
  assert.equal(
    x.calls.some((c) => c.body?.model === "synthetic-model"),
    false,
  );
  assert.equal(
    x.app.store.get("SELECT result_json FROM understanding_sources")
      .result_json,
    null,
  );
});
test("restart marks accepted preview work interrupted, clears its encrypted input and never replays it automatically", async (t) => {
  const x = await setup(t);
  await x.speech();
  const job = (
    await x.app.call("/speech/syntheses", "POST", {
      text: "服务退出前尚未开始的试听。",
      request_id: randomUUID(),
    })
  ).data;
  await x.app.close();
  const restarted = await fixture({
    dataDir: x.app.dataDir,
    testLocalMediaRunner: async () => {
      throw Error("must not replay");
    },
  });
  t.after(() => restarted.close());
  assert.equal(
    restarted.store.get(
      "SELECT status FROM local_media_jobs WHERE id=?",
      job.id,
    ).status,
    "interrupted",
  );
  assert.equal(
    restarted.store.get(
      "SELECT input_json FROM local_media_jobs WHERE id=?",
      job.id,
    ).input_json,
    null,
  );
  assert.equal(
    restarted.store.get("SELECT count(*) AS n FROM local_audio_assets").n,
    0,
  );
});
