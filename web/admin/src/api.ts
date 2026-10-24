const PREFIX = "/api/admin/v1";
let csrfToken = "";

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export function setCsrfToken(value: string) {
  csrfToken = value;
}
export function clearCsrfToken() {
  csrfToken = "";
}

export async function api<T>(
  path: string,
  options: {
    method?: string;
    body?: unknown;
    revision?: number | string;
    etag?: string;
    idempotencyKey?: string;
    file?: Blob;
    contentType?: string;
    signal?: AbortSignal;
  } = {},
): Promise<T> {
  const method = options.method ?? "GET";
  const headers: Record<string, string> = { Accept: "application/json" };
  if (options.body !== undefined) headers["Content-Type"] = "application/json";
  if (method !== "GET" && csrfToken) headers["X-CSRF-Token"] = csrfToken;
  if (options.revision !== undefined)
    headers["If-Match"] = `"${options.revision}"`;
  if (options.etag !== undefined)
    headers["If-Match"] = `"${options.etag.replace(/^"|"$/g, "")}"`;
  if (options.idempotencyKey)
    headers["Idempotency-Key"] = options.idempotencyKey;
  if (options.file)
    headers["Content-Type"] = options.contentType || "application/octet-stream";
  const response = await fetch(`${PREFIX}${path}`, {
    method,
    headers,
    credentials: "same-origin",
    cache: "no-store",
    body:
      options.file ??
      (options.body === undefined ? undefined : JSON.stringify(options.body)),
    signal: options.signal,
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) {
    const code =
      typeof data?.error === "object" ? data.error.code : data?.error;
    const message =
      typeof data?.error === "object" ? data.error.message : data?.message;
    throw new ApiError(
      response.status,
      String(code || "request_failed"),
      response.status === 409 &&
        (code === "revision_conflict" || path.startsWith("/config/"))
        ? "配置已被更新，请重新载入后合并修改。"
        : String(message || `请求未完成（${response.status}），请稍后重试。`),
    );
  }
  return data as T;
}

export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return "连接未完成。请检查服务状态与 SSH 隧道，然后重试。";
}
