import { bad } from "./errors.ts";
import { MEDIA_LIMIT } from "./qq-upload.ts";
type Box = { kind: string; data: Buffer; start: number; end: number };
function invalid(): never {
  throw bad("媒体容器无效或超限", "media_container_invalid");
}
const u32 = (b: Buffer, n: number) => {
  if (n + 4 > b.length) invalid();
  return b.readUInt32BE(n);
};
const u64 = (b: Buffer, n: number) => {
  if (n + 8 > b.length) invalid();
  const v = Number(b.readBigUInt64BE(n));
  if (!Number.isSafeInteger(v)) invalid();
  return v;
};
function boxes(b: Buffer, base = 0): Box[] {
  const out: Box[] = [];
  for (let at = 0; at < b.length;) {
    if (out.length >= 10000 || b.length - at < 8) invalid();
    const s = u32(b, at),
      header = s === 1 ? 16 : 8,
      size = s === 1 ? u64(b, at + 8) : s === 0 ? b.length - at : s;
    if (size < header || size > b.length - at) invalid();
    out.push({
      kind: b.toString("ascii", at + 4, at + 8),
      data: b.subarray(at + header, at + size),
      start: base + at + header,
      end: base + at + size,
    });
    at += size;
  }
  return out;
}
function one(items: Box[], kind: string) {
  const rows = items.filter((i) => i.kind === kind);
  if (rows.length !== 1) invalid();
  return rows[0]!.data;
}
function time(b: Buffer) {
  const v = b[0],
    scale = v === 0 ? u32(b, 12) : v === 1 ? u32(b, 20) : invalid(),
    duration = v === 0 ? u32(b, 16) : u64(b, 24);
  if (!scale || !duration || duration > scale * 60) invalid();
  return { scale, duration };
}
function avc(b: Buffer) {
  if (b.length < 7 || b[0] !== 1 || (b[4]! & 3) === 2) invalid();
  let at = 6;
  const sps = b[5]! & 31;
  if (!sps) invalid();
  for (let i = 0; i < sps; i++) {
    if (at + 2 > b.length) invalid();
    const n = b.readUInt16BE(at);
    at += 2;
    if (!n || at + n > b.length || (b[at]! & 31) !== 7) invalid();
    at += n;
  }
  const pps = b[at++];
  if (!pps) invalid();
  for (let i = 0; i < pps; i++) {
    if (at + 2 > b.length) invalid();
    const n = b.readUInt16BE(at);
    at += 2;
    if (!n || at + n > b.length || (b[at]! & 31) !== 8) invalid();
    at += n;
  }
}
function ranges(table: Box[], scale: number, mdat: Box[]) {
  const sz = one(table, "stsz"),
    fixed = u32(sz, 4),
    count = u32(sz, 8);
  if (!count || count > 60000 || sz.length !== 12 + (fixed ? 0 : count * 4))
    invalid();
  const sizes = Array.from(
    { length: count },
    (_, i) => fixed || u32(sz, 12 + i * 4),
  );
  if (sizes.some((n) => !n || n > MEDIA_LIMIT)) invalid();
  const ts = one(table, "stts"),
    entries = u32(ts, 4);
  if (!entries || entries > count || ts.length !== 8 + entries * 8) invalid();
  let samples = 0,
    duration = 0;
  for (let i = 0; i < entries; i++) {
    const n = u32(ts, 8 + i * 8),
      delta = u32(ts, 12 + i * 8);
    if (!n || !delta) invalid();
    samples += n;
    duration += n * delta;
    if (!Number.isSafeInteger(duration)) invalid();
  }
  if (samples !== count || duration > scale * 60) invalid();
  const offs = table.filter((b) => ["stco", "co64"].includes(b.kind));
  if (offs.length !== 1) invalid();
  const off = offs[0]!,
    width = off.kind === "co64" ? 8 : 4,
    chunks = u32(off.data, 4);
  if (
    !chunks ||
    chunks > 8192 ||
    chunks > count ||
    off.data.length !== 8 + chunks * width
  )
    invalid();
  const sc = one(table, "stsc"),
    n = u32(sc, 4);
  if (!n || n > chunks || sc.length !== 8 + n * 12) invalid();
  const map: { first: number; count: number }[] = [];
  for (let i = 0; i < n; i++) {
    const first = u32(sc, 8 + i * 12),
      num = u32(sc, 12 + i * 12);
    if (
      !first ||
      first > chunks ||
      (i === 0 && first !== 1) ||
      (i > 0 && map[i - 1]!.first >= first) ||
      !num ||
      num > count ||
      u32(sc, 16 + i * 12) !== 1
    )
      invalid();
    map.push({ first, count: num });
  }
  let sample = 0,
    index = 0;
  const out: [number, number][] = [];
  for (let i = 0; i < chunks; i++) {
    while (index + 1 < map.length && map[index + 1]!.first <= i + 1) index++;
    const num = map[index]!.count;
    if (sample + num > count) invalid();
    const length = sizes.slice(sample, sample + num).reduce((a, b) => a + b, 0),
      offset =
        width === 8
          ? u64(off.data, 8 + i * width)
          : u32(off.data, 8 + i * width),
      end = offset + length;
    sample += num;
    if (!mdat.some((b) => offset >= b.start && end <= b.end)) invalid();
    out.push([offset, end]);
  }
  if (sample !== count) invalid();
  return out;
}
/** Container verification only. It does not decode frames or claim video understanding. */
export function validateVideo(b: Buffer) {
  if (b.length < 32 || b.length > MEDIA_LIMIT) invalid();
  const root = boxes(b);
  if (
    root[0]?.kind !== "ftyp" ||
    root.some((b) => ["moof", "sidx"].includes(b.kind))
  )
    invalid();
  const ft = one(root, "ftyp");
  if (
    ft.length < 8 ||
    ft.length % 4 ||
    !Array.from({ length: ft.length / 4 }, (_, i) =>
      i === 1 ? "" : ft.toString("ascii", i * 4, i * 4 + 4),
    ).some((s) => ["isom", "iso2", "mp41", "mp42", "avc1"].includes(s))
  )
    invalid();
  const mdat = root.filter((b) => b.kind === "mdat" && b.data.length);
  if (!mdat.length) invalid();
  const movie = boxes(one(root, "moov")),
    clock = time(one(movie, "mvhd")),
    tracks = movie.filter((b) => b.kind === "trak");
  if (!tracks.length || tracks.length > 4) invalid();
  let video: { width: number; height: number } | undefined;
  const all: [number, number][] = [];
  for (const t of tracks) {
    const mdia = boxes(one(boxes(t.data), "mdia")),
      mt = time(one(mdia, "mdhd")),
      hdlr = one(mdia, "hdlr");
    if (hdlr.length < 12) invalid();
    const handler = hdlr.toString("ascii", 8, 12),
      minf = boxes(one(mdia, "minf")),
      dref = one(boxes(one(minf, "dinf")), "dref");
    if (u32(dref, 4) !== 1) invalid();
    const refs = boxes(dref.subarray(8));
    if (
      refs.length !== 1 ||
      !one(refs, "url ").equals(Buffer.from([0, 0, 0, 1]))
    )
      invalid();
    const table = boxes(one(minf, "stbl")),
      sd = one(table, "stsd");
    if (u32(sd, 4) !== 1) invalid();
    const descriptions = boxes(sd.subarray(8));
    if (descriptions.length !== 1) invalid();
    const d = descriptions[0]!,
      data = d.data;
    if (data.length < 8 || data.readUInt16BE(6) !== 1) invalid();
    if (handler === "vide") {
      if (video || !["avc1", "avc3"].includes(d.kind) || data.length < 78)
        invalid();
      const width = data.readUInt16BE(24),
        height = data.readUInt16BE(26);
      if (
        !width ||
        !height ||
        width > 4096 ||
        height > 4096 ||
        width * height > 8294400
      )
        invalid();
      avc(one(boxes(data.subarray(78)), "avcC"));
      video = { width, height };
    } else if (handler === "soun") {
      if (d.kind !== "mp4a" || data.length < 28) invalid();
      one(boxes(data.subarray(28)), "esds");
    } else invalid();
    all.push(...ranges(table, mt.scale, mdat));
  }
  all.sort((a, b) => a[0] - b[0]);
  if (all.some((r, i) => i > 0 && all[i - 1]![1] > r[0]) || !video) invalid();
  return {
    ...video,
    duration_ms: Math.floor((clock.duration * 1000) / clock.scale),
    mime: "video/mp4",
    extension: "mp4",
  };
}
export function validateVoice(b: Buffer) {
  if (!b.length || b.length > MEDIA_LIMIT) invalid();
  if (
    b.length >= 44 &&
    b.toString("ascii", 0, 4) === "RIFF" &&
    b.toString("ascii", 8, 12) === "WAVE"
  ) {
    if (b.readUInt32LE(4) + 8 !== b.length) invalid();
    let at = 12,
      format: Buffer | undefined,
      data = 0;
    while (at < b.length) {
      if (at + 8 > b.length) invalid();
      const kind = b.toString("ascii", at, at + 4),
        n = b.readUInt32LE(at + 4);
      at += 8;
      if (at + n > b.length) invalid();
      if (kind === "fmt ") {
        if (format) invalid();
        format = b.subarray(at, at + n);
      }
      if (kind === "data") {
        if (data) invalid();
        data = n;
      }
      at += n + (n % 2);
    }
    if (
      at !== b.length ||
      !format ||
      format.length < 16 ||
      format.readUInt16LE(0) !== 1 ||
      ![1, 2].includes(format.readUInt16LE(2))
    )
      invalid();
    const rate = format.readUInt32LE(4),
      bytes = format.readUInt32LE(8),
      align = format.readUInt16LE(12),
      bits = format.readUInt16LE(14);
    if (
      rate < 8000 ||
      rate > 96000 ||
      ![8, 16, 24, 32].includes(bits) ||
      align !== (format.readUInt16LE(2) * bits) / 8 ||
      bytes !== rate * align ||
      !data ||
      data % align ||
      data > bytes * 60
    )
      invalid();
    return { mime: "audio/wav", extension: "wav" };
  }
  // Legacy compressed formats are signature checked for transport; no codec is executed.
  if (
    b.subarray(0, 9).toString() === "#!SILK_V3" ||
    b.subarray(0, 10).toString() === "\x02#!SILK_V3"
  )
    return { mime: "audio/silk", extension: "silk" };
  if (b.length >= 27 && b.toString("ascii", 0, 4) === "OggS")
    return { mime: "audio/ogg", extension: "ogg" };
  if (
    b.length >= 10 &&
    (b.toString("ascii", 0, 3) === "ID3" ||
      (b[0] === 255 && (b[1]! & 224) === 224))
  )
    return { mime: "audio/mpeg", extension: "mp3" };
  return invalid();
}
