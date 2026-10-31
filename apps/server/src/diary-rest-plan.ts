import { ApiError } from "./errors.ts";
import { diaryBounds, diaryDate, dateAdd } from "./diary-time.ts";
import type { Row, Store } from "./store.ts";

/** Plans from frozen metadata only. A schema13 window is never widened on resume. */
export class DiaryRestPlan {
  constructor(readonly store: Store) {}
  changedRecord(s: Row, c: Row, happenedAt: number) {
    this.date(s, c, diaryDate(happenedAt, String(s.time_zone)));
  }
  private request(s: Row, period: string, date: string) {
    this.store.run(
      "INSERT OR IGNORE INTO rest_diary_requests(cycle_id,policy_id,period,date) VALUES(?,?,?,?)",
      String(s.cycle_id),
      String(s.policy_id),
      period,
      date,
    );
  }
  private date(s: Row, c: Row, date: string, includeDay = true) {
    for (const period of ["day", "week", "month"]) {
      if (period === "day" && !includeDay) continue;
      try {
        const b = diaryBounds(period, date, String(s.time_zone));
        if (period === "day" || b.end_at <= Number(c.frozen_at))
          this.request(s, period, b.date);
      } catch (error) {
        // An unsupported old civil date must not starve other valid dates.
        this.request(s, period, date);
        this.store.run(
          "INSERT OR IGNORE INTO rest_diary_targets VALUES(?,?,?,?,NULL,'blocked',?)",
          String(s.cycle_id),
          String(s.policy_id),
          period,
          date,
          error instanceof ApiError
            ? error.code
            : "rest_diary_period_unsupported",
        );
      }
    }
  }
  advance(s: Row, c: Row, budget: { plan: number }) {
    this.store.run(
      "INSERT OR IGNORE INTO rest_diary_plan_state(cycle_id,policy_id) VALUES(?,?)",
      String(s.cycle_id),
      String(s.policy_id),
    );
    let state = this.state(s);
    if (!state.seeded) {
      const day = diaryDate(Number(c.frozen_at), String(s.time_zone)),
        week = dateAdd(diaryBounds("week", day, String(s.time_zone)).date, -7),
        month = diaryBounds(
          "month",
          dateAdd(diaryBounds("month", day, String(s.time_zone)).date, -1),
          String(s.time_zone),
        ).date;
      for (const [period, date] of [
        ["day", day],
        ["week", week],
        ["month", month],
      ])
        this.request(s, period!, date!);
      this.store.run(
        "UPDATE rest_diary_plan_state SET seeded=1,refs_done=?,claims_done=? WHERE cycle_id=? AND policy_id=?",
        +(Number(s.from_at) > 0),
        +(Number(s.from_at) > 0),
        String(s.cycle_id),
        String(s.policy_id),
      );
      state = this.state(s);
    }
    if (!state.refs_done && budget.plan) {
      const rows = this.store.all(
        "SELECT rowid AS cursor,happened_at FROM rest_diary_refs WHERE cycle_id=? AND policy_id=? AND rowid>? ORDER BY rowid LIMIT ?",
        String(s.cycle_id),
        String(s.policy_id),
        Number(state.ref_cursor),
        budget.plan,
      );
      for (const row of rows) {
        this.date(
          s,
          c,
          diaryDate(Number(row.happened_at), String(s.time_zone)),
        );
        this.store.run(
          "UPDATE rest_diary_plan_state SET ref_cursor=? WHERE cycle_id=? AND policy_id=?",
          Number(row.cursor),
          String(s.cycle_id),
          String(s.policy_id),
        );
        budget.plan--;
      }
      if (
        !this.store.get(
          "SELECT rowid FROM rest_diary_refs WHERE cycle_id=? AND policy_id=? AND rowid>? LIMIT 1",
          String(s.cycle_id),
          String(s.policy_id),
          Number(rows.at(-1)?.cursor ?? state.ref_cursor),
        )
      )
        this.store.run(
          "UPDATE rest_diary_plan_state SET refs_done=1 WHERE cycle_id=? AND policy_id=?",
          String(s.cycle_id),
          String(s.policy_id),
        );
      state = this.state(s);
    }
    if (state.refs_done && !state.claims_done && budget.plan) {
      const rows = this.store.all(
        "SELECT c.rowid AS cursor,e.period,e.period_start,e.locked,e.deleted_at FROM rest_diary_claims c JOIN diary_entries e ON e.id=c.entry_id JOIN diary_entry_versions v ON v.entry_id=e.id AND v.revision=e.revision WHERE c.cycle_id=? AND e.policy_id=? AND c.rowid>? AND (v.source_count>0 OR v.generator='manual' AND v.facts IS NOT NULL) ORDER BY c.rowid LIMIT ?",
        String(s.cycle_id),
        String(s.policy_id),
        Number(state.claim_cursor),
        budget.plan,
      );
      for (const row of rows) {
        if (row.deleted_at === null) {
          if (row.period === "day")
            this.date(s, c, String(row.period_start), !row.locked);
          else if (
            !row.locked &&
            diaryBounds(row.period, row.period_start, String(s.time_zone))
              .end_at <= Number(c.frozen_at)
          )
            this.request(s, String(row.period), String(row.period_start));
        }
        this.store.run(
          "UPDATE rest_diary_plan_state SET claim_cursor=? WHERE cycle_id=? AND policy_id=?",
          Number(row.cursor),
          String(s.cycle_id),
          String(s.policy_id),
        );
        budget.plan--;
      }
      if (
        !this.store.get(
          "SELECT c.rowid FROM rest_diary_claims c JOIN diary_entries e ON e.id=c.entry_id JOIN diary_entry_versions v ON v.entry_id=e.id AND v.revision=e.revision WHERE c.cycle_id=? AND e.policy_id=? AND c.rowid>? AND (v.source_count>0 OR v.generator='manual' AND v.facts IS NOT NULL) LIMIT 1",
          String(s.cycle_id),
          String(s.policy_id),
          Number(rows.at(-1)?.cursor ?? state.claim_cursor),
        )
      )
        this.store.run(
          "UPDATE rest_diary_plan_state SET claims_done=1 WHERE cycle_id=? AND policy_id=?",
          String(s.cycle_id),
          String(s.policy_id),
        );
    }
    state = this.state(s);
    return !!state.refs_done && !!state.claims_done;
  }
  private state(s: Row) {
    return this.store.get(
      "SELECT * FROM rest_diary_plan_state WHERE cycle_id=? AND policy_id=?",
      String(s.cycle_id),
      String(s.policy_id),
    )!;
  }
}
