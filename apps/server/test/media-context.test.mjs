import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { deflateSync } from "node:zlib";
import { fixture, until } from "./helpers.mjs";
function chunk(type, data) {
  const body = Buffer.concat([Buffer.from(type), data]),
    result = Buffer.alloc(data.length + 12);
  result.writeUInt32BE(data.length);
  body.copy(result, 4);
  let crc = 0xffffffff;
  for (const byte of body) {
    crc ^= byte;
    for (let index = 0; index < 8; index++)
      crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
  }
  result.writeUInt32BE((crc ^ 0xffffffff) >>> 0, data.length + 8);
  return result;
}
function png() {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(1, 0);
  header.writeUInt32BE(1, 4);
  header[8] = 8;
  header[9] = 6;
  return Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    chunk("IHDR", header),
    chunk("tEXt", Buffer.from("private\0hidden-raw-metadata")),
    chunk("IDAT", deflateSync(Buffer.from([0, 255, 0, 0, 255]))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function insertPng(input, before, chunks) {
  let offset = 8;
  while (offset < input.length) {
    if (input.toString("ascii", offset + 4, offset + 8) === before)
      return Buffer.concat([
        input.subarray(0, offset),
        ...chunks,
        input.subarray(offset),
      ]);
    offset += input.readUInt32BE(offset) + 12;
  }
  throw new Error("Missing PNG insertion point");
}

test("PNG ingress preserves the old strict static-image boundary and never stores APNG or structurally invalid ancillary data", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const animation = Buffer.alloc(8);
  animation.writeUInt32BE(1);
  const frame = Buffer.alloc(26);
  frame.writeUInt32BE(1, 4);
  frame.writeUInt32BE(1, 8);
  const cases = [
    [
      "non-ASCII chunk name",
      insertPng(png(), "IDAT", [
        chunk(Buffer.from([0xf4, 0x45, 0x58, 0x74]), Buffer.alloc(0)),
      ]),
    ],
    [
      "APNG controls",
      insertPng(png(), "IDAT", [
        chunk("acTL", animation),
        chunk("fcTL", frame),
      ]),
    ],
    [
      "APNG frame data",
      insertPng(png(), "IDAT", [chunk("fdAT", Buffer.from([0, 0, 0, 0]))]),
    ],
    [
      "reserved chunk bit",
      insertPng(png(), "IDAT", [chunk("text", Buffer.alloc(0))]),
    ],
    [
      "palette after pixels",
      insertPng(png(), "IEND", [chunk("PLTE", Buffer.from([1, 2, 3]))]),
    ],
    [
      "duplicate palette",
      insertPng(png(), "IDAT", [
        chunk("PLTE", Buffer.from([1, 2, 3])),
        chunk("PLTE", Buffer.from([1, 2, 3])),
      ]),
    ],
    [
      "invalid palette length",
      insertPng(png(), "IDAT", [chunk("PLTE", Buffer.from([1]))]),
    ],
    [
      "redundant RGBA transparency",
      insertPng(png(), "IDAT", [
        chunk("tRNS", Buffer.from([0, 0, 0, 0, 0, 0])),
      ]),
    ],
    [
      "excessive chunks",
      insertPng(
        png(),
        "IDAT",
        Array.from({ length: 4097 }, () => chunk("tEXt", Buffer.alloc(0))),
      ),
    ],
    [
      "compressed stream suffix",
      (() => {
        const input = png();
        let offset = 8;
        while (input.toString("ascii", offset + 4, offset + 8) !== "IDAT")
          offset += input.readUInt32BE(offset) + 12;
        const length = input.readUInt32BE(offset);
        return Buffer.concat([
          input.subarray(0, offset),
          chunk(
            "IDAT",
            Buffer.concat([
              input.subarray(offset + 8, offset + 8 + length),
              Buffer.from("HIDDEN_TRAILING_BYTES"),
            ]),
          ),
          input.subarray(offset + 12 + length),
        ]);
      })(),
    ],
  ];
  for (const [label, bytes] of cases) {
    const response = await app.call("/media/assets", "POST", {
      conversation_id: conversation.id,
      data_base64: bytes.toString("base64"),
    });
    assert.equal(response.status, 400, label);
  }
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 0);
});
test("valid RGB transparency and suggested RGBA palettes remain usable without retaining textual metadata", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const header = Buffer.alloc(13);
  header.writeUInt32BE(1);
  header.writeUInt32BE(1, 4);
  header[8] = 8;
  header[9] = 2;
  const rgb = Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.from([0, 17, 34, 51]))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
  for (const bytes of [
    insertPng(rgb, "IDAT", [
      chunk("PLTE", Buffer.from([17, 34, 51])),
      chunk("tRNS", Buffer.from([0, 17, 0, 34, 0, 51])),
      chunk("tEXt", Buffer.from("private\0RGB_METADATA_CANARY")),
    ]),
    insertPng(png(), "IDAT", [chunk("PLTE", Buffer.from([255, 0, 0]))]),
  ]) {
    const asset = await app.call("/media/assets", "POST", {
      conversation_id: conversation.id,
      data_base64: bytes.toString("base64"),
    });
    assert.equal(asset.status, 201);
    const response = await fetch(
      app.origin + "/api/admin/v1/media/assets/" + asset.data.id,
      { headers: { cookie: app.cookie } },
    );
    assert.equal(response.status, 200);
    const stored = Buffer.from(await response.arrayBuffer());
    assert.ok(
      !stored.includes(Buffer.from("METADATA_CANARY")) &&
        !stored.includes(Buffer.from("hidden-raw-metadata")),
    );
    assert.equal(
      createHash("sha256").update(stored).digest("hex"),
      asset.data.sha256,
    );
  }
  for (const transparency of [
    Buffer.from([0]),
    Buffer.from([1, 0, 0, 0, 0, 0]),
  ]) {
    assert.equal(
      (
        await app.call("/media/assets", "POST", {
          conversation_id: conversation.id,
          data_base64: insertPng(rgb, "IDAT", [
            chunk("tRNS", transparency),
          ]).toString("base64"),
        })
      ).status,
      400,
    );
  }
});

