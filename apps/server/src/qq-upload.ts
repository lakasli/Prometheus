import http from "node:http";
import https from "node:https";
import { lookup } from "node:dns";
import { createHash } from "node:crypto";
import { publicAddress } from "./model.ts";
import { ApiError } from "./errors.ts";

export const MEDIA_LIMIT = 20 * 1024 * 1024;
export const DIRECT_LIMIT = 5 * 1024 * 1024;
export const uploadError = (code: string) =>
  new ApiError(502, code, "QQ媒体上传未完成，未自动重试。");
export function signedUploadUrl(raw: string, fixture?: string) {
  let u: URL;
  try {
    if (raw.length > 8192 || /[\x00-\x20\x7f\\]/.test(raw)) throw Error();
    u = new URL(raw);
  } catch {
    throw uploadError("upload_url_invalid");
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
        ![".myqcloud.com", ".qcloud.com", ".qq.com"].some((s) =>
          u.hostname.endsWith(s),
        ))) ||
    (test && !["http:", "https:"].includes(u.protocol))
  )
    throw uploadError("upload_url_not_allowed");
  return { url: u, test };
}
/** The lease covers the real socket through close, including abort and rejected responses. */
export function qqWire(
  url: URL,
  payload: Buffer,
  headers: Record<string, string>,
  signal: AbortSignal,
  method: string,
  loopback = false,
): Promise<{ status: number; body: Buffer }> {
  return new Promise((resolve, reject) => {
    let error: ApiError | undefined,
      result: { status: number; body: Buffer } | undefined;
    const req = (url.protocol === "https:" ? https : http).request(
      url,
      {
        method,
        signal,
        agent: false,
        headers: {
          ...headers,
          "content-length": String(payload.length),
          "accept-encoding": "identity",
          "user-agent": "Prometheus/0.1",
        },
        lookup(host, options, callback) {
          lookup(host, { all: true, verbatim: true }, (err, addresses) => {
            if (err) return callback(err, []);
            if (
              !addresses.length ||
              addresses.some(
                (a) =>
                  !publicAddress(a.address) &&
                  !(loopback && ["127.0.0.1", "::1"].includes(a.address)),
              )
            ) {
              error = uploadError("upload_dns_private");
              return callback(new Error("protected_network"), []);
            }
            if (options.all) callback(null, addresses);
            else callback(null, addresses[0]!.address, addresses[0]!.family);
          });
        },
      },
      (res) => {
        const chunks: Buffer[] = [];
        let size = 0;
        const length = res.headers["content-length"];
        if (
          (length !== undefined &&
            (!/^\d+$/.test(length) || Number(length) > 512 * 1024)) ||
          (res.headers["content-encoding"] &&
            res.headers["content-encoding"] !== "identity")
        ) {
          error = uploadError("upload_response_limit");
          req.destroy();
          return;
        }
        res.on("data", (b: Buffer) => {
          size += b.length;
          if (size > 512 * 1024) {
            error = uploadError("upload_response_limit");
            res.destroy();
          } else chunks.push(b);
        });
        res.on("error", () => {
          error ??= uploadError("upload_response_incomplete");
          req.destroy();
        });
        res.on("end", () => {
          result = { status: res.statusCode ?? 0, body: Buffer.concat(chunks) };
        });
      },
    );
    req.setTimeout(10000, () => {
      error = uploadError("upload_timeout");
      req.destroy();
    });
    req.on("error", () => {
      error ??= uploadError(
        signal.aborted ? "upload_cancelled" : "upload_connection_failed",
      );
    });
    req.on("close", () => {
      if (error) reject(error);
      else if (result) resolve(result);
      else reject(uploadError("upload_response_incomplete"));
    });
    req.end(payload);
  });
}
export async function qqJson(
  base: string,
  path: string,
  body: unknown,
  token: string,
  app: string,
  signal: AbortSignal,
  loopback = false,
  finalMessage = false,
) {
  const r = await qqWire(
    new URL(path, base),
    Buffer.from(JSON.stringify(body)),
    {
      authorization: `QQBot ${token}`,
      "x-union-appid": app,
      "content-type": "application/json",
    },
    signal,
    "POST",
    loopback,
  );
  let b: Record<string, any>;
  try {
    b = r.body.length ? JSON.parse(r.body.toString("utf8")) : {};
    if (!b || typeof b !== "object" || Array.isArray(b)) throw Error();
  } catch {
    throw uploadError(
      finalMessage ? "media_send_unknown" : "upload_invalid_response",
    );
  }
  const codes = ["err_code", "code", "retcode", "ret", "errcode"]
    .filter((k) => b[k] !== undefined)
    .map((k) => Number(b[k]));
  if (
    ![200, 204].includes(r.status) ||
    codes.some((n) => !Number.isFinite(n) || n !== 0)
  ) {
    const uncertain =
      [201, 202, 408, 429].includes(r.status) ||
      r.status >= 500 ||
      codes.some((n) => [304023, 304024].includes(n));
    throw new ApiError(
      502,
      finalMessage
        ? uncertain
          ? "media_send_unknown"
          : "media_send_rejected"
        : "upload_rejected",
      finalMessage
        ? "QQ媒体消息未获确认，未自动重试。"
        : "QQ媒体上传未获确认，尚未发送消息。",
    );
  }
  return b;
}
const digest = (name: string, b: Buffer) =>
  createHash(name).update(b).digest("hex");
