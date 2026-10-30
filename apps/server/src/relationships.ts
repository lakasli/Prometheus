import { randomUUID } from "node:crypto";
import { bad, fields, integer, string } from "./errors.ts";
import { now } from "./security.ts";
import type { Runtime } from "./runtime.ts";
import type { Store } from "./store.ts";
import type { Knowledge } from "./knowledge.ts";
export class Relationships {
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    readonly knowledge: Knowledge,
  ) {}
  policy(owner: string) {
    return this.knowledge.policy(`affinity:${owner}`, {
      initial: 0,
      min: -100,
      max: 100,
      decay_per_day: 0.1,
      max_event_delta: 10,
    });
  }
  savePolicy(owner: string, expected: number, value: unknown) {
    const body = fields(value, [
      "initial",
      "min",
      "max",
      "decay_per_day",
      "max_event_delta",
    ]);
    integer(body.min, "下限", -1000, 0);
    integer(body.max, "上限", 0, 1000);
    integer(body.initial, "初始值", Number(body.min), Number(body.max));
    integer(body.max_event_delta, "单次变化上限", 1, 50);
    if (
      typeof body.decay_per_day !== "number" ||
      !Number.isFinite(body.decay_per_day) ||
      body.decay_per_day < 0 ||
      body.decay_per_day > 10
    )
      throw bad("每日衰减须为0至10");
    return this.knowledge.setPolicy(`affinity:${owner}`, expected, body);
  }
  people(owner: string) {
    const policy = this.policy(owner).value;
    return this.runtime.records
      .list("people", owner, new URLSearchParams({ limit: "100" }))
      .items.map((person) => {
        const events = this.store.all(
          "SELECT * FROM affinity_events WHERE person_id=? AND owner_id=? AND reason IS NOT NULL ORDER BY created_at",
          person.id,
          owner,
        );
        let value = Number(policy.initial),
          last = Number(person.created_at);
        for (const event of events) {
          value = this.decay(
            value,
            Number(event.created_at) - last,
            Number(policy.decay_per_day),
          );
          value = Math.max(
            Number(policy.min),
            Math.min(Number(policy.max), value + Number(event.delta)),
          );
          last = Number(event.created_at);
        }
        value = this.decay(value, now() - last, Number(policy.decay_per_day));
        return {
          ...person,
          affinity: Math.round(value * 100) / 100,
          events: events.slice(-20),
          contact_permission: false,
          autonomous_sending: false,
        };
      });
  }
  add(owner: string, person: string, value: unknown) {
    this.runtime.records.get("people", person, owner);
    const body = fields(value, ["delta", "reason", "reviewed"]);
    if (body.reviewed !== true) throw bad("好感变化需要独立人工确认");
    const limit = Number(this.policy(owner).value.max_event_delta),
      delta = integer(body.delta, "变化", -limit, limit),
      reason = string(body.reason, "变化依据", 2000, 1),
      id = randomUUID();
    this.store.run(
      "INSERT INTO affinity_events VALUES(?,?,?,?,?,?)",
      id,
      owner,
      person,
      delta,
      reason,
      now(),
    );
    this.store.audit(
      owner,
      "relationships",
      "completed",
      "已记录人工确认的相处事件",
      person,
    );
    return this.people(owner).find((item) => item.id === person);
  }
  merge(owner: string, from: string, value: unknown) {
    const body = fields(value, [
        "into_id",
        "expected_from_revision",
        "expected_into_revision",
        "confirmed",
      ]),
      into = string(body.into_id, "目标人物", 36, 36);
    if (body.confirmed !== true || from === into)
      throw bad("请选择不同的同域人物并明确确认合并");
    const source = this.runtime.records.get("people", from, owner),
      target = this.runtime.records.get("people", into, owner);
    if (
      source.revision !== body.expected_from_revision ||
      target.revision !== body.expected_into_revision
    )
      throw bad("人物版本已变化，请重新核对");
    return this.store.transaction(() => {
      this.store.run(
        "INSERT INTO person_aliases VALUES(?,?,?,?)",
        from,
        into,
        owner,
        now(),
      );
      this.store.run(
        "UPDATE affinity_events SET person_id=? WHERE person_id=? AND owner_id=?",
        into,
        from,
        owner,
      );
      this.store.run(
        "UPDATE records SET payload=NULL,deleted_at=?,revision=revision+1 WHERE id=?",
        now(),
        from,
      );
      for (const row of this.store.all(
        "SELECT id,payload FROM records WHERE owner_id=? AND kind='relationships' AND deleted_at IS NULL",
        owner,
      )) {
        const data = JSON.parse(String(row.payload));
        if (data.from_id !== from && data.to_id !== from) continue;
        if (data.from_id === from) data.from_id = into;
        if (data.to_id === from) data.to_id = into;
        if (data.from_id === data.to_id)
          this.store.run(
            "UPDATE records SET payload=NULL,deleted_at=?,revision=revision+1 WHERE id=?",
            now(),
            String(row.id),
          );
        else
          this.store.run(
            "UPDATE records SET payload=?,revision=revision+1 WHERE id=?",
            JSON.stringify(data),
            String(row.id),
          );
      }
      this.runtime.invalidate(owner, { action: "corrected", id: from });
      this.store.audit(
        owner,
        "relationships",
        "completed",
        "同域人物已明确合并，别名关系已保留",
        into,
      );
      return {
        from_id: from,
        into_id: into,
        merged: true,
        alias_preserved: true,
      };
    });
  }
  forget(person: string) {
    this.store.run(
      "UPDATE affinity_events SET reason=NULL,delta=0 WHERE person_id=?",
      person,
    );
    this.store.run(
      "DELETE FROM person_aliases WHERE person_id=? OR alias_id=?",
      person,
      person,
    );
  }
  private decay(value: number, seconds: number, rate: number) {
    const reduction = (Math.max(0, seconds) / 86400) * rate;
    return value >= 0
      ? Math.max(0, value - reduction)
      : Math.min(0, value + reduction);
  }
}