test("stored legacy PNGs are revalidated before download or chat and an invalid or mismatched artifact is retained without being used", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const asset = await app.call("/media/assets", "POST", {
    conversation_id: conversation.id,
    data_base64: png().toString("base64"),
  });
  assert.equal(asset.status, 201);
  const original = Buffer.from(
      app.store.get("SELECT bytes FROM media_assets WHERE id=?", asset.data.id)
        .bytes,
    ),
    invalid = insertPng(original, "IEND", [
      chunk("PLTE", Buffer.from([1, 2, 3])),
    ]);
  app.store.run(
    "UPDATE media_assets SET bytes=?,digest=? WHERE id=?",
    invalid,
    createHash("sha256").update(invalid).digest("hex"),
    asset.data.id,
  );
  assert.equal(
    (
      await fetch(app.origin + "/api/admin/v1/media/assets/" + asset.data.id, {
        headers: { cookie: app.cookie },
      })
    ).status,
    400,
  );
  assert.equal(
    (
      await app.call("/media/assets/" + asset.data.id + "/chat", "POST", {
        content: "请查看这张图",
      })
    ).status,
    400,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 1);
  app.store.run(
    "UPDATE media_assets SET bytes=?,digest=? WHERE id=?",
    original,
    "0".repeat(64),
    asset.data.id,
  );
  const mismatch = await fetch(
    app.origin + "/api/admin/v1/media/assets/" + asset.data.id,
    { headers: { cookie: app.cookie } },
  );
  assert.equal(mismatch.status, 400);
  assert.equal((await mismatch.json()).error.code, "media_asset_invalid");
});

