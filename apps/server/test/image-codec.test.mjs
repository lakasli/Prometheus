import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer, request as httpRequest } from "node:http";
import { createHash, randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { deflateSync, createDeflate } from "node:zlib";
import { Readable } from "node:stream";
import { writeFileSync, readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import sharp from "sharp";
import pngjs from "pngjs";
import { fixture, until } from "./helpers.mjs";
import { privateDirectory } from "../dist/security.js";

function chunk(name, data) {
  const b = Buffer.concat([Buffer.from(name), data]),
    out = Buffer.alloc(data.length + 12);
  out.writeUInt32BE(data.length);
  b.copy(out, 4);
  let crc = 0xffffffff;
  for (const x of b) {
    crc ^= x;
    for (let k = 0; k < 8; k++)
      crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
  }
  out.writeUInt32BE((crc ^ 0xffffffff) >>> 0, data.length + 8);
  return out;
}
function png({
  width = 1,
  height = 1,
  color = 6,
  depth = 8,
  interlace = 0,
  raw = Buffer.from([0, 255, 0, 0, 255]),
  extra = [],
} = {}) {
  const h = Buffer.alloc(13);
  h.writeUInt32BE(width);
  h.writeUInt32BE(height, 4);
  h[8] = depth;
  h[9] = color;
  h[12] = interlace;
  return Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    chunk("IHDR", h),
    ...extra,
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
async function setup(t) {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const c = (await app.call("/conversations", "POST", {})).data;
  return {
    app,
    c,
    upload: (bytes) =>
      app.call("/media/assets", "POST", {
        conversation_id: c.id,
        data_base64: bytes.toString("base64"),
      }),
  };
}
async function pixels(app, asset) {
  const r = await fetch(app.origin + "/api/admin/v1/media/assets/" + asset.id, {
    headers: { cookie: app.cookie },
  });
  assert.equal(r.status, 200);
  assert.equal(r.headers.get("content-type"), "image/png");
  const bytes = Buffer.from(await r.arrayBuffer()),
    image = pngjs.PNG.sync.read(bytes);
  assert.equal(createHash("sha256").update(bytes).digest("hex"), asset.sha256);
  assert.deepEqual([image.width, image.height], [asset.width, asset.height]);
  assert.equal(bytes[24], 8);
  assert.equal(bytes[25], 6);
  let at = 8,
    names = [];
  while (at < bytes.length) {
    names.push(bytes.toString("latin1", at + 4, at + 8));
    at += bytes.readUInt32BE(at) + 12;
  }
  assert.deepEqual([...new Set(names)], ["IHDR", "IDAT", "IEND"]);
  return { ...image, bytes };
}
async function uploaded(app, upload, input) {
  const r = await upload(input);
  assert.equal(r.status, 201, JSON.stringify(r.data));
  return { asset: r.data, ...(await pixels(app, r.data)) };
}
function gif() {
  const header = Buffer.from("47494638396101000100800000ff00000000ff", "hex"),
    frame = (index) =>
      Buffer.concat([
        Buffer.from("21f90400010000002c0000000001000100000202", "hex"),
        Buffer.from([index ? 0x4c : 0x44, 1, 0]),
      ]);
  return Buffer.concat([header, frame(0), frame(1), Buffer.from([0x3b])]);
}
function bmp({
  width = 2,
  height = 1,
  bits = 24,
  core = false,
  topDown = false,
  palette = [],
  compression = 0,
  masks = [],
  gap = 0,
  payload = Buffer.from([0, 0, 255, 0, 255, 0, 0, 0]),
} = {}) {
  const size = core ? 12 : masks.length ? 56 : 40,
    offset = 14 + size + palette.length * (core ? 3 : 4) + gap,
    b = Buffer.alloc(offset + payload.length);
  b.write("BM");
  b.writeUInt32LE(b.length, 2);
  b.writeUInt32LE(offset, 10);
  b.writeUInt32LE(size, 14);
  if (core) {
    b.writeUInt16LE(width, 18);
    b.writeUInt16LE(height, 20);
    b.writeUInt16LE(1, 22);
    b.writeUInt16LE(bits, 24);
  } else {
    b.writeInt32LE(width, 18);
    b.writeInt32LE(topDown ? -height : height, 22);
    b.writeUInt16LE(1, 26);
    b.writeUInt16LE(bits, 28);
    b.writeUInt32LE(compression, 30);
    b.writeUInt32LE(payload.length, 34);
    b.writeUInt32LE(palette.length, 46);
    masks.forEach((x, i) => b.writeUInt32LE(x, 54 + i * 4));
  }
  palette.forEach((rgba, i) => {
    const at = 14 + size + i * (core ? 3 : 4);
    b[at] = rgba[2];
    b[at + 1] = rgba[1];
    b[at + 2] = rgba[0];
  });
  payload.copy(b, offset);
  return b;
}
let noisy;
async function noise() {
  if (!noisy) {
    const w = 2600,
      h = 1000,
      b = Buffer.alloc(w * h * 3);
    let n = 123456;
    for (let i = 0; i < b.length; i++) {
      n = (Math.imul(n, 1664525) + 1013904223) >>> 0;
      b[i] = n >>> 24;
    }
    noisy = await sharp(b, { raw: { width: w, height: h, channels: 3 } })
      .jpeg({ quality: 95 })
      .toBuffer();
    assert.ok(noisy.length > 1048576 && noisy.length < 8 * 1024 * 1024);
  }
  return noisy;
}

test("PNG is decoded and reencoded as actual RGBA pixels with transparency and no text, profile or compressed metadata", async (t) => {
  const { app, upload } = await setup(t),
    r = await uploaded(
      app,
      upload,
      png({
        width: 2,
        raw: Buffer.from([0, 255, 0, 0, 128, 0, 255, 0, 255]),
        extra: [
          chunk("tEXt", Buffer.from("private\0do not persist")),
          chunk("zTXt", Buffer.from("ignored\0\0not-a-zlib-stream")),
          chunk("iCCP", Buffer.from("ignored\0\0not-a-profile")),
        ],
      }),
    );
  assert.deepEqual([...r.data], [255, 0, 0, 128, 0, 255, 0, 255]);
  assert.equal(r.bytes.includes(Buffer.from("persist")), false);
});

test("legal grayscale, palette, sixteen-bit and Adam7 PNGs normalize to the expected independent RGBA pixels", async (t) => {
  const { app, upload } = await setup(t),
    cases = [
      [
        png({ color: 0, depth: 1, raw: Buffer.from([0, 128]) }),
        [255, 255, 255, 255],
      ],
      [
        png({ color: 0, depth: 2, raw: Buffer.from([0, 128]) }),
        [170, 170, 170, 255],
      ],
      [
        png({ color: 0, depth: 4, raw: Buffer.from([0, 128]) }),
        [136, 136, 136, 255],
      ],
      [
        png({ color: 0, depth: 8, raw: Buffer.from([0, 128]) }),
        [128, 128, 128, 255],
      ],
      [
        png({ color: 0, depth: 16, raw: Buffer.from([0, 128, 128]) }),
        [128, 128, 128, 255],
      ],
      [
        png({
          color: 2,
          depth: 16,
          raw: Buffer.from([0, 255, 255, 0, 0, 128, 128]),
        }),
        [255, 0, 128, 255],
      ],
      [
        png({ color: 4, depth: 8, raw: Buffer.from([0, 128, 128]) }),
        [128, 128, 128, 128],
      ],
      [
        png({ color: 4, depth: 16, raw: Buffer.from([0, 128, 128, 128, 128]) }),
        [128, 128, 128, 128],
      ],
      [
        png({
          color: 6,
          depth: 16,
          raw: Buffer.from([0, 255, 255, 0, 0, 128, 128, 128, 128]),
        }),
        [255, 0, 128, 128],
      ],
      [
        png({
          color: 3,
          depth: 1,
          raw: Buffer.from([0, 128]),
          extra: [
            chunk("PLTE", Buffer.from([255, 0, 0, 0, 255, 0])),
            chunk("tRNS", Buffer.from([255, 128])),
          ],
        }),
        [0, 255, 0, 128],
      ],
      [
        png({
          width: 2,
          height: 2,
          interlace: 1,
          raw: Buffer.from([
            0, 255, 0, 0, 255, 0, 0, 255, 0, 255, 0, 0, 0, 255, 255, 255, 255,
            255, 255,
          ]),
        }),
        [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255],
      ],
    ];
  for (const [input, expected] of cases) {
    const r = await uploaded(app, upload, input);
    assert.deepEqual([...r.data], expected);
  }
});

test("JPEG orientation is applied to pixels and dimensions before EXIF and all profiles are discarded", async (t) => {
  const { app, upload } = await setup(t),
    input = await sharp({
      create: { width: 20, height: 10, channels: 3, background: "red" },
    })
      .withMetadata({ orientation: 6 })
      .jpeg({ quality: 100 })
      .toBuffer(),
    r = await uploaded(app, upload, input);
  assert.deepEqual([r.width, r.height], [10, 20]);
  assert.ok(r.data[0] > 245 && r.data[1] < 10 && r.data[2] < 10);
  assert.equal(r.bytes.includes(Buffer.from("Exif")), false);
});

test("all eight EXIF orientations rotate or mirror distinct real JPEG corner pixels correctly", async (t) => {
  const { app, upload } = await setup(t),
    colors = [
      [255, 0, 0],
      [0, 255, 0],
      [0, 0, 255],
      [255, 255, 255],
    ],
    raw = Buffer.alloc(40 * 20 * 3);
  for (let y = 0; y < 20; y++)
    for (let x = 0; x < 40; x++) {
      const c = colors[(y >= 10 ? 2 : 0) + (x >= 20 ? 1 : 0)];
      for (let k = 0; k < 3; k++) raw[(y * 40 + x) * 3 + k] = c[k];
    }
  const expected = [
    [0, 1, 2, 3],
    [1, 0, 3, 2],
    [3, 2, 1, 0],
    [2, 3, 0, 1],
    [0, 2, 1, 3],
    [2, 0, 3, 1],
    [3, 1, 2, 0],
    [1, 3, 0, 2],
  ];
  for (let orientation = 1; orientation <= 8; orientation++) {
    const jpeg = await sharp(raw, {
        raw: { width: 40, height: 20, channels: 3 },
      })
        .withMetadata({ orientation })
        .jpeg({ quality: 100, chromaSubsampling: "4:4:4" })
        .toBuffer(),
      out = await uploaded(app, upload, jpeg),
      w = out.width,
      h = out.height;
    assert.deepEqual([w, h], orientation <= 4 ? [40, 20] : [20, 40]);
    [
      [3, 3],
      [w - 4, 3],
      [3, h - 4],
      [w - 4, h - 4],
    ].forEach(([x, y], i) => {
      const at = (y * w + x) * 4,
        c = colors[expected[orientation - 1][i]];
      for (let k = 0; k < 3; k++)
        assert.ok(
          Math.abs(out.data[at + k] - c[k]) < 12,
          "orientation " + orientation + " corner " + i,
        );
    });
  }
});

test("animated GIF and WebP yield only the first red frame rather than the later blue frame", async (t) => {
  const { app, upload } = await setup(t),
    red = await sharp({
      create: { width: 1, height: 1, channels: 4, background: "red" },
    })
      .png()
      .toBuffer(),
    blue = await sharp({
      create: { width: 1, height: 1, channels: 4, background: "blue" },
    })
      .png()
      .toBuffer(),
    webp = await sharp([red, blue], { join: { animated: true } })
      .webp({ lossless: true, loop: 0, delay: [100, 100] })
      .toBuffer();
  assert.equal((await sharp(webp).metadata()).pages, 2);
  for (const input of [gif(), webp]) {
    const r = await uploaded(app, upload, input);
    assert.deepEqual([...r.data], [255, 0, 0, 255]);
  }
});

test("BMP respects row orientation, padded pixel offsets, OS2 palettes, bitfields and RLE instead of treating headers as pixels", async (t) => {
  const { app, upload } = await setup(t),
    red = [255, 0, 0],
    green = [0, 255, 0],
    cases = [
      [bmp({ gap: 36 }), [...red, 255, ...green, 255]],
      [
        bmp({
          height: 2,
          topDown: true,
          payload: Buffer.from([
            0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0,
          ]),
        }),
        [...red, 255, ...green, 255, 0, 0, 255, 255, 255, 255, 255, 255],
      ],
      [
        bmp({
          bits: 1,
          core: true,
          palette: [red, green],
          payload: Buffer.from([0x40, 0, 0, 0]),
        }),
        [...red, 255, ...green, 255],
      ],
      [
        bmp({
          bits: 4,
          palette: [red, green],
          payload: Buffer.from([0x01, 0, 0, 0]),
        }),
        [...red, 255, ...green, 255],
      ],
      [
        bmp({
          bits: 8,
          palette: [red, green],
          payload: Buffer.from([0, 1, 0, 0]),
        }),
        [...red, 255, ...green, 255],
      ],
      [
        bmp({ bits: 16, payload: Buffer.from([0, 124, 224, 3]) }),
        [...red, 255, ...green, 255],
      ],
      [
        bmp({
          bits: 16,
          compression: 3,
          masks: [0xf800, 0x07e0, 0x001f, 0],
          payload: Buffer.from([0, 248, 224, 7]),
        }),
        [...red, 255, ...green, 255],
      ],
      [
        bmp({
          bits: 32,
          compression: 6,
          masks: [0x00ff0000, 0x0000ff00, 0x000000ff, 0xff000000],
          payload: Buffer.from([0, 0, 255, 128, 0, 255, 0, 255]),
        }),
        [...red, 128, ...green, 255],
      ],
      [
        bmp({
          bits: 4,
          compression: 2,
          palette: [red, green],
          payload: Buffer.from([2, 0x01, 0, 1]),
        }),
        [...red, 255, ...green, 255],
      ],
      [
        bmp({
          bits: 8,
          compression: 1,
          palette: [red, green],
          payload: Buffer.from([1, 0, 1, 1, 0, 1]),
        }),
        [...red, 255, ...green, 255],
      ],
    ];
  for (const [input, expected] of cases) {
    const r = await uploaded(app, upload, input);
    assert.deepEqual([...r.data], expected);
  }
});

test("large noisy photos exceed the old byte limit but are resized by dimensions, pixels and encoded PNG size before storage", async (t) => {
  const { app, upload } = await setup(t),
    r = await uploaded(app, upload, await noise());
  assert.ok(r.width < 2048 && r.height < 1000);
  assert.ok(r.width * r.height <= 2097152);
  assert.ok(r.bytes.length <= 2 * 1024 * 1024);
  assert.ok(r.bytes.length > 1048576);
  assert.ok(
    app.store.get(
      "SELECT length(bytes) AS n FROM media_assets WHERE id=?",
      r.asset.id,
    ).n <=
      2 * 1024 * 1024,
  );
});

test("the original eight-MiB boundary accepts valid metadata at the exact cap and rejects one extra byte without storing input", async (t) => {
  const { app, upload } = await setup(t),
    base = png(),
    padding = Buffer.concat([
      Buffer.from("x\0"),
      Buffer.alloc(8 * 1024 * 1024 - base.length - 14, 65),
    ]),
    large = Buffer.concat([
      base.subarray(0, 33),
      chunk("tEXt", padding),
      base.subarray(33),
    ]);
  assert.equal(large.length, 8 * 1024 * 1024);
  const r = await uploaded(app, upload, large);
  assert.deepEqual([...r.data], [255, 0, 0, 255]);
  assert.equal(
    (await upload(Buffer.concat([large, Buffer.from([0])]))).status,
    400,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 1);
});

test("unsupported formats, disguised magic, malformed pixels and noncanonical base64 are rejected before a Run or asset exists", async (t) => {
  const { app, c, upload } = await setup(t),
    badBmp = bmp();
  badBmp[0] = 0xc2;
  badBmp[1] = 0xcd;
  const corruptPalette = png({
    color: 3,
    depth: 8,
    raw: Buffer.from([0, 1]),
    extra: [chunk("PLTE", Buffer.from([255, 0, 0]))],
  });
  for (const b of [
    Buffer.from(
      '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>',
    ),
    Buffer.from("II*\0unsupported TIFF"),
    Buffer.from("not an image"),
    badBmp,
    bmp({
      bits: 8,
      palette: [[255, 0, 0]],
      payload: Buffer.from([3, 3, 0, 0]),
    }),
    bmp({
      compression: 1,
      bits: 8,
      palette: [[255, 0, 0]],
      payload: Buffer.from([3, 0, 0, 1]),
    }),
    corruptPalette,
    png({ raw: Buffer.from([5, 0, 0, 0, 255]) }),
    png({ interlace: 1, raw: Buffer.from([0]) }),
  ]) {
    const r = await upload(b);
    assert.equal(
      r.status,
      400,
      b.subarray(0, 30).toString("hex") + ":" + JSON.stringify(r.data),
    );
  }
  for (const value of [
    "Zg==",
    "Zh==",
    "Zg",
    "Zg==\n",
    "data:image/png;base64," + png().toString("base64"),
  ])
    assert.equal(
      (
        await app.call("/media/assets", "POST", {
          conversation_id: c.id,
          data_base64: value,
        })
      ).status,
      400,
    );
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM runs").n, 0);
});

test("oversized source dimensions and pixel products are rejected from headers before raster allocation", async (t) => {
  const { app, upload } = await setup(t);
  for (const [w, h] of [
    [12001, 1],
    [6001, 4000],
    [0, 1],
  ])
    for (const b of [
      png({ width: w, height: h }),
      bmp({ width: w, height: h }),
    ])
      assert.equal((await upload(b)).status, 400);
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 0);
});

test("palette indexes are validated after PNG Sub, Up and Paeth filters, including interlaced pass boundaries", async (t) => {
  const { app, upload } = await setup(t),
    palette = chunk("PLTE", Buffer.from([255, 0, 0, 0, 255, 0]));
  for (const filter of [1, 2, 3, 4]) {
    const raw =
      filter === 1
        ? [1, 0, 1, 1, 1, 255]
        : filter === 2
          ? [2, 0, 1, 2, 1, 255]
          : filter === 3
            ? [3, 0, 1, 3, 1, 255]
            : [4, 0, 1, 4, 1, 255];
    const good = await uploaded(
      app,
      upload,
      png({
        width: 2,
        height: 2,
        color: 3,
        raw: Buffer.from(raw),
        extra: [palette],
      }),
    );
    assert.deepEqual(
      [...good.data],
      [255, 0, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255],
    );
  }
  const invalid = png({
    width: 2,
    height: 2,
    color: 3,
    raw: Buffer.from([0, 0, 1, 2, 2, 255]),
    extra: [palette],
  });
  assert.equal((await upload(invalid)).status, 400);
  const adam = png({
    width: 2,
    height: 2,
    color: 3,
    interlace: 1,
    raw: Buffer.from([0, 0, 0, 1, 0, 1, 0]),
    extra: [palette],
  });
  assert.deepEqual(
    [...(await uploaded(app, upload, adam)).data],
    [255, 0, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 255, 0, 0, 255],
  );
});

test("a legacy four-megapixel PNG remains readable without rewriting its old digest, while new inputs obey the lower normalized pixel budget", async (t) => {
  const { app, c, upload } = await setup(t),
    width = 2048,
    height = 2048,
    rows = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++)
      rows[y * (width * 4 + 1) + 1 + x * 4 + 3] = 255;
  const original = png({ width, height, raw: rows }),
    id = randomUUID(),
    digest = createHash("sha256").update(original).digest("hex"),
    at = Math.floor(Date.now() / 1000);
  app.store.run(
    "INSERT INTO media_assets VALUES(?,?,?,?,?,?,?,?,?)",
    id,
    (await app.call("/auth/me")).data.user.id,
    c.id,
    "image",
    "image/png",
    digest,
    original,
    at,
    at + 86400,
  );
  const r = await fetch(app.origin + "/api/admin/v1/media/assets/" + id, {
    headers: { cookie: app.cookie },
  });
  assert.equal(r.status, 200);
  assert.deepEqual(Buffer.from(await r.arrayBuffer()), original);
  assert.equal(
    app.store.get("SELECT digest FROM media_assets WHERE id=?", id).digest,
    digest,
  );
  const normalized = await uploaded(app, upload, original);
  assert.ok(normalized.width * normalized.height <= 2097152);
  assert.notEqual(normalized.asset.sha256, digest);
});

function beginUpload(app, c, bytes) {
  const body = JSON.stringify({
    conversation_id: c.id,
    data_base64: bytes.toString("base64"),
  });
  const req = httpRequest(
    app.origin + "/api/admin/v1/media/assets",
    {
      method: "POST",
      headers: {
        origin: app.origin,
        cookie: app.cookie,
        "x-csrf-token": app.csrf,
        "content-type": "application/json",
        "content-length": Buffer.byteLength(body),
      },
    },
    (res) => res.resume(),
  );
  req.on("error", () => {});
  req.end(body);
  return req;
}
test("a disconnected real decode keeps its processing slot until completion, rejects a competing upload and never saves the orphan result", async (t) => {
  const { app, c, upload } = await setup(t),
    req = beginUpload(app, c, await noise());
  await until(
    async () => !!app.features.media.codec.active,
    (x) => x,
  );
  const active = app.features.media.codec.active;
  const competing = await upload(png());
  assert.equal(competing.status, 429);
  assert.equal(competing.data.error.code, "image_processing_busy");
  req.destroy();
  await active.catch(() => {});
  await until(
    async () => app.features.media.codec.active === undefined,
    (x) => x,
  );
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 0);
  assert.equal((await upload(png())).status, 201);
});

