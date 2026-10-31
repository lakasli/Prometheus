import { ApiError, bad } from "./errors.ts";
import { hash, now } from "./security.ts";
import type { ModelMessage } from "./model.ts";
import type { Row, Store } from "./store.ts";

export const visionDerivedSql = (run: string) =>
  `(EXISTS(SELECT 1 FROM understanding_sources us WHERE us.run_id=${run}) OR EXISTS(SELECT 1 FROM understanding_uses uu WHERE uu.run_id=${run}) OR EXISTS(SELECT 1 FROM run_media vm WHERE vm.run_id=${run}) OR EXISTS(SELECT 1 FROM vision_uses vu WHERE vu.run_id=${run}) OR EXISTS(SELECT 1 FROM channel_media_sources cm WHERE cm.run_id=${run}) OR EXISTS(SELECT 1 FROM qq_media_plans qp WHERE qp.run_id=${run}) OR EXISTS(SELECT 1 FROM qq_media_transfers qt JOIN delivery_parts p ON p.id=qt.part_id WHERE p.run_id=${run}))`;
export function safeVisualText(store: Store, text: string) {
  return (
    !!text &&
    Buffer.byteLength(text) <= 8000 &&
    store.redact(text) === text &&
    !text.includes("\0") &&
    !/(?:sk-[A-Za-z0-9_-]{16,}|(?:api[_ -]?key|password|密码|验证码|access[_ -]?token)\s*[:=：]\s*\S{4,})/i.test(
      text,
    )
  );
}
export function visualSourceAllowed(store: Store, message: Row, purpose = "") {
  if (
    store.get(
      "SELECT run_id FROM channel_media_sources WHERE run_id=? AND kind IN ('notice','legacy_unverified','video','voice','file')",
      String(message.run_id),
    )
  )
    return false;
  const direct = !!store.get(
    "SELECT run_id FROM run_media WHERE run_id=?",
    String(message.run_id),
  );
  const derived =
    direct ||
    !!store.get(
      "SELECT run_id FROM understanding_uses WHERE run_id=? LIMIT 1",
      String(message.run_id),
    ) ||
    !!store.get(
      "SELECT run_id FROM vision_uses WHERE run_id=? LIMIT 1",
      String(message.run_id),
    ) ||
    !!store.get(
      "SELECT run_id FROM qq_media_plans WHERE run_id=? UNION SELECT p.run_id FROM qq_media_transfers t JOIN delivery_parts p ON p.id=t.part_id WHERE p.run_id=?",
      String(message.run_id),
      String(message.run_id),
    );
  const unverified =
    (message.role === "assistant" || purpose === "summary") &&
    !!store.get(
      "SELECT m.id FROM messages m JOIN vision_legacy_fences f ON f.conversation_id=m.conversation_id WHERE m.id=? AND m.rowid<=f.message_cutoff",
      String(message.id),
    );
  return (
    !unverified &&
    !direct &&
    !(derived && (message.role === "assistant" || purpose === "summary"))
  );
}

/** Saved content stays readable, but excluded visual sources cannot be laundered through derivations. */
export function visualRecordAllowed(store: Store, id: string, purpose = "") {
  return store
    .all(
      "WITH RECURSIVE sources(type,id) AS (SELECT 'record',? UNION SELECT d.source_type,d.source_id FROM derivations d JOIN sources s ON s.type='record' AND d.record_id=s.id) SELECT m.id,m.role,m.run_id FROM sources s JOIN messages m ON s.type='message' AND m.id=s.id",
      id,
    )
    .every((message) => visualSourceAllowed(store, message, purpose));
}

