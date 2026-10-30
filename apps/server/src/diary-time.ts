import { Temporal } from "@js-temporal/polyfill";
import { bad, string } from "./errors.ts";
import { MAX_TIME, zone, zoned } from "./schedule-time.ts";
export function civilDate(value: unknown) {
  const text = string(value, "日记日期", 10, 10);
  if (!/^\d{4}-\d{2}-\d{2}$/.test(text)) throw bad("日记日期无效");
  try {
    return Temporal.PlainDate.from(text, { overflow: "reject" });
  } catch {
    throw bad("日记日期无效");
  }
}
export function diaryBounds(period: unknown, date: unknown, timeZone: unknown) {
  const name = zone(timeZone),
    chosen = civilDate(date);
  if (!["day", "week", "month"].includes(String(period)))
    throw bad("日记周期须为日、周或月");
  const start =
    period === "week"
      ? chosen.subtract({ days: chosen.dayOfWeek - 1 })
      : period === "month"
        ? chosen.with({ day: 1 })
        : chosen;
  const end = start.add(
    period === "week"
      ? { days: 7 }
      : period === "month"
        ? { months: 1 }
        : { days: 1 },
  );
  const instant = (day: Temporal.PlainDate) =>
    Number(day.toZonedDateTime(name).epochMilliseconds) / 1000;
  const begin = instant(start),
    finish = instant(end);
  if (begin < 0 || finish > MAX_TIME) throw bad("日记日期超出支持范围");
  return {
    period: String(period),
    date: start.toString(),
    end_date: end.toString(),
    time_zone: name,
    start_at: begin,
    end_at: finish,
    days: start.until(end, { largestUnit: "day" }).days,
  };
}
export const diaryDate = (at: number, timeZone: string) =>
  zoned(at, timeZone).toPlainDate().toString();
export const dateAdd = (date: string, days: number) =>
  civilDate(date).add({ days }).toString();
