import { bad, boolean, fields, integer, string } from "./errors.ts";
import { civilDate } from "./diary-time.ts";
export const profileFields = [
  "name",
  "nickname",
  "age",
  "birthday",
  "job",
  "personality",
  "hobby",
  "preference",
  "commitment",
] as const;
export function profileValue(field: unknown, value: unknown) {
  if (!profileFields.includes(field as (typeof profileFields)[number]))
    throw bad("不支持的人物字段", "unknown_profile_field");
  if (value == null || Buffer.byteLength(JSON.stringify(value)) > 4000)
    throw bad("未知资料请留空，字段值不超过4000字节");
  if (field === "age") {
    const b = fields(value, ["years", "stated_at"]);
    return {
      years: integer(b.years, "年龄", 0, 150),
      stated_at: civilDate(b.stated_at).toString(),
    };
  }
  if (field === "birthday") {
    const b = fields(value, [
        "calendar",
        "precision",
        "year",
        "month",
        "day",
        "leap_month",
      ]),
      calendar = string(b.calendar, "历法", 16, 1),
      precision = string(b.precision, "精度", 16, 1);
    if (
      !["gregorian", "lunar"].includes(calendar) ||
      !["year", "month_day", "date"].includes(precision)
    )
      throw bad("生日须明确公历/农历与年/月日/完整日期精度");
    if (precision === "year") {
      if (
        b.month !== undefined ||
        b.day !== undefined ||
        b.leap_month !== undefined
      )
        throw bad("年份精度不能包含月日或闰月");
      return {
        calendar,
        precision,
        year: integer(b.year, "生日年份", 1800, 2200),
      };
    }
    if (precision === "month_day" && b.year !== undefined)
      throw bad("月日精度不能推测年份");
    const year =
        precision === "date"
          ? integer(b.year, "生日年份", 1800, 2200)
          : undefined,
      month = integer(b.month, "月份", 1, 12),
      day = integer(b.day, "日期", 1, calendar === "lunar" ? 30 : 31);
    if (calendar === "gregorian") {
      if (b.leap_month !== undefined) throw bad("公历不使用闰月标记");
      civilDate(
        `${year ?? 2000}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`,
      );
    }
    return {
      calendar,
      precision,
      ...(year === undefined ? {} : { year }),
      month,
      day,
      ...(calendar === "lunar"
        ? { leap_month: boolean(b.leap_month ?? false, "闰月") }
        : {}),
    };
  }
  if (Array.isArray(value)) {
    if (!value.length || value.length > 20) throw bad("字段数组须1至20项");
    return value.map((item) => string(item, "字段内容", 200, 1));
  }
  return string(value, "字段内容", 2000, 1);
}