/** Authority for vision provenance; text markers never create source privileges. */
export class Vision {
  constructor(readonly store: Store) {
    this.initialize();
  }
  private initialize() {
    this.store.transaction(() => {
      const legacy = this.store.all(
        "SELECT DISTINCT r.conversation_id FROM run_media m JOIN runs r ON r.id=m.run_id WHERE NOT EXISTS(SELECT 1 FROM vision_sources v WHERE v.run_id=r.id) AND EXISTS(SELECT 1 FROM messages WHERE run_id=r.id AND role='user')",
      );
      // Old descriptions lack frozen policy/model and actual follow-up use records.
      // Preserve original chat and metadata, never invent that missing evidence.
      this.store.run(
        "INSERT INTO vision_sources(run_id,source_message_id,conversation_id,owner_id,context_epoch,asset_id,image_sha256,width,height,vision_model,policy_revision,status,created_at) SELECT r.id,(SELECT id FROM messages WHERE run_id=r.id AND role='user' ORDER BY rowid LIMIT 1),r.conversation_id,r.owner_id,r.context_epoch,m.asset_id,a.digest,0,0,'legacy-unverified',0,'invalidated',r.created_at FROM run_media m JOIN runs r ON r.id=m.run_id JOIN media_assets a ON a.id=m.asset_id WHERE NOT EXISTS(SELECT 1 FROM vision_sources v WHERE v.run_id=r.id) AND EXISTS(SELECT 1 FROM messages WHERE run_id=r.id AND role='user')",
      );
      for (const c of legacy) {
        this.store.run(
          "INSERT INTO vision_legacy_fences SELECT ?,coalesce(max(rowid),0) FROM messages WHERE conversation_id=? ON CONFLICT(conversation_id) DO UPDATE SET message_cutoff=max(message_cutoff,excluded.message_cutoff)",
          String(c.conversation_id),
          String(c.conversation_id),
        );
        this.store.run(
          "UPDATE model_jobs SET status='invalidated',input_json=NULL,output_json=NULL,error='旧识图会话来源缺少冻结依据',revision=revision+1 WHERE status NOT IN ('approved','rejected','cancelled','invalidated') AND EXISTS(SELECT 1 FROM json_each(model_jobs.source_json) j JOIN messages m ON m.id=json_extract(j.value,'$.id') WHERE json_extract(j.value,'$.type')='message' AND m.conversation_id=?)",
          String(c.conversation_id),
        );
        this.store.run(
          "UPDATE conversations SET context_epoch=context_epoch+1 WHERE id=?",
          String(c.conversation_id),
        );
        this.store.run(
          "DELETE FROM summaries WHERE conversation_id=?",
          String(c.conversation_id),
        );
        this.store.run(
          "DELETE FROM prompt_snapshots WHERE run_id IN (SELECT id FROM runs WHERE conversation_id=?)",
          String(c.conversation_id),
        );
        this.store.run(
          "UPDATE runs SET status='interrupted',error='旧识图来源缺少冻结证据，未重放',updated_at=? WHERE conversation_id=? AND status IN ('queued','running')",
          now(),
          String(c.conversation_id),
        );
      }
      this.store.run(
        "UPDATE run_media SET description=NULL,status=(SELECT status FROM vision_sources WHERE run_id=run_media.run_id) WHERE run_id IN (SELECT run_id FROM vision_sources)",
      );
      this.store.run(
        "UPDATE media_assets SET bytes=x'' WHERE kind='image' AND id IN (SELECT asset_id FROM vision_sources WHERE status NOT IN ('queued','running'))",
      );
      for (const row of this.store.all(
        "SELECT v.run_id FROM vision_sources v JOIN runs r ON r.id=v.run_id WHERE r.status NOT IN ('queued','running','completed') OR v.context_epoch<>(SELECT context_epoch FROM conversations WHERE id=v.conversation_id)",
      ))
        this.finish(String(row.run_id));
    });
  }
  pendingBytes() {
    return Number(
      this.store.get(
        "SELECT (SELECT count(*) FROM media_assets WHERE length(bytes)>0 AND expires_at>?)+(SELECT count(*) FROM vision_sources WHERE image IS NOT NULL) AS n",
        now(),
      )!.n,
    );
  }
  attach(
    run: string,
    asset: string,
    model: string,
    revision: number,
    width: number,
    height: number,
  ) {
    const row = this.store.get(
      "SELECT r.*,a.bytes,a.digest,a.kind FROM runs r JOIN media_assets a ON a.id=? AND a.owner_id=r.owner_id AND a.conversation_id=r.conversation_id WHERE r.id=? AND a.expires_at>? AND length(a.bytes)>0",
      asset,
      run,
      now(),
    );
    if (!row)
      throw bad("图片已消费或过期，请重新提供原图", "vision_input_unavailable");
    if (row.kind !== "image" && this.pendingBytes() >= 20)
      throw new ApiError(429, "media_capacity", "图片暂存容量已满");
    this.store.run(
      "INSERT INTO vision_sources(run_id,source_message_id,conversation_id,owner_id,context_epoch,asset_id,image_sha256,width,height,image,vision_model,policy_revision,status,created_at) VALUES(?,(SELECT id FROM messages WHERE run_id=? AND role='user'),?,?,?,?,?,?,?,?,?,?,'queued',?)",
      run,
      run,
      String(row.conversation_id),
      String(row.owner_id),
      Number(row.context_epoch),
      asset,
      String(row.digest),
      width,
      height,
      row.bytes!,
      model,
      revision,
      now(),
    );
    if (row.kind === "image")
      this.store.run("UPDATE media_assets SET bytes=x'' WHERE id=?", asset);
  }
  private source(id: string, current: Row) {
    const source = this.store.get(
      "SELECT v.*,r.status AS run_status,m.rowid AS sequence FROM vision_sources v JOIN runs r ON r.id=v.run_id JOIN messages m ON m.id=v.source_message_id AND m.run_id=r.id AND m.role='user' WHERE v.run_id=? AND v.owner_id=? AND v.conversation_id=? AND v.context_epoch=? AND v.status='described' AND v.description IS NOT NULL AND (r.status='completed' OR r.id=? AND r.status='running')",
      id,
      String(current.owner_id),
      String(current.conversation_id),
      Number(current.context_epoch),
      String(current.id),
    );
    const floor = Math.max(
      Number(
        this.store.get(
          "SELECT value FROM metadata WHERE key='source_cutoff:global'",
        )?.value ?? 0,
      ),
      Number(
        this.store.get(
          "SELECT value FROM metadata WHERE key=?",
          `source_cutoff:${hash(String(current.owner_id))}`,
        )?.value ?? 0,
      ),
    );
    return source &&
      Number(source.sequence) > floor &&
      safeVisualText(this.store, String(source.description))
      ? source
      : undefined;
  }
  context(
    run: string,
    history: string[],
    isolated = false,
  ): { messages: ModelMessage[]; sources: string[] } {
    if (isolated) return { messages: [], sources: [] };
    const current = this.store.get("SELECT * FROM runs WHERE id=?", run)!;
    const ids = new Set<string>();
    if (this.store.get("SELECT run_id FROM run_media WHERE run_id=?", run))
      ids.add(run);
    for (const old of history) {
      if (this.store.get("SELECT run_id FROM run_media WHERE run_id=?", old))
        ids.add(old);
      for (const use of this.store.all(
        "SELECT source_run_id FROM vision_uses WHERE run_id=?",
        old,
      ))
        ids.add(String(use.source_run_id));
    }
    const messages: ModelMessage[] = [],
      sources: string[] = [];
    for (const id of ids) {
      const source = this.source(id, current);
      if (!source)
        throw bad(
          "识图来源已失效，旧回答不能继续作为依据",
          "vision_source_invalidated",
        );
      sources.push(id);
      messages.push({
        role: "user",
        content:
          "PROMETHEUS_VISION_DESCRIPTION_SOURCE_V1\n" +
          JSON.stringify({
            source: "vision_description",
            source_run_id: id,
            source_message_id: source.source_message_id,
            image_sha256: source.image_sha256,
            model: source.vision_model,
            description: source.description,
          }),
      });
    }
    return { messages, sources };
  }
  record(run: string, sources: string[]) {
    this.store.run("DELETE FROM vision_uses WHERE run_id=?", run);
    for (const source of sources)
      this.store.run("INSERT INTO vision_uses VALUES(?,?)", run, source);
  }
  valid(run: string) {
    if (
      this.store.get(
        "SELECT run_id FROM channel_media_sources WHERE run_id=? AND kind IN ('notice','legacy_unverified')",
        run,
      )
    )
      return false;
    const current = this.store.get("SELECT * FROM runs WHERE id=?", run);
    if (!current) return false;
    if (
      this.store.get("SELECT run_id FROM run_media WHERE run_id=?", run) &&
      !this.source(run, current)
    )
      return false;
    return this.store
      .all("SELECT source_run_id FROM vision_uses WHERE run_id=?", run)
      .every((u) => !!this.source(String(u.source_run_id), current));
  }
  finish(run: string) {
    const row = this.store.get(
      "SELECT r.status,r.context_epoch,c.context_epoch AS epoch FROM runs r JOIN conversations c ON c.id=r.conversation_id WHERE r.id=?",
      run,
    );
    if (!row) return;
    const completed =
      row.status === "completed" && row.context_epoch === row.epoch;
    this.store.run(
      "UPDATE vision_sources SET image=NULL,description=CASE WHEN ? THEN description ELSE NULL END,status=CASE WHEN status='invalidated' THEN status WHEN ? AND status='described' THEN status ELSE ? END WHERE run_id=?",
      +completed,
      +completed,
      row.context_epoch !== row.epoch
        ? "invalidated"
        : row.status === "cancelled"
          ? "cancelled"
          : row.status === "interrupted"
            ? "interrupted"
            : "failed",
      run,
    );
    this.store.run(
      "UPDATE run_media SET status=coalesce((SELECT status FROM vision_sources v WHERE v.run_id=run_media.run_id),status),description=NULL WHERE run_id=?",
      run,
    );
    if (!completed && this.isDerived(run))
      this.store.run("DELETE FROM prompt_snapshots WHERE run_id=?", run);
  }
  invalidate(conversation: string) {
    this.store.run(
      "UPDATE vision_sources SET image=NULL,description=NULL,status='invalidated' WHERE conversation_id=?",
      conversation,
    );
    this.store.run(
      "UPDATE media_assets SET bytes=x'' WHERE kind='image' AND conversation_id=?",
      conversation,
    );
  }
  resetContexts() {
    this.store.run(
      "UPDATE vision_sources SET image=NULL,description=NULL,status='invalidated' WHERE run_id IN (SELECT id FROM runs WHERE status<>'queued')",
    );
    this.store.run(
      "UPDATE vision_sources SET context_epoch=(SELECT context_epoch FROM runs WHERE id=run_id) WHERE status='queued' AND run_id IN (SELECT id FROM runs WHERE status='queued')",
    );
  }
  isDerived(run: string) {
    return !!this.store.get(
      `SELECT id FROM runs r WHERE r.id=? AND ${visionDerivedSql("r.id")}`,
      run,
    );
  }
  metadata(run: string) {
    const channel = this.store.get(
      "SELECT kind,code FROM channel_media_sources WHERE run_id=?",
      run,
    );
    const image = this.store.get(
      "SELECT image_sha256,width,height,status,source_message_id FROM vision_sources WHERE run_id=?",
      run,
    );
    return {
      basis:
        channel && ["video", "voice", "file"].includes(String(channel.kind))
          ? "channel_media_transport"
          : channel?.kind === "legacy_unverified"
            ? "legacy_unverified"
            : channel?.kind === "notice"
              ? "unavailable_media"
              : !image &&
                  this.store.get(
                    "SELECT run_id FROM qq_media_plans WHERE run_id=? UNION SELECT p.run_id FROM qq_media_transfers t JOIN delivery_parts p ON p.id=t.part_id WHERE p.run_id=?",
                    run,
                    run,
                  )
                ? "channel_media_reply"
                : this.isDerived(run)
                  ? "vision_description"
                  : this.store.get(
                        "SELECT r.id FROM runs r JOIN messages m ON m.run_id=r.id JOIN vision_legacy_fences f ON f.conversation_id=r.conversation_id WHERE r.id=? AND m.rowid<=f.message_cutoff LIMIT 1",
                        run,
                      )
                    ? "legacy_unverified"
                    : "text",
      ...(channel
        ? { attachment: { kind: channel.kind, code: channel.code } }
        : {}),
      ...(image
        ? {
            image: {
              ...image,
              dimensions_known:
                Number(image.width) > 0 && Number(image.height) > 0,
            },
          }
        : {}),
    };
  }
}
