import { randomUUID } from "node:crypto";
import { bad, fields, integer, missing, string } from "./errors.ts";
import { hash, now } from "./security.ts";
import type { Store, Row } from "./store.ts";
import type { Knowledge } from "./knowledge.ts";
import type { Diary } from "./diary.ts";
export type ProfileRef = {
  type: string;
  id: string;
  revision: number;
  policy_id: string | null;
  policy_revision: number | null;
  scope: string | null;
};
export type ProfileSource = {
  statement_type?: string;
  ref: ProfileRef;
  owner: string;
  visibility: string;
  content: string;
  conversation_id: string | null;
  happened_at: number;
};
export class ProfileSources {
  constructor(
    readonly store: Store,
    readonly knowledge: Knowledge,
    readonly diary: Diary,
  ) {}
  resolve(actor: string, value: unknown, model = false): ProfileSource {
    const b = fields(value, ["type", "id", "revision"]),
      type = string(b.type, "来源类型", 24, 1);
    if (type === "manual") {
      if (b.id !== undefined || b.revision !== undefined)
        throw bad("人工来源标识由宿主生成");
      return {
        ref: {
          type,
          id: randomUUID(),
          revision: 1,
          policy_id: null,
          policy_revision: null,
          scope: null,
        },
        owner: actor,
        visibility: "private",
        content: "",
        conversation_id: null,
        happened_at: now(),
      };
    }
    return this.read(
      actor,
      {
        type,
        id: string(b.id, "来源ID", 36, 36),
        revision: integer(b.revision, "来源版本", 1, Number.MAX_SAFE_INTEGER),
        policy_id: null,
        policy_revision: null,
        scope: null,
      },
      model,
    );
  }
  read(
    actor: string,
    ref: ProfileRef,
    model = false,
    humanContinuity = false,
  ): ProfileSource {
    const { type, id, revision } = ref;
    const tomb = this.store.get(
      "SELECT source_revision FROM profile_source_tombstones WHERE source_type=? AND source_id=?",
      type,
      id,
    );
    if (tomb && Number(tomb.source_revision) >= revision)
      throw bad("来源版本已失效", "source_invalidated");
    if (type === "manual")
      return {
        ref,
        owner: actor,
        visibility: "private",
        content: "",
        conversation_id: null,
        happened_at: 0,
      };
    if (type === "diary_event" || type === "diary_entry") {
      const s = this.diary.sourceFor(actor, type, id, revision, model);
      if (
        ref.policy_id !== null &&
        (s.policy_id !== ref.policy_id ||
          (!humanContinuity && s.policy_revision !== ref.policy_revision))
      )
        throw bad("日记来源许可版本变化", "source_invalidated");
      return {
        ...s,
        ref: {
          type,
          id,
          revision,
          policy_id: s.policy_id,
          policy_revision: s.policy_revision,
          scope: null,
        },
        conversation_id:
          s.conversation_id === null ? null : String(s.conversation_id),
      };
    }
    let row: Row | undefined,
      owner: string,
      visibility: string,
      scope: string | null,
      content: string,
      conversation: string | null = null,
      happened: number;
    if (type === "message") {
      row = this.store.get(
        "SELECT m.*,m.rowid AS sequence,c.owner_id,c.source_key,r.status,r.created_at AS user_sent_at FROM messages m JOIN conversations c ON c.id=m.conversation_id JOIN runs r ON r.id=m.run_id WHERE m.id=?",
        id,
      );
      if (
        !row ||
        row.role !== "user" ||
        row.status !== "completed" ||
        revision !== 1 ||
        this.store.get(
          "SELECT run_id FROM local_runs WHERE run_id=? UNION SELECT run_id FROM automatic_runs WHERE run_id=?",
          String(row.run_id),
          String(row.run_id),
        )
      )
        throw bad(
          "只接受已完成的真实用户消息，消息来源版本为1",
          "source_invalidated",
        );
      if (
        this.store.get(
          "SELECT id FROM diary_events WHERE origin_kind='message' AND origin_id=? AND (deleted_at IS NOT NULL OR revision>1)",
          id,
        )
      )
        throw bad("原文已更正或遗忘", "source_invalidated");
      owner = String(row.owner_id);
      visibility = "private";
      conversation = String(row.conversation_id);
      scope = String(row.source_key ?? conversation);
      content = String(row.content);
      happened = Number(row.user_sent_at);
      const cutoff = Math.max(
        Number(
          this.store.get(
            "SELECT value FROM metadata WHERE key='source_cutoff:global'",
          )?.value ?? 0,
        ),
        Number(
          this.store.get(
            "SELECT value FROM metadata WHERE key=?",
            `source_cutoff:${hash(owner)}`,
          )?.value ?? 0,
        ),
      );
      if (Number(row.sequence) <= cutoff)
        throw bad("原文已失效", "source_invalidated");
    } else if (type === "memory") {
      row = this.store.get(
        "SELECT * FROM records WHERE id=? AND kind='memory' AND deleted_at IS NULL",
        id,
      );
      if (!row || row.revision !== revision)
        throw bad("记忆已更正或遗忘", "source_invalidated");
      owner = String(row.owner_id);
      visibility = String(row.visibility);
      scope = null;
      content = String(row.payload);
      happened = Number(row.updated_at);
    } else throw bad("支持人工、用户原文、记忆、日记事件与日记条目来源");
    if (
      owner !== actor &&
      visibility !== "shared" &&
      !owner.startsWith("qq:") &&
      !owner.startsWith("weixin:")
    )
      throw missing();
    let grant: Row | undefined;
    if (scope) {
      this.knowledge.source(actor, scope);
      grant = this.store.get(
        "SELECT * FROM source_grants WHERE source_key=?",
        scope,
      );
      if (
        (owner !== actor || model) &&
        (!grant ||
          grant.capture !== 1 ||
          (model && grant.allow_model !== 1) ||
          (grant.expires_at !== null && Number(grant.expires_at) <= now()))
      )
        throw bad("消息来源未授权或许可已到期", "source_forbidden");
      if (
        grant &&
        (grant.capture !== 1 ||
          (model && grant.allow_model !== 1) ||
          (grant.expires_at !== null && Number(grant.expires_at) <= now()) ||
          Number(row.sequence) <= Number(grant.enabled_after))
      )
        throw bad("消息不在有效采集范围内", "source_forbidden");
      if (
        !humanContinuity &&
        ref.policy_revision !== null &&
        ref.policy_revision !== Number(grant?.revision ?? 0)
      )
        throw bad("消息来源许可版本变化", "source_invalidated");
    }
    return {
      ref: {
        type,
        id,
        revision,
        policy_id: null,
        policy_revision: scope ? Number(grant?.revision ?? 0) : null,
        scope,
      },
      owner,
      visibility,
      content,
      conversation_id: conversation,
      happened_at: happened,
    };
  }
}