test("service shutdown waits for an admitted real decode, clears its slot and closes storage without saving after shutdown", async (t) => {
  const { app, c } = await setup(t),
    req = beginUpload(app, c, await noise());
  await until(
    async () => !!app.features.media.codec.active,
    (x) => x,
  );
  await app.close();
  req.destroy();
  assert.equal(app.features.media.codec.active, undefined);
  const db = new DatabaseSync(join(app.dataDir, "prometheus.db"), {
    readOnly: true,
  });
  assert.equal(db.prepare("SELECT count(*) AS n FROM media_assets").get().n, 0);
  assert.equal(
    db.prepare("PRAGMA integrity_check").get().integrity_check,
    "ok",
  );
  db.close();
});

async function model(t, image) {
  const calls = [],
    server = createServer(async (req, res) => {
      let raw = "";
      for await (const b of req) raw += b;
      const body = JSON.parse(raw);
      calls.push(body);
      res.writeHead(200, { "content-type": "application/json" });
      res.end(
        req.url.endsWith("/images/generations")
          ? JSON.stringify({ data: [{ b64_json: image.toString("base64") }] })
          : JSON.stringify({
              choices: [
                {
                  message: {
                    content:
                      body.model === "vision"
                        ? "图片是红色的。"
                        : "这条回答来自图片描述。",
                  },
                  finish_reason: "stop",
                },
              ],
            }),
      );
    });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  t.after(
    () =>
      new Promise((r) => {
        server.closeAllConnections();
        server.close(r);
      }),
  );
  return { calls, url: "http://127.0.0.1:" + server.address().port };
}
async function policy(app, generation = false) {
  const p = (await app.call("/media/policy")).data;
  return app.call(
    "/media/policy",
    "PUT",
    {
      vision_enabled: true,
      vision_model: "vision",
      image_generation_enabled: generation,
      image_model: generation ? "image" : "",
    },
    { headers: { "if-match": '"' + p.revision + '"' } },
  );
}
test("normalized common-format pixels reach the real Vision request as bounded PNG, then Native receives scoped user material and temporary input is cleared", async (t) => {
  const { app, c, upload } = await setup(t),
    remote = await model(t, png());
  await app.configure(remote.url);
  await policy(app);
  const jpeg = await sharp({
    create: { width: 1, height: 1, channels: 3, background: "red" },
  })
    .jpeg()
    .toBuffer();
  for (const b of [jpeg, gif(), bmp()]) {
    const a = await upload(b);
    assert.equal(a.status, 201);
    const run = (
      await app.call("/media/assets/" + a.data.id + "/chat", "POST", {
        content: "这张图里有什么？",
        client_message_id: randomUUID(),
      })
    ).data;
    const result = await until(
      () => app.call("/runs/" + run.run_id),
      (r) => !["queued", "running"].includes(r.data.status),
    );
    assert.equal(result.data.status, "completed", JSON.stringify(result.data));
    const request = remote.calls.findLast((v) => v.model === "vision"),
      url = request.messages[1].content[1].image_url.url;
    assert.ok(url.startsWith("data:image/png;base64,"));
    const pixel = pngjs.PNG.sync.read(Buffer.from(url.split(",")[1], "base64"));
    assert.ok(pixel.data[0] > 240);
    const native = remote.calls.at(-1);
    assert.equal(native.messages[0].content.includes("图片是红色的。"), false);
    assert.ok(
      native.messages.some(
        (m) => m.role === "user" && m.content.includes("图片是红色的。"),
      ),
    );
    assert.equal(
      app.store.get(
        "SELECT image FROM vision_sources WHERE run_id=?",
        run.run_id,
      ).image,
      null,
    );
    assert.equal(
      app.store.get(
        "SELECT length(bytes) AS n FROM media_assets WHERE id=?",
        a.data.id,
      ).n,
      0,
    );
  }
  assert.equal(remote.calls.length, 6);
  assert.equal(
    app.store.get(
      "SELECT count(*) AS n FROM messages WHERE conversation_id=?",
      c.id,
    ).n,
    6,
  );
});

