import { describe, expect, it } from "vitest";
import {
  calendarRange,
  dateKey,
  localInput,
  localTimestamp,
} from "../src/life-systems";

import { shiftMonth } from "../src/calendar-extras";

describe("calendar wall-clock boundaries", () => {
  it("uses the selected zone, including Shanghai midnight", () => {
    const t = localTimestamp("2026-10-26T00:00", "Asia/Shanghai");
    expect(new Date(t * 1000).toISOString()).toBe("2026-10-25T16:00:00.000Z");
    expect(dateKey(t)).toBe("2026-10-26");
    expect(localInput(t)).toBe("2026-10-26T00:00");
  });
  it("does not stretch a 23-hour DST day or truncate a 25-hour day", () => {
    const spring = calendarRange("2026-03-08", "day", "America/New_York");
    const autumn = calendarRange("2026-11-01", "day", "America/New_York");
    expect(spring.end - spring.start).toBe(23 * 3600);
    expect(autumn.end - autumn.start).toBe(25 * 3600);
    expect(() =>
      localTimestamp("2026-03-08T02:30", "America/New_York"),
    ).toThrow();
    expect(() =>
      localTimestamp("2026-11-01T01:30", "America/New_York"),
    ).toThrow();
  });
  it("bounds month expansion and starts weeks on Monday across years", () => {
    expect(shiftMonth("2026-01-31", 1)).toBe("2026-02-28");
    expect(shiftMonth("2026-12-31", 1)).toBe("2027-01-31");
    const range = calendarRange("2027-01-01", "week", "Asia/Shanghai");
    expect(range.days[0]).toBe("2026-12-28");
    expect(range.days.at(-1)).toBe("2027-01-03");
    expect(
      calendarRange("2026-10-26", "month", "Asia/Shanghai").days,
    ).toHaveLength(42);
  });
});
