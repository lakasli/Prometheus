import { randomUUID } from "node:crypto";
import { ApiError, bad, fields, missing, string } from "./errors.ts";
import { hash, now } from "./security.ts";
import { imageBase64 } from "./image-codec.ts";
import { normalizedPng } from "./png.ts";
import { validateVideo, validateVoice } from "./media-container.ts";
import { MEDIA_LIMIT, uploadQq, qqJson } from "./qq-upload.ts";
import { strictModelJson } from "./schedule-json.ts";
import type { Store, Row } from "./store.ts";
import type { Runtime } from "./runtime.ts";
import type { Media } from "./media.ts";
import type { Config } from "./config.ts";
export type Part = {
  type: string;
  text: string;
  asset_id?: string;
  delay_ms: number;
};
interface Scope {
  id: string;
  owner_id: string;
  conversation_id: string;
  context_epoch: number;
  deadline: number;
  target: Record<string, string>;
}
interface Asset {
  id: string;
  kind: string;
  mime: string;
  extension: string;
  digest: string;
  bytes: Buffer;
  source_run_id: string;
  expires_at: number;
}
export class QqOutgoing {
  active?: { part: string; run: string; abort: AbortController };
  private receiving = false;
  private accepting = false;
  async acceptUpload(
    actor: string,
    readBody: () => Promise<unknown>,
    available: () => boolean,
  ) {
    if (this.accepting)
      throw new ApiError(429, "media_upload_busy", "媒体接收正在处理");
    this.accepting = true;
    try {
      return await this.upload(actor, await readBody(), available);
    } finally {
      this.accepting = false;
    }
  }
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    readonly media: Media,
    readonly base: string,
    readonly token: (c: Config) => Promise<string>,
    readonly admitted: (run: string) => boolean,
    readonly loopback = false,
    readonly putFixture?: string,
  ) {
    // Store already marks an interrupted sending part unknown. A persisted fence
    // distinguishes upload-only work, which cannot have delivered a message.
    store.run(
      "UPDATE delivery_parts SET status='failed',error='上传随进程中断，尚未发送消息' WHERE status='unknown' AND id IN (SELECT part_id FROM qq_media_transfers WHERE message_started_at IS NULL)",
    );
    store.run(
      "UPDATE qq_media_transfers SET stage=CASE WHEN message_started_at IS NULL THEN 'failed' ELSE 'unknown' END,request_finished_at=?,error_code='service_restarted' WHERE request_started_at IS NOT NULL AND request_finished_at IS NULL",
      now(),
    );
  }
  context(run: string) {
    let r: ReturnType<QqOutgoing["live"]>;
    try {
      r = this.live(run, true);
    } catch {
      return "";
    }
    this.prune();
    const received = this.store.all(
      "SELECT id FROM received_media_assets WHERE owner_id=? AND conversation_id=? AND expires_at>? ORDER BY created_at DESC LIMIT 8",
      String(r.owner_id),
      String(r.conversation_id),
      now(),
    );
    for (const item of received) {
      const a = this.store.database
        .prepare("SELECT * FROM received_media_assets WHERE id=?")
        .get(String(item.id))!;
      if (
        this.store.get(
          "SELECT id FROM qq_media_assets WHERE source_kind='received' AND source_id=?",
          String(a.id),
        )
      )
        continue;
      if (!this.capacity(Number((a.bytes as Uint8Array).length))) break;
      this.store.run(
        "INSERT INTO qq_media_assets VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        String(a.id),
        String(a.owner_id),
        String(a.conversation_id),
        String(a.source_run_id),
        "host",
        "received",
        String(a.id),
        "image",
        "image/png",
        "png",
        String(a.digest),
        a.bytes as Uint8Array,
        Number(r.context_epoch),
        Number(a.created_at),
        Number(a.expires_at),
        null,
      );
    }
    const assets = this.store.all(
      "SELECT id,kind,digest FROM qq_media_assets WHERE owner_id=? AND conversation_id=? AND context_epoch=? AND expires_at>? AND revoked_at IS NULL AND source_kind='received' ORDER BY created_at DESC LIMIT 8",
      String(r.owner_id),
      String(r.conversation_id),
      Number(r.context_epoch),
      now(),
    );
    for (const a of assets)
      this.store.run(
        "INSERT OR IGNORE INTO qq_media_offers VALUES(?,?,?)",
        run,
        String(a.id),
        String(a.digest),
      );
    if (!assets.length) return "";
    return (
      "\n宿主QQ媒体回复规则：只有明确需要回传时才引用下列同会话制品；这些是规范化来源图片，不能称原始附件。回复可为1至4项JSON数组，每项仅type(text/image)、text、asset_id(图片必填)、delay_ms。不能指定URL、路径或收件人；这不授予主动联系或其他执行权限。制品目录：" +
      JSON.stringify(assets) +
      "\n"
    );
  }
  resolve(run: string, raw: string) {
    if (
      !this.store.get(
        "SELECT run_id FROM run_targets WHERE run_id=? AND channel='qq'",
        run,
      )
    )
      return null;
    let value: any;
    const input = raw.trim();
    if (!input.startsWith("[") && !input.startsWith('{"items"')) return null;
    try {
      value = strictModelJson(input);
    } catch {
      throw bad("媒体回复JSON无效", "media_plan_invalid");
    }
    const items = Array.isArray(value) ? value : value?.items;
    if (
      !Array.isArray(items) ||
      !items.some((p: any) => p?.type && p.type !== "text")
    )
      return null;
    this.live(run, true);
    if (!items.length || items.length > 4)
      throw bad("媒体回复最多四项", "media_plan_invalid");
    const parts: Part[] = items.map((v: any, i: number) => {
      const b = fields(v, ["type", "text", "asset_id", "delay_ms"]),
        text = string(b.text ?? "", "回复", 6000).trim(),
        type = String(b.type);
      if (
        !["text", "image", "video", "voice"].includes(type) ||
        [...text].length > 1500 ||
        (type === "text" && !text) ||
        !Number.isSafeInteger(b.delay_ms ?? 0) ||
        Number(b.delay_ms ?? 0) < 0 ||
        Number(b.delay_ms ?? 0) > 8000
      )
        throw bad("媒体回复字段无效", "media_plan_invalid");
      if (type === "text" && b.asset_id !== undefined)
        throw bad("文字不能携带媒体引用", "media_plan_invalid");
      const asset_id =
        type === "text" ? undefined : string(b.asset_id, "媒体引用", 36, 36);
      if (asset_id) {
        const offer = this.store.get(
            "SELECT digest FROM qq_media_offers WHERE run_id=? AND asset_id=?",
            run,
            asset_id,
          ),
          asset = this.asset(run, asset_id, true);
        if (!offer || offer.digest !== asset.digest || asset.kind !== type)
          throw bad("媒体不在本轮实际提供的目录中", "media_source_not_offered");
      }
      return {
        type,
        text,
        asset_id,
        delay_ms: i ? Math.max(1500, Number(b.delay_ms) || 1500) : 0,
      };
    });
    return {
      reply: parts
        .map((p) =>
          p.type === "text"
            ? p.text
            : `[${({ image: "图片", video: "视频", voice: "语音" } as Record<string, string>)[p.type]}]${p.text}`,
        )
        .join("\n"),
      parts,
    };
  }
  commit(run: string, parts: Part[]) {
    for (const p of parts) if (p.asset_id) this.asset(run, p.asset_id, true);
    this.store.run(
      "INSERT INTO qq_media_plans VALUES(?,?)",
      run,
      JSON.stringify(parts),
    );
  }
  plan(run: string): Part[] | undefined {
    const p = this.store.get(
      "SELECT parts_json FROM qq_media_plans WHERE run_id=?",
      run,
    );
    return p ? JSON.parse(String(p.parts_json)) : undefined;
  }
  live(run: string, running = false): Scope {
    const r = this.store.get(
      "SELECT r.id,r.owner_id,r.conversation_id,r.context_epoch,r.status,c.context_epoch AS current_epoch,t.channel,t.target_json,t.account_id,t.credential_ref,t.deadline FROM runs r JOIN conversations c ON c.id=r.conversation_id JOIN run_targets t ON t.run_id=r.id WHERE r.id=?",
      run,
    );
    if (!r) throw missing();
    const c = this.store.config().config,
      target = this.store.unseal<Record<string, string>>(String(r.target_json));
    if (
      r.channel !== "qq" ||
      target.kind !== "c2c" ||
      !target.target ||
      !/^[A-Za-z0-9_]{1,256}$/.test(target.target) ||
      target.account !== r.account_id ||
      !target.message_id ||
      !c.qq.enabled ||
      c.qq.app_id !== r.account_id ||
      c.qq.credential_ref !== r.credential_ref ||
      Number(r.deadline) <= now() ||
      r.context_epoch !== r.current_epoch ||
      !(r.status === "completed" || (running && r.status === "running")) ||
      this.store.get("SELECT state FROM run_publications WHERE run_id=?", run)
        ?.state === "suppressed"
    )
      throw new ApiError(
        409,
        "media_delivery_revoked",
        "QQ来源、账户或被动回复窗口已失效",
      );
    return { ...r, target } as unknown as Scope;
  }
  private capacity(bytes: number) {
    const q = this.store.get(
      "SELECT count(*) AS n,coalesce(sum(length(bytes)),0) AS bytes FROM qq_media_assets WHERE revoked_at IS NULL",
    )!;
    return Number(q.n) < 32 && Number(q.bytes) + bytes <= 128 * 1024 * 1024;
  }
  async upload(
    actor: string,
    value: unknown,
    available: () => boolean = () => true,
  ) {
    if (this.receiving)
      throw new ApiError(429, "media_upload_busy", "媒体接收正在处理");
    const b = fields(value, ["run_id", "kind", "data_base64", "request_id"]),
      run = string(b.run_id, "来源Run", 36, 36),
      request = string(b.request_id, "上传幂等标识", 128, 1),
      kind = string(b.kind, "媒体类型", 10, 1);
    if (!["image", "video", "voice"].includes(kind)) throw bad("媒体类型无效");
    const r = this.live(run),
      encoded = string(
        b.data_base64,
        "媒体数据",
        Math.ceil(MEDIA_LIMIT / 3) * 4,
        1,
      );
    if (encoded.length % 4 || !/^[A-Za-z0-9+/]+={0,2}$/.test(encoded))
      throw bad("媒体须为规范base64");
    const bytes = Buffer.from(encoded, "base64");
    if (
      !bytes.length ||
      bytes.length > MEDIA_LIMIT ||
      bytes.toString("base64") !== encoded
    )
      throw bad("媒体大小或base64无效");
    const fingerprint = hash(run + ":" + kind + ":" + hash(bytes)),
      key = hash(actor + ":" + request),
      old = this.store.get("SELECT * FROM qq_media_uploads WHERE id=?", key);
    if (old) {
      if (old.fingerprint !== fingerprint)
        throw new ApiError(
          409,
          "revision_conflict",
          "同一上传标识的来源或内容已改变",
        );
      return this.metadata(this.asset(run, String(old.asset_id)));
    }
    this.prune();
    if (!this.capacity(bytes.length))
      throw new ApiError(429, "media_capacity", "媒体暂存容量已满");
    this.receiving = true;
    try {
      let output = bytes,
        meta: { mime: string; extension: string };
      if (kind === "image") {
        const image = await this.media.decodeIncoming(
          imageBase64(encoded),
          AbortSignal.timeout(20000),
        );
        output = image.bytes;
        meta = { mime: "image/png", extension: "png" };
      } else
        meta = kind === "video" ? validateVideo(bytes) : validateVoice(bytes);
      if (!available())
        throw bad("媒体上传连接已关闭", "media_upload_cancelled");
      const current = this.live(run);
      if (current.context_epoch !== r.context_epoch) throw bad("来源已失效");
      if (!this.capacity(output.length))
        throw new ApiError(429, "media_capacity", "媒体暂存容量已满");
      const id = randomUUID();
      this.store.transaction(() => {
        this.store.run(
          "INSERT INTO qq_media_assets VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
          id,
          String(r.owner_id),
          String(r.conversation_id),
          run,
          actor,
          "manual",
          key,
          kind,
          meta.mime,
          meta.extension,
          hash(output),
          output,
          Number(r.context_epoch),
          now(),
          now() + 86400,
          null,
        );
        this.store.run(
          "INSERT INTO qq_media_uploads VALUES(?,?,?,?)",
          key,
          fingerprint,
          id,
          actor,
        );
      });
      return this.metadata(this.asset(run, id));
    } finally {
      this.receiving = false;
    }
  }
  private metadata(a: any) {
    return {
      id: a.id,
      run_id: a.source_run_id,
      kind: a.kind,
      mime: a.mime,
      sha256: a.digest,
      bytes: a.bytes.length,
      expires_at: a.expires_at,
    };
  }
  asset(run: string, id: string, running = false): Asset {
    const r = this.live(run, running),
      a = this.store.database
        .prepare(
          "SELECT * FROM qq_media_assets WHERE id=? AND owner_id=? AND conversation_id=? AND context_epoch=? AND revoked_at IS NULL AND expires_at>?",
        )
        .get(
          id,
          String(r.owner_id),
          String(r.conversation_id),
          Number(r.context_epoch),
          now(),
        );
    if (!a || !a.bytes) throw missing();
    const bytes = Buffer.from(a.bytes as Uint8Array);
    if (!bytes.length || hash(bytes) !== a.digest)
      throw bad("媒体制品摘要不符", "media_digest_mismatch");
    if (a.kind === "image") normalizedPng(bytes);
    else if (a.kind === "video") validateVideo(bytes);
    else if (a.kind === "voice") validateVoice(bytes);
    else throw bad("媒体类型无效");
    if (
      a.source_kind === "received" &&
      !this.store.get(
        "SELECT id FROM received_media_assets WHERE id=? AND expires_at>?",
        String(a.source_id),
        now(),
      )
    )
      throw bad("来源图片已撤销", "media_source_revoked");
    return { ...a, bytes } as unknown as Asset;
  }
  queue(actor: string, value: unknown) {
    const b = fields(value, [
        "run_id",
        "asset_id",
        "text",
        "request_id",
        "acknowledge_effects",
      ]),
      run = string(b.run_id, "来源Run", 36, 36),
      assetId = string(b.asset_id, "媒体引用", 36, 36),
      text = string(b.text ?? "", "说明", 6000),
      request = string(b.request_id, "发送幂等标识", 128, 1);
    if (b.acknowledge_effects !== true || [...text].length > 1500)
      throw bad("请明确确认当前私聊媒体发送");
    const key = hash(actor + ":" + request),
      fingerprint = hash(JSON.stringify({ run, assetId, text })),
      old = this.store.get("SELECT * FROM qq_media_requests WHERE id=?", key);
    if (old) {
      if (old.fingerprint !== fingerprint)
        throw new ApiError(409, "revision_conflict", "发送标识已用于其他内容");
      return {
        part_id: old.part_id,
        status: this.store.get(
          "SELECT status FROM delivery_parts WHERE id=?",
          String(old.part_id),
        )?.status,
      };
    }
    const a = this.asset(run, assetId),
      id = randomUUID();
    return this.store.transaction(() => {
      const sequence = Number(
        this.store.get(
          "SELECT coalesce(max(sequence),0)+1 AS n FROM delivery_parts WHERE run_id=?",
          run,
        )!.n,
      );
      if (sequence === 1)
        throw new ApiError(
          409,
          "media_reply_not_queued",
          "请等待原回复进入投递队列后再追加媒体",
        );
      if (
        sequence > 4 ||
        this.store.get(
          "SELECT id FROM delivery_parts WHERE run_id=? AND status NOT IN ('queued','sending','sent')",
          run,
        )
      )
        throw bad("回复已达四项或前序投递未确认", "media_sequence_exhausted");
      this.store.run(
        "INSERT INTO delivery_parts VALUES(?,?,?,?,?,'queued',NULL,NULL,?,?)",
        id,
        run,
        sequence,
        String(a.kind),
        `[${({ image: "图片", video: "视频", voice: "语音" } as Record<string, string>)[a.kind]}]${text}`,
        Date.now() + 1500,
        now(),
      );
      this.transfer(id, assetId);
      this.store.run(
        "INSERT INTO qq_media_requests VALUES(?,?,?,?,?)",
        key,
        fingerprint,
        id,
        actor,
        now(),
      );
      this.store.audit(
        actor,
        "delivery",
        "queued",
        "已明确确认同一被动回复目标的媒体",
        run,
      );
      return { part_id: id, status: "queued" };
    });
  }
  transfer(part: string, asset: string) {
    this.store.run(
      "INSERT INTO qq_media_transfers(part_id,asset_id,stage,completed_parts) VALUES(?,?,'queued',0)",
      part,
      asset,
    );
  }
  refresh() {
    this.prune();
    if (this.active) {
      try {
        this.live(this.active.run);
        if (!this.admitted(this.active.run)) throw Error();
        const a = this.store.get(
          "SELECT asset_id FROM qq_media_transfers WHERE part_id=?",
          this.active.part,
        )!;
        this.asset(this.active.run, String(a.asset_id));
      } catch {
        this.active.abort.abort();
      }
    }
  }
  invalidate(owner: string) {
    if (
      this.active &&
      (owner === "*" ||
        this.store.get("SELECT owner_id FROM runs WHERE id=?", this.active.run)
          ?.owner_id === owner)
    )
      this.active.abort.abort();
    this.store.run(
      "UPDATE qq_media_assets SET bytes=x'',revoked_at=? WHERE revoked_at IS NULL AND (?='*' OR owner_id=?)",
      now(),
      owner,
      owner,
    );
    this.prune();
  }
  contexts() {
    const result = [];
    for (const r of this.store.all(
      "SELECT r.id,max(e.received_at) AS received_at FROM runs r JOIN channel_events e ON e.run_id=r.id WHERE r.status='completed' AND e.channel='qq' GROUP BY r.id ORDER BY received_at DESC LIMIT 50",
    )) {
      try {
        const live = this.live(String(r.id)),
          count = Number(
            this.store.get(
              "SELECT coalesce(max(sequence),0) AS n FROM delivery_parts WHERE run_id=?",
              String(r.id),
            )!.n,
          );
        if (
          count > 0 &&
          count < 4 &&
          !this.store.get(
            "SELECT id FROM delivery_parts WHERE run_id=? AND status NOT IN ('queued','sending','sent')",
            String(r.id),
          )
        )
          result.push({
            run_id: r.id,
            target_id: live.target.target,
            received_at: r.received_at,
            deadline: live.deadline,
            remaining_parts: 4 - count,
          });
      } catch {
        /* Not an eligible current private reply. */
      }
    }
    return result;
  }
  prune() {
    this.store.run(
      "UPDATE qq_media_assets SET bytes=x'',revoked_at=? WHERE revoked_at IS NULL AND (expires_at<=? OR context_epoch<>(SELECT context_epoch FROM conversations WHERE id=conversation_id))",
      now(),
      now(),
    );
    this.store.run(
      "UPDATE qq_media_assets SET bytes=x'',revoked_at=? WHERE revoked_at IS NULL AND source_kind='received' AND NOT EXISTS(SELECT 1 FROM received_media_assets r WHERE r.id=qq_media_assets.source_id AND r.expires_at>?)",
      now(),
      now(),
    );
  }
  list() {
    return this.store.all(
      "SELECT p.id,p.run_id,p.sequence,p.kind,p.status,p.platform_id,p.error,t.stage,t.completed_parts,t.message_started_at,t.request_started_at,t.request_finished_at,t.error_code,a.id AS asset_id,a.mime,a.digest,a.expires_at FROM qq_media_transfers t JOIN delivery_parts p ON p.id=t.part_id JOIN qq_media_assets a ON a.id=t.asset_id ORDER BY p.rowid DESC LIMIT 100",
    );
  }
  async send(row: Row, shutdown: AbortSignal) {
    if (this.active) throw bad("媒体正在上传", "media_upload_busy");
    const run = String(row.run_id),
      part = String(row.id),
      abort = new AbortController();
    this.active = { run, part, abort };
    const signal = AbortSignal.any([
      shutdown,
      abort.signal,
      AbortSignal.timeout(90000),
    ]);
    const stage = (stage: string, n?: number) =>
      this.store.run(
        "UPDATE qq_media_transfers SET stage=?,completed_parts=coalesce(?,completed_parts) WHERE part_id=?",
        stage,
        n ?? null,
        part,
      );
    const admit = () => {
      if (signal.aborted || !this.admitted(run))
        throw new ApiError(
          409,
          "media_delivery_revoked",
          "媒体发送已暂停或撤销",
        );
      this.live(run);
      const a = this.store.get(
        "SELECT asset_id FROM qq_media_transfers WHERE part_id=?",
        part,
      )!;
      this.asset(run, String(a.asset_id));
    };
    this.store.run(
      "UPDATE qq_media_transfers SET request_started_at=?,request_finished_at=NULL WHERE part_id=?",
      now(),
      part,
    );
    try {
      admit();
      const r = this.live(run),
        config = this.store.config().config,
        transfer = this.store.get(
          "SELECT asset_id FROM qq_media_transfers WHERE part_id=?",
          part,
        )!,
        asset = this.asset(run, String(transfer.asset_id));
      const token = await this.token(config);
      admit();
      const file_info = await uploadQq({
        base: this.base,
        user: r.target.target!,
        token,
        app: config.qq.app_id,
        kind: String(asset.kind),
        bytes: asset.bytes,
        extension: String(asset.extension),
        signal,
        loopback: this.loopback,
        putFixture: this.putFixture,
        admit,
        stage,
      });
      admit();
      stage("sending");
      this.store.run(
        "UPDATE qq_media_transfers SET message_started_at=? WHERE part_id=?",
        now(),
        part,
      );
      const result = await qqJson(
        this.base,
        `/v2/users/${r.target.target}/messages`,
        {
          msg_type: 7,
          content: asset.kind === "image" ? String(row.content) || " " : " ",
          media: { file_info },
          msg_id: r.target.message_id,
          msg_seq: Number(row.sequence),
        },
        token,
        config.qq.app_id,
        signal,
        this.loopback,
        true,
      );
      const id = result.id ?? result.message?.id ?? result.message_id;
      if (typeof id !== "string" || !id || id.length > 512)
        throw bad("QQ未返回消息标识", "media_send_unknown");
      stage("sent");
      return id;
    } catch (error) {
      const started = this.store.get(
          "SELECT message_started_at FROM qq_media_transfers WHERE part_id=?",
          part,
        )?.message_started_at,
        code = error instanceof ApiError ? error.code : "upload_failed",
        unknown = !!started && code !== "media_send_rejected";
      this.store.run(
        "UPDATE qq_media_transfers SET stage=?,error_code=? WHERE part_id=?",
        unknown ? "unknown" : "failed",
        code,
        part,
      );
      throw new ApiError(
        502,
        unknown ? "media_send_unknown" : "media_upload_failed",
        unknown
          ? "媒体消息结果未知，未自动重试"
          : "媒体上传或授权检查未完成，未确认发送消息",
      );
    } finally {
      this.store.run(
        "UPDATE qq_media_transfers SET request_finished_at=? WHERE part_id=?",
        now(),
        part,
      );
      this.active = undefined;
    }
  }
}
