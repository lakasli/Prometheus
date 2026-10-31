import { bad } from "./errors.ts";

/** Decode only bounded BMP pixels; palettes, header gaps and profiles never leave this parser. */
export function bmpPixels(input: Buffer) {
  const invalid = () => bad("BMP像素、调色板或压缩流无效", "invalid_image");
  if (input.length < 26 || input.toString("latin1", 0, 2) !== "BM")
    throw invalid();
  const header = input.readUInt32LE(14),
    offset = input.readUInt32LE(10),
    fileSize = input.readUInt32LE(2);
  if (
    ![12, 40, 52, 56, 108, 124].includes(header) ||
    14 + header > input.length ||
    offset < 14 + header ||
    offset > input.length ||
    (fileSize && (fileSize > input.length || fileSize < offset))
  )
    throw invalid();
  const core = header === 12,
    width = core ? input.readUInt16LE(18) : input.readInt32LE(18),
    signedHeight = core ? input.readUInt16LE(20) : input.readInt32LE(22),
    height = Math.abs(signedHeight),
    bottomUp = signedHeight > 0;
  const planes = input.readUInt16LE(core ? 22 : 26),
    bits = input.readUInt16LE(core ? 24 : 28),
    compression = core ? 0 : input.readUInt32LE(30),
    colors = core ? 0 : input.readUInt32LE(46);
  if (
    planes !== 1 ||
    width < 1 ||
    height < 1 ||
    width > 12000 ||
    height > 12000 ||
    width * height > 24000000 ||
    ![1, 4, 8, 16, 24, 32].includes(bits) ||
    ![0, 1, 2, 3, 6].includes(compression) ||
    (compression === 1 && bits !== 8) ||
    (compression === 2 && bits !== 4) ||
    ([3, 6].includes(compression) && ![16, 32].includes(bits)) ||
    ([1, 2].includes(compression) && !bottomUp)
  )
    throw invalid();
  let cursor = 14 + header;
  let masks =
    bits === 16
      ? [0x7c00, 0x03e0, 0x001f, 0]
      : [0x00ff0000, 0x0000ff00, 0x000000ff, 0];
  if (
    [16, 32].includes(bits) &&
    (header >= 52 || [3, 6].includes(compression))
  ) {
    const at = 54,
      count = header >= 56 || compression === 6 ? 4 : 3;
    if (at + count * 4 > offset) throw invalid();
    const declared = Array.from({ length: 4 }, (_, i) =>
      i < count ? input.readUInt32LE(at + i * 4) : 0,
    );
    if (compression !== 0 || declared.slice(0, 3).some(Boolean))
      masks = declared;
    if (header === 40) cursor += count * 4;
  }
  const components = masks.map((mask, i) => {
    if (!mask) {
      if (i < 3) throw invalid();
      return { mask: 0, shift: 0, max: 1 };
    }
    if (
      (bits === 16 && mask > 0xffff) ||
      masks.slice(0, i).some((other) => (other! & mask) !== 0)
    )
      throw invalid();
    let shift = 0;
    while (shift < 32 && ((mask >>> shift) & 1) === 0) shift++;
    const value = mask >>> shift;
    if ((value & (value + 1)) !== 0) throw invalid();
    return { mask, shift, max: value };
  });
  const count = bits <= 8 ? colors || 2 ** bits : colors;
  if (count > (bits <= 8 ? 2 ** bits : 256)) throw invalid();
  const entrySize = core ? 3 : 4;
  if (cursor + count * entrySize > offset) throw invalid();
  const palette = Array.from({ length: count }, (_, i) => {
    const at = cursor + i * entrySize;
    return [input[at + 2]!, input[at + 1]!, input[at]!, 255];
  });
  const end = fileSize || input.length,
    stride = Math.ceil((width * bits) / 32) * 4;
  if (compression === 0 || compression === 3 || compression === 6)
    if (offset + stride * height > end) throw invalid();
  const data = Buffer.alloc(width * height * 4);
  function put(x: number, y: number, color: readonly number[]) {
    if (x < 0 || x >= width || y < 0 || y >= height) throw invalid();
    const at = ((bottomUp ? height - 1 - y : y) * width + x) * 4;
    for (let c = 0; c < 4; c++) data[at + c] = color[c]!;
  }
  function indexed(x: number, y: number, index: number) {
    const color = palette[index];
    if (!color) throw invalid();
    put(x, y, color);
  }
  if (compression === 1 || compression === 2) {
    if (!palette[0]) throw invalid();
    for (let y = 0; y < height; y++)
      for (let x = 0; x < width; x++) put(x, y, palette[0]);
    let at = offset,
      x = 0,
      y = 0,
      ended = false;
    const byte = () => {
      if (at >= end) throw invalid();
      return input[at++]!;
    };
    while (at < end) {
      const run = byte(),
        value = byte();
      if (run) {
        if (x + run > width || y >= height) throw invalid();
        for (let i = 0; i < run; i++)
          indexed(
            x++,
            y,
            bits === 8 ? value : i % 2 ? value & 15 : value >>> 4,
          );
      } else if (value === 0) {
        x = 0;
        y++;
        if (y > height) throw invalid();
      } else if (value === 1) {
        ended = true;
        break;
      } else if (value === 2) {
        x += byte();
        y += byte();
        if (x > width || y >= height) throw invalid();
      } else {
        if (x + value > width || y >= height) throw invalid();
        const bytes = bits === 8 ? value : Math.ceil(value / 2),
          start = at;
        if (start + bytes + (bytes % 2) > end) throw invalid();
        for (let i = 0; i < value; i++) {
          const b = input[start + (bits === 8 ? i : Math.floor(i / 2))]!;
          indexed(x++, y, bits === 8 ? b : i % 2 ? b & 15 : b >>> 4);
        }
        at += bytes + (bytes % 2);
      }
    }
    if (!ended) throw invalid();
  } else {
    for (let y = 0; y < height; y++)
      for (let x = 0; x < width; x++) {
        const row = offset + y * stride;
        if (bits <= 8) {
          const bit = x * bits,
            b = input[row + Math.floor(bit / 8)]!,
            index = (b >>> (8 - bits - (bit % 8))) & ((1 << bits) - 1);
          indexed(x, y, index);
        } else if (bits === 24) {
          const at = row + x * 3;
          put(x, y, [input[at + 2]!, input[at + 1]!, input[at]!, 255]);
        } else {
          const at = row + x * (bits / 8),
            value =
              bits === 16 ? input.readUInt16LE(at) : input.readUInt32LE(at);
          put(
            x,
            y,
            components.map((c, i) =>
              c.mask
                ? Math.round((((value & c.mask) >>> c.shift) / c.max) * 255)
                : i === 3
                  ? 255
                  : 0,
            ),
          );
        }
      }
  }
  return { data, width, height };
}
