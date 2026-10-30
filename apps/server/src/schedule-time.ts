import { Temporal } from "@js-temporal/polyfill";
import { ApiError, bad, boolean, fields, integer, string } from "./errors.ts";
export const DAY = 86400,
  MAX_TIME = 4102444800,
  MAX_ITEMS = 500;
export type Repeat = {
  frequency: "daily" | "weekly" | "monthly";
  interval: number;
  count: number | null;
  until: number | null;
};
export interface Activity {
  title: string;
  type: "reading" | "writing" | "video" | "consolidation" | "chat" | "action";
  purpose: string;
  start_at: number;
  end_at: number;
  time_zone: string;
  resource_id: string | null;
  target_ref: string | null;
  repeat: Repeat | null;
  movable: boolean;
  grant_id: string | null;
  restart_reading: boolean;
}
export function zone(value: unknown): string {
  const name = string(value, "IANA时区", 128, 1);
  if (/^[+-]/.test(name))
    throw bad("请选择IANA时区，不能使用固定偏移代替民用时区");
  try {
    return new Intl.DateTimeFormat("en", { timeZone: name }).resolvedOptions()
      .timeZone;
  } catch {
    throw bad("需要有效的IANA时区");
  }
}
export function zoned(at: number, timeZone: string) {
  return Temporal.Instant.fromEpochMilliseconds(at * 1000).toZonedDateTimeISO(
    timeZone,
  );
}
export function localText(at: number, timeZone: string) {
  return zoned(at, timeZone)
    .toPlainDateTime()
    .toString({ smallestUnit: "second" });
}
export function localInstant(value: unknown, timeZone: string) {
  const text = string(value, "本地日期时间", 19, 16);
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?$/.test(text))
    throw bad("本地日期时间格式无效");
  try {
    return (
      Number(
        Temporal.PlainDateTime.from(text, {
          overflow: "reject",
        }).toZonedDateTime(timeZone, { disambiguation: "reject" })
          .epochMilliseconds,
      ) / 1000
    );
  } catch {
    throw bad(
      "日期无效，或该民用时刻因夏令时不存在/有歧义，请选择明确时刻",
      "civil_time_invalid",
    );
  }
}
export function reference(value: unknown, name: string): string | null {
  if (value == null) return null;
  const id = string(value, name, 36, 36);
  if (!/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(id))
    throw bad(`${name}须为有效引用`);
  return id;
}
export function activity(value: unknown): Activity {
  const body = fields(value, [
    "title",
    "type",
    "purpose",
    "start_at",
    "end_at",
    "start_local",
    "end_local",
    "time_zone",
    "resource_id",
    "target_ref",
    "repeat",
    "movable",
    "grant_id",
    "restart_reading",
  ]);
  const timeZone = zone(body.time_zone),
    type = string(body.type, "活动类型", 30, 1);
  if (
    ![
      "reading",
      "writing",
      "video",
      "consolidation",
      "chat",
      "action",
    ].includes(type)
  )
    throw bad("活动类型无效");
  const instant = (epoch: unknown, local: unknown, name: string) => {
    if (epoch !== undefined && local !== undefined)
      throw bad("不能同时指定绝对时刻和本地时刻");
    return integer(
      local !== undefined ? localInstant(local, timeZone) : epoch,
      name,
      0,
      MAX_TIME,
    );
  };
  const start = instant(body.start_at, body.start_local, "开始时间"),
    end = instant(body.end_at, body.end_local, "结束时间");
  if (end <= start || end - start > 8 * 3600)
    throw bad("活动时长须大于0且不超过8小时");
  if (
    !zoned(start, timeZone)
      .toPlainDate()
      .equals(zoned(end, timeZone).toPlainDate())
  )
    throw bad("跨日活动请拆成各自然日分片");
  let repeat: Repeat | null = null;
  if (body.repeat != null) {
    const rule = fields(body.repeat, [
        "frequency",
        "interval",
        "count",
        "until",
        "until_local",
      ]),
      frequency = string(rule.frequency, "重复频率", 16, 1);
    if (!["daily", "weekly", "monthly"].includes(frequency))
      throw bad("重复频率无效");
    if (rule.until != null && rule.until_local)
      throw bad("不能同时指定两种重复结束时间");
    repeat = {
      frequency: frequency as Repeat["frequency"],
      interval: integer(rule.interval ?? 1, "重复间隔", 1, 31),
      count:
        rule.count == null ? null : integer(rule.count, "重复次数", 1, 3660),
      until:
        rule.until == null && !rule.until_local
          ? null
          : integer(
              rule.until_local
                ? localInstant(rule.until_local, timeZone)
                : rule.until,
              "重复结束时间",
              start,
              MAX_TIME,
            ),
    };
  }
  const result: Activity = {
    title: string(body.title, "标题", 240, 1),
    type: type as Activity["type"],
    purpose: string(body.purpose, "目的", 4000, 1),
    start_at: start,
    end_at: end,
    time_zone: timeZone,
    repeat,
    resource_id: reference(body.resource_id, "资料"),
    target_ref:
      body.target_ref == null
        ? null
        : string(body.target_ref, "固定目标引用", 256, 1),
    grant_id: reference(body.grant_id, "自主授权"),
    movable: boolean(body.movable ?? false, "允许自主移动"),
    restart_reading: boolean(body.restart_reading ?? false, "明确重读"),
  };
  if (result.type === "reading" && !result.resource_id)
    throw bad("阅读须引用明确授权的资料");
  if (result.type === "chat" && !result.target_ref)
    throw bad("聊天须绑定固定目标引用");
  if (
    [result.title, result.purpose, result.target_ref ?? ""].some((text) =>
      text.includes("\0"),
    )
  )
    throw bad("活动文本不能含空字符");
  return result;
}
export function opportunity(a: Activity, ordinal: number): number | null {
  if (!a.repeat) return ordinal === 0 ? a.start_at : null;
  if (ordinal >= (a.repeat.count ?? 3660) || ordinal >= 3660) return null;
  const base = zoned(a.start_at, a.time_zone).toPlainDateTime();
  try {
    const date = base.add(
      a.repeat.frequency === "monthly"
        ? { months: ordinal * a.repeat.interval }
        : {
            days:
              ordinal *
              a.repeat.interval *
              (a.repeat.frequency === "weekly" ? 7 : 1),
          },
      { overflow: "reject" },
    );
    const at =
      Number(
        date.toZonedDateTime(a.time_zone, { disambiguation: "reject" })
          .epochMilliseconds,
      ) / 1000;
    if (
      at < 0 ||
      at > MAX_TIME ||
      (a.repeat.until !== null && at > a.repeat.until)
    )
      return null;
    return at;
  } catch {
    return null;
  } // Invalid month days, DST gaps and repeated wall times are skipped.
}
export function expand(
  a: Activity,
  start: number,
  end: number,
  limit = MAX_ITEMS,
) {
  const result: {
    nominal_at: number;
    start_at: number;
    end_at: number;
    ordinal: number;
  }[] = [];
  const duration = a.end_at - a.start_at;
  if (
    a.repeat?.until !== null &&
    a.repeat?.until !== undefined &&
    (a.repeat.until < a.start_at || a.repeat.until + duration <= start)
  )
    return result;
  let firstOrdinal = 0;
  let lastOrdinal = a.repeat?.count ?? (a.repeat ? 3660 : 1);
  const ordinalNear = (timestamp: number) => {
    const base = zoned(a.start_at, a.time_zone).toPlainDate(),
      target = zoned(timestamp, a.time_zone).toPlainDate(),
      rule = a.repeat!;
    return rule.frequency === "monthly"
      ? ((target.year - base.year) * 12 + target.month - base.month) /
          rule.interval
      : base.until(target, { largestUnit: "day" }).days /
          (rule.interval * (rule.frequency === "weekly" ? 7 : 1));
  };
  if (a.repeat)
    lastOrdinal = Math.min(
      lastOrdinal,
      Math.max(0, Math.ceil(ordinalNear(end)) + 3),
    );
  if (a.repeat && start > a.start_at) {
    const base = zoned(a.start_at, a.time_zone).toPlainDate(),
      target = zoned(start, a.time_zone).toPlainDate();
    const steps =
      a.repeat.frequency === "monthly"
        ? ((target.year - base.year) * 12 + target.month - base.month) /
          a.repeat.interval
        : base.until(target, { largestUnit: "day" }).days /
          (a.repeat.interval * (a.repeat.frequency === "weekly" ? 7 : 1));
    firstOrdinal = Math.max(0, Math.floor(steps) - 2);
  }
  for (let ordinal = firstOrdinal; ordinal < lastOrdinal; ordinal++) {
    const at = opportunity(a, ordinal);
    if (at === null) continue;
    if (at >= end) break;
    if (at + duration <= start) continue;
    if (
      !zoned(at, a.time_zone)
        .toPlainDate()
        .equals(zoned(at + duration, a.time_zone).toPlainDate())
    )
      continue;
    result.push({
      nominal_at: at,
      start_at: at,
      end_at: at + duration,
      ordinal,
    });
    if (result.length > limit)
      throw new ApiError(
        409,
        "calendar_expansion_limit",
        "范围内活动过多，请缩短查询范围",
      );
  }
  return result;
}
export function calendarRange(date: string, view: string, timeZone: string) {
  if (
    !/^\d{4}-\d{2}-\d{2}$/.test(date) ||
    !["month", "week", "day"].includes(view)
  )
    throw bad("日历日期或视图无效");
  let chosen: Temporal.PlainDate;
  try {
    chosen = Temporal.PlainDate.from(date, { overflow: "reject" });
  } catch {
    throw bad("日历日期无效");
  }
  let first = view === "month" ? chosen.with({ day: 1 }) : chosen;
  if (view !== "day") first = first.subtract({ days: first.dayOfWeek - 1 });
  const count = view === "month" ? 42 : view === "week" ? 7 : 1;
  const boundary = (value: Temporal.PlainDate) =>
    Number(value.toZonedDateTime(timeZone).epochMilliseconds) / 1000;
  const cells = Array.from({ length: count }, (_, offset) => {
    const value = first.add({ days: offset });
    return {
      key: value.toString(),
      number: value.day,
      current: value.month === chosen.month,
      start_at: boundary(value),
      end_at: boundary(value.add({ days: 1 })),
    };
  });
  return {
    start: integer(cells[0]!.start_at, "范围起点", 0, MAX_TIME),
    end: integer(cells.at(-1)!.end_at, "范围终点", 0, MAX_TIME),
    cells,
  };
}
