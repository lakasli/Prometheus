import http from "node:http";
import https from "node:https";
import { lookup } from "node:dns";
import { publicAddress } from "./model.ts";
import { ApiError } from "./errors.ts";

export const QQ_IMAGE_BYTES = 8 * 1024 * 1024;
export const mediaFailure = (code: string) =>
  new ApiError(400, code, "QQ附件未完成读取，请重新发送或改用文字。");
export function imageMime(value: string) {
  return [
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/pjpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/x-ms-bmp",
  ].includes(value.split(";")[0]!.trim().toLowerCase());
}
export function qqCdnUrl(raw: string, loopbackOrigin?: string) {
  let url: URL;
  try {
    if (raw.length > 8192 || /[\x00-\x20\x7f\\]/.test(raw)) throw Error();
    url = new URL(raw.startsWith("//") ? "https:" + raw : raw);
  } catch {
    throw mediaFailure("media_url_invalid");
  }
  const host = url.hostname.toLowerCase(),
    allowed =
      [
        "multimedia.nt.qq.com.cn",
        "gchat.qpic.cn",
        "c2cpicdw.qpic.cn",
        "p.qpic.cn",
      ].includes(host) ||
      (host.startsWith("gchat-") && host.endsWith(".myqcloud.com"));
  const fixture =
    loopbackOrigin &&
    new URL(loopbackOrigin).origin === url.origin &&
    ["127.0.0.1", "[::1]"].includes(host) &&
    ["http:", "https:"].includes(url.protocol);
  if (
    url.username ||
    url.password ||
    url.hash ||
    (!fixture &&
      (url.protocol !== "https:" ||
        (url.port && url.port !== "443") ||
        !allowed))
  )
    throw mediaFailure("media_url_not_allowed");
  return { url, fixture: !!fixture };
}
/** A credential-free, redirect-free GET. DNS approval is applied to the actual socket lookup. */
export function downloadQqImage(
  raw: string,
  signal: AbortSignal,
  loopbackOrigin?: string,
  kind = "image",
): Promise<Buffer> {
  const { url, fixture } = qqCdnUrl(raw, loopbackOrigin);
  return new Promise((resolve, reject) => {
    let failure: ApiError | undefined,
      bytes: Buffer | undefined,
      ended = false;
    const fail = (code: string) => (failure ??= mediaFailure(code));
    const request = (url.protocol === "https:" ? https : http).request(
      url,
      {
        method: "GET",
        agent: false,
        signal,
        headers: {
          accept: "image/*, application/octet-stream",
          "accept-encoding": "identity",
          "user-agent": "Prometheus/0.1",
        },
        lookup(host, options, callback) {
          lookup(host, { all: true, verbatim: true }, (error, addresses) => {
            if (error) return callback(error, []);
            if (
              !addresses.length ||
              addresses.some(
                (a) =>
                  !publicAddress(a.address) &&
                  !(fixture && ["127.0.0.1", "::1"].includes(a.address)),
              )
            ) {
              fail("media_dns_private");
              return callback(new Error("protected_network"), []);
            }
            if (options.all) callback(null, addresses);
            else callback(null, addresses[0]!.address, addresses[0]!.family);
          });
        },
      },
      (response) => {
        const length = response.headers["content-length"],
          type = response.headers["content-type"],
          encoding = response.headers["content-encoding"];
        if (response.statusCode !== 200) {
          fail("media_download_http");
          request.destroy();
          return;
        }
        if (
          length !== undefined &&
          (!/^\d+$/.test(length) ||
            !Number.isSafeInteger(Number(length)) ||
            Number(length) >
              (kind === "image" ? QQ_IMAGE_BYTES : 20 * 1024 * 1024))
        ) {
          fail("media_size_limit");
          request.destroy();
          return;
        }
        if (
          (encoding && encoding !== "identity") ||
          (type &&
            !imageMime(type) &&
            !(
              kind !== "image" &&
              type.split(";")[0]!.trim().toLowerCase() === "video/mp4"
            ) &&
            type.split(";")[0]!.trim().toLowerCase() !==
              "application/octet-stream")
        ) {
          fail("media_type_mismatch");
          request.destroy();
          return;
        }
        const chunks: Buffer[] = [];
        let size = 0;
        response.on("data", (chunk: Buffer) => {
          size += chunk.length;
          if (size > (kind === "image" ? QQ_IMAGE_BYTES : 20 * 1024 * 1024)) {
            fail("media_size_limit");
            response.destroy();
          } else chunks.push(chunk);
        });
        response.on("error", () => {
          fail("media_download_incomplete");
          request.destroy();
        });
        response.on("end", () => {
          if (!size) {
            fail("media_image_invalid");
            request.destroy();
            return;
          }
          bytes = Buffer.concat(chunks);
          ended = true;
        });
      },
    );
    request.setTimeout(3000, () => {
      fail("media_download_timeout");
      request.destroy();
    });
    request.on("error", () => {
      fail(
        signal.aborted ? "media_download_cancelled" : "media_download_failed",
      );
    });
    request.on("close", () => {
      if (failure) reject(failure);
      else if (ended && bytes) resolve(bytes);
      else reject(mediaFailure("media_download_incomplete"));
    });
    request.end();
  });
}

