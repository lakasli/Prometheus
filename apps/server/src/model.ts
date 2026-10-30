import { lookup } from "node:dns";
import { isIP } from "node:net";
import http from "node:http";
import https from "node:https";
import { modelUrl } from "./config.ts";
import type { Config } from "./config.ts";
import { ApiError } from "./errors.ts";

export interface ModelMessage {
  role: "system" | "user" | "assistant";
  content: string;
}
/** This policy is checked on the addresses supplied to the socket, including DNS changes. */
export function publicAddress(address: string): boolean {
  if (isIP(address) === 4) {
    const [a, b, c] = address.split(".").map(Number) as [
      number,
      number,
      number,
      number,
    ];
    return !(
      a === 0 ||
      a === 10 ||
      a === 127 ||
      a >= 224 ||
      (a === 169 && b === 254) ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 &&
        (b === 168 || b === 0 || b === 2 || (b === 88 && c === 99))) ||
      (a === 100 && b >= 64 && b <= 127) ||
      (a === 198 && (b === 18 || b === 19 || (b === 51 && c === 100))) ||
      (a === 203 && b === 0 && c === 113)
    );
  }
  if (isIP(address) !== 6) return false;
  const normalized = address.toLowerCase();
  const [first, second] = normalized.split(":");
  const prefix = Number.parseInt(first!, 16),
    subnet = Number.parseInt(second || "0", 16);
  return (
    prefix >= 0x2000 &&
    prefix <= 0x3fff &&
    prefix !== 0x2002 &&
    prefix !== 0x3fff &&
    !(prefix === 0x2001 && (subnet < 0x200 || subnet === 0xdb8))
  );
}
const loopbackAddress = (address: string) =>
  address === "127.0.0.1" || address === "::1";
export function allowedAddress(
  address: string,
  allowLoopback: boolean,
): boolean {
  return publicAddress(address) || (allowLoopback && loopbackAddress(address));
}
export function requestJson(
  url: URL,
  body: unknown,
  headers: Record<string, string>,
  signal: AbortSignal,
  allowLoopback = false,
): Promise<unknown> {
  const hostname = url.hostname.replace(/^\[|\]$/g, "");
  if (isIP(hostname) && !allowedAddress(hostname, allowLoopback))
    return Promise.reject(
      new ApiError(400, "private_model_address", "模型地址解析到受保护的网络"),
    );
  const payload = Buffer.from(JSON.stringify(body));
  if (payload.length > 128 * 1024)
    return Promise.reject(
      new ApiError(413, "context_too_large", "模型请求超过字节预算"),
    );
  return new Promise((resolve, reject) => {
    let failure: ApiError | null = null;
    const connectionError = () =>
      new ApiError(
        signal.aborted ? 408 : 502,
        signal.aborted ? "model_cancelled" : "model_connection_failed",
        signal.aborted
          ? "模型请求已取消或超时"
          : "模型连接失败；请检查地址、凭据和网络策略",
      );
    const request = (url.protocol === "https:" ? https : http).request(
      url,
      {
        method: "POST",
        signal,
        agent: false,
        headers: {
          ...headers,
          "content-type": "application/json",
          accept: "application/json",
          "content-length": String(payload.length),
        },
        lookup(host, options, callback) {
          lookup(host, { all: true, verbatim: true }, (error, addresses) => {
            if (error) return callback(error, []);
            if (
              !addresses.length ||
              addresses.some(
                (item) => !allowedAddress(item.address, allowLoopback),
              )
            )
              return callback(new Error("protected_network"), []);
            if (options.all) callback(null, addresses);
            else callback(null, addresses[0]!.address, addresses[0]!.family);
          });
        },
      },
      (response) => {
        if (
          !response.statusCode ||
          response.statusCode < 200 ||
          response.statusCode >= 300
        ) {
          failure = new ApiError(
            502,
            "model_http_error",
            `模型服务返回 HTTP ${response.statusCode ?? "未知"}；响应正文未写入日志`,
          );
          request.destroy();
          return;
        }
        const chunks: Buffer[] = [];
        let size = 0;
        response.on("data", (chunk: Buffer) => {
          size += chunk.length;
          if (size > 1024 * 1024)
            response.destroy(new Error("response_too_large"));
          else chunks.push(chunk);
        });
        response.on("error", () => {
          failure ??= connectionError();
          if (request.closed) reject(failure);
          else request.destroy();
        });
        response.on("end", () => {
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
          } catch {
            reject(
              new ApiError(
                502,
                "invalid_model_response",
                "模型响应不是有效 JSON",
              ),
            );
          }
        });
      },
    );
    request.on("error", (error) => {
      failure ??= error instanceof ApiError ? error : connectionError();
    });
    // Cancellation/errors keep the physical lease until the socket is closed.
    request.on("close", () => {
      if (failure) reject(failure);
    });
    request.end(payload);
  });
}
export async function complete(
  config: Config,
  key: string,
  messages: ModelMessage[],
  signal: AbortSignal,
  allowLoopback = false,
): Promise<string> {
  const base = modelUrl(config.model.base_url, allowLoopback);
  base.pathname = `${base.pathname.replace(/\/+$/, "")}/chat/completions`;
  const response = (await requestJson(
    base,
    { model: config.model.model, messages, max_tokens: 4096, stream: false },
    { authorization: `Bearer ${key}` },
    signal,
    allowLoopback,
  )) as {
    choices?: { message?: { content?: unknown; tool_calls?: unknown } }[];
  };
  const message = response?.choices?.[0]?.message;
  if (
    typeof message?.content !== "string" ||
    !message.content.trim() ||
    Buffer.byteLength(message.content) > 32000 ||
    message.tool_calls
  )
    throw new ApiError(
      502,
      "invalid_model_response",
      "模型未返回有效的文字回复",
    );
  return message.content;
}
