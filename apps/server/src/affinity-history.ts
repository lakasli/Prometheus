import { randomUUID } from "node:crypto";
import { now } from "./security.ts";
import type { Store } from "./store.ts";
export type AffinityPolicy = {
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
export type AffinityState = {
  score_micro: number;
  settled_at: number;
  last_direct_at: number | null;
  remainder: number;
  policy_revision: number;
  created_at: number;
};
export const DAY = 86400,
  MICRO = 1000000,
  micro = (value: number) => Math.round(value * MICRO);
export function advanceAffinity(
  state: AffinityState,
  policy: AffinityPolicy,
  at: number,
): AffinityState {
  const end = Math.max(at, state.settled_at),
    start = Math.max(
      state.settled_at,
      (state.last_direct_at ?? state.created_at) + policy.grace_seconds,
    ),
    numerator =
      BigInt(Math.max(0, end - start)) * BigInt(micro(policy.decay_per_day)) +
      BigInt(state.remainder),
    loss = Number(numerator / BigInt(DAY)),
    current = state.score_micro;
  const score = Math.max(
    micro(policy.min),
    Math.min(
      micro(policy.max),
      policy.decay_mode === "zero"
        ? current >= 0
          ? Math.max(0, current - loss)
          : Math.min(0, current + loss)
        : current - loss,
    ),
  );
  return {
    ...state,
    score_micro: score,
    settled_at: end,
    remainder:
      score === micro(policy.min) ||
      (policy.decay_mode === "zero" && score === 0)
        ? 0
        : Number(numerator % BigInt(DAY)),
  };
}
export class AffinityHistory {
  constructor(
    readonly store: Store,
    private policy: (owner: string) => {
      revision: number;
      value: AffinityPolicy;
    },
  ) {}
  state(person: string): AffinityState {
    const row = this.store.get(
      "SELECT s.*,r.created_at FROM affinity_states s JOIN records r ON r.id=s.person_id WHERE s.person_id=?",
      person,
    )!;
    return {
      score_micro: Number(row.score_micro),
      settled_at: Number(row.settled_at),
      last_direct_at:
        row.last_direct_at === null ? null : Number(row.last_direct_at),
      remainder: Number(row.remainder),
      policy_revision: Number(row.policy_revision),
      created_at: Number(row.created_at),
    };
  }
  ensure(person: string) {
    if (
      this.store.get(
        "SELECT person_id FROM affinity_replay_bases WHERE person_id=?",
        person,
      )
    )
      return;
    const s = this.store.get(
      "SELECT * FROM affinity_states WHERE person_id=?",
      person,
    );
    if (!s) return;
    this.store.run(
      "INSERT INTO affinity_replay_bases VALUES(?,?,?,?)",
      person,
      JSON.stringify(this.state(person)),
      JSON.stringify(this.policy(String(s.owner_id)).value),
      now(),
    );
  }
  initialize() {
    for (const r of this.store.all("SELECT person_id FROM affinity_states"))
      this.ensure(String(r.person_id));
  }
  record(
    person: string,
    kind: string,
    at: number,
    key: string,
    payload: Record<string, unknown>,
    event: string | null = null,
  ) {
    this.ensure(person);
    this.store.run(
      "INSERT OR IGNORE INTO affinity_transitions(person_id,kind,unique_key,occurred_at,event_id,payload_json) VALUES(?,?,?,?,?,?)",
      person,
      kind,
      key,
      at,
      event,
      JSON.stringify(payload),
    );
  }
  write(person: string, s: AffinityState) {
    this.store.run(
      "UPDATE affinity_states SET score_micro=?,settled_at=?,last_direct_at=?,remainder=?,policy_revision=? WHERE person_id=?",
      s.score_micro,
      s.settled_at,
      s.last_direct_at,
      s.remainder,
      s.policy_revision,
      person,
    );
  }
  replay(person: string, code = "source_invalidated") {
    const base = this.store.get(
        "SELECT * FROM affinity_replay_bases WHERE person_id=?",
        person,
      ),
      old = this.store.get(
        "SELECT * FROM affinity_states WHERE person_id=?",
        person,
      );
    if (!base || !old) return;
    let s = JSON.parse(String(base.state_json)) as AffinityState,
      policy = JSON.parse(String(base.policy_json)) as AffinityPolicy,
      valid = 0;
    for (const transition of this.store.all(
      "SELECT * FROM affinity_transitions WHERE person_id=? ORDER BY id",
      person,
    )) {
      const p = JSON.parse(String(transition.payload_json)),
        at = Number(transition.occurred_at);
      if (transition.kind === "policy") {
        s = advanceAffinity(s, policy, at);
        policy = p.policy;
        s.score_micro = Math.max(
          micro(policy.min),
          Math.min(micro(policy.max), s.score_micro),
        );
        s.policy_revision = p.revision;
      } else if (transition.kind === "outage") {
        s = advanceAffinity(s, policy, p.prior);
        const start = Math.max(
          s.settled_at,
          (s.last_direct_at ?? s.created_at) + policy.grace_seconds,
        );
        s = advanceAffinity(
          s,
          policy,
          Math.max(s.settled_at, Math.min(p.end, start + p.cap)),
        );
        s.settled_at = Math.max(s.settled_at, p.end);
      } else if (transition.kind === "event") {
        const e = this.store.get(
          "SELECT e.reason,s.invalidated_at FROM affinity_events e LEFT JOIN affinity_event_sources s ON s.event_id=e.id WHERE e.id=?",
          String(transition.event_id),
        );
        if (!e || e.reason === null || e.invalidated_at !== null) continue;
        s = advanceAffinity(s, policy, at);
        policy = p.policy;
        s.policy_revision = p.policy_revision;
        s.score_micro = Math.max(
          micro(policy.min),
          Math.min(micro(policy.max), s.score_micro + p.requested_micro),
        );
        if (p.direct_at !== null)
          s.last_direct_at = Math.max(s.last_direct_at ?? 0, p.direct_at);
        valid++;
      }
    }
    s = advanceAffinity(s, policy, Math.max(now(), Number(old.settled_at)));
    this.write(person, s);
    this.store.run(
      "INSERT INTO affinity_replays VALUES(?,?,?,?,?,?,?)",
      randomUUID(),
      person,
      Number(old.score_micro),
      s.score_micro,
      valid,
      code,
      now(),
    );
  }
  forget(person: string) {
    for (const table of [
      "affinity_replay_bases",
      "affinity_transitions",
      "affinity_replays",
    ])
      this.store.run(`DELETE FROM ${table} WHERE person_id=?`, person);
  }
}