test("generation uses the same JPEG normalization and retains its physical model slot during real decode; revoking policy prevents publication", async (t) => {
  const { app, c } = await setup(t),
    remote = await model(t, await noise());
  await app.configure(remote.url);
  await policy(app, true);
  const generating = app.call("/media/generate", "POST", {
    conversation_id: c.id,
    prompt: "生成合成红色图片",
  });
  await until(
    async () => !!app.features.media.codec.active,
    (x) => x,
  );
  assert.equal(app.runtime.runningCount, 1);
  const p = (await app.call("/media/policy")).data;
  const revoked = await app.call(
    "/media/policy",
    "PUT",
    { ...p.value, image_generation_enabled: false, image_model: "" },
    { headers: { "if-match": '"' + p.revision + '"' } },
  );
  assert.equal(revoked.status, 200);
  const result = await generating;
  assert.equal(result.status, 400);
  assert.equal(app.runtime.runningCount, 0);
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 0);
  assert.equal(remote.calls.length, 1);
});

test("a normalized PNG beyond the former request-body ceiling completes real Vision then Native without persisting input pixels", async (t) => {
  const { app, upload } = await setup(t),
    remote = await model(t, png());
  await app.configure(remote.url);
  await policy(app);
  const rgba = Buffer.alloc(700 * 600 * 4);
  let n = 123;
  for (let i = 0; i < rgba.length; i++) {
    n = (Math.imul(n, 1664525) + 1013904223) >>> 0;
    rgba[i] = n >>> 24;
  }
  const source = await sharp(rgba, {
    raw: { width: 700, height: 600, channels: 4 },
  })
    .png()
    .toBuffer();
  const asset = await uploaded(app, upload, source);
  assert.ok(
    Buffer.byteLength(asset.bytes.toString("base64")) > 2 * 1024 * 1024,
  );
  const run = (
    await app.call("/media/assets/" + asset.asset.id + "/chat", "POST", {
      content: "请描述这幅合成图片。",
      client_message_id: randomUUID(),
    })
  ).data;
  const r = await until(
    () => app.call("/runs/" + run.run_id),
    (x) => !["queued", "running"].includes(x.data.status),
  );
  assert.equal(r.data.status, "completed", JSON.stringify(r.data));
  assert.equal(remote.calls.length, 2);
  const url = remote.calls[0].messages[1].content[1].image_url.url;
  assert.deepEqual(Buffer.from(url.split(",")[1], "base64"), asset.bytes);
  assert.equal(
    app.store.get("SELECT image FROM vision_sources WHERE run_id=?", run.run_id)
      .image,
    null,
  );
});

