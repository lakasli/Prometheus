export const stateLabels: Record<string, string> = {
  draft: "草稿",
  pending_approval: "待批准",
  enabled: "已启用",
  paused: "已暂停",
  cancelled: "已取消",
  pending: "待执行",
  queued: "待执行",
  running: "进行中",
  completed: "已完成",
  partial: "部分完成",
  failed: "失败",
  skipped: "已跳过",
  blocked: "已阻断",
  unknown: "结果未知",
  final: "已封存",
  invalidated: "来源已失效",
  active: "有效",
  revoked: "已撤销",
  accepted: "已确认",
  rejected: "已拒绝",
  planned: "已计划",
  incomplete: "观测不完整",
  day: "日记",
  week: "周记",
  month: "月记",
};
export const activityLabels: Record<string, string> = {
  writing: "写作",
  sleep: "睡觉",
  wake: "起床",
  reading: "阅读",
  read: "阅读",
  video: "视频学习",
  consolidation: "整理",
  diary: "日记整理",
  chat: "主动聊天",
  action: "已注册动作",
  legacy_trigger: "兼容任务",
  daily: "日记",
  weekly: "周记",
  monthly: "月记",
};
export function label(value: string) {
  return stateLabels[value] || activityLabels[value] || value;
}
export function dateKey(timestamp: number, timeZone = "Asia/Shanghai") {
  const p = new Intl.DateTimeFormat("en-CA", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(new Date(timestamp * 1000));
  const part = (type: string) => p.find((x) => x.type === type)!.value;
  return `${part("year")}-${part("month")}-${part("day")}`;
}
export function shiftDay(day: string, amount: number) {
  const date = new Date(`${day}T12:00:00Z`);
  date.setUTCDate(date.getUTCDate() + amount);
  return date.toISOString().slice(0, 10);
}
function localParts(timestamp: number, timeZone: string) {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  }).formatToParts(new Date(timestamp * 1000));
  const n = (type: string) => Number(parts.find((x) => x.type === type)!.value);
  return (
    Date.UTC(
      n("year"),
      n("month") - 1,
      n("day"),
      n("hour"),
      n("minute"),
      n("second"),
    ) / 1000
  );
}
/** Resolve a wall time using the selected IANA zone, never the browser zone.
 * Reject DST gaps and ambiguous clock times rather than silently moving a plan. */
export function localTimestamp(value: string, timeZone = "Asia/Shanghai") {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/.test(value))
    throw new Error("请填写完整日期与时间。");
  const wall = Date.parse(`${value}Z`) / 1000;
  if (!Number.isFinite(wall)) throw new Error("日期与时间无效。");
  const offsets = new Set<number>();
  for (const hours of [-36, 0, 36]) {
    const sample = wall + hours * 3600;
    offsets.add(localParts(sample, timeZone) - sample);
  }
  const matches = [...offsets]
    .map((offset) => wall - offset)
    .filter((t) => localParts(t, timeZone) === wall);
  if (matches.length !== 1)
    throw new Error("这个时间处于夏令时跳变或重复区间，请选择另一个明确时间。");
  return matches[0]!;
}
export function localInput(timestamp: number, timeZone = "Asia/Shanghai") {
  return new Date(localParts(timestamp, timeZone) * 1000)
    .toISOString()
    .slice(0, 16);
}
export function displayTime(
  timestamp: number | null | undefined,
  timeZone = "Asia/Shanghai",
) {
  if (timestamp == null) return "未指定";
  return new Intl.DateTimeFormat("zh-CN", {
    timeZone,
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date(timestamp * 1000));
}
export type CalendarView = "month" | "week" | "day" | "agenda";
export function calendarDays(anchor: string, view: CalendarView) {
  if (view === "day") return [anchor];
  const weekday = (new Date(`${anchor}T12:00:00Z`).getUTCDay() + 6) % 7;
  const first =
    view === "month" ? `${anchor.slice(0, 7)}-01` : shiftDay(anchor, -weekday);
  const start =
    view === "month"
      ? shiftDay(first, -((new Date(`${first}T12:00:00Z`).getUTCDay() + 6) % 7))
      : first;
  return Array.from({ length: view === "month" ? 42 : 7 }, (_, index) =>
    shiftDay(start, index),
  );
}
export function calendarRange(
  anchor: string,
  view: CalendarView,
  timeZone: string,
) {
  const days = calendarDays(anchor, view);
  return {
    days,
    start: localTimestamp(`${days[0]}T00:00`, timeZone),
    end: localTimestamp(`${shiftDay(days.at(-1)!, 1)}T00:00`, timeZone),
  };
}
