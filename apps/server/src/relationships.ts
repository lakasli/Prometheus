import { randomUUID } from "node:crypto";
import { bad, conflict, fields, missing, string } from "./errors.ts";
import { now } from "./security.ts";
import { Affinity } from "./affinity.ts";
import type { Runtime } from "./runtime.ts";
import type { Store } from "./store.ts";
import type { Knowledge } from "./knowledge.ts";
export class Relationships {
  readonly affinity: Affinity;
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    readonly knowledge: Knowledge,
  ) {
    this.affinity = new Affinity(store, runtime, knowledge);
  }
  policy(owner: string) {
    return this.affinity.policy(owner);
  }
  savePolicy(owner: string, expected: number, value: unknown) {
    return this.affinity.save(owner, expected, value);
  }
  people(owner: string) {
    return this.runtime.records
      .list("people", owner, new URLSearchParams({ limit: "100" }))
      .items.map((person) => ({
        ...person,
        ...this.affinity.describe(person),
        merged_into:
          this.store.get(
            "SELECT person_id FROM person_aliases WHERE alias_id=? AND owner_id=?",
            person.id,
            owner,
          )?.person_id ?? null,
      }));
  }
  legacyMappings(owner: string) {
    return this.store
      .all(
        "SELECT a.alias_id,a.person_id FROM person_aliases a JOIN records r ON r.id=a.alias_id WHERE a.owner_id=? AND (r.deleted_at IS NOT NULL OR r.payload IS NULL)",
        owner,
      )
      .map((row) => ({
        ...row,
        reversible: false,
        reason:
          "早期版本清空了来源档案并搬移事件，不能推测恢复；请核对原完整备份",
      }));
  }
  add(owner: string, person: string, expected: number, value: unknown) {
    return this.affinity.add(owner, person, expected, value);
  }
  created(id: string) {
    const person = this.store.get(
      "SELECT * FROM records WHERE id=? AND kind='people' AND deleted_at IS NULL",
      id,
    );
    if (person) this.affinity.ensure(person);
  }
  merge(owner: string, from: string, value: unknown) {
    const body = fields(value, [
        "into_id",
        "expected_from_revision",
        "expected_into_revision",
        "confirmed",
        "reason",
      ]),
      into = string(body.into_id, "目标人物", 36, 36);
    if (body.confirmed !== true || from === into)
      throw bad("请选择不同的同域人物并明确确认合并");
    const reason = string(body.reason, "合并依据", 2000, 1);
    return this.store.transaction(() => {
      const source = this.runtime.records.get("people", from, owner),
        target = this.runtime.records.get("people", into, owner);
      if (
        source.revision !== body.expected_from_revision ||
        target.revision !== body.expected_into_revision
      )
        throw conflict();
      if (
        this.store.get(
          "SELECT person_id FROM profile_group_members WHERE person_id IN (?,?)",
          from,
          into,
        )
      )
        throw bad("身份关联组中的人物须先拆分关联，再做同域合并");
      if (
        this.store.get(
          "SELECT alias_id FROM person_aliases WHERE alias_id IN (?,?)",
          from,
          into,
        )
      )
        throw bad("请先拆开已有合并映射，不能建立链式映射");
      if (
        this.store.get(
          "SELECT alias_id FROM person_aliases WHERE person_id=?",
          from,
        )
      )
        throw bad("待合并档案已有来源映射，请先拆开");
      if (
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM person_aliases WHERE person_id=?",
            into,
          )!.n,
        ) >= 10
      )
        throw bad("单一档案最多保留10个合并来源");
      this.store.run(
        "INSERT INTO person_aliases VALUES(?,?,?,?)",
        from,
        into,
        owner,
        now(),
      );
      this.bump(from, into);
      this.mapping(owner, from, into, "merge", reason);
      this.runtime.invalidate(owner, { action: "corrected", id: from });
      this.runtime.invalidate(owner, { action: "corrected", id: into });
      this.store.audit(
        owner,
        "relationships",
        "completed",
        "人物映射已确认，原档案、关系端点和独立好感账本保留",
        from,
      );
      return {
        from_id: from,
        into_id: into,
        merged: true,
        alias_preserved: true,
        original_records_preserved: true,
        affinity_combined: false,
      };
    });
  }
  split(owner: string, from: string, value: unknown) {
    const body = fields(value, [
      "expected_from_revision",
      "expected_into_revision",
      "confirmed",
      "reason",
    ]);
    if (body.confirmed !== true) throw bad("拆分需要明确确认");
    const reason = string(body.reason, "拆分依据", 2000, 1);
    return this.store.transaction(() => {
      const source = this.runtime.records.get("people", from, owner),
        alias = this.store.get(
          "SELECT person_id FROM person_aliases WHERE alias_id=? AND owner_id=?",
          from,
          owner,
        );
      if (!alias) throw missing();
      const into = String(alias.person_id),
        target = this.runtime.records.get("people", into, owner);
      if (
        source.revision !== body.expected_from_revision ||
        target.revision !== body.expected_into_revision
      )
        throw conflict();
      this.store.run("DELETE FROM person_aliases WHERE alias_id=?", from);
      this.bump(from, into);
      this.mapping(owner, from, into, "split", reason);
      this.runtime.invalidate(owner, { action: "corrected", id: from });
      this.runtime.invalidate(owner, { action: "corrected", id: into });
      this.store.audit(
        owner,
        "relationships",
        "completed",
        "人物映射已拆分，原资料与账本继续独立",
        from,
      );
      return this.runtime.records.get("people", from, owner);
    });
  }
  private mapping(
    owner: string,
    from: string,
    into: string,
    action: string,
    reason: string,
  ) {
    this.store.run(
      "INSERT INTO person_mapping_history VALUES(?,?,?,?,?,?,?)",
      randomUUID(),
      owner,
      from,
      into,
      action,
      this.store.redact(reason),
      now(),
    );
  }
  private bump(...ids: string[]) {
    for (const id of ids)
      this.store.run(
        "UPDATE records SET revision=revision+1,updated_at=? WHERE id=?",
        now(),
        id,
      );
  }
  forget(person: string) {
    const alias = this.store.get(
      "SELECT person_id FROM person_aliases WHERE alias_id=?",
      person,
    );
    if (alias) this.bump(String(alias.person_id));
    this.store.run(
      "UPDATE affinity_events SET reason=NULL,delta=0 WHERE person_id=?",
      person,
    );
    this.store.run(
      "DELETE FROM affinity_event_details WHERE person_id=?",
      person,
    );
    this.affinity.forget(person);
    this.store.run(
      "DELETE FROM person_aliases WHERE person_id=? OR alias_id=?",
      person,
      person,
    );
    this.store.run(
      "UPDATE person_mapping_history SET reason=NULL WHERE person_id=? OR target_id=?",
      person,
      person,
    );
  }
}