export async function uploadQq(options: {
  base: string;
  user: string;
  token: string;
  app: string;
  kind: string;
  bytes: Buffer;
  extension: string;
  signal: AbortSignal;
  loopback?: boolean;
  putFixture?: string;
  admit: () => void;
  stage: (stage: string, parts?: number) => void;
}) {
  const o = options,
    file_type = ({ image: 1, video: 2, voice: 3 } as Record<string, number>)[
      o.kind
    ];
  if (!file_type || !o.bytes.length || o.bytes.length > MEDIA_LIMIT)
    throw uploadError("upload_media_invalid");
  const file_name = "prometheus-" + o.kind + "." + o.extension,
    root = `/v2/users/${o.user}`;
  const post = async (path: string, body: unknown) => {
    o.admit();
    return qqJson(
      o.base,
      root + path,
      body,
      o.token,
      o.app,
      o.signal,
      o.loopback,
    );
  };
  let body: Record<string, unknown>;
  if (o.bytes.length <= DIRECT_LIMIT) {
    o.stage("uploading");
    body = {
      file_type,
      file_name,
      file_data: o.bytes.toString("base64"),
      srv_send_msg: false,
    };
  } else {
    o.stage("preparing");
    const p = await post("/upload_prepare", {
      file_type,
      file_name,
      file_size: String(o.bytes.length),
      md5: digest("md5", o.bytes),
      sha1: digest("sha1", o.bytes),
      md5_10m: digest("md5", o.bytes.subarray(0, 10_002_432)),
    });
    const block =
      typeof p.block_size === "number"
        ? p.block_size
        : typeof p.block_size === "string" && /^[1-9]\d*$/.test(p.block_size)
          ? Number(p.block_size)
          : NaN;
    if (
      typeof p.upload_id !== "string" ||
      !p.upload_id ||
      p.upload_id.length > 512 ||
      !Number.isSafeInteger(block) ||
      block < 1 ||
      block > MEDIA_LIMIT ||
      !Array.isArray(p.parts) ||
      !p.parts.length ||
      p.parts.length > 128 ||
      p.parts.length !== Math.ceil(o.bytes.length / block)
    )
      throw uploadError("upload_prepare_invalid");
    const first = p.parts[0]?.index;
    if (
      ![0, 1].includes(first) ||
      p.parts.some(
        (part: any, i: number) =>
          !Number.isSafeInteger(part.index) ||
          part.index !== first + i ||
          typeof part.presigned_url !== "string",
      )
    )
      throw uploadError("upload_indices_invalid");
    const urls = p.parts.map((part: any) =>
      signedUploadUrl(part.presigned_url, o.putFixture),
    );
    for (let i = 0; i < p.parts.length; i++) {
      o.admit();
      o.stage("uploading", i);
      const bytes = o.bytes.subarray(
          i * block,
          Math.min(o.bytes.length, (i + 1) * block),
        ),
        target = urls[i]!;
      const r = await qqWire(
        target.url,
        bytes,
        { "content-type": "application/octet-stream" },
        o.signal,
        "PUT",
        target.test,
      );
      if (r.status < 200 || r.status >= 300)
        throw uploadError("upload_part_rejected");
      o.stage("confirming", i);
      await post("/upload_part_finish", {
        upload_id: p.upload_id,
        part_index: p.parts[i].index,
        block_size: String(bytes.length),
        md5: digest("md5", bytes),
      });
      o.stage("confirming", i + 1);
    }
    body = {
      file_type,
      file_name,
      upload_id: p.upload_id,
      srv_send_msg: false,
    };
    o.stage("finalizing");
  }
  const result = await post("/files", body);
  if (
    typeof result.file_info !== "string" ||
    !result.file_info ||
    Buffer.byteLength(result.file_info) > 32768
  )
    throw uploadError("upload_file_info_missing");
  o.admit();
  o.stage("ready");
  return result.file_info as string;
}
