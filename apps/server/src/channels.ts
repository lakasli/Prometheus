import {
  createPrivateKey,
  createPublicKey,
  randomBytes,
  randomUUID,
  sign,
  verify,
} from "node:crypto";
import { lookup } from "node:dns";
import WebSocket from "ws";
import type { IncomingMessage } from "node:http";
import type { Config } from "./config.ts";
import {
  ApiError,
  bad,
  conflict,
  fields,
  missing,
  object,
  string,
} from "./errors.ts";
import { allowedAddress, requestJson } from "./model.ts";
import { hash, now } from "./security.ts";
import type { Store } from "./store.ts";
import type { Runtime } from "./runtime.ts";
import { explicitCorrection } from "./runtime.ts";

type Json = Record<string, any>;
export interface ChannelEndpoints {
  qq: string;
  weixin: string;
}
interface Target {
  kind: string;
  target: string;
  message_id: string;
  account: string;
  context?: string;
  sender: string;
}
const defaults: ChannelEndpoints = {
  qq: "https://api.bot.qq.com",
  weixin: "https://ilinkai.weixin.qq.com",
};
const id = (value: unknown) => string(value, "平台标识", 256, 1);
const qqId = (value: unknown) => {
  const result = id(value);
  if (!/^[a-zA-Z0-9_]+$/.test(result)) throw bad("QQ 标识无效");
  return result;
};
function key(secret: string) {
  const bytes = Buffer.from(secret),
    seed = Buffer.alloc(32);
  if (!bytes.length) throw bad("QQ 凭据为空");
  for (let index = 0; index < 32; index++)
    seed[index] = bytes[index % bytes.length]!;
  return createPrivateKey({
    key: Buffer.concat([
      Buffer.from("302e020100300506032b657004220420", "hex"),
      seed,
    ]),
    format: "der",
    type: "pkcs8",
  });
}
export function qqSignature(
  secret: string,
  timestamp: string,
  body: Buffer,
): string {
  return sign(
    null,
    Buffer.concat([Buffer.from(timestamp), body]),
    key(secret),
  ).toString("hex");
}
export function verifyQq(
  secret: string,
  timestamp: string,
  signature: string,
  body: Buffer,
): boolean {
  if (
    !/^[0-9]{1,20}$/.test(timestamp) ||
    Math.abs(now() - Number(timestamp)) > 300 ||
    !/^[0-9a-f]{128}$/i.test(signature)
  )
    return false;
  return verify(
    null,
    Buffer.concat([Buffer.from(timestamp), body]),
    createPublicKey(key(secret)),
    Buffer.from(signature, "hex"),
  );
}
export function replyParts(content: string): { kind: string; text: string }[] {
  let items: { kind: string; text: string }[] = [];
  try {
    const value = JSON.parse(content);
    if (Array.isArray(value.items))
      items = value.items.map((item: Json) => ({
        kind: item.type,
        text: item.text,
      }));
  } catch {
    /* Plain text remains supported. */
  }
  if (items.length) {
    if (
      items.length > 4 ||
      items.some(
        (item) =>
          !["text", "voice"].includes(item.kind) ||
          typeof item.text !== "string" ||
          !item.text.trim() ||
          [...item.text].length > 1500,
      )
    )
      throw bad("回复计划无效");
    return items.map((item) => ({ kind: "text", text: item.text }));
  }
  const characters = [...content];
  if (!characters.length) throw bad("回复内容为空");
  if (characters.length > 6000)
    return [
      {
        kind: "text",
        text: "这次回复太长，暂时无法完整发出。请缩小问题范围，我再认真说清楚。",
      },
    ];
  for (let index = 0; index < characters.length; index += 1500)
    items.push({
      kind: "text",
      text: characters.slice(index, index + 1500).join(""),
    });
  return items;
}
function business(value: unknown, requireRet = false): Json {
  const result = object(value) as Json;
  for (const name of ["ret", "errcode", "code"])
    if (result[name] !== undefined && Number(result[name]) !== 0)
      throw new ApiError(
        502,
        "channel_rejected",
        `平台拒绝请求，代码 ${Number.isFinite(Number(result[name])) ? Number(result[name]) : "未知"}`,
      );
  if (requireRet && result.ret === undefined)
    throw new ApiError(502, "invalid_channel_response", "平台缺少业务回执");
  return result;
}
export class Channels {
  private stopping = false;
  private timer?: ReturnType<typeof setTimeout>;
  private tasks = new Set<Promise<unknown>>();
  private gateway?: {
    fingerprint: string;
    abort: AbortController;
    task: Promise<unknown>;
  };
  private receiver?: {
    fingerprint: string;
    abort: AbortController;
    task: Promise<unknown>;
  };
  private controller = new AbortController();
  private tokenCache?: { identity: string; token: string; expiry: number };
  private tokenPending?: Promise<string>;
  private deliveryBusy = false;
  private accountNext = 0;
  private challengeAttempts: number[] = [];
  private blockedWeixinGeneration = "";
  readonly connection = { qq: "disabled", weixin: "disabled" };
  canDeliver?: (run?: string) => boolean;
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    private allowLoopback = false,
    private endpoints: ChannelEndpoints = defaults,
  ) {
    if (
      endpoints !== defaults &&
      (!allowLoopback ||
        Object.values(endpoints).some(
          (value) => !["127.0.0.1", "[::1]"].includes(new URL(value).hostname),
        ))
    )
      throw bad("测试端点只能指向显式 loopback");
  }
  start() {
    this.store.run(
      "UPDATE login_jobs SET status='failed',secret_json=NULL,error='创建二维码时服务退出，请重新开始' WHERE status='creating'",
    );
    for (const row of this.store.all(
      "SELECT id FROM login_jobs WHERE status IN ('wait','scaned','need_verifycode') AND expires_at>?",
      now(),
    ))
      this.track(
        this.pollLogin(String(row.id)).catch(() => {
          this.store.run(
            "UPDATE login_jobs SET status='failed',secret_json=NULL WHERE id=?",
            String(row.id),
          );
        }),
      );
    this.tick();
  }
  private track<T>(task: Promise<T>) {
    this.tasks.add(task);
    void task.finally(() => this.tasks.delete(task)).catch(() => {});
    return task;
  }
  async stop() {
    this.stopping = true;
    if (this.timer) clearTimeout(this.timer);
    this.controller.abort();
    this.gateway?.abort.abort();
    this.receiver?.abort.abort();
    await Promise.allSettled([...this.tasks]);
  }
  private async http(
    base: string,
    path: string,
    method: string,
    value: unknown,
    headers: Record<string, string> = {},
    signal = this.controller.signal,
    timeout = 15000,
  ): Promise<Json> {
    return object(
      await requestJson(
        new URL(path, base),
        value,
        { "user-agent": "Prometheus/0.1", ...headers },
        AbortSignal.any([signal, AbortSignal.timeout(timeout)]),
        this.allowLoopback,
        { method, maxRequestBytes: 1024 * 1024 },
      ),
    ) as Json;
  }
  private weixinBase(value?: string): string {
    if (!value) return this.endpoints.weixin;
    const url = new URL(value);
    if (
      this.allowLoopback &&
      url.origin === new URL(this.endpoints.weixin).origin
    )
      return url.origin;
    if (
      url.protocol !== "https:" ||
      url.port ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      url.pathname !== "/" ||
      !(
        url.hostname === "ilinkai.weixin.qq.com" ||
        url.hostname.endsWith(".ilinkai.weixin.qq.com")
      )
    )
      throw bad("微信返回了不可信端点");
    return url.origin;
  }
  private async qqToken(config: Config): Promise<string> {
    const identity = `${config.qq.app_id}:${config.qq.credential_ref}`;
    if (
      this.tokenCache?.identity === identity &&
      this.tokenCache.expiry > now() + 30
    )
      return this.tokenCache.token;
    if (this.tokenPending) {
      await this.tokenPending;
      return this.qqToken(config);
    }
    this.tokenPending = (async () => {
      const value = business(
        await this.http(this.endpoints.qq, "/app/getAppAccessToken", "POST", {
          appId: config.qq.app_id,
          clientSecret: this.store.secret(
            config.qq.credential_ref!,
            "qq_app_secret",
          ),
        }),
      );
      const token = string(value.access_token, "QQ 访问凭据", 8192, 1);
      this.tokenCache = {
        identity,
        token,
        expiry:
          now() + Math.max(60, Math.min(7200, Number(value.expires_in) || 300)),
      };
      return token;
    })();
    try {
      return await this.tokenPending;
    } finally {
      this.tokenPending = undefined;
    }
  }
  private wxHeaders(token?: string) {
    return {
      "iLink-App-Id": "bot",
      "iLink-App-ClientVersion": "132106",
      AuthorizationType: "ilink_bot_token",
      "X-WECHAT-UIN": Buffer.from(
        String(randomBytes(4).readUInt32BE()),
      ).toString("base64"),
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    };
  }
  private wxState(): {
    revision: number;
    generation: string;
    account: Json;
  } | null {
    const row = this.store.get(
      "SELECT * FROM channel_state WHERE channel='weixin'",
    );
    return row
      ? {
          revision: Number(row.revision),
          generation: String(row.generation),
          account: this.store.unseal<Json>(String(row.value_json)),
        }
      : null;
  }
  status() {
    const state = this.wxState();
    return {
      connection: this.connection,
      qq: {
        configured: !!this.store.config().config.qq.credential_ref,
        ...this.store.config().config.qq,
        credential_ref: undefined,
      },
      weixin: state
        ? {
            configured: true,
            revision: state.revision,
            account_id: state.account.account_id,
            linked_owner: state.account.owner_id,
            enabled: this.store.config().config.weixin.enabled,
          }
        : { configured: false, revision: 0 },
      delivery: this.store.all(
        "SELECT channel,status,count(*) AS count FROM delivery_parts p JOIN run_targets t ON t.run_id=p.run_id GROUP BY channel,status",
      ),
    };
  }
  async webhook(request: IncomingMessage, raw: Buffer): Promise<unknown> {
    const config = this.store.config().config;
    if (!config.qq.enabled || config.qq.transport !== "webhook")
      throw new ApiError(503, "channel_disabled", "QQ Webhook 未启用");
    if (
      request.headers["x-bot-appid"] !== undefined &&
      request.headers["x-bot-appid"] !== config.qq.app_id
    )
      throw new ApiError(403, "qq_application_mismatch", "QQ 应用不匹配");
    const secret = this.store.secret(
      config.qq.credential_ref!,
      "qq_app_secret",
    );
    let payload: Json;
    try {
      payload = object(JSON.parse(raw.toString("utf8"))) as Json;
    } catch {
      throw bad("QQ 事件 JSON 无效");
    }
    if (payload.op === 13) {
      this.challengeAttempts = this.challengeAttempts.filter(
        (value) => value > Date.now() - 60000,
      );
      if (this.challengeAttempts.length >= 10)
        throw new ApiError(429, "rate_limited", "验证请求过于频繁");
      this.challengeAttempts.push(Date.now());
      const plain = string(payload.d?.plain_token, "验证内容", 256, 8),
        timestamp = string(payload.d?.event_ts, "验证时间", 20, 1);
      if (!/^[a-zA-Z0-9_+\/=\-]+$/.test(plain) || !/^\d+$/.test(timestamp))
        throw bad("QQ 验证请求无效");
      return {
        plain_token: plain,
        signature: qqSignature(secret, timestamp, Buffer.from(plain)),
      };
    }
    if (
      !verifyQq(
        secret,
        String(request.headers["x-signature-timestamp"] ?? ""),
        String(request.headers["x-signature-ed25519"] ?? ""),
        raw,
      )
    )
      throw new ApiError(403, "qq_signature_forbidden", "QQ 签名无效或已过期");
    if (payload.op === 0) this.ingestQq(config, payload);
    return { op: 12, d: 0 };
  }
  ingestQq(config: Config, payload: Json) {
    if (
      Number(
        this.store.get(
          "SELECT count(*) AS n FROM channel_events WHERE status='queued'",
        )!.n,
      ) >= 1000
    )
      throw new ApiError(
        503,
        "receive_queue_full",
        "持久接收队列已满，等待释放容量",
      );
    if (
      ![
        "C2C_MESSAGE_CREATE",
        "GROUP_AT_MESSAGE_CREATE",
        "AT_MESSAGE_CREATE",
        "DIRECT_MESSAGE_CREATE",
      ].includes(payload.t)
    )
      return;
    const data = object(payload.d) as Json;
    if (data.author?.bot === true) return;
    const message = id(data.id),
      event = id(payload.id ?? message);
    const type = (
      {
        C2C_MESSAGE_CREATE: "c2c",
        GROUP_AT_MESSAGE_CREATE: "group",
        AT_MESSAGE_CREATE: "channel",
        DIRECT_MESSAGE_CREATE: "dms",
      } as Json
    )[payload.t] as string;
    const target = qqId(
      type === "c2c"
        ? data.author?.user_openid
        : type === "group"
          ? data.group_openid
          : type === "dms"
            ? data.guild_id
            : data.channel_id,
    );
    const sender = qqId(data.author?.user_openid ?? data.author?.id);
    let content = string(data.content ?? "", "消息", 16000)
      .replace(/<@!?[^>]{1,200}>/g, "")
      .trim();
    if (data.attachments?.length)
      content +=
        "\n[宿主：本条包含媒体，媒体理解尚未完成时不能声称看到了画面或听到了声音。]";
    if (!content) return;
    const owner = `qq:${config.qq.app_id}:${type}:${target}${type === "c2c" ? "" : `:${sender}`}`;
    const eventKey = `qq:${config.qq.app_id}:${payload.t}:${event}`;
    const deadline = now() + (type === "c2c" || type === "group" ? 3600 : 300);
    const ready =
      Date.now() +
      (type === "c2c" &&
      !content.startsWith("/") &&
      !data.attachments?.length &&
      content.length < 360 &&
      !explicitCorrection(content)
        ? 3500
        : 0);
    this.store.run(
      "INSERT OR IGNORE INTO channel_events VALUES(?,?,?,?,?,?,?,?, 'queued',NULL,?,?,?,NULL)",
      randomUUID(),
      "qq",
      config.qq.app_id,
      eventKey,
      owner,
      owner,
      this.store.seal({
        content,
        target: {
          kind: type,
          target,
          message_id: message,
          account: config.qq.app_id,
          sender,
        },
        message_id: message,
      }),
      config.qq.credential_ref!,
      Date.now(),
      ready,
      deadline,
    );
  }
  private processEvents() {
    const config = this.store.config().config;
    for (const event of this.store.all(
      "SELECT * FROM channel_events WHERE status='queued' AND ready_at<=? ORDER BY received_at LIMIT 20",
      Date.now(),
    )) {
      if (
        this.store.get(
          "SELECT status FROM channel_events WHERE id=?",
          String(event.id),
        )?.status !== "queued"
      )
        continue;
      if (Number(event.deadline) <= now()) {
        this.store.run(
          "UPDATE channel_events SET status='expired',payload_json=NULL,error='被动回复窗口已过期' WHERE id=?",
          String(event.id),
        );
        continue;
      }
      const current =
        event.channel === "qq"
          ? config.qq.enabled &&
            config.qq.app_id === event.account_id &&
            config.qq.credential_ref === event.credential_ref
          : config.weixin.enabled &&
            this.wxState()?.account.account_id === event.account_id &&
            this.wxState()?.account.credential_ref === event.credential_ref;
      if (!current) {
        this.store.run(
          "UPDATE channel_events SET status='cancelled',payload_json=NULL,error='账户配置已变化' WHERE id=?",
          String(event.id),
        );
        continue;
      }
      const data = this.store.unseal<Json>(String(event.payload_json)),
        batch = [event];
      if (
        event.channel === "qq" &&
        data.target.kind === "c2c" &&
        !data.content.startsWith("/") &&
        data.content.length < 360 &&
        !explicitCorrection(data.content)
      ) {
        for (const next of this.store.all(
          "SELECT * FROM channel_events WHERE source_key=? AND status='queued' AND received_at>? AND received_at<=? ORDER BY received_at LIMIT 7",
          String(event.source_key),
          Number(event.received_at),
          Number(event.received_at) + 8000,
        )) {
          const part = this.store.unseal<Json>(String(next.payload_json));
          if (
            next.credential_ref !== event.credential_ref ||
            part.content.startsWith("/") ||
            explicitCorrection(part.content) ||
            part.content.length >= 360 ||
            part.content.includes("[宿主：")
          )
            break;
          if (
            Number(next.ready_at) > Date.now() &&
            Date.now() < Number(event.received_at) + 8000
          ) {
            this.store.run(
              "UPDATE channel_events SET ready_at=? WHERE id=?",
              Math.min(Number(next.ready_at), Number(event.received_at) + 8000),
              String(event.id),
            );
            break;
          }
          batch.push(next);
        }
        if (
          Number(
            this.store.get(
              "SELECT ready_at FROM channel_events WHERE id=?",
              String(event.id),
            )!.ready_at,
          ) > Date.now()
        )
          continue;
      }
      try {
        this.store.transaction(() => {
          let conversation = this.store.get(
            "SELECT id FROM conversations WHERE source_key=?",
            String(event.source_key),
          );
          if (!conversation) {
            const created = this.runtime.createConversation(
              String(event.owner_id),
              `${event.channel} ${data.target.kind}`,
            );
            this.store.run(
              "UPDATE conversations SET source_key=? WHERE id=?",
              String(event.source_key),
              created.id,
            );
            conversation = { id: created.id };
          }
          const texts = batch.map(
            (item) =>
              this.store.unseal<Json>(String(item.payload_json))
                .content as string,
          );
          const run = this.runtime.enqueue(
            String(conversation.id),
            String(event.owner_id),
            texts.join("\n"),
            String(event.event_key),
          );
          const last = this.store.unseal<Json>(
            String(batch.at(-1)!.payload_json),
          );
          this.store.run(
            "INSERT OR IGNORE INTO run_targets VALUES(?,?,?,?,?,?)",
            run.run_id,
            String(event.channel),
            this.store.seal(last.target),
            String(event.credential_ref),
            Number(event.deadline),
            String(event.account_id),
          );
          for (const item of batch)
            this.store.run(
              "UPDATE channel_events SET status='completed',run_id=?,payload_json=NULL WHERE id=?",
              run.run_id,
              String(item.id),
            );
          this.store.audit(
            String(event.owner_id),
            "delivery",
            "received",
            `已接收${batch.length}条渠道消息`,
            run.run_id,
          );
        });
      } catch (error) {
        if (!(
          error instanceof ApiError &&
          ["model_not_configured", "queue_full"].includes(error.code)
        ))
          this.store.run(
            "UPDATE channel_events SET status='failed',error='事件无法处理' WHERE id=?",
            String(event.id),
          );
      }
    }
  }
  private seedDeliveries() {
    for (const row of this.store.all(
      "SELECT r.id,r.response,r.owner_id FROM runs r JOIN run_targets t ON t.run_id=r.id WHERE r.status='completed' AND NOT EXISTS(SELECT 1 FROM delivery_parts p WHERE p.run_id=r.id) LIMIT 20",
    )) {
      let parts;
      try {
        parts = replyParts(String(row.response));
      } catch {
        parts = [
          { kind: "text", text: "这次回复格式没有整理好，请再给我一次机会。" },
        ];
      }
      this.store.transaction(() => {
        for (const [index, part] of parts.entries())
          this.store.run(
            "INSERT OR IGNORE INTO delivery_parts VALUES(?,?,?,?,?,'queued',NULL,NULL,?,?)",
            randomUUID(),
            String(row.id),
            index + 1,
            part.kind,
            part.text,
            Date.now() + index * 1000,
            now(),
          );
      });
    }
  }
  private async deliver() {
    if (this.deliveryBusy || this.stopping || Date.now() < this.accountNext)
      return;
    const row = this.store
      .all(
        "SELECT p.*,t.channel,t.target_json,t.credential_ref,t.deadline,t.account_id,r.owner_id,r.conversation_id,r.context_epoch,r.status AS run_status,c.context_epoch AS current_epoch FROM delivery_parts p JOIN run_targets t ON t.run_id=p.run_id JOIN runs r ON r.id=p.run_id JOIN conversations c ON c.id=r.conversation_id WHERE p.status='queued' AND p.not_before<=? AND NOT EXISTS(SELECT 1 FROM delivery_parts older WHERE older.run_id=p.run_id AND older.sequence<p.sequence AND older.status!='sent') ORDER BY p.rowid LIMIT 100",
        Date.now(),
      )
      .find((row) => !this.canDeliver || this.canDeliver(String(row.run_id)));
    if (!row) return;
    const target = this.store.unseal<Target>(String(row.target_json)),
      config = this.store.config().config,
      wx = this.wxState();
    const allowed =
      row.channel === "qq"
        ? config.qq.enabled &&
          config.qq.app_id === row.account_id &&
          config.qq.credential_ref === row.credential_ref
        : config.weixin.enabled &&
          wx?.account.account_id === row.account_id &&
          wx?.account.credential_ref === row.credential_ref;
    if (
      !allowed ||
      Number(row.deadline) <= now() ||
      row.run_status !== "completed" ||
      row.context_epoch !== row.current_epoch
    ) {
      this.store.run(
        "UPDATE delivery_parts SET status='cancelled',error='来源、账户或回复窗口已失效',updated_at=? WHERE id=?",
        now(),
        String(row.id),
      );
      return;
    }
    this.deliveryBusy = true;
    this.store.run(
      "UPDATE delivery_parts SET status='sending',updated_at=? WHERE id=?",
      now(),
      String(row.id),
    );
    try {
      let platformId: string;
      if (row.channel === "qq") {
        const token = await this.qqToken(config);
        const current = this.store.config().config;
        if (
          !current.qq.enabled ||
          current.qq.app_id !== row.account_id ||
          current.qq.credential_ref !== row.credential_ref ||
          this.store.get(
            "SELECT context_epoch FROM conversations WHERE id=?",
            String(row.conversation_id),
          )!.context_epoch !== row.context_epoch
        )
          throw new ApiError(409, "delivery_revoked", "发送授权已撤销");
        const path =
          target.kind === "c2c"
            ? `/v2/users/${target.target}/messages`
            : target.kind === "group"
              ? `/v2/groups/${target.target}/messages`
              : target.kind === "dms"
                ? `/dms/${target.target}/messages`
                : `/channels/${target.target}/messages`;
        const body = ["channel", "dms"].includes(target.kind)
          ? { content: row.content, msg_id: target.message_id }
          : {
              content: row.content,
              msg_type: 0,
              msg_id: target.message_id,
              msg_seq: row.sequence,
            };
        const value = business(
          await this.http(this.endpoints.qq, path, "POST", body, {
            Authorization: `QQBot ${token}`,
            "X-Union-Appid": config.qq.app_id,
          }),
        );
        platformId = id(value.id ?? value.message_id);
      } else {
        const account = wx!.account;
        const value = business(
          await this.http(
            this.weixinBase(account.base),
            "/ilink/bot/sendmessage",
            "POST",
            {
              msg: {
                from_user_id: "",
                to_user_id: target.target,
                client_id: String(row.id),
                message_type: 2,
                message_state: 2,
                context_token: target.context,
                item_list: [{ type: 1, text_item: { text: row.content } }],
              },
              base_info: {
                channel_version: "2.4.10",
                bot_agent: "Prometheus/0.1",
              },
            },
            this.wxHeaders(
              this.store.secret(account.credential_ref, "weixin_token"),
            ),
          ),
          true,
        );
        platformId = String(value.message_id ?? row.id);
      }
      this.store.run(
        "UPDATE delivery_parts SET status='sent',platform_id=?,updated_at=? WHERE id=?",
        platformId,
        now(),
        String(row.id),
      );
      this.store.audit(
        String(row.owner_id),
        "delivery",
        "sent",
        "平台已确认发送",
        String(row.run_id),
      );
    } catch (error) {
      const known =
        error instanceof ApiError &&
        ["channel_rejected", "delivery_revoked"].includes(error.code);
      this.store.run(
        "UPDATE delivery_parts SET status=?,error=?,updated_at=? WHERE id=?",
        known ? "failed" : "unknown",
        error instanceof ApiError && known
          ? error.message
          : "发送结果未确认，未自动重试",
        now(),
        String(row.id),
      );
      this.store.run(
        "UPDATE delivery_parts SET status='cancelled',error='前序投递未确认' WHERE run_id=? AND sequence>? AND status='queued'",
        String(row.run_id),
        Number(row.sequence),
      );
    } finally {
      this.deliveryBusy = false;
      this.accountNext = Date.now() + 500;
    }
  }
  private tick = () => {
    if (this.stopping) return;
    try {
      this.processEvents();
      this.seedDeliveries();
      this.track(this.deliver());
      this.synchronize();
    } catch {
      /* Pending durable entries remain available for inspection. */
    }
    this.timer = setTimeout(this.tick, 200);
    this.timer.unref();
  };
  private synchronize() {
    const state = this.store.config(),
      config = state.config;
    const fingerprint = `${state.revision}:${config.qq.enabled}:${config.qq.app_id}:${config.qq.credential_ref}:${config.qq.transport}`;
    if (this.gateway && this.gateway.fingerprint !== fingerprint) {
      this.gateway.abort.abort();
      return;
    }
    if (
      !this.gateway &&
      config.qq.enabled &&
      config.qq.transport === "websocket"
    ) {
      const abort = new AbortController();
      const task = this.track(
        this.gatewayLoop(
          config,
          state.revision,
          AbortSignal.any([abort.signal, this.controller.signal]),
        ).finally(() => {
          this.gateway = undefined;
        }),
      );
      this.gateway = { fingerprint, abort, task };
    } else if (!config.qq.enabled) this.connection.qq = "disabled";
    else if (config.qq.transport === "webhook") this.connection.qq = "webhook";
    const wx = this.wxState(),
      wxKey = `${config.weixin.enabled}:${wx?.generation}`;
    if (this.receiver && this.receiver.fingerprint !== wxKey) {
      this.receiver.abort.abort();
      return;
    }
    if (
      !this.receiver &&
      config.weixin.enabled &&
      wx &&
      wx.generation !== this.blockedWeixinGeneration
    ) {
      const abort = new AbortController(),
        task = this.track(
          this.weixinLoop(
            wx,
            AbortSignal.any([abort.signal, this.controller.signal]),
          ).finally(() => {
            this.receiver = undefined;
          }),
        );
      this.receiver = { fingerprint: wxKey, abort, task };
    } else if (!config.weixin.enabled) this.connection.weixin = "disabled";
  }
  private async pause(ms: number, signal: AbortSignal) {
    if (signal.aborted) return;
    await new Promise<void>((resolve) => {
      const done = () => {
        clearTimeout(timer);
        signal.removeEventListener("abort", done);
        resolve();
      };
      const timer = setTimeout(done, ms);
      signal.addEventListener("abort", done, { once: true });
    });
  }
  private async gatewayLoop(
    config: Config,
    revision: number,
    signal: AbortSignal,
  ) {
    let failures = 0,
      resume: { session: string; seq: number } | undefined;
    while (!signal.aborted) {
      try {
        this.connection.qq = "connecting";
        const token = await this.qqToken(config),
          gateway = business(
            await this.http(
              this.endpoints.qq,
              "/gateway",
              "GET",
              undefined,
              { Authorization: `QQBot ${token}` },
              signal,
            ),
          );
        const url = new URL(string(gateway.url, "网关", 2048, 1));
        if (
          !(
            this.allowLoopback &&
            ["ws:", "wss:"].includes(url.protocol) &&
            ["127.0.0.1", "[::1]"].includes(url.hostname)
          ) &&
          (url.protocol !== "wss:" ||
            url.username ||
            url.password ||
            !(
              url.hostname.endsWith(".bot.qq.com") ||
              url.hostname.endsWith(".sgroup.qq.com")
            ))
        )
          throw bad("QQ 网关端点不可信");
        await new Promise<void>((resolve, reject) => {
          const socket = new WebSocket(url, {
            maxPayload: 128 * 1024,
            handshakeTimeout: 15000,
            followRedirects: false,
            lookup: (host, options, callback) =>
              lookup(host, { all: true }, (error, addresses) => {
                if (error) return callback(error, []);
                if (
                  !addresses.length ||
                  addresses.some(
                    (item) => !allowedAddress(item.address, this.allowLoopback),
                  )
                )
                  return callback(new Error("protected_network"), []);
                if (options.all) callback(null, addresses);
                else
                  callback(null, addresses[0]!.address, addresses[0]!.family);
              }),
          });
          let heartbeat: ReturnType<typeof setInterval> | undefined,
            ack = true,
            hello = false;
          const stop = () => socket.terminate();
          signal.addEventListener("abort", stop, { once: true });
          const helloTimeout = setTimeout(() => socket.terminate(), 15000);
          socket.on("message", (data) => {
            try {
              const frame = object(JSON.parse(data.toString())) as Json;
              if (frame.op === 10) {
                if (hello) throw bad("重复握手");
                hello = true;
                clearTimeout(helloTimeout);
                const interval = Math.max(
                  1000,
                  Math.min(
                    120000,
                    Number(frame.d?.heartbeat_interval) || 45000,
                  ),
                );
                socket.send(
                  JSON.stringify(
                    resume
                      ? {
                          op: 6,
                          d: {
                            token: `QQBot ${token}`,
                            session_id: resume.session,
                            seq: resume.seq,
                          },
                        }
                      : {
                          op: 2,
                          d: {
                            token: `QQBot ${token}`,
                            intents: (1 << 25) | (1 << 30),
                            shard: [0, 1],
                            properties: {
                              $os: "windows",
                              $browser: "prometheus",
                              $device: "windows",
                            },
                          },
                        },
                  ),
                );
                heartbeat = setInterval(() => {
                  if (!ack) {
                    socket.terminate();
                    return;
                  }
                  ack = false;
                  socket.send(
                    JSON.stringify({ op: 1, d: resume?.seq ?? null }),
                  );
                }, interval);
              } else if (frame.op === 11) ack = true;
              else if (frame.op === 1)
                socket.send(JSON.stringify({ op: 1, d: resume?.seq ?? null }));
              else if (frame.op === 7) socket.close();
              else if (frame.op === 9) {
                if (!frame.d) resume = undefined;
                socket.close();
              } else if (frame.op === 0) {
                if (frame.t === "READY")
                  resume = {
                    session: id(frame.d?.session_id),
                    seq: Number(frame.s) || 0,
                  };
                if (frame.t === "READY" || frame.t === "RESUMED") {
                  this.connection.qq = "connected";
                  failures = 0;
                }
                this.ingestQq(config, frame);
                if (resume && Number.isSafeInteger(frame.s))
                  resume.seq = frame.s;
              }
            } catch {
              socket.terminate();
            }
          });
          socket.on("error", () => {});
          socket.on("close", (code) => {
            if (heartbeat) clearInterval(heartbeat);
            clearTimeout(helloTimeout);
            signal.removeEventListener("abort", stop);
            if (
              [4004, 4006, 4007, 4009].includes(code) ||
              (code >= 4900 && code <= 4913)
            ) {
              this.tokenCache = undefined;
              resume = undefined;
            }
            if ([4914, 4915].includes(code))
              reject(new ApiError(403, "qq_terminal", "QQ 平台要求人工处理"));
            else resolve();
          });
        });
      } catch (error) {
        if (error instanceof ApiError && error.code === "qq_terminal") {
          this.connection.qq = "blocked";
          while (!signal.aborted && this.store.config().revision === revision)
            await this.pause(1000, signal);
          continue;
        }
      }
      if (!signal.aborted) {
        this.connection.qq = "backoff";
        await this.pause(
          Math.min(60000, 1000 * 2 ** Math.min(++failures, 6)),
          signal,
        );
      }
    }
  }
  async createWeixinLogin(owner: string, expected: number) {
    const state = this.wxState();
    if ((state?.revision ?? 0) !== expected) throw conflict();
    if (
      this.store.get(
        "SELECT id FROM login_jobs WHERE status IN ('creating','wait','scaned','need_verifycode') AND expires_at>?",
        now(),
      )
    )
      throw new ApiError(409, "login_busy", "已有扫码登录进行中");
    const id = randomUUID();
    this.store.run(
      "INSERT INTO login_jobs VALUES(?,?,'weixin','creating',1,?,NULL,?,?,NULL)",
      id,
      owner,
      expected,
      now() + 300,
      now(),
    );
    this.track(
      (async () => {
        try {
          const qr = business(
            await this.http(
              this.endpoints.weixin,
              "/ilink/bot/get_bot_qrcode?bot_type=3",
              "POST",
              { local_token_list: [] },
              this.wxHeaders(),
            ),
          );
          const qrcode = string(qr.qrcode, "二维码凭据", 65536, 1),
            display = string(qr.qrcode_img_content, "二维码内容", 65536, 1);
          this.store.run(
            "UPDATE login_jobs SET status='wait',secret_json=?,revision=revision+1 WHERE id=? AND status='creating'",
            this.store.seal({
              qrcode,
              display,
              base: this.endpoints.weixin,
              redirects: 0,
            }),
            id,
          );
          await this.pollLogin(id);
        } catch {
          this.store.run(
            "UPDATE login_jobs SET status='failed',secret_json=NULL,error='扫码连接未完成',revision=revision+1 WHERE id=? AND status NOT IN ('confirmed','cancelled')",
            id,
          );
        }
      })(),
    );
    return { id, status: "creating", revision: 1, expires_at: now() + 300 };
  }
  loginState(id: string, owner: string) {
    const job = this.store.get(
      "SELECT * FROM login_jobs WHERE id=? AND owner_id=?",
      id,
      owner,
    );
    if (!job) throw missing();
    if (
      Number(job.expires_at) <= now() &&
      !["confirmed", "cancelled", "failed"].includes(String(job.status))
    )
      this.store.run(
        "UPDATE login_jobs SET status='expired',secret_json=NULL WHERE id=?",
        id,
      );
    const current = this.store.get("SELECT * FROM login_jobs WHERE id=?", id)!;
    const privateState = current.secret_json
      ? this.store.unseal<Json>(String(current.secret_json))
      : {};
    return {
      id,
      status: current.status,
      revision: current.revision,
      expires_at: current.expires_at,
      display_content: privateState.display ?? null,
      error: current.error,
    };
  }
  cancelLogin(id: string, owner: string, expected: number) {
    const state = this.loginState(id, owner);
    if (state.revision !== expected) throw conflict();
    this.store.run(
      "UPDATE login_jobs SET status='cancelled',secret_json=NULL,revision=revision+1 WHERE id=?",
      id,
    );
    return this.loginState(id, owner);
  }
  verifyLogin(id: string, owner: string, expected: number, code: string) {
    const state = this.loginState(id, owner);
    if (state.revision !== expected || state.status !== "need_verifycode")
      throw conflict();
    if (!/^\d{1,16}$/.test(code)) throw bad("验证码无效");
    const row = this.store.get(
      "SELECT secret_json FROM login_jobs WHERE id=?",
      id,
    )!;
    const value = this.store.unseal<Json>(String(row.secret_json));
    value.verification = code;
    this.store.run(
      "UPDATE login_jobs SET secret_json=?,status='wait',revision=revision+1 WHERE id=?",
      this.store.seal(value),
      id,
    );
    return this.loginState(id, owner);
  }
  private async pollLogin(id: string) {
    while (!this.stopping) {
      const row = this.store.get("SELECT * FROM login_jobs WHERE id=?", id);
      if (
        !row ||
        ["confirmed", "failed", "cancelled", "expired"].includes(
          String(row.status),
        )
      )
        return;
      if (Number(row.expires_at) <= now()) {
        this.store.run(
          "UPDATE login_jobs SET status='expired',secret_json=NULL WHERE id=?",
          id,
        );
        return;
      }
      if (row.status === "need_verifycode") {
        await this.pause(500, this.controller.signal);
        continue;
      }
      const value = this.store.unseal<Json>(String(row.secret_json));
      const query = new URLSearchParams({
        qrcode: value.qrcode,
        ...(value.verification ? { verify_code: value.verification } : {}),
      });
      let result;
      try {
        result = business(
          await this.http(
            this.weixinBase(value.base),
            `/ilink/bot/get_qrcode_status?${query}`,
            "GET",
            undefined,
            {},
            this.controller.signal,
            35000,
          ),
        );
      } catch {
        await this.pause(1000, this.controller.signal);
        continue;
      }
      const latest = this.store.get(
        "SELECT status,revision FROM login_jobs WHERE id=?",
        id,
      )!;
      if (latest.revision !== row.revision || latest.status === "cancelled")
        continue;
      if (result.status === "confirmed") {
        const account = idValue(result.ilink_bot_id),
          owner = idValue(result.ilink_user_id),
          base = this.weixinBase(string(result.baseurl, "微信端点", 1024, 1));
        this.store.transaction(() => {
          if ((this.wxState()?.revision ?? 0) !== row.account_revision)
            throw conflict();
          const credential = this.store.addSecret(
            "weixin_token",
            string(result.bot_token, "微信凭据", 8192, 1),
            String(row.owner_id),
          );
          this.store.run(
            "INSERT INTO channel_state VALUES('weixin',?,?,?,?) ON CONFLICT(channel) DO UPDATE SET revision=excluded.revision,generation=excluded.generation,value_json=excluded.value_json,updated_at=excluded.updated_at",
            Number(row.account_revision) + 1,
            randomUUID(),
            this.store.seal({
              account_id: account,
              owner_id: owner,
              base,
              credential_ref: credential.id,
              cursor: "",
            }),
            now(),
          );
          this.store.run(
            "UPDATE login_jobs SET status='confirmed',secret_json=NULL,revision=revision+1 WHERE id=?",
            id,
          );
        });
        return;
      }
      if (result.status === "scaned_but_redirect") {
        if (Number(value.redirects) >= 2) throw bad("微信重定向次数超限");
        value.base = this.weixinBase(
          `https://${idValue(result.redirect_host)}`,
        );
        value.redirects++;
        this.store.run(
          "UPDATE login_jobs SET secret_json=?,revision=revision+1 WHERE id=?",
          this.store.seal(value),
          id,
        );
      } else if (
        ["expired", "verify_code_blocked", "binded_redirect"].includes(
          result.status,
        )
      ) {
        this.store.run(
          "UPDATE login_jobs SET status=?,secret_json=NULL,revision=revision+1 WHERE id=?",
          result.status === "expired" ? "expired" : "failed",
          id,
        );
        return;
      } else if (["wait", "scaned", "need_verifycode"].includes(result.status))
        this.store.run(
          "UPDATE login_jobs SET status=?,revision=revision+1 WHERE id=?",
          result.status,
          id,
        );
      else throw bad("微信扫码状态无效");
      await this.pause(300, this.controller.signal);
    }
  }
  private async weixinLoop(
    state: { revision: number; generation: string; account: Json },
    signal: AbortSignal,
  ) {
    const account = state.account;
    let cursor = String(account.cursor ?? "");
    while (!signal.aborted) {
      try {
        this.connection.weixin = "polling";
        const result = business(
          await this.http(
            this.weixinBase(account.base),
            "/ilink/bot/getupdates",
            "POST",
            {
              get_updates_buf: cursor,
              base_info: {
                channel_version: "2.4.10",
                bot_agent: "Prometheus/0.1",
              },
            },
            this.wxHeaders(
              this.store.secret(account.credential_ref, "weixin_token"),
            ),
            signal,
            35000,
          ),
          true,
        );
        if (
          !Array.isArray(result.msgs ?? []) ||
          (result.msgs ?? []).length > 100
        )
          throw bad("微信消息批次超限");
        this.store.transaction(() => {
          if (this.wxState()?.generation !== state.generation) throw conflict();
          for (const message of result.msgs ?? [])
            this.ingestWeixin(account, message);
          cursor = string(result.get_updates_buf ?? cursor, "微信游标", 65536);
          account.cursor = cursor;
          this.store.run(
            "UPDATE channel_state SET value_json=?,updated_at=? WHERE channel='weixin' AND generation=?",
            this.store.seal(account),
            now(),
            state.generation,
          );
        });
        this.connection.weixin = "connected";
        await this.pause(200, signal);
      } catch (error) {
        if (error instanceof ApiError && /-14/.test(error.message)) {
          this.connection.weixin = "credentials_expired";
          this.blockedWeixinGeneration = state.generation;
          return;
        }
        this.connection.weixin = "backoff";
        await this.pause(3000, signal);
      }
    }
  }
  ingestWeixin(account: Json, value: Json) {
    if (
      Number(
        this.store.get(
          "SELECT count(*) AS n FROM channel_events WHERE status='queued'",
        )!.n,
      ) >= 1000
    )
      throw new ApiError(503, "receive_queue_full", "持久接收队列已满");
    if (
      value.message_type !== 1 ||
      value.message_state !== 2 ||
      value.group_id ||
      value.delete_time_ms ||
      value.from_user_id !== account.owner_id ||
      value.to_user_id !== account.account_id
    )
      return;
    if (
      !Array.isArray(value.item_list) ||
      value.item_list.some(
        (item: Json) =>
          item.type !== 1 || typeof item.text_item?.text !== "string",
      )
    )
      return;
    const message = idValue(
        value.message_id ??
          value.item_list.find((item: Json) => item.msg_id)?.msg_id,
      ),
      sender = idValue(value.from_user_id),
      context = string(value.context_token, "微信回复上下文", 65536, 1);
    const content = string(
      value.item_list.map((item: Json) => item.text_item.text).join("\n"),
      "微信文字",
      16000,
      1,
    );
    if (content.includes("\0")) return;
    const owner = `weixin:${account.account_id}:${sender}`;
    this.store.run(
      "INSERT OR IGNORE INTO channel_events VALUES(?,?,?,?,?,?,?,?, 'queued',NULL,?,?,?,NULL)",
      randomUUID(),
      "weixin",
      account.account_id,
      `weixin:${account.account_id}:${message}`,
      owner,
      owner,
      this.store.seal({
        content,
        target: {
          kind: "c2c",
          target: sender,
          sender,
          message_id: message,
          account: account.account_id,
          context,
        },
      }),
      account.credential_ref,
      Date.now(),
      Date.now(),
      now() + 3600,
    );
  }
  deliveries(owner: string) {
    return this.store
      .all(
        "SELECT p.id,p.run_id,p.sequence,p.kind,p.status,p.error,p.platform_id,p.updated_at,t.channel FROM delivery_parts p JOIN run_targets t ON t.run_id=p.run_id ORDER BY p.rowid DESC LIMIT 100",
      )
      .map((row) => ({ ...row, operator: owner }));
  }
  catalog() {
    const catalog = JSON.parse(requireAsset("qq-api-catalog.json"));
    return {
      ...catalog,
      model_tool_access: false,
      group_apis: false,
      operations: catalog.operations.map((operation: Json) => ({
        ...operation,
        live_status: "not_executed_on_windows",
        historical_live_status: undefined,
        implemented: true,
        platform_verified: false,
      })),
    };
  }
  async operate(owner: string, operationId: string, value: unknown) {
    const operation = this.catalog().operations.find(
      (entry: Json) => entry.id === operationId,
    );
    if (!operation) throw missing();
    const input = fields(value, [
        "parameters",
        "query",
        "body",
        "dry_run",
        "acknowledge_effects",
        "request_id",
      ]),
      parameters = object(input.parameters ?? {}),
      query = object(input.query ?? {});
    fields(parameters, operation.parameters);
    fields(query, operation.query_fields);
    let path = String(operation.path);
    for (const name of operation.parameters) {
      const value = string(parameters[name], name, 200, 1);
      if (!/^[a-zA-Z0-9_@.\-]+$/.test(value) || value.includes(".."))
        throw bad("路径参数无效");
      path = path.replace(`{${name}}`, encodeURIComponent(value));
    }
    const params = new URLSearchParams(operation.query_defaults ?? {});
    for (const [key, value] of Object.entries(query)) {
      if (
        !["string", "number", "boolean"].includes(typeof value) ||
        String(value).length > 1024
      )
        throw bad("查询参数无效");
      params.set(key, String(value));
    }
    for (const required of operation.required_query_fields ?? [])
      if (!params.get(required)) throw bad(`缺少查询字段 ${required}`);
    const body =
      input.body === undefined
        ? undefined
        : fields(input.body, operation.body_fields);
    for (const required of operation.required_body_fields ?? [])
      if (body?.[required] === undefined || body[required] === null)
        throw bad(`缺少请求字段 ${required}`);
    if (params.size) path += `?${params}`;
    if (input.dry_run === true)
      return {
        operation_id: operationId,
        method: operation.method,
        path,
        body_fields: body ? Object.keys(body) : [],
        dispatched: false,
        platform_verified: false,
      };
    if (!operation.read_only && input.acknowledge_effects !== true)
      throw bad("实际变更需要操作者明确确认影响");
    const requestId = operation.read_only
        ? String(input.request_id ?? randomUUID())
        : string(input.request_id, "变更幂等标识", 128, 1),
      fingerprint = hash(JSON.stringify({ operationId, path, body }));
    const previous = this.store.get(
      "SELECT * FROM operator_calls WHERE id=?",
      requestId,
    );
    if (previous) {
      if (previous.owner_id !== owner || previous.fingerprint !== fingerprint)
        throw conflict();
      return previous.result_json
        ? JSON.parse(String(previous.result_json))
        : {
            outcome: previous.outcome,
            dispatched: true,
            automatic_retry: false,
          };
    }
    const config = this.store.config().config;
    if (!config.qq.credential_ref) throw bad("请先配置QQ凭据");
    this.store.run(
      "INSERT INTO operator_calls VALUES(?,?,?,?,'requesting',NULL,?)",
      requestId,
      owner,
      operationId,
      fingerprint,
      now(),
    );
    try {
      const token = await this.qqToken(config),
        result = business(
          await this.http(this.endpoints.qq, path, operation.method, body, {
            Authorization: `QQBot ${token}`,
            "X-Union-Appid": config.qq.app_id,
          }),
        );
      const safe = JSON.parse(this.store.redact(JSON.stringify(scrub(result))));
      const response = {
        operation_id: operationId,
        outcome: "confirmed",
        dispatched: true,
        result: safe,
      };
      this.store.run(
        "UPDATE operator_calls SET outcome='confirmed',result_json=? WHERE id=?",
        JSON.stringify(response),
        requestId,
      );
      this.store.audit(
        owner,
        "qq",
        "completed",
        "操作者接口调用获得回执",
        operationId,
      );
      return response;
    } catch (error) {
      const outcome =
        error instanceof ApiError && error.code === "channel_rejected"
          ? "rejected"
          : "unknown";
      this.store.run(
        "UPDATE operator_calls SET outcome=? WHERE id=?",
        outcome,
        requestId,
      );
      return {
        operation_id: operationId,
        outcome,
        dispatched: true,
        automatic_retry: false,
      };
    }
  }
}
function scrub(value: unknown): unknown {
  if (Array.isArray(value)) return value.slice(0, 100).map(scrub);
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value)
        .filter(
          ([key]) =>
            !/token|secret|password|authorization|url|file_info|sign/i.test(
              key,
            ),
        )
        .map(([key, value]) => [key, scrub(value)]),
    );
  return value;
}
function idValue(value: unknown): string {
  const result = id(value);
  if (/[\s\x00-\x1f]/.test(result)) throw bad("微信标识无效");
  return result;
}
import { readFileSync } from "node:fs";
const requireAsset = (name: string) =>
  readFileSync(new URL(`./assets/${name}`, import.meta.url), "utf8");
