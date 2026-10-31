import { createInflate } from "node:zlib";
import { ApiError, bad, unavailable } from "./errors.ts";
import { pngStructure, normalizedPng } from "./png.ts";
import { bmpPixels } from "./image-bmp.ts";

export const IMAGE_INPUT_BYTES = 8 * 1024 * 1024;
export const IMAGE_CANONICAL_BYTES = 2 * 1024 * 1024;
export const IMAGE_MAX_BASE64 = 4 * Math.ceil(IMAGE_INPUT_BYTES / 3);
let factory: Promise<typeof import("sharp").default> | undefined;
async function imageSharp() {
  factory ??= import("sharp").then(({ default: sharp }) => {
    sharp.cache(false);
    sharp.concurrency(1);
    return sharp;
  });
  try {
    return await factory;
  } catch {
    throw unavailable("Windows图片解码库");
  }
}

export function imageBase64(value: string) {
  if (
    !value ||
    value.length > IMAGE_MAX_BASE64 ||
    value.length % 4 ||
    !/^[A-Za-z0-9+/]+={0,2}$/.test(value)
  )
    throw bad("图片编码无效", "invalid_image");
  const bytes = Buffer.from(value, "base64");
  if (bytes.length > IMAGE_INPUT_BYTES || bytes.toString("base64") !== value)
    throw bad("图片编码或大小无效", "invalid_image");
  return bytes;
}
function live(signal: AbortSignal) {
  if (signal.aborted) throw bad("图片处理已取消", "image_cancelled");
}

/** Count and validate scanlines without retaining a potentially 192MiB 16-bit raster. */
async function pngInput(input: Buffer, signal: AbortSignal) {
  const p = pngStructure(input, true),
    inflater = createInflate();
  let pass = 0,
    row = 0,
    inRow = 0,
    total = 0,
    filter = 0;
  let previous: Buffer | undefined, current: Buffer | undefined;
  const abort = () => inflater.destroy(new Error("cancelled"));
  signal.addEventListener("abort", abort, { once: true });
  try {
    live(signal);
    inflater.end(p.compressed);
    for await (const raw of inflater) {
      const bytes = raw as Buffer;
      let at = 0;
      while (at < bytes.length) {
        const layout = p.rows[pass];
        if (!layout) throw Error("excess raster");
        if (inRow === 0) {
          filter = bytes[at]!;
          if (filter > 4) throw Error("invalid filter");
          if (p.color === 3 && !previous) {
            previous = Buffer.alloc(layout.length - 1);
            current = Buffer.alloc(layout.length - 1);
          }
        }
        const n = Math.min(bytes.length - at, layout.length - inRow);
        if (p.color === 3) {
          for (let k = 0; k < n; k++) {
            const pos = inRow + k - 1;
            if (pos < 0) continue;
            const a = pos ? current![pos - 1]! : 0,
              b = previous![pos]!,
              c = pos ? previous![pos - 1]! : 0;
            let prediction = 0;
            if (filter === 1) prediction = a;
            if (filter === 2) prediction = b;
            if (filter === 3) prediction = Math.floor((a + b) / 2);
            if (filter === 4) {
              const v = a + b - c,
                pa = Math.abs(v - a),
                pb = Math.abs(v - b),
                pc = Math.abs(v - c);
              prediction = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
            }
            current![pos] = (bytes[at + k]! + prediction) & 255;
          }
        }
        at += n;
        inRow += n;
        total += n;
        if (inRow === layout.length) {
          if (p.color === 3) {
            for (let x = 0; x < layout.width; x++) {
              const bit = x * p.depth,
                index =
                  (current![Math.floor(bit / 8)]! >>>
                    (8 - p.depth - (bit % 8))) &
                  ((1 << p.depth) - 1);
              if (index >= p.paletteEntries) throw Error("palette index");
            }
            [previous, current] = [current, previous];
          }
          inRow = 0;
          if (++row === layout.count) {
            row = 0;
            pass++;
            previous = undefined;
            current = undefined;
          }
        }
      }
    }
    if (
      pass !== p.rows.length ||
      row ||
      inRow ||
      !total ||
      inflater.bytesWritten !== p.compressed.length
    )
      throw Error("incomplete raster");
  } catch {
    live(signal);
    throw bad("PNG像素或压缩流无效", "invalid_image");
  } finally {
    signal.removeEventListener("abort", abort);
    inflater.destroy();
  }
  return p;
}

