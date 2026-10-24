import type { SystemStatus } from "./types";

export function describeQqConnection(
  status: SystemStatus | null,
  stale: boolean,
) {
  if (stale)
    return { label: "状态刷新失败", short: "待刷新", connected: false };
  const connection = status?.channels?.qq?.connection_status;
  const labels: Record<string, { label: string; short: string }> = {
    disabled: { label: "未启用", short: "未启用" },
    connecting: { label: "正在连接 WebSocket", short: "连接中" },
    connected: { label: "WebSocket 已连接", short: "已连接" },
    backoff: { label: "连接失败，等待重试", short: "重试中" },
    webhook: { label: "Webhook 模式，等待回调", short: "等待回调" },
    stopped: { label: "连接任务已停止", short: "已停止" },
  };
  const result = connection ? labels[connection] : undefined;
  return {
    ...(result || { label: "尚未取得连接状态", short: "待采样" }),
    connected: connection === "connected",
  };
}
