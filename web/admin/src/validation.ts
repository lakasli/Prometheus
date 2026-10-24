import type { FieldError, RuntimeConfig, Timestamp } from "./types";

export function validateConfig(value: RuntimeConfig): FieldError[] {
  const errors: FieldError[] = [];
  if (!value.assistant.name.trim())
    errors.push({ field: "assistant.name", message: "请为助手填写一个名称。" });
  if (new TextEncoder().encode(value.assistant.name).length > 128)
    errors.push({
      field: "assistant.name",
      message: "助手名称请控制在 128 字节以内。",
    });
  if (new TextEncoder().encode(value.assistant.system_prompt).length > 32000)
    errors.push({
      field: "assistant.system_prompt",
      message: "系统提示词请控制在 32,000 字节以内。",
    });
  if (
    !Number.isInteger(value.assistant.max_concurrent_runs) ||
    value.assistant.max_concurrent_runs < 1 ||
    value.assistant.max_concurrent_runs > 2
  )
    errors.push({
      field: "assistant.max_concurrent_runs",
      message: "树莓派初始并发应为 1 或 2。",
    });
  if (
    !Number.isInteger(value.assistant.timeout_seconds) ||
    value.assistant.timeout_seconds < 5 ||
    value.assistant.timeout_seconds > 300
  )
    errors.push({
      field: "assistant.timeout_seconds",
      message: "超时应为 5 到 300 秒。",
    });
  try {
    const endpoint = new URL(value.model.base_url);
    if (
      !["https:", "http:"].includes(endpoint.protocol) ||
      endpoint.username ||
      endpoint.password
    )
      throw new Error();
  } catch {
    errors.push({
      field: "model.base_url",
      message: "请填写不含账户密码的 HTTP(S) API 地址。",
    });
  }
  if (!value.model.model.trim())
    errors.push({ field: "model.model", message: "请填写模型标识。" });
  if (
    value.qq.enabled &&
    (!/^\d{1,32}$/.test(value.qq.app_id) || !value.qq.credential_ref)
  )
    errors.push({
      field: "qq",
      message: "启用 QQ 前请填写数字 App ID（最多 32 位）和 App Secret。",
    });
  return errors;
}

export function formatBytes(value?: number | null): string {
  if (value === undefined || value === null || !Number.isFinite(value))
    return "等待采样";
  return value >= 1024 ** 3
    ? `${(value / 1024 ** 3).toFixed(1)} GB`
    : `${Math.round(value / 1024 ** 2)} MB`;
}

export function formatDate(value?: Timestamp | null): string {
  if (value === undefined || value === null || value === "") return "—";
  const date = new Date(
    typeof value === "number" && value < 1e12 ? value * 1000 : value,
  );
  return Number.isNaN(date.getTime())
    ? String(value)
    : new Intl.DateTimeFormat("zh-CN", {
        dateStyle: "medium",
        timeStyle: "short",
        hour12: false,
      }).format(date);
}
