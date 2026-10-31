import { ApiError, bad } from "./errors.ts";
import { now } from "./security.ts";
import { diaryBounds, diaryDate, dateAdd } from "./diary-time.ts";
import type { Row, Store } from "./store.ts";
export type RestSlice = { cycle: string; asOf: number };
type Hooks = {
  discover: () => void;
  effective: (p: Row, model?: boolean) => boolean;
  floor: (p: Row) => number;
  capture: (p: Row, m: Row, cycle: string) => boolean;
  seal: (p: Row, period: string, date: string, slice: RestSlice) => Row;
  request: (e: Row, p: Row) => void;
};
const code = (e: unknown) =>
  e instanceof ApiError ? e.code : "rest_diary_preparation_failed";
export class DiaryRest {
  constructor(
    readonly store: Store,
    private hooks: Hooks,
  ) {}
  private window(p: Row, at: number) {
    const day = diaryDate(at, String(p.time_zone)),
      week = dateAdd(diaryBounds("week", day, String(p.time_zone)).date, -7),
      month = diaryBounds(
        "month",
        dateAdd(diaryBounds("month", day, String(p.time_zone)).date, -1),
        String(p.time_zone),
      ).date;
    return {
      day,
      week,
      month,
      from: Math.min(
        diaryBounds("week", week, String(p.time_zone)).start_at,
        diaryBounds("month", month, String(p.time_zone)).start_at,
      ),
    };
  }
  freeze(cycle: string) {
    if (
      this.store.get(
        "SELECT cycle_id FROM rest_diary_cycles WHERE cycle_id=?",
        cycle,
      )
    )
      return;
    const at = now();
    this.hooks.discover();
    this.store.run(
      "INSERT INTO rest_diary_cycles(cycle_id,frozen_at,message_cutoff) VALUES(?,?,?)",
      cycle,
      at,
      Number(
        this.store.get("SELECT coalesce(max(rowid),0) AS n FROM messages")!.n,
      ),
    );
    let cursor = "";
    while (true) {
      const policies = this.store.all(
        "SELECT * FROM diary_policies WHERE id>? ORDER BY id LIMIT 16",
        cursor,
      );
      if (!policies.length) break;
      for (const p of policies) {
        cursor = String(p.id);
        if (!this.hooks.effective(p)) continue;
        const w = this.window(p, at),
          floor = this.hooks.floor(p);
        this.store.run(
          "INSERT INTO rest_diary_sources(cycle_id,policy_id,policy_revision,manager_id,source_key,owner_id,visibility,time_zone,allow_model,floor,from_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
          cycle,
          String(p.id),
          Number(p.revision),
          String(p.manager_id),
          String(p.source_key),
          String(p.owner_id),
          String(p.visibility),
          String(p.time_zone),
          +this.hooks.effective(p, true),
          floor,
          w.from,
          at,
        );
        this.store.run(
          "INSERT INTO rest_diary_refs SELECT ?,?,id,revision,happened_at FROM diary_events WHERE policy_id=? AND deleted_at IS NULL AND content IS NOT NULL AND happened_at>=? AND happened_at<=?",
          cycle,
          String(p.id),
          String(p.id),
          w.from,
          at,
        );
        this.store.run(
          "INSERT INTO rest_diary_claims(cycle_id,entry_id,expected_revision,base_fingerprint) SELECT ?,id,revision,fingerprint FROM diary_entries WHERE policy_id=?",
          cycle,
          String(p.id),
        );
        if (!String(p.source_key).startsWith("self:")) {
          this.store.run(
            "INSERT INTO rest_diary_messages(cycle_id,policy_id,message_id,sequence,happened_at) SELECT ?,?,m.id,m.rowid,m.created_at FROM messages m JOIN runs r ON r.id=m.run_id JOIN conversations c ON c.id=m.conversation_id WHERE coalesce(c.source_key,c.id)=? AND c.owner_id=? AND r.status='completed' AND m.rowid>? AND (m.created_at>=? OR m.role='assistant' AND EXISTS(SELECT 1 FROM delivery_parts d WHERE d.run_id=r.id AND d.status='sent' AND d.updated_at>=?)) AND m.created_at<=? AND NOT EXISTS(SELECT 1 FROM local_runs l WHERE l.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM automatic_runs a WHERE a.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM diary_events e WHERE e.policy_id=? AND e.origin_kind='message' AND e.origin_id=m.id AND (e.revision>1 OR e.deleted_at IS NOT NULL)) AND (m.role='user' OR m.role='assistant' AND NOT EXISTS(SELECT 1 FROM run_publications v WHERE v.run_id=r.id AND v.state='suppressed') AND (NOT EXISTS(SELECT 1 FROM run_targets t WHERE t.run_id=r.id) OR NOT EXISTS(SELECT 1 FROM delivery_parts d WHERE d.run_id=r.id AND d.status IN ('queued','sending')) AND EXISTS(SELECT 1 FROM delivery_parts d WHERE d.run_id=r.id AND d.status='sent')))",
            cycle,
            String(p.id),
            String(p.source_key),
            String(p.owner_id),
            floor,
            w.from,
            w.from,
            at,
            String(p.id),
          );
          this.store.run(
            "INSERT INTO rest_diary_parts SELECT ?,?,s.message_id,d.id,d.updated_at FROM rest_diary_messages s JOIN messages m ON m.id=s.message_id JOIN delivery_parts d ON d.run_id=m.run_id WHERE s.cycle_id=? AND s.policy_id=? AND m.role='assistant' AND d.status='sent' AND d.kind='text'",
            cycle,
            String(p.id),
            cycle,
            String(p.id),
          );
          this.store.run(
            "UPDATE rest_diary_messages SET happened_at=(SELECT max(sent_at) FROM rest_diary_parts d WHERE d.cycle_id=rest_diary_messages.cycle_id AND d.policy_id=rest_diary_messages.policy_id AND d.message_id=rest_diary_messages.message_id) WHERE cycle_id=? AND policy_id=? AND EXISTS(SELECT 1 FROM rest_diary_parts d WHERE d.cycle_id=rest_diary_messages.cycle_id AND d.policy_id=rest_diary_messages.policy_id AND d.message_id=rest_diary_messages.message_id)",
            cycle,
            String(p.id),
          );
        }
      }
    }
  }
  begin(cycle: string) {
    if (
      !this.store.get(
        "SELECT cycle_id FROM rest_diary_cycles WHERE cycle_id=?",
        cycle,
      )
    ) {
      this.store.run(
        "INSERT INTO rest_diary_cycles(cycle_id,frozen_at,message_cutoff,status,error_code,ended_at) VALUES(?, ?,0,'interrupted','legacy_rest_snapshot_missing',?)",
        cycle,
        now(),
        now(),
      );
      return;
    }
    this.store.run(
      "UPDATE rest_diary_cycles SET status='preparing',started_at=coalesce(started_at,?) WHERE cycle_id=? AND status='frozen'",
      now(),
      cycle,
    );
    this.tick();
  }
  private policy(s: Row) {
    const p = this.store.get(
      "SELECT * FROM diary_policies WHERE id=?",
      String(s.policy_id),
    );
    if (
      !p ||
      p.revision !== s.policy_revision ||
      !this.hooks.effective(p) ||
      p.owner_id !== s.owner_id ||
      p.source_key !== s.source_key ||
      p.visibility !== s.visibility ||
      p.time_zone !== s.time_zone ||
      this.hooks.floor(p) > Number(s.floor)
    )
      throw bad("冻结的来源许可或删除水位已变化", "rest_diary_source_changed");
    return p;
  }
  assertSlice(p: Row, period: string, date: string, slice: RestSlice) {
    const s = this.store.get(
      "SELECT * FROM rest_diary_sources WHERE cycle_id=? AND policy_id=?",
      slice.cycle,
      String(p.id),
    );
    if (!s) throw bad("没有冻结的日期来源", "rest_diary_source_changed");
    this.policy(s);
    const b = diaryBounds(period, date, String(p.time_zone));
    if (
      this.store.get(
        "SELECT r.event_id FROM rest_diary_refs r JOIN diary_events e ON e.id=r.event_id WHERE r.cycle_id=? AND r.policy_id=? AND r.happened_at>=? AND r.happened_at<? AND (e.revision<>r.event_revision OR e.deleted_at IS NOT NULL OR e.content IS NULL) LIMIT 1",
        slice.cycle,
        String(p.id),
        b.start_at,
        b.end_at,
      ) ||
      this.store.get(
        "SELECT message_id FROM rest_diary_messages WHERE cycle_id=? AND policy_id=? AND happened_at>=? AND happened_at<? AND status='invalidated' LIMIT 1",
        slice.cycle,
        String(p.id),
        b.start_at,
        b.end_at,
      )
    )
      throw bad("冻结来源已被更正或遗忘", "rest_diary_reference_changed");
  }
  claim(id: string, slice: RestSlice, write = true) {
    this.store.run(
      "INSERT OR IGNORE INTO rest_diary_claims(cycle_id,entry_id,expected_revision) VALUES(?,?,0)",
      slice.cycle,
      id,
    );
    const c = this.store.get(
        "SELECT * FROM rest_diary_claims WHERE cycle_id=? AND entry_id=?",
        slice.cycle,
        id,
      )!,
      e = this.store.get("SELECT * FROM diary_entries WHERE id=?", id);
    if (!e) return;
    if ((write && e.locked) || e.deleted_at !== null)
      throw bad("人工保护或已遗忘周期没有覆盖", "rest_diary_target_protected");
    if (e.revision === c.expected_revision || e.revision === c.owned_revision)
      return;
    const v = this.store.get(
      "SELECT generator FROM diary_entry_versions WHERE entry_id=? AND revision=?",
      id,
      Number(e.revision),
    );
    if (
      String(v?.generator).startsWith("model:") &&
      (e.fingerprint === c.base_fingerprint ||
        e.fingerprint === c.owned_fingerprint)
    )
      return;
    throw bad("睡眠冻结后的周期版本已改变", "rest_diary_target_changed");
  }
  owned(e: Row, slice: RestSlice) {
    this.store.run(
      "UPDATE rest_diary_claims SET owned_revision=?,owned_fingerprint=? WHERE cycle_id=? AND entry_id=?",
      Number(e.revision),
      String(e.fingerprint),
      slice.cycle,
      String(e.id),
    );
  }
  registerParent(p: Row, e: Row, slice: RestSlice) {
    const s = this.store.get(
      "SELECT * FROM rest_diary_sources WHERE cycle_id=? AND policy_id=?",
      slice.cycle,
      String(p.id),
    )!;
    if (
      this.store.get(
        "SELECT entry_id FROM rest_diary_targets WHERE cycle_id=? AND policy_id=? AND period=? AND date=?",
        slice.cycle,
        String(p.id),
        String(e.period),
        String(e.period_start),
      )
    )
      return;
    const task = this.store.get(
      "SELECT * FROM diary_tasks WHERE entry_id=?",
      String(e.id),
    )!;
    let state = "local",
      error: string | null = null;
    if (["blocked", "failed"].includes(String(task.state))) {
      state = "blocked";
      error = String(task.error_code ?? "rest_diary_incomplete");
    } else if (s.allow_model === 1 && this.hooks.effective(p, true)) {
      this.hooks.request(e, p);
      this.store.run(
        "UPDATE diary_tasks SET cycle_id=? WHERE entry_id=?",
        slice.cycle,
        String(e.id),
      );
      const current = this.store.get(
        "SELECT state FROM diary_tasks WHERE entry_id=?",
        String(e.id),
      )!;
      state = ["empty", "completed"].includes(String(current.state))
        ? "completed"
        : "pending";
    }
    this.store.run(
      "INSERT INTO rest_diary_targets VALUES(?,?,?,?,?,?,?)",
      slice.cycle,
      String(p.id),
      String(e.period),
      String(e.period_start),
      String(e.id),
      state,
      error,
    );
  }
  private prepare(s: Row, c: Row) {
    const p = this.policy(s),
      w = this.window(p, Number(c.frozen_at)),
      slice = { cycle: String(c.cycle_id), asOf: Number(c.frozen_at) };
    for (const [period, date] of [
      ["day", w.day],
      ["week", w.week],
      ["month", w.month],
    ]) {
      if (
        this.store.get(
          "SELECT entry_id FROM rest_diary_targets WHERE cycle_id=? AND policy_id=? AND period=? AND date=?",
          slice.cycle,
          String(p.id),
          period!,
          date!,
        )
      )
        continue;
      this.store.transaction(() => {
        let entry: Row | undefined,
          state = "local",
          error: string | null = null;
        try {
          this.assertSlice(p, period!, date!, slice);
          entry = this.hooks.seal(p, period!, date!, slice);
          const task = this.store.get(
            "SELECT * FROM diary_tasks WHERE entry_id=?",
            String(entry.id),
          )!;
          if (task.state === "blocked" || task.state === "failed") {
            state = "blocked";
            error = String(task.error_code ?? "rest_diary_incomplete");
          } else if (s.allow_model === 1 && this.hooks.effective(p, true)) {
            this.hooks.request(entry, p);
            this.store.run(
              "UPDATE diary_tasks SET cycle_id=? WHERE entry_id=?",
              slice.cycle,
              String(entry.id),
            );
            const updated = this.store.get(
              "SELECT state FROM diary_tasks WHERE entry_id=?",
              String(entry.id),
            )!;
            state = ["empty", "completed"].includes(String(updated.state))
              ? "completed"
              : "pending";
          }
        } catch (e) {
          state = "blocked";
          error = code(e);
        }
        this.store.run(
          "INSERT INTO rest_diary_targets VALUES(?,?,?,?,?,?,?)",
          slice.cycle,
          String(p.id),
          period!,
          date!,
          entry?.id ?? null,
          state,
          error,
        );
      });
    }
  }
  tick() {
    const c = this.store.get(
      "SELECT d.* FROM rest_diary_cycles d JOIN rest_cycles r ON r.id=d.cycle_id WHERE r.completed_at IS NULL AND r.phase='consolidating' AND d.status IN ('frozen','preparing','model_wait') ORDER BY r.started_at DESC LIMIT 1",
    );
    if (!c) return;
    let remaining = 64;
    for (const s of this.store.all(
      "SELECT * FROM rest_diary_sources WHERE cycle_id=? AND status IN ('pending','capturing') ORDER BY updated_at,rowid LIMIT 16",
      String(c.cycle_id),
    )) {
      try {
        const p = this.policy(s);
        const messages = this.store.all(
          "SELECT * FROM rest_diary_messages WHERE cycle_id=? AND policy_id=? AND sequence>? ORDER BY sequence LIMIT ?",
          String(c.cycle_id),
          String(s.policy_id),
          Number(s.cursor),
          remaining,
        );
        for (const m of messages) {
          this.store.transaction(() => {
            let state = "captured",
              error: string | null = null;
            try {
              if (!this.hooks.capture(p, m, String(c.cycle_id)))
                state = "excluded";
            } catch (e) {
              state = "invalidated";
              error = code(e);
            }
            this.store.run(
              "UPDATE rest_diary_messages SET status=?,error_code=? WHERE cycle_id=? AND policy_id=? AND message_id=?",
              state,
              error,
              String(c.cycle_id),
              String(p.id),
              String(m.message_id),
            );
            this.store.run(
              "UPDATE rest_diary_sources SET status='capturing',cursor=?,processed=processed+1,excluded=excluded+?,updated_at=? WHERE cycle_id=? AND policy_id=?",
              Number(m.sequence),
              +(state === "excluded"),
              now(),
              String(c.cycle_id),
              String(p.id),
            );
          });
          remaining--;
        }
        const current = this.store.get(
          "SELECT * FROM rest_diary_sources WHERE cycle_id=? AND policy_id=?",
          String(c.cycle_id),
          String(p.id),
        )!;
        const pending = this.store.get(
          "SELECT message_id FROM rest_diary_messages WHERE cycle_id=? AND policy_id=? AND sequence>? LIMIT 1",
          String(c.cycle_id),
          String(p.id),
          Number(current.cursor),
        );
        if (!pending) {
          this.prepare(current, c);
          this.store.run(
            "UPDATE rest_diary_sources SET status='prepared',updated_at=? WHERE cycle_id=? AND policy_id=?",
            now(),
            String(c.cycle_id),
            String(p.id),
          );
        }
      } catch (e) {
        this.store.run(
          "UPDATE rest_diary_sources SET status='blocked',error_code=?,updated_at=? WHERE cycle_id=? AND policy_id=?",
          code(e),
          now(),
          String(c.cycle_id),
          String(s.policy_id),
        );
      }
    }
    this.refresh(String(c.cycle_id));
  }
  private refresh(cycle: string) {
    const batch = this.store.get(
      "SELECT status FROM rest_diary_cycles WHERE cycle_id=?",
      cycle,
    );
    if (!batch || ["interrupted", "partial"].includes(String(batch.status)))
      return;
    for (const row of this.store.all(
      "SELECT t.*,s.policy_revision FROM rest_diary_targets t JOIN rest_diary_sources s ON s.cycle_id=t.cycle_id AND s.policy_id=t.policy_id WHERE t.cycle_id=? AND t.state IN ('pending','completed','local')",
      cycle,
    )) {
      let state = String(row.state),
        error: string | null = null;
      try {
        this.policy(
          this.store.get(
            "SELECT * FROM rest_diary_sources WHERE cycle_id=? AND policy_id=?",
            cycle,
            String(row.policy_id),
          )!,
        );
        const e = this.store.get(
          "SELECT * FROM diary_entries WHERE id=?",
          String(row.entry_id),
        );
        if (!e || e.state === "invalidated" || e.deleted_at !== null)
          throw bad("周期来源已失效", "rest_diary_reference_changed");
        if (state === "pending") {
          const t = this.store.get(
            "SELECT * FROM diary_tasks WHERE entry_id=?",
            String(row.entry_id),
          );
          if (!t || t.cycle_id !== cycle)
            throw bad("任务所属周期已改变", "rest_diary_target_changed");
          if (["completed", "empty"].includes(String(t.state)))
            state = "completed";
          else if (
            ["failed", "blocked", "invalidated", "interrupted"].includes(
              String(t.state),
            )
          ) {
            state = "blocked";
            error = String(t.error_code ?? "rest_diary_incomplete");
          }
        }
      } catch (e) {
        state = "invalidated";
        error = code(e);
      }
      this.store.run(
        "UPDATE rest_diary_targets SET state=?,error_code=? WHERE cycle_id=? AND policy_id=? AND period=? AND date=?",
        state,
        error,
        cycle,
        String(row.policy_id),
        String(row.period),
        String(row.date),
      );
    }
    const prep = this.store.get(
        "SELECT policy_id FROM rest_diary_sources WHERE cycle_id=? AND status IN ('pending','capturing') LIMIT 1",
        cycle,
      ),
      pending = this.store.get(
        "SELECT entry_id FROM rest_diary_targets WHERE cycle_id=? AND state='pending' LIMIT 1",
        cycle,
      );
    if (!prep && !pending) {
      const problem = this.store.get(
        "SELECT policy_id FROM rest_diary_sources WHERE cycle_id=? AND status<>'prepared' UNION ALL SELECT policy_id FROM rest_diary_targets WHERE cycle_id=? AND state NOT IN ('local','completed') LIMIT 1",
        cycle,
        cycle,
      );
      this.store.run(
        "UPDATE rest_diary_cycles SET status=?,ended_at=coalesce(ended_at,?) WHERE cycle_id=?",
        problem ? "partial" : "completed",
        now(),
        cycle,
      );
    } else
      this.store.run(
        "UPDATE rest_diary_cycles SET status=? WHERE cycle_id=?",
        prep ? "preparing" : "model_wait",
        cycle,
      );
  }
  pending(cycle: string) {
    this.refresh(cycle);
    return !!this.store.get(
      "SELECT cycle_id FROM rest_diary_cycles WHERE cycle_id=? AND status IN ('frozen','preparing','model_wait')",
      cycle,
    );
  }
  admitTask(task: Row) {
    if (!task.cycle_id) return true;
    const target = this.store.get(
      "SELECT t.*,d.status AS cycle_status FROM rest_diary_targets t JOIN rest_diary_cycles d ON d.cycle_id=t.cycle_id WHERE t.cycle_id=? AND t.entry_id=?",
      String(task.cycle_id),
      String(task.entry_id),
    );
    return (
      !!target &&
      target.state === "pending" &&
      ["preparing", "model_wait"].includes(String(target.cycle_status))
    );
  }
  end(cycle: string) {
    this.refresh(cycle);
    this.store.run(
      "UPDATE rest_diary_cycles SET status='partial',error_code='rest_cycle_ended',ended_at=? WHERE cycle_id=? AND status IN ('frozen','preparing','model_wait')",
      now(),
      cycle,
    );
    this.store.run(
      "UPDATE rest_diary_targets SET state='interrupted',error_code='rest_cycle_ended' WHERE cycle_id=? AND state='pending'",
      cycle,
    );
    this.store.run(
      "UPDATE rest_diary_sources SET status='interrupted',error_code='rest_cycle_ended' WHERE cycle_id=? AND status IN ('pending','capturing')",
      cycle,
    );
  }
  summaries(actor: string) {
    const cycles = this.store.all(
      "SELECT c.cycle_id,c.frozen_at,c.status,c.error_code,c.started_at,c.ended_at FROM rest_diary_cycles c WHERE EXISTS(SELECT 1 FROM rest_diary_sources s WHERE s.cycle_id=c.cycle_id AND s.manager_id=?) OR c.error_code='legacy_rest_snapshot_missing' ORDER BY c.frozen_at DESC,c.rowid DESC LIMIT 10",
      actor,
    );
    return {
      items: cycles.map((c) => ({
        ...c,
        sources: this.store.all(
          "SELECT policy_id,source_key,owner_id,visibility,time_zone,allow_model,status,processed,excluded,error_code FROM rest_diary_sources WHERE cycle_id=? AND manager_id=? ORDER BY policy_id",
          String(c.cycle_id),
          actor,
        ),
        targets: this.store.all(
          "SELECT t.* FROM rest_diary_targets t JOIN rest_diary_sources s ON s.cycle_id=t.cycle_id AND s.policy_id=t.policy_id WHERE t.cycle_id=? AND s.manager_id=? ORDER BY t.period,t.date,t.policy_id",
          String(c.cycle_id),
          actor,
        ),
      })),
    };
  }
}