/** Trusted bounded input conversion only; does not grant any media/tool worker execution. */
export class ImageCodec {
  private active?: Promise<ReturnType<typeof normalizedPng>>;
  private stopping = false;
  get busy() {
    return !!this.active;
  }
  async normalize(input: Buffer, signal: AbortSignal) {
    if (this.stopping) throw unavailable("图片处理正在停止");
    live(signal);
    if (this.active)
      throw new ApiError(
        429,
        "image_processing_busy",
        "另一张图片正在处理，请稍后重试",
      );
    const task = this.convert(input, signal);
    this.active = task;
    try {
      return await task;
    } finally {
      if (this.active === task) this.active = undefined;
    }
  }
  private async convert(input: Buffer, signal: AbortSignal) {
    if (input.length > IMAGE_INPUT_BYTES || input.length < 12)
      throw bad("图片大小或格式无效", "invalid_image");
    const format = input
      .subarray(0, 8)
      .equals(Buffer.from("89504e470d0a1a0a", "hex"))
      ? "png"
      : input[0] === 0xff && input[1] === 0xd8 && input[2] === 0xff
        ? "jpeg"
        : ["GIF87a", "GIF89a"].includes(input.toString("latin1", 0, 6))
          ? "gif"
          : input.toString("latin1", 0, 4) === "RIFF" &&
              input.toString("latin1", 8, 12) === "WEBP"
            ? "webp"
            : input.toString("latin1", 0, 2) === "BM"
              ? "bmp"
              : null;
    if (!format)
      throw bad("只接受PNG、JPEG、GIF、WebP或BMP图片", "invalid_image");
    const options = {
      limitInputPixels: 24000000,
      failOn: "warning" as const,
      pages: 1,
      unlimited: false,
    };
    try {
      live(signal);
      const clean =
        format === "png" ? (await pngInput(input, signal)).bytes : input;
      live(signal);
      const bmp = format === "bmp" ? bmpPixels(clean) : null;
      const sharp = await imageSharp();
      live(signal);
      const pipeline = () =>
        bmp
          ? sharp(bmp.data, {
              raw: { width: bmp.width, height: bmp.height, channels: 4 },
            })
          : sharp(clean, options);
      const metadata = await pipeline().metadata();
      live(signal);
      if (
        !metadata.width ||
        !metadata.height ||
        metadata.width > 12000 ||
        metadata.height > 12000 ||
        metadata.width * metadata.height > 24000000 ||
        (!bmp && metadata.format !== format)
      )
        throw Error("dimensions or format");
      const rotated =
          (metadata.orientation ?? 0) >= 5 && (metadata.orientation ?? 0) <= 8,
        sourceWidth = rotated ? metadata.height : metadata.width,
        sourceHeight = rotated ? metadata.width : metadata.height;
      const scale = Math.min(
        1,
        2048 / sourceWidth,
        2048 / sourceHeight,
        Math.sqrt(2097152 / (sourceWidth * sourceHeight)),
      );
      let width = Math.max(1, Math.floor(sourceWidth * scale)),
        height = Math.max(1, Math.floor(sourceHeight * scale));
      for (let n = 0; n < 8; n++) {
        live(signal);
        const result = await pipeline()
          .autoOrient()
          .resize(width, height, { fit: "fill", withoutEnlargement: true })
          .toColourspace("srgb")
          .ensureAlpha()
          .png({ compressionLevel: 6, palette: false })
          .timeout({ seconds: 3 })
          .toBuffer();
        live(signal);
        if (result.length <= IMAGE_CANONICAL_BYTES) {
          const normalized = normalizedPng(result);
          if (normalized.width * normalized.height > 2097152)
            throw Error("canonical pixels");
          return normalized;
        }
        width = Math.max(1, Math.floor((width * 3) / 4));
        height = Math.max(1, Math.floor((height * 3) / 4));
      }
    } catch (error) {
      live(signal);
      if (error instanceof ApiError) throw error;
    }
    throw bad("图片没有完整打开或超过处理限制，请换一张图片", "invalid_image");
  }
  async stop() {
    this.stopping = true;
    await this.active?.catch(() => {});
  }
}
