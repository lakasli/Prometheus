import { createHash } from "node:crypto";
import { inflateSync } from "node:zlib";
import { bad } from "./errors.ts";
const signature = Buffer.from("89504e470d0a1a0a", "hex");
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc(bytes: Buffer) {
  let value = 0xffffffff;
  for (const byte of bytes)
    value = crcTable[(value ^ byte) & 255]! ^ (value >>> 8);
  return (value ^ 0xffffffff) >>> 0;
}
export function pngStructure(input: Buffer, extended = false) {
  const maxBytes = extended ? 8 * 1024 * 1024 : 2 * 1024 * 1024;
  if (
    input.length > maxBytes ||
    input.length < 45 ||
    !input.subarray(0, 8).equals(signature)
  )
    throw bad("PNG大小或签名无效");
  let offset = 8,
    width = 0,
    height = 0,
    channels = 0,
    depth = 0,
    color = 0,
    interlace = 0,
    ended = false,
    header = false,
    idatStarted = false,
    idatEnded = false,
    palette = false,
    paletteEntries = 0,
    transparency = false,
    chunks = 0;
  const output: Buffer[] = [signature],
    compressed: Buffer[] = [];
  while (offset < input.length) {
    if (++chunks > 4096) throw bad("PNG块数量超过限制");
    if (offset + 12 > input.length) throw bad("PNG块被截断");
    const size = input.readUInt32BE(offset);
    if (size > maxBytes || offset + size + 12 > input.length)
      throw bad("PNG块长度无效");
    const name = input.toString("latin1", offset + 4, offset + 8),
      data = input.subarray(offset + 8, offset + 8 + size),
      chunk = input.subarray(offset, offset + size + 12);
    if (
      !/^[A-Za-z]{4}$/.test(name) ||
      name[2] !== name[2]!.toUpperCase() ||
      ["acTL", "fcTL", "fdAT"].includes(name) ||
      crc(input.subarray(offset + 4, offset + 8 + size)) !==
        input.readUInt32BE(offset + 8 + size)
    )
      throw bad("PNG校验失败");
    if (!header && name !== "IHDR") throw bad("PNG头顺序无效");
    if (name === "IHDR") {
      if (header || size !== 13) throw bad("PNG头无效");
      header = true;
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      color = data[9]!;
      depth = data[8]!;
      interlace = data[12]!;
      channels =
        ({ 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 } as Record<number, number>)[color] ??
        0;
      const depths =
        (
          {
            0: [1, 2, 4, 8, 16],
            2: [8, 16],
            3: [1, 2, 4, 8],
            4: [8, 16],
            6: [8, 16],
          } as Record<number, number[]>
        )[color] ?? [];
      if (
        !width ||
        !height ||
        width > (extended ? 12000 : 2048) ||
        height > (extended ? 12000 : 2048) ||
        width * height > (extended ? 24000000 : 4194304) ||
        !depths.includes(depth) ||
        (!extended &&
          (depth !== 8 || ![2, 6].includes(color) || interlace !== 0)) ||
        !channels ||
        data[10] ||
        data[11] ||
        interlace > 1
      )
        throw bad("PNG尺寸、颜色或交错格式不受支持");
      output.push(chunk);
    } else if (name === "IDAT") {
      if (idatEnded || (color === 3 && !palette))
        throw bad("PNG数据或调色板顺序无效");
      idatStarted = true;
      compressed.push(data);
      output.push(chunk);
    } else if (name === "IEND") {
      if (size || !idatStarted) throw bad("PNG结尾无效");
      ended = true;
      output.push(chunk);
      offset += size + 12;
      break;
    } else {
      if (idatStarted) idatEnded = true;
      if (name === "PLTE") {
        if (
          palette ||
          transparency ||
          idatStarted ||
          !size ||
          size > 768 ||
          size % 3 ||
          [0, 4].includes(color) ||
          (color === 3 && size / 3 > 2 ** depth)
        )
          throw bad("PNG调色板无效");
        palette = true;
        paletteEntries = size / 3;
        output.push(chunk);
      } else if (name === "tRNS") {
        const maxSample = 2 ** depth - 1;
        const valid =
          color === 0
            ? size === 2 && data.readUInt16BE(0) <= maxSample
            : color === 2
              ? size === 6 &&
                [0, 2, 4].every((i) => data.readUInt16BE(i) <= maxSample)
              : color === 3 && palette && size > 0 && size <= paletteEntries;
        if (transparency || idatStarted || !valid)
          throw bad("PNG透明度数据无效");
        transparency = true;
        output.push(chunk);
      } else if (name[0] === name[0]!.toUpperCase())
        throw bad("PNG含未知关键块");
    }
    offset += size + 12;
  }
  if (!ended || offset !== input.length) throw bad("PNG尾部或数据无效");
  const passes = interlace
    ? [
        [0, 0, 8, 8],
        [4, 0, 8, 8],
        [0, 4, 4, 8],
        [2, 0, 4, 4],
        [0, 2, 2, 4],
        [1, 0, 2, 2],
        [0, 1, 1, 2],
      ]
    : [[0, 0, 1, 1]];
  const rows: { length: number; count: number; width: number }[] = [];
  for (const [x, y, dx, dy] of passes as [number, number, number, number][]) {
    const w = Math.max(0, Math.ceil((width - x) / dx)),
      h = Math.max(0, Math.ceil((height - y) / dy));
    if (w && h)
      rows.push({
        length: 1 + Math.ceil((w * channels * depth) / 8),
        count: h,
        width: w,
      });
  }
  return {
    bytes: Buffer.concat(output),
    width,
    height,
    compressed: Buffer.concat(compressed),
    rows,
    depth,
    color,
    paletteEntries,
  };
}
/** Validate stored 8-bit RGB(A) PNG; legacy four-megapixel assets stay readable. */
export function normalizedPng(input: Buffer) {
  const p = pngStructure(input),
    expected = p.rows.reduce((n, r) => n + r.length * r.count, 0);
  try {
    const result = inflateSync(p.compressed, {
      maxOutputLength: expected,
      info: true,
    }) as unknown as { buffer: Buffer; engine: { bytesWritten: number } };
    if (
      result.engine.bytesWritten !== p.compressed.length ||
      result.buffer.length !== expected
    )
      throw Error();
    for (let at = 0; at < expected; at += p.rows[0]!.length)
      if (result.buffer[at]! > 4) throw Error();
  } catch {
    throw bad("PNG像素、压缩流或资源限制无效");
  }
  return {
    bytes: p.bytes,
    width: p.width,
    height: p.height,
    digest: createHash("sha256").update(p.bytes).digest("hex"),
  };
}
