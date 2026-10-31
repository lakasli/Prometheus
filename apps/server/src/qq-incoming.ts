import { ApiError } from "./errors.ts";
import { now, hash } from "./security.ts";
import type { Store, Row } from "./store.ts";
import type { Media } from "./media.ts";
import type { normalizedPng } from "./png.ts";
import {
  downloadQqImage,
  mediaFailure,
  mediaNotice,
} from "./qq-media-download.ts";
type Json = Record<string, any>;
type Image = ReturnType<typeof normalizedPng>;
export class QqIncoming {
  active?: {
    id: string;
    generation: number;
    abort: AbortController;
    task: Promise<void>;
  };
  private stopping = false;
  constructor(
    readonly store: Store,
    readonly media: Media,
    private commit: (
      event: Row,
      data: Json,
      image?: Image,
      code?: string,
    ) => void,
    private loopbackOrigin?: string,
  ) {
    this.initialize();
  }
  private epoch(owner: string) {
    return Number(
      this.store.get(
        "SELECT value FROM metadata WHERE key=?",
        owner === "*" ? "qq.scope:global" : "qq.scope:" + hash(owner),
      )?.value ?? 0,
    );
  }
  private initialize() {
    this.store.transaction(() => {
      const legacy = this.store.all(
        "SELECT DISTINCT r.id,r.conversation_id FROM runs r JOIN run_targets t ON t.run_id=r.id WHERE t.channel='qq' AND NOT EXISTS(SELECT 1 FROM channel_media_sources s WHERE s.run_id=r.id) AND NOT EXISTS(SELECT 1 FROM qq_inbound_receipts i JOIN channel_events e ON e.id=i.event_id WHERE e.run_id=r.id) AND r.input LIKE ?",
        "%[宿主：本条包含媒体，媒体理解尚未完成时不能声称看到了画面或听到了声音。]",
      );
      for (const r of legacy) {
        const event = this.store.get(
          "SELECT id FROM channel_events WHERE run_id=? ORDER BY rowid LIMIT 1",
          String(r.id),
        );
        if (!event) continue;
        this.store.run(
          "INSERT INTO vision_legacy_fences SELECT ?,coalesce(max(rowid),0) FROM messages WHERE conversation_id=? ON CONFLICT(conversation_id) DO UPDATE SET message_cutoff=max(message_cutoff,excluded.message_cutoff)",
          String(r.conversation_id),
          String(r.conversation_id),
        );
        this.store.run(
          "INSERT INTO channel_media_sources VALUES(?,?,'legacy_unverified','legacy_media_reference_lost')",
          String(r.id),
          String(event.id),
        );
        this.store.run(
          "UPDATE model_jobs SET status='invalidated',input_json=NULL,output_json=NULL,error='旧渠道附件缺少实际读取依据',revision=revision+1 WHERE status NOT IN ('approved','rejected','cancelled','invalidated') AND EXISTS(SELECT 1 FROM json_each(model_jobs.source_json) j JOIN messages m ON m.id=json_extract(j.value,'$.id') WHERE json_extract(j.value,'$.type')='message' AND m.conversation_id=?)",
          String(r.conversation_id),
        );
        this.store.run(
          "DELETE FROM summaries WHERE conversation_id=?",
          String(r.conversation_id),
        );
        this.store.run(
          "DELETE FROM prompt_snapshots WHERE run_id IN (SELECT id FROM runs WHERE conversation_id=?)",
          String(r.conversation_id),
        );
        this.store.run(
          "UPDATE conversations SET context_epoch=context_epoch+1 WHERE id=?",
          String(r.conversation_id),
        );
        this.store.run(
          "UPDATE runs SET status='interrupted',error='旧附件没有读取依据，未重放',updated_at=? WHERE conversation_id=? AND status IN ('queued','running')",
          now(),
          String(r.conversation_id),
        );
        this.store.run(
          "UPDATE delivery_parts SET status='cancelled',error='旧附件缺少读取依据' WHERE run_id IN (SELECT id FROM runs WHERE conversation_id=?) AND status='queued'",
          String(r.conversation_id),
        );
        this.store.run(
          "INSERT OR REPLACE INTO run_publications SELECT r.id,'suppressed','旧渠道附件来源待复核',? FROM runs r JOIN run_targets t ON t.run_id=r.id WHERE r.conversation_id=?",
          now(),
          String(r.conversation_id),
        );
      }
      for (const e of this.store.all(
        "SELECT * FROM channel_events WHERE channel='qq' AND NOT EXISTS(SELECT 1 FROM qq_inbound_receipts r WHERE r.event_id=channel_events.id)",
      )) {
        try {
          let data: Json | undefined = e.payload_json
            ? this.store.unseal<Json>(String(e.payload_json))
            : undefined;
          if (!data && e.run_id) {
            const target = this.store.get(
              "SELECT target_json FROM run_targets WHERE run_id=?",
              String(e.run_id),
            );
            if (target)
              data = {
                target: this.store.unseal<Json>(String(target.target_json)),
              };
          }
          if (!data?.target?.message_id) continue;
          if (
            e.status === "queued" &&
            String(data.content).endsWith(
              "[宿主：本条包含媒体，媒体理解尚未完成时不能声称看到了画面或听到了声音。]",
            )
          ) {
            data.media = { code: "legacy_media_reference_lost" };
            this.store.run(
              "UPDATE channel_events SET payload_json=? WHERE id=?",
              this.store.seal(data),
              String(e.id),
            );
          }
          this.record(
            String(e.id),
            String(e.account_id),
            String(data.target.kind),
            String(data.target.message_id),
            !!data.media,
          );
        } catch {
          /* A lost historical identity is not fabricated. Event-key dedup remains. */
        }
      }
      this.store.run(
        "UPDATE channel_events SET status='queued' WHERE channel='qq' AND status='processing'",
      );
      this.store.run(
        "UPDATE qq_inbound_receipts SET generation=generation+1,request_finished_at=CASE WHEN request_started_at IS NOT NULL AND request_finished_at IS NULL THEN ? ELSE request_finished_at END WHERE event_id IN (SELECT id FROM channel_events WHERE channel='qq' AND status='queued')",
        now(),
      );
      this.store.run(
        "UPDATE channel_events SET payload_json=NULL WHERE channel='qq' AND status IN ('failed','expired','cancelled')",
      );
    });
  }
  duplicate(account: string, kind: string, message: string) {
    return this.store.get(
      "SELECT event_id FROM qq_inbound_receipts WHERE account_id=? AND event_type=? AND message_id=?",
      account,
      kind,
      message,
    );
  }
  record(
    event: string,
    account: string,
    kind: string,
    message: string,
    media: boolean,
  ) {
    const e = this.store.get(
        "SELECT owner_id,source_key FROM channel_events WHERE id=?",
        event,
      )!,
      conv = this.store.get(
        "SELECT id,context_epoch FROM conversations WHERE source_key=?",
        String(e.source_key),
      );
    this.store.run(
      "INSERT OR IGNORE INTO qq_inbound_receipts(event_id,account_id,event_type,message_id,media,owner_epoch,global_epoch,conversation_id,context_epoch) VALUES(?,?,?,?,?,?,?,?,?)",
      event,
      account,
      kind,
      message,
      +media,
      this.epoch(String(e.owner_id)),
      this.epoch("*"),
      conv?.id ?? null,
      conv?.context_epoch ?? null,
    );
  }
  invalidate(owner: string) {
    const key = owner === "*" ? "qq.scope:global" : "qq.scope:" + hash(owner);
    this.store.run(
      "INSERT INTO metadata VALUES(?, '1') ON CONFLICT(key) DO UPDATE SET value=cast(value AS INTEGER)+1",
      key,
    );
    this.store.run(
      "UPDATE channel_events SET status='cancelled',payload_json=NULL,error='来源上下文已失效' WHERE channel='qq' AND status IN ('queued','processing') AND (?='*' OR owner_id=?) AND EXISTS(SELECT 1 FROM qq_inbound_receipts r WHERE r.event_id=channel_events.id AND r.media=1)",
      owner,
      owner,
    );
    this.refresh();
    this.media.pruneReceived();
  }
  refresh() {
    this.store.run(
      "UPDATE channel_events SET status='expired',payload_json=NULL,error='原回复期限已过期' WHERE channel='qq' AND status IN ('queued','processing') AND deadline<=?",
      now(),
    );
    if (this.active && !this.current(this.active.id, this.active.generation))
      this.active.abort.abort();
  }
  private current(id: string, generation: number, allowStopping = false) {
    const e = this.store.get(
      "SELECT e.*,r.generation,r.owner_epoch,r.global_epoch,r.conversation_id,r.context_epoch,r.config_revision,r.media_revision FROM channel_events e JOIN qq_inbound_receipts r ON r.event_id=e.id WHERE e.id=?",
      id,
    );
    if (
      !e ||
      e.status !== "processing" ||
      e.generation !== generation ||
      Number(e.deadline) <= now() ||
      (this.stopping && !allowStopping)
    )
      return false;
    const config = this.store.config();
    if (
      !config.config.qq.enabled ||
      config.config.qq.app_id !== e.account_id ||
      config.config.qq.credential_ref !== e.credential_ref ||
      config.revision !== e.config_revision ||
      this.media.policy().revision !== e.media_revision ||
      this.epoch(String(e.owner_id)) !== e.owner_epoch ||
      this.epoch("*") !== e.global_epoch
    )
      return false;
    if (
      e.conversation_id &&
      this.store.get(
        "SELECT context_epoch FROM conversations WHERE id=?",
        String(e.conversation_id),
      )?.context_epoch !== e.context_epoch
    )
      return false;
    return true;
  }
  begin(event: Row, data: Json) {
    if (this.active || this.stopping) return null;
    const r = this.store.get(
      "SELECT * FROM qq_inbound_receipts WHERE event_id=?",
      String(event.id),
    );
    if (!r) return null;
    if (
      this.epoch(String(event.owner_id)) !== r.owner_epoch ||
      this.epoch("*") !== r.global_epoch
    ) {
      this.store.run(
        "UPDATE channel_events SET status='cancelled',payload_json=NULL,error='来源已失效' WHERE id=?",
        String(event.id),
      );
      return null;
    }
    if (
      !data.media.code &&
      (this.media.decoding ||
        this.media.vision.pendingBytes() >= 20 ||
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM runs WHERE status='queued'",
          )!.n,
        ) >= 128)
    ) {
      this.store.run(
        "UPDATE channel_events SET ready_at=? WHERE id=?",
        Date.now() + 500,
        String(event.id),
      );
      return null;
    }
    const generation = Number(r.generation) + 1,
      abort = new AbortController(),
      conv = this.store.get(
        "SELECT id,context_epoch FROM conversations WHERE source_key=?",
        String(event.source_key),
      );
    this.store.transaction(() => {
      this.store.run(
        "UPDATE channel_events SET status='processing' WHERE id=? AND status='queued'",
        String(event.id),
      );
      this.store.run(
        "UPDATE qq_inbound_receipts SET generation=?,conversation_id=?,context_epoch=?,config_revision=?,media_revision=?,request_started_at=NULL,request_finished_at=NULL WHERE event_id=?",
        generation,
        conv?.id ?? null,
        conv?.context_epoch ?? null,
        this.store.config().revision,
        this.media.policy().revision,
        String(event.id),
      );
    });
    const task = this.process(event, data, generation, abort.signal);
    this.active = { id: String(event.id), generation, abort, task };
    void task
      .finally(() => {
        if (
          this.store.get(
            "SELECT status FROM channel_events WHERE id=?",
            String(event.id),
          )?.status === "processing"
        ) {
          if (this.stopping && this.current(String(event.id), generation, true))
            this.requeue(String(event.id));
          else
            this.store.run(
              "UPDATE channel_events SET status='cancelled',payload_json=NULL,error='接收授权已失效' WHERE id=?",
              String(event.id),
            );
        }
        if (
          this.active?.generation === generation &&
          this.active.id === event.id
        )
          this.active = undefined;
      })
      .catch(() => {});
    return task;
  }
  private async process(
    event: Row,
    data: Json,
    generation: number,
    signal: AbortSignal,
  ) {
    const id = String(event.id),
      combined = AbortSignal.any([signal, AbortSignal.timeout(20000)]);
    let image: Image | undefined,
      code: string | undefined = data.media.code;
    try {
      if (!code) {
        const config = this.store.config().config,
          policy = this.media.policy();
        if (
          !policy.value.vision_enabled ||
          !policy.value.vision_model ||
          !config.model.credential_ref
        )
          code = "vision_provider_unavailable";
        else {
          this.store.run(
            "UPDATE qq_inbound_receipts SET request_started_at=? WHERE event_id=? AND generation=?",
            now(),
            id,
            generation,
          );
          let bytes: Buffer;
          try {
            bytes = await downloadQqImage(
              String(data.media.url),
              combined,
              this.loopbackOrigin,
            );
          } finally {
            this.store.run(
              "UPDATE qq_inbound_receipts SET request_finished_at=? WHERE event_id=? AND generation=?",
              now(),
              id,
              generation,
            );
          }
          if (!this.current(id, generation)) return;
          image = await this.media.decodeIncoming(bytes, combined);
        }
      }
    } catch (error) {
      if (
        error instanceof ApiError &&
        ["image_processing_busy", "media_capacity"].includes(error.code) &&
        this.current(id, generation)
      ) {
        this.requeue(id);
        return;
      }
      code = error instanceof ApiError ? error.code : "media_image_invalid";
    }
    if (!this.current(id, generation)) {
      if (this.stopping && this.current(id, generation, true)) {
        this.requeue(id);
        return;
      }
      this.store.run(
        "UPDATE channel_events SET status=?,payload_json=NULL,error='媒体接收期限、账户或来源已失效' WHERE id=? AND status='processing'",
        this.stopping
          ? "cancelled"
          : Number(event.deadline) <= now()
            ? "expired"
            : "cancelled",
        id,
      );
      return;
    }
    try {
      this.store.transaction(() => {
        if (!this.current(id, generation))
          throw mediaFailure("media_receive_revoked");
        this.commit(event, data, image, code);
        this.store.run(
          "UPDATE qq_inbound_receipts SET code=? WHERE event_id=? AND generation=?",
          code ?? null,
          id,
          generation,
        );
      });
    } catch (error) {
      if (
        error instanceof ApiError &&
        [
          "queue_full",
          "model_not_configured",
          "media_capacity",
          "image_processing_busy",
        ].includes(error.code) &&
        this.current(id, generation)
      )
        this.requeue(id);
      else if (!(error instanceof ApiError))
        this.store.run(
          "UPDATE channel_events SET status='queued',ready_at=?,error='存储暂不可用，已保留接收记录等待重试' WHERE id=? AND status='processing'",
          Date.now() + 5000,
          id,
        );
      else
        this.store.run(
          "UPDATE channel_events SET status='failed',payload_json=NULL,error='附件未能提交，未启动重复模型请求' WHERE id=? AND status='processing'",
          id,
        );
    }
  }
  private requeue(id: string) {
    this.store.run(
      "UPDATE channel_events SET status='queued',ready_at=?,error='暂存或运行队列等待容量' WHERE id=? AND status='processing'",
      Date.now() + 500,
      id,
    );
  }
  async stop() {
    this.stopping = true;
    this.active?.abort.abort();
    await this.active?.task;
    this.store.run(
      "UPDATE channel_events SET status='queued' WHERE channel='qq' AND status='processing'",
    );
  }
  notice(code: string) {
    return { response: mediaNotice(code), kind: "channel_media_notice" };
  }
}
