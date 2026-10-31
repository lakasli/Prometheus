import http from "node:http";
import https from "node:https";
import { lookup } from "node:dns";
import {
  createCipheriv,
  createDecipheriv,
  createHash,
  randomBytes,
} from "node:crypto";
import { ApiError, bad } from "./errors.ts";
import { publicAddress } from "./model.ts";
import { validateVideo, validateVoice } from "./media-container.ts";
import type { Row } from "./store.ts";
const LIMIT = 20 * 1024 * 1024;
const failure = (code: string) =>
  new ApiError(502, code, "微信媒体传输未完成，未读取或重试消息。");
export const WX_CDN = "https://novac2c.cdn.weixin.qq.com/c2c";
export function weixinCdn(raw: string, fixture?: string) {
  let u: URL;
  try {
    if (raw.length > 8192 || /[\x00-\x20\x7f\\]/.test(raw)) throw Error();
    u = new URL(raw);
  } catch {
    throw failure("wx_media_url_invalid");
  }
  const test =
    !!fixture &&
    u.origin === new URL(fixture).origin &&
    ["127.0.0.1", "[::1]"].includes(u.hostname);
  if (
    u.username ||
    u.password ||
    u.hash ||
    (!test &&
      (u.protocol !== "https:" ||
        (u.port && u.port !== "443") ||
        !u.hostname.endsWith(".cdn.weixin.qq.com"))) ||
    (test && !["http:", "https:"].includes(u.protocol))
  )
    throw failure("wx_media_url_forbidden");
  return { url: u, test };
}
export function weixinKey(value: unknown, hex = false) {
  if (typeof value !== "string" || value.length > 128)
    throw failure("wx_media_key_invalid");
  if (hex) {
    if (!/^[a-f\d]{32}$/i.test(value)) throw failure("wx_media_key_invalid");
    return Buffer.from(value, "hex");
  }
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(value) || value.length % 4)
    throw failure("wx_media_key_invalid");
  const b = Buffer.from(value, "base64");
  if (b.toString("base64") !== value) throw failure("wx_media_key_invalid");
  if (b.length === 16) return b;
  if (b.length === 32 && /^[a-f\d]{32}$/i.test(b.toString("ascii")))
    return Buffer.from(b.toString("ascii"), "hex");
  throw failure("wx_media_key_invalid");
}
export function weixinCrypt(bytes: Buffer, key: Buffer, decrypt = false) {
  try {
    if (key.length !== 16 || (decrypt && (!bytes.length || bytes.length % 16)))
      throw Error();
    const cipher = decrypt
      ? createDecipheriv("aes-128-ecb", key, null)
      : createCipheriv("aes-128-ecb", key, null);
    return Buffer.concat([cipher.update(bytes), cipher.final()]);
  } catch {
    throw failure("wx_media_decrypt_failed");
  }
}
export function weixinWire(
  raw: string,
  method: string,
  body: Buffer | undefined,
  signal: AbortSignal,
  limit: number,
  fixture?: string,
): Promise<{ bytes: Buffer; parameter?: string }> {
  const { url, test } = weixinCdn(raw, fixture);
  return new Promise((resolve, reject) => {
    let error: ApiError | undefined,
      result: { bytes: Buffer; parameter?: string } | undefined;
    const request = (url.protocol === "https:" ? https : http).request(
      url,
      {
        method,
        signal,
        agent: false,
        headers: {
          "accept-encoding": "identity",
          "user-agent": "Prometheus/0.1",
          ...(body
            ? {
                "content-type": "application/octet-stream",
                "content-length": String(body.length),
              }
            : {}),
        },
        lookup(host, options, callback) {
          lookup(host, { all: true, verbatim: true }, (e, addresses) => {
            if (e) return callback(e, []);
            if (
              !addresses.length ||
              addresses.some(
                (a) =>
                  !publicAddress(a.address) &&
                  !(test && ["127.0.0.1", "::1"].includes(a.address)),
              )
            ) {
              error = failure("wx_media_dns_private");
              return callback(new Error("protected_network"), []);
            }
            if (options.all) callback(null, addresses);
            else callback(null, addresses[0]!.address, addresses[0]!.family);
          });
        },
      },
      (res) => {
        const length = res.headers["content-length"];
        if (
          res.statusCode !== 200 ||
          (length !== undefined &&
            (!/^\d+$/.test(length) || Number(length) > limit)) ||
          (res.headers["content-encoding"] &&
            res.headers["content-encoding"] !== "identity")
        ) {
          error = failure("wx_media_response_rejected");
          request.destroy();
          return;
        }
        const chunks: Buffer[] = [];
        let size = 0;
        res.on("data", (b: Buffer) => {
          size += b.length;
          if (size > limit) {
            error = failure("wx_media_size_limit");
            res.destroy();
          } else chunks.push(b);
        });
        res.on("error", () => {
          error ??= failure("wx_media_incomplete");
          request.destroy();
        });
        res.on("end", () => {
          const parameter = res.headers["x-encrypted-param"];
          if (
            parameter !== undefined &&
            (typeof parameter !== "string" ||
              !parameter ||
              Buffer.byteLength(parameter) > 32768)
          ) {
            error = failure("wx_media_parameter_invalid");
            return;
          }
          result = {
            bytes: Buffer.concat(chunks),
            ...(typeof parameter === "string" ? { parameter } : {}),
          };
        });
      },
    );
    request.setTimeout(10000, () => {
      error = failure("wx_media_timeout");
      request.destroy();
    });
    request.on("error", () => {
      error ??= failure(
        signal.aborted ? "wx_media_cancelled" : "wx_media_connection_failed",
      );
    });
    request.on("close", () => {
      if (error) reject(error);
      else if (result) resolve(result);
      else reject(failure("wx_media_incomplete"));
    });
    request.end(body);
  });
}
type Json = Record<string, any>;
export function weixinAttachment(
  items: Json[],
  content: string,
): Json | undefined {
  const media = items.filter((i) => i.type !== 1);
  if (!media.length) return;
  if (media.length !== 1) return { code: "media_count_unsupported" };
  if (content.trimStart().startsWith("/") || content.includes("\0"))
    return { code: "media_control_command" };
  const item = media[0]!,
    kind = (
      { 2: "image", 3: "voice", 4: "file", 5: "video" } as Record<
        number,
        string
      >
    )[item.type];
  if (!kind) return { code: "media_format_unsupported" };
  const value =
    item[
      (
        {
          image: "image_item",
          video: "video_item",
          voice: "voice_item",
          file: "file_item",
        } as Record<string, string>
      )[kind]!
    ];
  if (!value?.media || typeof value.media !== "object")
    return { code: "media_url_missing" };
  // The entire reference stays only inside the encrypted durable event payload.
  return {
    kind,
    reference: value.media,
    ...(kind === "image" && value.aeskey ? { hex_key: value.aeskey } : {}),
    ...(kind === "file"
      ? { length: value.len, md5: value.md5 }
      : kind === "video"
        ? { md5: value.video_md5 }
        : {}),
    ...(kind === "voice" ? { encode_type: value.encode_type } : {}),
  };
}
export async function downloadWeixin(
  media: Json,
  signal: AbortSignal,
  fixture?: string,
) {
  const r = media.reference ?? {},
    limit = media.kind === "image" ? 8 * 1024 * 1024 : LIMIT;
  if (r.encrypt_type !== undefined && ![0, 1].includes(r.encrypt_type))
    throw failure("wx_media_encrypt_type_invalid");
  const key = media.hex_key
    ? weixinKey(media.hex_key, true)
    : r.aes_key
      ? weixinKey(r.aes_key)
      : undefined;
  if (!key && media.kind !== "image") throw failure("wx_media_key_missing");
  try {
    const base = fixture ?? WX_CDN;
    const raw =
      typeof r.full_url === "string"
        ? r.full_url
        : typeof r.encrypt_query_param === "string" &&
            r.encrypt_query_param.length > 0 &&
            r.encrypt_query_param.length <= 4096
          ? base.replace(/\/$/, "") +
            "/download?encrypted_query_param=" +
            encodeURIComponent(r.encrypt_query_param)
          : undefined;
    if (!raw) throw failure("wx_media_url_missing");
    const result = await weixinWire(
        raw,
        "GET",
        undefined,
        signal,
        limit + (key ? 16 : 0),
        fixture,
      ),
      bytes = key ? weixinCrypt(result.bytes, key, true) : result.bytes;
    if (!bytes.length || bytes.length > limit)
      throw failure("wx_media_size_limit");
    if (
      media.length !== undefined &&
      (!/^[1-9]\d*$/.test(String(media.length)) ||
        Number(media.length) !== bytes.length)
    )
      throw failure("wx_media_length_mismatch");
    if (
      media.md5 !== undefined &&
      (typeof media.md5 !== "string" ||
        !/^[a-f\d]{32}$/i.test(media.md5) ||
        createHash("md5").update(bytes).digest("hex") !==
          media.md5.toLowerCase())
    )
      throw failure("wx_media_digest_mismatch");
    return bytes;
  } finally {
    key?.fill(0);
  }
}
export interface WeixinMediaBridge {
  current: (r: Row, target: Json) => boolean;
  call: (path: string, body: unknown, signal: AbortSignal) => Promise<Json>;
  fixture?: string;
}
export async function sendWeixinMedia(o: {
  bridge: WeixinMediaBridge;
  asset: { kind: string; bytes: Buffer; extension: string };
  target: Record<string, string>;
  part: string;
  signal: AbortSignal;
  admit: () => void;
  stage: (s: string, n?: number) => unknown;
  started: () => void;
}) {
  const { asset: a, target: t, bridge: b } = o,
    type = (
      { image: 1, video: 2, file: 3, voice: 4 } as Record<string, number>
    )[a.kind];
  if (!type) throw bad("微信媒体类型无效");
  if (a.kind === "voice" && !["silk", "mp3"].includes(a.extension))
    throw bad(
      "微信语音仅传输已有SILK/MP3；其他音频请作为文件发送",
      "wx_voice_format_unsupported",
    );
  if (a.kind === "video") validateVideo(a.bytes);
  if (a.kind === "voice") validateVoice(a.bytes);
  const key = randomBytes(16),
    ciphertext = weixinCrypt(a.bytes, key),
    filekey = randomBytes(16).toString("hex");
  try {
    o.admit();
    o.stage("preparing");
    const p = await b.call(
      "/ilink/bot/getuploadurl",
      {
        filekey,
        media_type: type,
        to_user_id: t.target,
        rawsize: a.bytes.length,
        rawfilemd5: createHash("md5").update(a.bytes).digest("hex"),
        filesize: ciphertext.length,
        no_need_thumb: true,
        aeskey: key.toString("hex"),
      },
      o.signal,
    );
    o.admit();
    const raw =
      typeof p.upload_full_url === "string"
        ? p.upload_full_url
        : typeof p.upload_param === "string" &&
            p.upload_param &&
            p.upload_param.length <= 4096
          ? (b.fixture ?? WX_CDN).replace(/\/$/, "") +
            "/upload?encrypted_query_param=" +
            encodeURIComponent(p.upload_param) +
            "&filekey=" +
            filekey
          : undefined;
    if (!raw) throw failure("wx_media_upload_url_missing");
    o.stage("uploading");
    const cdn = await weixinWire(
      raw,
      "POST",
      ciphertext,
      o.signal,
      512 * 1024,
      b.fixture,
    );
    if (!cdn.parameter) throw failure("wx_media_download_parameter_missing");
    o.admit();
    o.stage("ready");
    const media = {
      encrypt_query_param: cdn.parameter,
      aes_key: Buffer.from(key.toString("hex")).toString("base64"),
      encrypt_type: 1,
    };
    const item =
      a.kind === "image"
        ? { type: 2, image_item: { media, mid_size: ciphertext.length } }
        : a.kind === "video"
          ? { type: 5, video_item: { media, video_size: ciphertext.length } }
          : a.kind === "voice"
            ? {
                type: 3,
                voice_item: {
                  media,
                  encode_type: a.extension === "mp3" ? 7 : 6,
                },
              }
            : {
                type: 4,
                file_item: {
                  media,
                  file_name: "prometheus-attachment.bin",
                  len: String(a.bytes.length),
                },
              };
    o.admit();
    o.stage("sending");
    o.started();
    const result = await b.call(
      "/ilink/bot/sendmessage",
      {
        msg: {
          from_user_id: "",
          to_user_id: t.target,
          client_id: o.part,
          message_type: 2,
          message_state: 2,
          context_token: t.context,
          item_list: [item],
        },
      },
      o.signal,
    );
    if (result.ret !== 0) throw failure("wx_media_send_rejected");
    if (
      result.message_id !== undefined &&
      ((typeof result.message_id !== "string" &&
        typeof result.message_id !== "number") ||
        !String(result.message_id) ||
        String(result.message_id).length > 256)
    )
      throw failure("wx_media_send_unknown");
    o.stage("sent");
    return String(result.message_id ?? o.part);
  } finally {
    key.fill(0);
    ciphertext.fill(0);
  }
}
