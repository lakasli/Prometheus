let csrf = "";
export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}
export const setCsrf = (value: string) => {
  csrf = value;
};
export async function api<T>(
  path: string,
  method = "GET",
  body?: unknown,
  revision?: number,
  signal?: AbortSignal,
): Promise<T> {
  const headers: Record<string, string> = { accept: "application/json" };
  if (body !== undefined) headers["content-type"] = "application/json";
  if (method !== "GET") headers["x-csrf-token"] = csrf;
  if (revision !== undefined) headers["if-match"] = `"${revision}"`;
  const response = await fetch(`/api/admin/v1${path}`, {
    method,
    headers,
    credentials: "same-origin",
    cache: "no-store",
    redirect: "error",
    body: body === undefined ? undefined : JSON.stringify(body),
    signal,
  });
  const data = await response.json().catch(() => null);
  if (!response.ok)
    throw new ApiError(
      response.status,
      data?.error?.code ?? "request_failed",
      data?.error?.message ?? "连接未完成，请稍后重试",
    );
  return data as T;
}
