import { randomUUID } from "node:crypto";
import {
  ApiError,
  bad,
  conflict,
  fields,
  integer,
  string,
  unavailable,
} from "./errors.ts";
import { hash, now } from "./security.ts";
import type { Store, Row } from "./store.ts";
import type { Runtime } from "./runtime.ts";
import type { Knowledge } from "./knowledge.ts";
const DAY = 86400,
  MICRO = 1000000;
type Policy = {
  initial: number;
  min: number;
  max: number;
  decay_per_day: number;
  max_event_delta: number;
  max_daily_delta: number;
  grace_seconds: number;
  downtime_cap_seconds: number;
  decay_mode: "minimum" | "zero";
};
const defaults = (): Policy => ({
  initial: 50,
  min: 0,
  max: 100,
  decay_per_day: 1,
  max_event_delta: 5,
  max_daily_delta: 10,
  grace_seconds: 3 * DAY,
  downtime_cap_seconds: 7 * DAY,
  decay_mode: "minimum",
});
const legacy = (): Policy => ({
  ...defaults(),
  initial: 0,
  min: -100,
  max: 100,
  decay_per_day: 0.1,
  max_event_delta: 10,
  grace_seconds: 0,
  decay_mode: "zero",
});
const round = (value: number) => Math.round(value * MICRO);
export class Affinity {
  private timer?: ReturnType<typeof setInterval>;
  private heartbeat = 0;
  constructor(
    private store: Store,
    private runtime: Runtime,
    private knowledge: Knowledge,
  ) {
    this.initialize();
    this.recover(now());
  }
  policy(owner: string) {
    const result = this.knowledge.policy(`affinity:${owner}`, defaults());
    return {
      revision: result.revision,
      value: result.value as unknown as Policy,
    };
  }
  private initialize() {
    this.store.transaction(() => {
      if (
        this.store.get(
          "SELECT value FROM metadata WHERE key='affinity.ledger.initialized'",
        )
      )
        return;
      const cutoff = Number(
        this.store.get(
          "SELECT value FROM metadata WHERE key='affinity.legacy_cutoff'",
        )?.value ?? 0,
      );
      // Preserve saved policies, including former custom scales. New defaults do not rewrite existing data.
      for (const row of this.store.all(
        "SELECT * FROM feature_policies WHERE name LIKE 'affinity:%'",
      )) {
        const value = { ...legacy(), ...JSON.parse(String(row.value_json)) };
        this.store.run(
          "UPDATE feature_policies SET value_json=? WHERE name=?",
          JSON.stringify(value),
          String(row.name),
        );
        this.store.run(
          "INSERT OR IGNORE INTO affinity_policy_history VALUES(?,?,?,?,?)",
          String(row.name).slice("affinity:".length),
          Number(row.revision),
          JSON.stringify(value),
          cutoff || now(),
          "兼容迁移时刻的策略快照，不推测此前版本",
        );
      }
      for (const person of this.store.all(
        "SELECT * FROM records WHERE kind='people' AND deleted_at IS NULL",
      )) {
        const owner = String(person.owner_id);
        if (cutoff) {
          if (
            !this.store.get(
              "SELECT name FROM feature_policies WHERE name=?",
              `affinity:${owner}`,
            )
          )
            this.store.run(
              "INSERT INTO feature_policies VALUES(?,0,?,?)",
              `affinity:${owner}`,
              JSON.stringify(legacy()),
              cutoff,
            );
          const policy = this.policy(owner);
          let value = policy.value.initial,
            last = Number(person.created_at);
          for (const event of this.store.all(
            "SELECT * FROM affinity_events WHERE person_id=? AND reason IS NOT NULL ORDER BY created_at,rowid",
            String(person.id),
          )) {
            value = this.legacyDecay(
              value,
              Number(event.created_at) - last,
              policy.value.decay_per_day,
            );
            value = Math.max(
              policy.value.min,
              Math.min(policy.value.max, value + Number(event.delta)),
            );
            last = Number(event.created_at);
          }
          value = this.legacyDecay(
            value,
            Math.max(0, cutoff - last),
            policy.value.decay_per_day,
          );
          this.store.run(
            "INSERT INTO affinity_states VALUES(?,?,?,?,NULL,0,?)",
            String(person.id),
            owner,
            round(value),
            cutoff,
            policy.revision,
          );
          this.store.run(
            "INSERT OR IGNORE INTO affinity_policy_history VALUES(?,?,?,?,?)",
            owner,
            policy.revision,
            JSON.stringify(policy.value),
            cutoff,
            "兼容迁移时刻的策略快照，不推测此前版本",
          );
        } else this.ensure(person);
      }
      this.store.run(
        "INSERT INTO metadata VALUES('affinity.ledger.initialized','1')",
      );
    });
  }
  private legacyDecay(value: number, elapsed: number, rate: number) {
    const reduction = (Math.max(0, elapsed) / DAY) * rate;
    return value >= 0
      ? Math.max(0, value - reduction)
      : Math.min(0, value + reduction);
  }
  ensure(person: Row) {
    const owner = String(person.owner_id),
      policy = this.policy(owner);
    this.store.run(
      "INSERT OR IGNORE INTO affinity_states VALUES(?,?,?,?,NULL,0,?)",
      String(person.id),
      owner,
      round(policy.value.initial),
      Number(person.created_at),
      policy.revision,
    );
    this.store.run(
      "INSERT OR IGNORE INTO affinity_policy_history VALUES(?,?,?,?,?)",
      owner,
      policy.revision,
      JSON.stringify(policy.value),
      Number(person.created_at),
      "初始策略或兼容迁移快照",
    );
  }
  private state(person: string) {
    return this.store.get(
      "SELECT * FROM affinity_states WHERE person_id=?",
      person,
    )!;
  }
  settle(person: string, at = now(), supplied?: Policy) {
    const state = this.state(person),
      policy = supplied ?? this.policy(String(state.owner_id)).value;
    const end = Math.max(at, Number(state.settled_at));
    const created = Number(
      this.store.get("SELECT created_at FROM records WHERE id=?", person)!
        .created_at,
    );
    const start = Math.max(
      Number(state.settled_at),
      Number(state.last_direct_at ?? created) + policy.grace_seconds,
    );
    const numerator =
      BigInt(Math.max(0, end - start)) * BigInt(round(policy.decay_per_day)) +
      BigInt(Number(state.remainder));
    const loss = Number(numerator / BigInt(DAY)),
      current = Number(state.score_micro);
    const score = Math.max(
      round(policy.min),
      Math.min(
        round(policy.max),
        policy.decay_mode === "zero"
          ? current >= 0
            ? Math.max(0, current - loss)
            : Math.min(0, current + loss)
          : current - loss,
      ),
    );
    const settled =
      score === round(policy.min) ||
      (policy.decay_mode === "zero" && score === 0);
    this.store.run(
      "UPDATE affinity_states SET score_micro=?,settled_at=?,remainder=? WHERE person_id=?",
      score,
      end,
      settled ? 0 : Number(numerator % BigInt(DAY)),
      person,
    );
    return this.state(person);
  }
  private validate(value: unknown, confirmation = true) {
    const body = fields(value, [
      ...Object.keys(defaults()),
      "confirmed",
      "reason",
    ]);
    if (confirmation && body.confirmed !== true)
      throw bad("好感规则修改需要明确确认，旧区间按原策略结算");
    const reason = string(
      body.reason ?? "",
      "策略变更依据",
      2000,
      confirmation ? 1 : 0,
    );
    const min = integer(body.min, "下限", 0, 100),
      max = integer(body.max, "上限", 1, 100);
    if (min >= max) throw bad("下限须小于上限");
    const rate = body.decay_per_day;
    if (
      typeof rate !== "number" ||
      !Number.isFinite(rate) ||
      rate < 0 ||
      rate > 10
    )
      throw bad("每日衰减须为0至10");
    if (body.decay_mode !== "minimum" && body.decay_mode !== "zero")
      throw bad("衰减方向无效");
    const policy: Policy = {
      initial: integer(body.initial, "初始值", min, max),
      min,
      max,
      decay_per_day: round(rate) / MICRO,
      max_event_delta: integer(body.max_event_delta, "单次上限", 1, 5),
      max_daily_delta: integer(body.max_daily_delta, "每日绝对变化上限", 1, 10),
      grace_seconds: integer(body.grace_seconds, "空闲宽限", 0, 30 * DAY),
      downtime_cap_seconds: integer(
        body.downtime_cap_seconds,
        "停机补算上限",
        0,
        30 * DAY,
      ),
      decay_mode: body.decay_mode,
    };
    return { policy, reason };
  }
  save(owner: string, expected: number, value: unknown) {
    const { policy, reason } = this.validate(value);
    return this.store.transaction(() => {
      const current = this.policy(owner);
      if (current.revision !== expected) throw conflict();
      const at = Math.max(
        now(),
        Number(
          this.store.get(
            "SELECT max(settled_at) AS n FROM affinity_states WHERE owner_id=?",
            owner,
          )?.n ?? 0,
        ),
      );
      for (const person of this.store.all(
        "SELECT * FROM records WHERE owner_id=? AND kind='people' AND deleted_at IS NULL",
        owner,
      )) {
        this.ensure(person);
        this.settle(String(person.id), at, current.value);
      }
      this.store.run(
        "INSERT OR IGNORE INTO affinity_policy_history VALUES(?,?,?,?,?)",
        owner,
        current.revision,
        JSON.stringify(current.value),
        0,
        "修改前策略快照",
      );
      const saved = this.knowledge.setPolicy(
        `affinity:${owner}`,
        expected,
        policy,
      );
      this.store.run(
        "INSERT INTO affinity_policy_history VALUES(?,?,?,?,?)",
        owner,
        saved.revision,
        JSON.stringify(policy),
        at,
        this.store.redact(reason),
      );
      this.store.run(
        "UPDATE affinity_states SET score_micro=max(?,min(?,score_micro)),policy_revision=? WHERE owner_id=?",
        round(policy.min),
        round(policy.max),
        saved.revision,
        owner,
      );
      this.store.audit(
        owner,
        "relationships",
        "completed",
        "好感规则已分段结算并保存新版本",
        `revision:${saved.revision}`,
      );
      return this.policy(owner);
    });
  }
  history(owner: string) {
    return {
      items: this.store
        .all(
          "SELECT revision,value_json,effective_at,reason FROM affinity_policy_history WHERE owner_id=? ORDER BY revision DESC LIMIT 100",
          owner,
        )
        .map((row) => ({
          revision: row.revision,
          value: JSON.parse(String(row.value_json)),
          effective_at: row.effective_at,
          reason: this.store.redact(String(row.reason)),
        })),
    };
  }
  preview(owner: string, value: unknown) {
    const body = fields(value, ["policy", "idle_seconds"]),
      { policy } = this.validate(body.policy, false);
    const idle = integer(body.idle_seconds, "空闲时间", 0, 3650 * DAY),
      loss =
        (Math.max(0, idle - policy.grace_seconds) / DAY) * policy.decay_per_day;
    return {
      preview_only: true,
      current_revision: this.policy(owner).revision,
      projected_from_initial: Math.max(
        policy.min,
        Math.min(policy.max, policy.initial - loss),
      ),
      contact_permission_changed: false,
    };
  }
  describe(person: { id: string; owner_id: string; created_at: number }) {
    this.ensure(person as unknown as Row);
    const state = this.settle(person.id);
    return {
      affinity: Number(state.score_micro) / MICRO,
      settled_at: state.settled_at,
      last_direct_at: state.last_direct_at,
      policy_revision: state.policy_revision,
      events: this.store.all(
        "SELECT e.*,d.event_key,d.requested_micro,d.policy_revision,d.kind FROM affinity_events e LEFT JOIN affinity_event_details d ON d.event_id=e.id WHERE e.person_id=? AND e.owner_id=? AND e.reason IS NOT NULL ORDER BY e.created_at DESC,e.rowid DESC LIMIT 20",
        person.id,
        person.owner_id,
      ),
      contact_permission: false,
      autonomous_sending: false,
    };
  }
  add(owner: string, person: string, expected: number, value: unknown) {
    const body = fields(value, [
      "delta",
      "reason",
      "reviewed",
      "event_key",
      "kind",
    ]);
    if (body.reviewed !== true) throw bad("好感变化需要独立人工确认");
    if (body.kind === "direct_interaction")
      throw unavailable("具有来源证明和人物会话绑定的直接互动计分");
    if (body.kind !== "manual_adjustment") throw bad("好感事件类型无效");
    const reason = string(body.reason, "变化依据", 2000, 1),
      eventKey = string(body.event_key, "幂等键", 160, 1);
    if (typeof body.delta !== "number" || !Number.isFinite(body.delta))
      throw bad("变化值无效");
    const requested = round(body.delta),
      fingerprint = hash(
        JSON.stringify({ delta: requested, reason, kind: body.kind }),
      );
    return this.store.transaction(() => {
      const profile = this.runtime.records.get("people", person, owner);
      this.ensure(profile as unknown as Row);
      const duplicate = this.store.get(
        "SELECT fingerprint FROM affinity_event_details WHERE person_id=? AND event_key=?",
        person,
        eventKey,
      );
      if (duplicate) {
        if (duplicate.fingerprint !== fingerprint) throw conflict();
        return { ...profile, ...this.describe(profile) };
      }
      if (profile.revision !== expected) throw conflict();
      const policy = this.policy(owner);
      if (Math.abs(requested) > round(policy.value.max_event_delta))
        throw bad("单次好感变化超过上限");
      const at = Math.max(now(), Number(this.state(person).settled_at)),
        dayStart = at - (at % DAY);
      const total = Number(
        this.store.get(
          "SELECT coalesce(sum(abs(coalesce(d.requested_micro,round(e.delta*1000000)))),0) AS n FROM affinity_events e LEFT JOIN affinity_event_details d ON d.event_id=e.id WHERE e.person_id=? AND e.owner_id=? AND e.reason IS NOT NULL AND e.created_at>=?",
          person,
          owner,
          dayStart,
        )!.n,
      );
      if (total + Math.abs(requested) > round(policy.value.max_daily_delta))
        throw new ApiError(
          400,
          "daily_affinity_limit",
          "每日好感变化绝对累计超过上限",
        );
      const state = this.settle(person, at),
        score = Number(state.score_micro),
        next = Math.max(
          round(policy.value.min),
          Math.min(round(policy.value.max), score + requested),
        ),
        id = randomUUID();
      this.store.run(
        "INSERT INTO affinity_events VALUES(?,?,?,?,?,?)",
        id,
        owner,
        person,
        (next - score) / MICRO,
        reason,
        at,
      );
      this.store.run(
        "INSERT INTO affinity_event_details VALUES(?,?,?,?,?,?,?)",
        id,
        person,
        eventKey,
        fingerprint,
        requested,
        policy.revision,
        "manual_adjustment",
      );
      this.store.run(
        "UPDATE affinity_states SET score_micro=?,policy_revision=? WHERE person_id=?",
        next,
        policy.revision,
        person,
      );
      this.store.run(
        "UPDATE records SET revision=revision+1,updated_at=? WHERE id=?",
        at,
        person,
      );
      this.runtime.invalidate(owner, { action: "corrected", id: person });
      this.store.audit(
        owner,
        "relationships",
        "completed",
        "已记录人工好感调整，未授予联系权限",
        person,
      );
      return {
        ...this.runtime.records.get("people", person, owner),
        ...this.describe(profile),
      };
    });
  }
  forget(person: string) {
    this.store.run("DELETE FROM affinity_states WHERE person_id=?", person);
  }
  recover(at: number) {
    this.store.transaction(() => {
      const row = this.store.get(
          "SELECT value FROM metadata WHERE key='affinity.heartbeat'",
        ),
        prior = Number(row?.value ?? at);
      if (at > prior)
        for (const state of this.store.all(
          "SELECT s.* FROM affinity_states s JOIN records r ON r.id=s.person_id WHERE r.deleted_at IS NULL",
        )) {
          const person = String(state.person_id),
            policy = this.policy(String(state.owner_id)).value;
          this.settle(person, prior, policy);
          const current = this.state(person),
            created = Number(
              this.store.get(
                "SELECT created_at FROM records WHERE id=?",
                person,
              )!.created_at,
            );
          const start = Math.max(
            Number(current.settled_at),
            Number(current.last_direct_at ?? created) + policy.grace_seconds,
          );
          this.settle(
            person,
            Math.max(
              Number(current.settled_at),
              Math.min(at, start + policy.downtime_cap_seconds),
            ),
            policy,
          );
          this.store.run(
            "UPDATE affinity_states SET settled_at=max(settled_at,?) WHERE person_id=?",
            at,
            person,
          );
        }
      this.writeHeartbeat(at);
    });
  }
  private writeHeartbeat(at: number) {
    this.store.run(
      "INSERT INTO metadata VALUES('affinity.heartbeat',?) ON CONFLICT(key) DO UPDATE SET value=max(CAST(value AS INTEGER),CAST(excluded.value AS INTEGER))",
      String(at),
    );
    this.heartbeat = at;
  }
  start() {
    this.timer = setInterval(() => {
      if (now() - this.heartbeat >= 10) this.writeHeartbeat(now());
    }, 1000);
    this.timer.unref();
  }
  stop() {
    if (this.timer) clearInterval(this.timer);
    this.writeHeartbeat(now());
  }
}
