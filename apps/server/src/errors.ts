export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}
export const bad = (message: string, code = "invalid_input") =>
  new ApiError(400, code, message);
export const missing = () =>
  new ApiError(404, "not_found", "记录不存在或不可访问");
export const conflict = () =>
  new ApiError(409, "revision_conflict", "版本已变化，请重新读取后合并修改");
export const unavailable = (capability: string) =>
  new ApiError(
    503,
    "capability_unavailable",
    `${capability}尚未完成 TypeScript / Windows 迁移与验收，当前不可用`,
  );
export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value))
    throw bad("请求必须是 JSON 对象");
  return value as Record<string, unknown>;
}
export function fields(
  value: unknown,
  names: readonly string[],
): Record<string, unknown> {
  const result = object(value);
  if (Object.keys(result).some((key) => !names.includes(key)))
    throw bad("请求包含未知字段");
  return result;
}
export function string(
  value: unknown,
  name: string,
  max: number,
  min = 0,
): string {
  if (
    typeof value !== "string" ||
    Buffer.byteLength(value) > max ||
    Buffer.byteLength(value.trim()) < min
  )
    throw bad(`${name}长度须在 ${min}–${max} 字节之间`);
  return value;
}
export function integer(
  value: unknown,
  name: string,
  min: number,
  max: number,
): number {
  if (
    !Number.isSafeInteger(value) ||
    (value as number) < min ||
    (value as number) > max
  )
    throw bad(`${name}须为 ${min}–${max} 的整数`);
  return value as number;
}
export function boolean(value: unknown, name: string): boolean {
  if (typeof value !== "boolean") throw bad(`${name}须为布尔值`);
  return value;
}
export function revision(value: string | undefined): number {
  if (!value)
    throw new ApiError(428, "if_match_required", "必须提供当前版本 If-Match");
  const match = /^(?:"([1-9][0-9]*)"|([1-9][0-9]*))$/.exec(value);
  if (!match) throw bad("If-Match 必须为正整数版本");
  return integer(
    Number(match[1] ?? match[2]),
    "If-Match",
    1,
    Number.MAX_SAFE_INTEGER,
  );
}