export type IncomingMedia = {
  url?: string;
  type?: string;
  size?: number;
  code?: string;
  kind?: string;
};
export function qqAttachment(
  kind: string,
  content: string,
  value: unknown,
): IncomingMedia | undefined {
  if (value === undefined || (Array.isArray(value) && value.length === 0))
    return undefined;
  if (kind !== "c2c") return { code: "media_channel_unsupported" };
  if (!Array.isArray(value) || value.length !== 1)
    return { code: "media_count_unsupported" };
  if (content.trimStart().startsWith("/") || content.includes("\0"))
    return { code: "media_control_command" };
  const a = value[0];
  if (!a || typeof a !== "object") return { code: "media_url_missing" };
  const type =
    typeof a.content_type === "string"
      ? a.content_type.split(";")[0]!.trim().toLowerCase()
      : undefined;
  if (
    type &&
    !imageMime(type) &&
    type !== "video/mp4" &&
    type !== "application/octet-stream"
  )
    return { code: "media_format_unsupported" };
  if (
    a.size !== undefined &&
    (!Number.isSafeInteger(a.size) ||
      a.size < 0 ||
      a.size > (type && imageMime(type) ? QQ_IMAGE_BYTES : 20 * 1024 * 1024))
  )
    return { code: "media_size_limit" };
  if (typeof a.url !== "string" || a.url.length > 8192)
    return { code: "media_url_missing" };
  return {
    url: a.url,
    type,
    size: a.size,
    kind:
      type && imageMime(type)
        ? "image"
        : type === "video/mp4"
          ? "video"
          : "auto",
  };
}
export function mediaNotice(code: string) {
  if (code.startsWith("wx_media_"))
    return "这条微信附件未能下载或校验，我没有读取内容。可以重新发送或用文字说明。";
  if (code === "media_video_unread")
    return "视频已经收到，我还不能读取里面的画面和声音。可以保存并回传，但不能据此判断内容。";
  if (code === "media_voice_unread")
    return "音频已经收到，这次没有做语音识别，可以按原来源回传。";
  if (code === "media_file_unread")
    return "文件已经收到，没有读取或执行文件内容，可以按原来源回传。";
  return code === "media_reply_failed"
    ? "这张图的回复没能完成，这次先不下判断。你可以重发一下，或者用文字告诉我。"
    : code === "media_count_unsupported"
      ? "先一张一张发我吧，我好仔细看。"
      : code === "media_channel_unsupported"
        ? "这张图我这边没打开，可以单独发给我吗？"
        : code === "media_control_command"
          ? "图片和要做的事分开发我吧，我好接着看。"
          : code === "media_size_limit"
            ? "这张图有点大，我这边没打开。换张小一点的发我吧。"
            : code === "vision_provider_unavailable"
              ? "图片理解还没有配置好，我这次没看到图片。可以先用文字跟我说。"
              : code === "media_format_unsupported"
                ? "这条附件我这边还没打开。可以换张图片，或者直接说说内容。"
                : code === "legacy_media_reference_lost"
                  ? "先前这条图片的下载引用没有保留下来，我没看到图片。请再发一次给我吧。"
                  : "这张图我这边没打开，你再发一次给我吧。";
}