test("explicit image generation accepts only validated PNG bytes and never follows provider image URLs", async (t) => {
  let count = 0;
  const provider = createServer(async (req, res) => {
    for await (const chunk of req) void chunk;
    count++;
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify(
        count === 1
          ? { data: [{ b64_json: png().toString("base64") }] }
          : { data: [{ url: "https://must-never-fetch.invalid/private" }] },
      ),
    );
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
  assert.equal(
    (
      await app.call(
        "/media/policy",
        "PUT",
        {
          vision_enabled: false,
          vision_model: "",
          image_generation_enabled: true,
          image_model: "synthetic-images",
        },
        { headers: { "if-match": '"0"' } },
      )
    ).status,
    200,
  );
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const first = await app.call("/media/generate", "POST", {
    conversation_id: conversation.id,
    prompt: "合成红色像素",
  });
  assert.equal(first.status, 201);
  assert.equal(first.data.mime, "image/png");
  const second = await app.call("/media/generate", "POST", {
    conversation_id: conversation.id,
    prompt: "合成URL拒绝",
  });
  assert.equal(second.status, 400);
  assert.equal(count, 2);
  assert.equal(app.runtime.runningCount, 0);
});
test("PNG upload strips metadata; actual vision result enters ordinary conversation with source proof", async (t) => {
  const requests = [];
  const server = createServer(async (req, res) => {
    let raw = "";
    for await (const data of req) raw += data;
    const body = JSON.parse(raw);
    requests.push(body);
    const content =
      body.model === "synthetic-vision"
        ? "一块红色像素。"
        : "依据带来源的图像描述回答。";
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ choices: [{ message: { content } }] }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(
    () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(resolve);
      }),
  );
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(`http://127.0.0.1:${server.address().port}`);
  assert.equal(
    (
      await app.call(
        "/media/policy",
        "PUT",
        {
          vision_enabled: true,
          vision_model: "synthetic-vision",
          image_generation_enabled: false,
          image_model: "",
        },
        { headers: { "if-match": '"0"' } },
      )
    ).status,
    200,
  );
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const asset = await app.call("/media/assets", "POST", {
    conversation_id: conversation.id,
    data_base64: png().toString("base64"),
  });
  assert.equal(asset.status, 201);
  const downloaded = await fetch(
    `${app.origin}/api/admin/v1/media/assets/${asset.data.id}`,
    { headers: { cookie: app.cookie } },
  );
  assert.equal(downloaded.status, 200);
  assert.equal(
    Buffer.from(await downloaded.arrayBuffer()).includes(
      Buffer.from("hidden-raw-metadata"),
    ),
    false,
  );
  const run = (
    await app.call(`/media/assets/${asset.data.id}/chat`, "POST", {
      content: "这是什么颜色？",
      client_message_id: "image-source",
    })
  ).data;
  const result = await until(
    () => app.call(`/runs/${run.run_id}`),
    (value) => value.data.status === "completed",
  );
  assert.match(result.data.response, /图像/);
  assert.equal(requests.length, 2);
  assert.match(
    requests[0].messages[1].content[1].image_url.url,
    /^data:image\/png;base64,/,
  );
  const visualFrame = requests[1].messages.find(
    (m) =>
      m.role === "user" &&
      m.content.startsWith("PROMETHEUS_VISION_DESCRIPTION_SOURCE_V1\n"),
  );
  assert.ok(visualFrame);
  assert.match(visualFrame.content, /一块红色像素/);
  assert.ok(visualFrame.content.includes(asset.data.sha256));
  assert.ok(!requests[1].messages[0].content.includes("一块红色像素"));
  const corrupted = png();
  corrupted[corrupted.length - 1] ^= 1;
  assert.equal(
    (
      await app.call("/media/assets", "POST", {
        conversation_id: conversation.id,
        data_base64: corrupted.toString("base64"),
      })
    ).status,
    400,
  );
  assert.equal(
    (await fetch(`${app.origin}/api/admin/v1/media/assets/${asset.data.id}`))
      .status,
    401,
  );
});
test("generated plugin versions retain empty permissions and cannot approve or execute without Windows isolation", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const manifest = {
    name: "合成插件",
    description: "测试制品门禁",
    engine: "wasm_component",
    permissions: [],
    abi: "json.v1",
  };
  assert.equal(
    (
      await app.call("/plugins", "POST", {
        manifest: { ...manifest, permissions: ["network"] },
        source: "fn main() {}",
      })
    ).status,
    400,
  );
  const created = await app.call("/plugins", "POST", {
    manifest,
    source: "fn calculate() -> i32 { 1 }",
  });
  assert.equal(created.status, 201);
  const proof = await app.call(
    `/plugins/${created.data.id}/test`,
    "POST",
    {},
    { headers: { "if-match": '"1"' } },
  );
  assert.equal(proof.data.status, "blocked");
  for (const step of ["approve", "activate", "execute"])
    assert.equal(
      (
        await app.call(
          `/plugins/${created.data.id}/${step}`,
          "POST",
          {},
          { headers: { "if-match": '"2"' } },
        )
      ).status,
      503,
    );
  assert.equal((await app.call("/plugins")).data.execution_available, false);
});
test("context budgets bind to the actual provider and reviewed commitments do not create permissions or schedules", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const annotation = await app.call(
    `/context/conversations/${conversation.id}`,
    "POST",
    {
      kind: "commitment",
      content: "下次聊一聊有来源的阅读计划。",
      expires_at: null,
    },
  );
  assert.equal(annotation.status, 201);
  assert.equal(annotation.data.grants_permissions, false);
  assert.equal(annotation.data.creates_schedule, false);
  const policy = (await app.call("/context/policy")).data;
  policy.value.provider_context_window = 64000;
  policy.value.window_basis = "administrator_declared";
  policy.value.provider_fingerprint = "incorrect";
  assert.equal(
    (
      await app.call("/context/policy", "PUT", policy.value, {
        headers: { "if-match": '"0"' },
      })
    ).status,
    400,
  );
  policy.value.provider_fingerprint = policy.current_provider_fingerprint;
  assert.equal(
    (
      await app.call("/context/policy", "PUT", policy.value, {
        headers: { "if-match": '"0"' },
      })
    ).status,
    200,
  );
  assert.equal((await app.call("/records/schedule")).data.total, 0);
});
test("memory commands remain local and usable when no remote model has been configured", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const send = () =>
    app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "/memory remember 合成本地事实",
      client_message_id: "local-memory",
    });
  const first = await send();
  assert.equal(first.status, 202);
  assert.equal(
    (await app.call(`/runs/${first.data.run_id}`)).data.status,
    "completed",
  );
  assert.equal((await send()).data.run_id, first.data.run_id);
  assert.equal((await app.call("/records/memory")).data.total, 1);
});