test("JPEG generation returns normalized downloadable PNG and a later vision use clears only its independent temporary copy", async (t) => {
  const { app, c } = await setup(t),
    remote = await model(t, await noise());
  await app.configure(remote.url);
  await policy(app, true);
  const result = await app.call("/media/generate", "POST", {
    conversation_id: c.id,
    prompt: "生成合成图片",
  });
  assert.equal(result.status, 201, JSON.stringify(result.data));
  const image = await pixels(app, result.data);
  assert.ok(image.bytes.length <= 2 * 1024 * 1024);
  assert.equal(
    app.store.get("SELECT kind FROM media_assets WHERE id=?", result.data.id)
      .kind,
    "generated_image",
  );
  const submitted = await app.call(
    "/media/assets/" + result.data.id + "/chat",
    "POST",
    { content: "描述这幅已生成的图片。", client_message_id: randomUUID() },
  );
  assert.equal(submitted.status, 202);
  const run = await until(
    () => app.call("/runs/" + submitted.data.run_id),
    (r) => !["queued", "running"].includes(r.data.status),
  );
  assert.equal(run.data.status, "completed", JSON.stringify(run.data));
  assert.equal(
    app.store.get(
      "SELECT image FROM vision_sources WHERE run_id=?",
      submitted.data.run_id,
    ).image,
    null,
  );
  assert.deepEqual((await pixels(app, result.data)).bytes, image.bytes);
  assert.equal(remote.calls.length, 3);
});

