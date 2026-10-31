import { ApiError, bad, integer, missing, string } from "./errors.ts";
import { now } from "./security.ts";
import { diaryBounds } from "./diary-time.ts";
import type { Row, Store } from "./store.ts";
import { DiaryRestPlan } from "./diary-rest-plan.ts";
export type RestSlice = { cycle: string; asOf: number };
type Hooks = {
  discover: () => void;
  effective: (p: Row, model?: boolean) => boolean;
  floor: (p: Row) => number;
  capture: (p: Row, m: Row, cycle: string) => boolean;
  captureRecord: (p: Row, r: Row, cycle: string) => boolean;
  seal: (p: Row, period: string, date: string, slice: RestSlice) => Row;
  request: (e: Row, p: Row) => void;
};
const code = (e: unknown) =>
  e instanceof ApiError ? e.code : "rest_diary_preparation_failed";
export class DiaryRest {
  readonly planner: DiaryRestPlan;
  constructor(
    readonly store: Store,
    private hooks: Hooks,
  ) {
    this.planner = new DiaryRestPlan(store);
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
        const floor = this.hooks.floor(p);
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
          0,
          at,
        );
        this.store.run(
          "INSERT INTO rest_diary_refs SELECT ?,?,id,revision,happened_at FROM diary_events WHERE policy_id=? AND deleted_at IS NULL AND content IS NOT NULL AND happened_at<=?",
          cycle,
          String(p.id),
          String(p.id),
          at,
        );
        this.store.run(
          "INSERT INTO rest_diary_claims(cycle_id,entry_id,expected_revision,base_fingerprint) SELECT ?,id,revision,fingerprint FROM diary_entries WHERE policy_id=?",
          cycle,
          String(p.id),
        );
        if (String(p.source_key).startsWith("self:"))
          this.store.run(
            "INSERT INTO rest_diary_records(cycle_id,policy_id,record_id,record_revision,sequence,happened_at) SELECT ?,?,r.id,r.revision,r.rowid,CASE WHEN r.kind='diary' AND json_valid(r.payload) THEN coalesce(unixepoch(json_extract(r.payload,'$.date')||'T00:00:00+08:00'),r.created_at) ELSE r.created_at END FROM records r WHERE r.kind IN ('memory','diary') AND r.owner_id=? AND r.visibility=? AND r.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_entries e WHERE e.id=r.id)",
            cycle,
            String(p.id),
            String(p.owner_id),
            String(p.visibility),
          );
        if (!String(p.source_key).startsWith("self:")) {
          this.store.run(
            "INSERT INTO rest_diary_messages(cycle_id,policy_id,message_id,sequence,happened_at) SELECT ?,?,m.id,m.rowid,m.created_at FROM messages m JOIN runs r ON r.id=m.run_id JOIN conversations c ON c.id=m.conversation_id WHERE coalesce(c.source_key,c.id)=? AND c.owner_id=? AND r.status='completed' AND m.rowid>? AND m.created_at<=? AND NOT EXISTS(SELECT 1 FROM local_runs l WHERE l.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM automatic_runs a WHERE a.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM diary_events e WHERE e.policy_id=? AND e.origin_kind='message' AND e.origin_id=m.id AND (e.revision>1 OR e.deleted_at IS NOT NULL)) AND (m.role='user' OR m.role='assistant' AND NOT EXISTS(SELECT 1 FROM run_publications v WHERE v.run_id=r.id AND v.state='suppressed') AND (NOT EXISTS(SELECT 1 FROM run_targets t WHERE t.run_id=r.id) OR NOT EXISTS(SELECT 1 FROM delivery_parts d WHERE d.run_id=r.id AND d.status IN ('queued','sending')) AND EXISTS(SELECT 1 FROM delivery_parts d WHERE d.run_id=r.id AND d.status='sent')))",
            cycle,
            String(p.id),
            String(p.source_key),
            String(p.owner_id),
            floor,
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
      ) ||
      this.store.get(
        "SELECT record_id FROM rest_diary_records WHERE cycle_id=? AND policy_id=? AND happened_at>=? AND happened_at<? AND status='invalidated' LIMIT 1",
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
    if (
      ((write && e.locked) || e.deleted_at !== null) &&
      !(
        write &&
        e.locked &&
        e.deleted_at === null &&
        e.revision === c.expected_revision
      )
    )
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
    if (!e.locked && ["blocked", "failed"].includes(String(task.state))) {
      state = "blocked";
      error = String(task.error_code ?? "rest_diary_incomplete");
    } else if (
      !e.locked &&
      s.allow_model === 1 &&
      this.hooks.effective(p, true)
    ) {
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
  private prepare(s: Row, c: Row, budget: { requests: number }) {
    const p = this.policy(s),
      slice = { cycle: String(c.cycle_id), asOf: Number(c.frozen_at) };
    for (const r of this.store.all(
      "SELECT * FROM rest_diary_requests WHERE cycle_id=? AND policy_id=? AND status='pending' ORDER BY CASE period WHEN 'day' THEN 0 WHEN 'week' THEN 1 ELSE 2 END,date LIMIT ?",
      slice.cycle,
      String(p.id),
      Math.min(4, budget.requests),
    )) {
      this.store.transaction(() => {
        if (
          !this.store.get(
            "SELECT date FROM rest_diary_targets WHERE cycle_id=? AND policy_id=? AND period=? AND date=?",
            slice.cycle,
            String(p.id),
            String(r.period),
            String(r.date),
          )
        ) {
          let entry: Row | undefined,
            state = "local",
            error: string | null = null;
          try {
            this.assertSlice(p, String(r.period), String(r.date), slice);
            entry = this.hooks.seal(p, String(r.period), String(r.date), slice);
            const task = this.store.get(
              "SELECT * FROM diary_tasks WHERE entry_id=?",
              String(entry.id),
            )!;
            if (
              !entry.locked &&
              ["blocked", "failed"].includes(String(task.state))
            ) {
              state = "blocked";
              error = String(task.error_code ?? "rest_diary_incomplete");
            } else if (
              !entry.locked &&
              s.allow_model === 1 &&
              this.hooks.effective(p, true)
            ) {
              this.hooks.request(entry, p);
              this.store.run(
                "UPDATE diary_tasks SET cycle_id=? WHERE entry_id=?",
                slice.cycle,
                String(entry.id),
              );
              state = ["empty", "completed"].includes(
                String(
                  this.store.get(
                    "SELECT state FROM diary_tasks WHERE entry_id=?",
                    String(entry.id),
                  )!.state,
                ),
              )
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
            String(r.period),
            String(r.date),
            entry?.id ?? null,
            state,
            error,
          );
        }
        this.store.run(
          "UPDATE rest_diary_requests SET status='prepared' WHERE cycle_id=? AND policy_id=? AND period=? AND date=?",
          slice.cycle,
          String(p.id),
          String(r.period),
          String(r.date),
        );
      });
      budget.requests--;
    }
    return !this.store.get(
      "SELECT date FROM rest_diary_requests WHERE cycle_id=? AND policy_id=? AND status='pending' LIMIT 1",
      slice.cycle,
      String(p.id),
    );
  }
  tick() {
    const c = this.store.get(
      "SELECT d.* FROM rest_diary_cycles d JOIN rest_cycles r ON r.id=d.cycle_id WHERE r.completed_at IS NULL AND r.phase='consolidating' AND d.status IN ('frozen','preparing','model_wait') ORDER BY r.started_at DESC LIMIT 1",
    );
    if (!c) return;
    const budget = { messages: 64, plan: 64, requests: 8 };
    let turn = Number(
      this.store.get(
        "SELECT coalesce(max(turn),0) AS n FROM rest_diary_plan_state WHERE cycle_id=?",
        String(c.cycle_id),
      )!.n,
    );
    for (const s of this.store.all(
      "SELECT s.* FROM rest_diary_sources s LEFT JOIN rest_diary_plan_state p ON p.cycle_id=s.cycle_id AND p.policy_id=s.policy_id WHERE s.cycle_id=? AND s.status IN ('pending','capturing','planning','preparing') ORDER BY coalesce(p.turn,0),s.rowid LIMIT 16",
      String(c.cycle_id),
    )) {
      if (
        (["pending", "capturing"].includes(String(s.status)) &&
          !budget.messages) ||
        (s.status === "planning" && !budget.plan) ||
        (s.status === "preparing" && !budget.requests)
      )
        continue;
      this.store.run(
        "INSERT OR IGNORE INTO rest_diary_plan_state(cycle_id,policy_id) VALUES(?,?)",
        String(c.cycle_id),
        String(s.policy_id),
      );
      try {
        const p = this.policy(s);
        if (["pending", "capturing"].includes(String(s.status))) {
          for (const record of this.store.all(
            "SELECT * FROM rest_diary_records WHERE cycle_id=? AND policy_id=? AND status='pending' ORDER BY sequence LIMIT ?",
            String(c.cycle_id),
            String(s.policy_id),
            budget.messages,
          )) {
            this.store.transaction(() => {
              let state = "captured",
                error: string | null = null;
              try {
                if (!this.hooks.captureRecord(p, record, String(c.cycle_id)))
                  state = "excluded";
              } catch (e) {
                state = "invalidated";
                error = code(e);
              }
              if (state === "invalidated")
                this.planner.changedRecord(s, c, Number(record.happened_at));
              this.store.run(
                "UPDATE rest_diary_records SET status=?,error_code=? WHERE cycle_id=? AND policy_id=? AND record_id=?",
                state,
                error,
                String(c.cycle_id),
                String(p.id),
                String(record.record_id),
              );
              this.store.run(
                "UPDATE rest_diary_sources SET status='capturing',processed=processed+1,excluded=excluded+?,error_code=coalesce(error_code,?),updated_at=? WHERE cycle_id=? AND policy_id=?",
                +(state === "excluded"),
                error,
                now(),
                String(c.cycle_id),
                String(p.id),
              );
            });
            budget.messages--;
          }
          for (const m of this.store.all(
            "SELECT * FROM rest_diary_messages WHERE cycle_id=? AND policy_id=? AND sequence>? ORDER BY sequence LIMIT ?",
            String(c.cycle_id),
            String(s.policy_id),
            Number(s.cursor),
            budget.messages,
          )) {
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
              s.cursor = Number(m.sequence);
            });
            budget.messages--;
          }
          if (
            this.store.get(
              "SELECT record_id FROM rest_diary_records WHERE cycle_id=? AND policy_id=? AND status='pending' LIMIT 1",
              String(c.cycle_id),
              String(p.id),
            ) ||
            this.store.get(
              "SELECT message_id FROM rest_diary_messages WHERE cycle_id=? AND policy_id=? AND sequence>? LIMIT 1",
              String(c.cycle_id),
              String(p.id),
              Number(s.cursor),
            )
          )
            continue;
          s.status = "planning";
        }
        if (
          s.status === "planning" &&
          this.store.transaction(() => this.planner.advance(s, c, budget))
        )
          s.status = "preparing";
        if (s.status === "preparing" && this.prepare(s, c, budget))
          s.status = "prepared";
        this.store.run(
          "UPDATE rest_diary_sources SET status=?,updated_at=? WHERE cycle_id=? AND policy_id=?",
          String(s.status),
          now(),
          String(c.cycle_id),
          String(p.id),
        );
      } catch (e) {
        this.store.run(
          "UPDATE rest_diary_sources SET status='blocked',error_code=?,updated_at=? WHERE cycle_id=? AND policy_id=?",
          code(e),
          now(),
          String(c.cycle_id),
          String(s.policy_id),
        );
        this.store.run(
          "UPDATE rest_diary_requests SET status='interrupted' WHERE cycle_id=? AND policy_id=? AND status='pending'",
          String(c.cycle_id),
          String(s.policy_id),
        );
      } finally {
        this.store.run(
          "UPDATE rest_diary_plan_state SET turn=? WHERE cycle_id=? AND policy_id=?",
          ++turn,
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
    // Validate each source once; update terminal target states in SQL rather than
    // issuing several reads and a write for every historical day on every tick.
    for (const source of this.store.all(
      "SELECT * FROM rest_diary_sources WHERE cycle_id=? AND status<>'blocked'",
      cycle,
    )) {
      try {
        this.policy(source);
      } catch (e) {
        this.store.run(
          "UPDATE rest_diary_sources SET status='blocked',error_code=? WHERE cycle_id=? AND policy_id=?",
          code(e),
          cycle,
          String(source.policy_id),
        );
        this.store.run(
          "UPDATE rest_diary_targets SET state='invalidated',error_code=? WHERE cycle_id=? AND policy_id=? AND state IN ('pending','completed','local')",
          code(e),
          cycle,
          String(source.policy_id),
        );
        this.store.run(
          "UPDATE rest_diary_requests SET status='interrupted' WHERE cycle_id=? AND policy_id=? AND status='pending'",
          cycle,
          String(source.policy_id),
        );
      }
    }
    this.store.run(
      "UPDATE rest_diary_targets AS t SET state='invalidated',error_code='rest_diary_reference_changed' WHERE t.cycle_id=? AND t.state IN ('pending','completed','local') AND NOT EXISTS(SELECT 1 FROM diary_entries e WHERE e.id=t.entry_id AND e.state<>'invalidated' AND e.deleted_at IS NULL)",
      cycle,
    );
    this.store.run(
      "UPDATE rest_diary_targets AS t SET state='invalidated',error_code='rest_diary_target_changed' WHERE t.cycle_id=? AND t.state='pending' AND NOT EXISTS(SELECT 1 FROM diary_tasks d WHERE d.entry_id=t.entry_id AND d.cycle_id=t.cycle_id)",
      cycle,
    );
    this.store.run(
      "UPDATE rest_diary_targets AS t SET state=CASE WHEN (SELECT state FROM diary_tasks d WHERE d.entry_id=t.entry_id) IN ('completed','empty') THEN 'completed' ELSE 'blocked' END,error_code=CASE WHEN (SELECT state FROM diary_tasks d WHERE d.entry_id=t.entry_id) IN ('completed','empty') THEN NULL ELSE coalesce((SELECT error_code FROM diary_tasks d WHERE d.entry_id=t.entry_id),'rest_diary_incomplete') END WHERE t.cycle_id=? AND t.state='pending' AND EXISTS(SELECT 1 FROM diary_tasks d WHERE d.entry_id=t.entry_id AND d.cycle_id=t.cycle_id AND d.state IN ('completed','empty','failed','blocked','invalidated','interrupted'))",
      cycle,
    );
    const prep = this.store.get(
        "SELECT policy_id FROM rest_diary_sources WHERE cycle_id=? AND status IN ('pending','capturing','planning','preparing') LIMIT 1",
        cycle,
      ),
      pending = this.store.get(
        "SELECT entry_id FROM rest_diary_targets WHERE cycle_id=? AND state='pending' LIMIT 1",
        cycle,
      );
    if (!prep && !pending) {
      const problem = this.store.get(
        "SELECT policy_id FROM rest_diary_sources WHERE cycle_id=? AND (status<>'prepared' OR error_code IS NOT NULL) UNION ALL SELECT policy_id FROM rest_diary_targets WHERE cycle_id=? AND state NOT IN ('local','completed') LIMIT 1",
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
      "UPDATE rest_diary_records SET status='interrupted',error_code='rest_cycle_ended' WHERE cycle_id=? AND status='pending'",
      cycle,
    );
    this.store.run(
      "UPDATE rest_diary_requests SET status='interrupted' WHERE cycle_id=? AND status='pending'",
      cycle,
    );
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
      "UPDATE rest_diary_sources SET status='interrupted',error_code='rest_cycle_ended' WHERE cycle_id=? AND status IN ('pending','capturing','planning','preparing')",
      cycle,
    );
  }
  targetPage(actor: string, cycle: string, q = new URLSearchParams()) {
    string(cycle, "睡眠批次", 36, 36);
    const owns = this.store.get(
      "SELECT c.cycle_id FROM rest_diary_cycles c WHERE c.cycle_id=? AND (c.error_code='legacy_rest_snapshot_missing' OR EXISTS(SELECT 1 FROM rest_diary_sources s WHERE s.cycle_id=c.cycle_id AND s.manager_id=?))",
      cycle,
      actor,
    );
    if (!owns) throw missing();
    const before = q.has("before")
        ? integer(
            Number(q.get("before")),
            "分页游标",
            1,
            Number.MAX_SAFE_INTEGER,
          )
        : Number.MAX_SAFE_INTEGER,
      limit = integer(Number(q.get("limit") ?? 50), "分页", 1, 100),
      policy = q.has("policy_id")
        ? string(q.get("policy_id"), "来源分区", 36, 36)
        : null;
    const rows = this.store.all(
        "SELECT t.rowid AS cursor,t.* FROM rest_diary_targets t JOIN rest_diary_sources s ON s.cycle_id=t.cycle_id AND s.policy_id=t.policy_id WHERE t.cycle_id=? AND s.manager_id=? AND t.rowid<? AND (? IS NULL OR t.policy_id=?) ORDER BY t.rowid DESC LIMIT ?",
        cycle,
        actor,
        before,
        policy,
        policy,
        limit + 1,
      ),
      more = rows.length > limit;
    const items = rows.slice(0, limit);
    return { items, next: more ? items.at(-1)!.cursor : null };
  }
  summaries(actor: string) {
    const cycles = this.store.all(
      "SELECT c.cycle_id,c.frozen_at,c.status,c.error_code,c.started_at,c.ended_at FROM rest_diary_cycles c WHERE EXISTS(SELECT 1 FROM rest_diary_sources s WHERE s.cycle_id=c.cycle_id AND s.manager_id=?) OR c.error_code='legacy_rest_snapshot_missing' ORDER BY c.frozen_at DESC,c.rowid DESC LIMIT 10",
      actor,
    );
    return {
      items: cycles.map((c) => {
        const cycle = String(c.cycle_id),
          page = this.targetPage(actor, cycle),
          counts = (table: string, column: string) =>
            Object.fromEntries(
              this.store
                .all(
                  "SELECT t." +
                    column +
                    " AS state,count(*) AS n FROM " +
                    table +
                    " t JOIN rest_diary_sources s ON s.cycle_id=t.cycle_id AND s.policy_id=t.policy_id WHERE t.cycle_id=? AND s.manager_id=? GROUP BY t." +
                    column,
                  cycle,
                  actor,
                )
                .map((r) => [String(r.state), Number(r.n)]),
            );
        return {
          ...c,
          sources: this.store.all(
            "SELECT s.policy_id,s.source_key,s.owner_id,s.visibility,s.time_zone,s.allow_model,s.status,s.processed,s.excluded,s.error_code,CASE WHEN s.from_at=0 THEN 1 ELSE 0 END AS historical_backlog,coalesce(p.refs_done,0) AS refs_planned,coalesce(p.claims_done,0) AS entries_planned FROM rest_diary_sources s LEFT JOIN rest_diary_plan_state p ON p.cycle_id=s.cycle_id AND p.policy_id=s.policy_id WHERE s.cycle_id=? AND s.manager_id=? ORDER BY s.policy_id",
            cycle,
            actor,
          ),
          targets: page.items,
          targets_next: page.next,
          target_counts: counts("rest_diary_targets", "state"),
          request_counts: counts("rest_diary_requests", "status"),
        };
      }),
    };
  }
}
