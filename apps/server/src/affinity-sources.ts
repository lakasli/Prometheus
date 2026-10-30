import { bad, conflict, fields, missing } from "./errors.ts";
import { now } from "./security.ts";
import type { Affinity, AffinityEvidence } from "./affinity.ts";
import type { Profiles } from "./profiles.ts";
import type { ProfileSource, ProfileRef } from "./profile-sources.ts";
import type { Store, Row } from "./store.ts";
import type { Runtime } from "./runtime.ts";
const parse = (v: unknown) => JSON.parse(String(v));
export class AffinitySources {
  private refreshing = false;
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    readonly profiles: Profiles,
    readonly affinity: Affinity,
  ) {}
  private sourceScope(p: Row, s: ProfileSource) {
    if (s.ref.type === "manual") return;
    if (
      (p.visibility === "shared" && s.visibility !== "shared") ||
      (p.visibility === "private" &&
        s.visibility !== "shared" &&
        s.owner !== p.owner_id)
    )
      throw bad("好感证据与人物来源域不一致", "affinity_scope_conflict");
  }
  add(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, [
        "delta",
        "reason",
        "reviewed",
        "event_key",
        "kind",
        "source",
      ]),
      p = this.profiles.person(actor, id, true),
      s = this.profiles.sources.resolve(actor, b.source ?? { type: "manual" });
    this.sourceScope(p, s);
    const proof: AffinityEvidence = {
      actor,
      ref: s.ref,
      direct_at: null,
      identity_id: null,
      identity_revision: null,
      direct_owner_id: null,
      direct_message_id: null,
    };
    if (b.kind === "direct_interaction") {
      if (
        s.ref.type !== "message" ||
        !s.conversation_id ||
        p.visibility !== "private" ||
        s.owner !== p.owner_id
      )
        throw bad(
          "直接互动只能引用同域私聊的完成用户消息",
          "direct_evidence_required",
        );
      const identity = this.store.get(
        "SELECT * FROM profile_identities WHERE person_id=? AND conversation_id=? AND owner_id=? AND revoked_at IS NULL",
        id,
        s.conversation_id,
        String(p.owner_id),
      );
      if (!identity)
        throw bad(
          "请先独立核对并绑定该人物的私聊会话",
          "direct_identity_required",
        );
      if (
        !Number.isSafeInteger(s.happened_at) ||
        s.happened_at < 0 ||
        s.happened_at > now()
      )
        throw bad("用户消息时间无效", "direct_message_time_invalid");
      Object.assign(proof, {
        direct_at: s.happened_at,
        identity_id: String(identity.id),
        identity_revision: Number(identity.revision),
        direct_owner_id: s.owner,
        direct_message_id: s.ref.id,
      });
    }
    const body = {
      delta: b.delta,
      reason: b.reason,
      reviewed: b.reviewed,
      event_key: b.event_key,
      kind: b.kind,
    };
    return this.store.transaction(() => {
      this.refresh(id);
      this.profiles.sources.read(actor, proof.ref);
      this.affinity.add(String(p.owner_id), id, expected, body, proof);
      return this.get(actor, id);
    });
  }
  get(actor: string, id: string) {
    this.profiles.person(actor, id, true);
    this.refresh(id);
    const p = this.profiles.person(actor, id, true);
    return {
      id,
      kind: "people",
      owner_id: p.owner_id,
      visibility: p.visibility,
      data: parse(p.payload),
      created_at: p.created_at,
      updated_at: p.updated_at,
      person_id: id,
      revision: p.revision,
      ...this.affinity.describe({
        id,
        owner_id: String(p.owner_id),
        created_at: Number(p.created_at),
      }),
      events: this.store.all(
        "SELECT e.*,d.event_key,d.requested_micro,d.policy_revision,d.kind,s.source_type,s.source_id,s.source_revision,s.invalidated_at,s.identity_id FROM affinity_events e LEFT JOIN affinity_event_details d ON d.event_id=e.id LEFT JOIN affinity_event_sources s ON s.event_id=e.id WHERE e.person_id=? ORDER BY e.created_at DESC,e.rowid DESC LIMIT 100",
        id,
      ),
      replays: this.store.all(
        "SELECT * FROM affinity_replays WHERE person_id=? ORDER BY rowid DESC LIMIT 30",
        id,
      ),
      baseline: {
        scope: "schema10_checkpoint_or_creation",
        history_before_checkpoint:
          "preserved_without_invented_source_provenance",
      },
      delivery_available: false,
    };
  }
  sources(actor: string, id: string) {
    const p = this.profiles.person(actor, id, true);
    this.refresh(id);
    return {
      items: this.store
        .all(
          "SELECT m.id,m.content,m.created_at FROM messages m JOIN runs r ON r.id=m.run_id JOIN conversations c ON c.id=m.conversation_id JOIN profile_identities i ON i.conversation_id=c.id AND i.person_id=? AND i.revoked_at IS NULL WHERE c.owner_id=? AND m.role='user' AND r.status='completed' AND NOT EXISTS(SELECT 1 FROM local_runs l WHERE l.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM automatic_runs a WHERE a.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM affinity_event_sources s WHERE s.direct_owner_id=c.owner_id AND s.direct_message_id=m.id) ORDER BY m.rowid DESC LIMIT 100",
          id,
          String(p.owner_id),
        )
        .flatMap((row) => {
          try {
            const s = this.profiles.sources.resolve(actor, {
              type: "message",
              id: row.id,
              revision: 1,
            });
            return [
              {
                type: "message",
                id: row.id,
                revision: 1,
                excerpt: [...this.store.redact(s.content)]
                  .slice(0, 160)
                  .join(""),
                happened_at: s.happened_at,
              },
            ];
          } catch {
            return [];
          }
        }),
    };
  }
  invalidateSource(kind: string, id: string, revision: number) {
    this.invalidateWhere(
      "source_type=? AND source_id=? AND source_revision<=?",
      [kind, id, revision],
      "source_invalidated",
    );
    if (kind === "diary_event") {
      const e = this.store.get(
        "SELECT origin_kind,origin_id FROM diary_events WHERE id=?",
        id,
      );
      if (e?.origin_kind === "message")
        this.invalidateWhere(
          "source_type='message' AND source_id=?",
          [String(e.origin_id)],
          "raw_message_corrected",
        );
    }
  }
  invalidateIdentity(person: string) {
    this.refresh(person);
  }
  private invalidateWhere(
    where: string,
    args: (string | number)[],
    code: string,
  ) {
    const rows = this.store.all(
      `SELECT * FROM affinity_event_sources WHERE invalidated_at IS NULL AND (${where})`,
      ...args,
    );
    if (!rows.length) return;
    this.store.transaction(() => {
      const ids = new Set<string>();
      for (const row of rows) {
        this.store.run(
          "UPDATE affinity_event_sources SET invalidated_at=? WHERE event_id=?",
          now(),
          String(row.event_id),
        );
        this.store.run(
          "UPDATE affinity_events SET reason=NULL,delta=0 WHERE id=?",
          String(row.event_id),
        );
        ids.add(String(row.person_id));
      }
      for (const id of ids) {
        const p = this.store.get(
          "SELECT * FROM records WHERE id=? AND deleted_at IS NULL",
          id,
        );
        if (!p) continue;
        this.affinity.replayLedger.replay(id, code);
        this.store.run(
          "UPDATE records SET revision=revision+1,updated_at=? WHERE id=?",
          now(),
          id,
        );
        this.runtime.invalidate(String(p.owner_id), {
          action: "corrected",
          id,
        });
        this.store.audit(
          "system",
          "relationships",
          "invalidated",
          "来源/身份失效，按原策略与仍有效事件重放独立账本",
          id,
        );
      }
    });
  }
  refresh(person?: string) {
    if (this.refreshing) return;
    this.refreshing = true;
    try {
      for (const row of this.store.all(
        "SELECT * FROM affinity_event_sources WHERE invalidated_at IS NULL AND (? IS NULL OR person_id=?)",
        person ?? null,
        person ?? null,
      )) {
        if (row.source_type === "manual") continue;
        try {
          const p = this.profiles.person(
              String(row.actor_id),
              String(row.person_id),
              true,
            ),
            s = this.profiles.sources.read(
              String(row.actor_id),
              parse(row.ref_json) as ProfileRef,
              false,
              true,
            );
          this.sourceScope(p, s);
          if (
            row.identity_id !== null &&
            !this.store.get(
              "SELECT id FROM profile_identities WHERE id=? AND revision=? AND person_id=? AND revoked_at IS NULL",
              String(row.identity_id),
              Number(row.identity_revision),
              String(row.person_id),
            )
          )
            throw bad("身份核对已撤销");
        } catch {
          this.invalidateWhere(
            "event_id=?",
            [String(row.event_id)],
            "source_or_identity_invalidated",
          );
        }
      }
    } finally {
      this.refreshing = false;
    }
  }
  revoke(
    actor: string,
    person: string,
    event: string,
    expected: number,
    value: unknown,
  ) {
    fields(value, []);
    return this.store.transaction(() => {
      const p = this.profiles.person(actor, person, true);
      if (p.revision !== expected) throw conflict();
      const row = this.store.get(
        "SELECT event_id FROM affinity_event_sources WHERE event_id=? AND person_id=?",
        event,
        person,
      );
      if (!row) throw missing();
      this.invalidateWhere("event_id=?", [event], "operator_revoked");
      this.store.audit(
        actor,
        "relationships",
        "revoked",
        "管理者明确撤销计分并按仍有效来源重算",
        event,
      );
      return this.get(actor, person);
    });
  }
}