test("anonymous image uploads stay protected and the larger body allowance does not widen configuration or credential routes", async (t) => {
  const { app, c } = await setup(t);
  assert.equal(
    (
      await app.call(
        "/media/assets",
        "POST",
        { conversation_id: c.id, data_base64: png().toString("base64") },
        { auth: false },
      )
    ).status,
    401,
  );
  assert.equal(
    (await app.call("/config", "PUT", { padding: "a".repeat(150000) })).status,
    413,
  );
  assert.equal(
    (await app.call("/secrets", "POST", { padding: "a".repeat(150000) }))
      .status,
    413,
  );
  assert.equal(
    (
      await app.call("/media/assets", "POST", {
        padding: "a".repeat(12 * 1024 * 1024),
      })
    ).status,
    413,
  );
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 0);
});

test("the full 24-million-pixel sixteen-bit PNG source is streamed and normalized within the output budget without retaining its 192MB raw stream", async (t) => {
  const { app, upload } = await setup(t),
    width = 6000,
    height = 4000,
    row = Buffer.alloc(width * 8 + 1);
  for (let x = 0; x < width; x++) {
    row.writeUInt16BE(65535, 1 + x * 8);
    row.writeUInt16BE(65535, 7 + x * 8);
  }
  const parts = [];
  for await (const b of Readable.from(
    (function* () {
      for (let y = 0; y < height; y++) yield row;
    })(),
  ).pipe(createDeflate({ level: 1 })))
    parts.push(b);
  const h = Buffer.alloc(13);
  h.writeUInt32BE(width);
  h.writeUInt32BE(height, 4);
  h[8] = 16;
  h[9] = 6;
  const source = Buffer.concat([
    Buffer.from("89504e470d0a1a0a", "hex"),
    chunk("IHDR", h),
    chunk("IDAT", Buffer.concat(parts)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
  const started = performance.now(),
    baseline = process.memoryUsage().rss;
  let peak = baseline;
  const timer = setInterval(
    () => (peak = Math.max(peak, process.memoryUsage().rss)),
    10,
  );
  let result;
  try {
    result = await uploaded(app, upload, source);
  } finally {
    clearInterval(timer);
  }
  peak = Math.max(peak, process.memoryUsage().rss);
  assert.ok(
    result.width <= 2048 &&
      result.height <= 2048 &&
      result.width * result.height <= 2097152,
  );
  assert.deepEqual([...result.data.subarray(0, 4)], [255, 0, 0, 255]);
  assert.ok(result.bytes.length <= 2 * 1024 * 1024);
  const report = {
    time: new Date().toISOString(),
    commit: (await (await fetch(app.origin + "/health/ready")).json()).commit,
    source_bytes: source.length,
    source_pixels: width * height,
    source_bit_depth: 16,
    raw_scanline_bytes: row.length * height,
    output_bytes: result.bytes.length,
    output_width: result.width,
    output_height: result.height,
    elapsed_ms: performance.now() - started,
    baseline_rss: baseline,
    sampled_peak_rss: peak,
    peak_delta: peak - baseline,
  };
  writeFileSync(
    ".local/prometheus/image-codec-resource.json",
    JSON.stringify(report, null, 2),
  );
});

test("an admitted upload rechecks the shared twenty-image quota after a generated asset acquires a temporary vision copy", async (t) => {
  const { app, c, upload } = await setup(t),
    remote = await model(t, png());
  await app.configure(remote.url);
  await policy(app);
  app.runtime.pump = () => {};
  const existing = (await upload(png())).data;
  app.store.run(
    "UPDATE media_assets SET kind='generated_image' WHERE id=?",
    existing.id,
  );
  for (let i = 0; i < 18; i++)
    app.store.run(
      "INSERT INTO media_assets SELECT ?,owner_id,conversation_id,'image',mime,digest,bytes,created_at,expires_at FROM media_assets WHERE id=?",
      randomUUID(),
      existing.id,
    );
  assert.equal(app.features.media.vision.pendingBytes(), 19);
  const pending = upload(await noise());
  await until(
    async () => !!app.features.media.codec.active,
    (x) => x,
  );
  const queued = await app.call(
    "/media/assets/" + existing.id + "/chat",
    "POST",
    {
      content: "保留这张生成图片的临时来源。",
      client_message_id: randomUUID(),
    },
  );
  assert.equal(queued.status, 202);
  assert.equal(app.features.media.vision.pendingBytes(), 20);
  const blocked = await pending;
  assert.equal(blocked.status, 429);
  assert.equal(blocked.data.error.code, "media_capacity");
  assert.equal(app.store.get("SELECT count(*) AS n FROM media_assets").n, 19);
  assert.equal(app.features.media.vision.pendingBytes(), 20);
  assert.equal(
    (await app.call("/runs/" + queued.data.run_id, "DELETE")).status,
    200,
  );
  assert.equal(app.features.media.vision.pendingBytes(), 19);
  assert.equal((await upload(png())).status, 201);
  assert.equal(remote.calls.length, 0);
});

test("schema16 source data and its diary repair checkpoint survive the format-version upgrade and a second start without being rewound", async (t) => {
  const dir = privateDirectory(
      resolve(".local/prometheus/tests/" + randomUUID()),
    ),
    db = new DatabaseSync(join(dir, "prometheus.db"));
  db.exec(readFileSync(new URL("./schema16.sql", import.meta.url), "utf8"));
  db.prepare("INSERT INTO metadata VALUES(?,?)").run("prometheus.schema", "16");
  db.prepare("INSERT INTO metadata VALUES(?,?)").run(
    "assistant.identity",
    "image-format-upgrade-stable",
  );
  db.prepare("INSERT INTO metadata VALUES(?,?)").run(
    "diary.record_cursor",
    "987654",
  );
  db.close();
  let app = await fixture({ dataDir: dir });
  t.after(() => app.close());
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .value,
    "20",
  );
  assert.equal(app.store.assistantId, "image-format-upgrade-stable");
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='diary.record_cursor'")
      .value,
    "987654",
  );
  await app.close();
  const names = readdirSync(join(dir, "backups"));
  assert.equal(names.length, 1);
  assert.ok(names[0].startsWith("schema16-"));
  const snapshot = new DatabaseSync(join(dir, "backups", names[0]), {
    readOnly: true,
  });
  assert.equal(
    snapshot
      .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .get().value,
    "16",
  );
  assert.equal(
    snapshot
      .prepare("SELECT value FROM metadata WHERE key='diary.record_cursor'")
      .get().value,
    "987654",
  );
  assert.equal(
    snapshot.prepare("PRAGMA integrity_check").get().integrity_check,
    "ok",
  );
  snapshot.close();
  app = await fixture({ dataDir: dir });
  assert.equal(
    app.store.get("SELECT value FROM metadata WHERE key='diary.record_cursor'")
      .value,
    "987654",
  );
  assert.equal(readdirSync(join(dir, "backups")).length, 1);
  assert.equal(app.store.get("PRAGMA integrity_check").integrity_check, "ok");
});
