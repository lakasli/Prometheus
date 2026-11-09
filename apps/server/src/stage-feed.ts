import { channelFallbackText } from "./channels.ts";
import { missing } from "./errors.ts";
import type { StageChannel, StageChannelAccess } from "./stage-channel.ts";
import type { StageExpressionId } from "./stage-expression.ts";
import type { Store } from "./store.ts";

const bubbleLifetime = 12_000;
const expressionLifetime = 5_000;
const maxActions = 256;

type Action = { id: StageExpressionId; expiresAt: number; grant?: string };
type Bubble = {
  id: string;
  runId?: string;
  role: "user" | "assistant";
  text: string;
  createdAt: number;
  firstText?: boolean;
  confirmedAt?: number;
};

export class StageFeed {
  private readonly actions = new Map<string, Action>();

  constructor(
    private readonly store: Store,
    private readonly channelAccess: StageChannelAccess,
  ) {}

  publish(runId: string, id: StageExpressionId, grant?: string): void {
    const currentTime = Date.now();
    for (const [key, action] of this.actions)
      if (action.expiresAt <= currentTime) this.actions.delete(key);
    if (this.actions.size >= maxActions)
      this.actions.delete(this.actions.keys().next().value!);
    this.actions.set(runId, {
      id,
      expiresAt: currentTime + (grant ? 3_600_000 : expressionLifetime),
      grant,
    });
  }

  conversations(owner: string) {
    const redact = this.store.redactor();
    return {
      items: [
        ...this.store
          .all(
            "SELECT id,title FROM conversations WHERE owner_id=? AND source_key IS NULL ORDER BY rowid DESC LIMIT 100",
            owner,
          )
          .map((row) => ({
            id: String(row.id),
            title: redact(String(row.title)),
            channel: "native",
          })),
        ...this.channelAccess.channels(owner).flatMap((scope) => {
          const conversation = this.store.get(
            "SELECT id FROM conversations WHERE source_key=? AND owner_id=?",
            scope.sourceKey,
            scope.sourceKey,
          );
          return conversation
            ? [
                {
                  id: String(conversation.id),
                  title: scope.channel === "qq" ? "QQ 私聊" : "微信私聊",
                  channel: scope.channel,
                },
              ]
            : [];
        }),
      ],
    };
  }

  read(conversation: string, owner: string, replay: boolean) {
    const source = this.store.get(
      "SELECT owner_id,source_key FROM conversations WHERE id=?",
      conversation,
    );
    if (!source) throw missing();
    const currentTime = Date.now();
    if (source.source_key !== null) {
      const scope = this.channelAccess
        .channels(owner)
        .find(
          (item) =>
            item.sourceKey === source.source_key &&
            item.sourceKey === source.owner_id,
        );
      if (!scope) throw missing();
      return this.channelFeed(conversation, scope, replay, currentTime);
    }
    if (source.owner_id !== owner) throw missing();
    const earliest = replay
      ? 0
      : Math.floor((currentTime - bubbleLifetime) / 1_000) + 1;
    const rows = this.store.all(
      `SELECT m.id,m.run_id,m.role,m.content,m.created_at
       FROM runs r JOIN messages m ON m.run_id=r.id AND m.conversation_id=r.conversation_id
       WHERE r.conversation_id=? AND r.owner_id=? AND m.created_at>=?
         AND NOT EXISTS(SELECT 1 FROM local_runs l WHERE l.run_id=r.id)
         AND NOT EXISTS(SELECT 1 FROM automatic_runs a WHERE a.run_id=r.id)
         AND NOT EXISTS(SELECT 1 FROM run_targets t WHERE t.run_id=r.id)
         AND (m.role='user' OR r.status='completed' AND NOT EXISTS(
           SELECT 1 FROM run_publications p WHERE p.run_id=r.id AND p.state='suppressed'))
       ORDER BY m.rowid DESC LIMIT ?`,
      conversation,
      owner,
      earliest,
      replay ? 4 : 8,
    );
    return this.present(
      rows.reverse().map((row) => ({
        id: String(row.id),
        runId: String(row.run_id),
        role: row.role as "user" | "assistant",
        text: String(row.content),
        createdAt: Number(row.created_at) * 1_000,
      })),
      replay,
      currentTime,
    );
  }

  private channelFeed(
    conversation: string,
    scope: StageChannel,
    replay: boolean,
    currentTime: number,
  ) {
    const firstLive = currentTime - bubbleLifetime;
    const pending = this.store
      .all(
        `SELECT id,received_at,payload_json FROM channel_events
       WHERE source_key=? AND owner_id=? AND channel=? AND account_id=? AND credential_ref=?
         AND received_at>? AND status IN ('queued','processing') AND payload_json IS NOT NULL
       ORDER BY received_at DESC,rowid DESC LIMIT 8`,
        scope.sourceKey,
        scope.sourceKey,
        scope.channel,
        scope.account,
        scope.credential,
        firstLive,
      )
      .flatMap((row): Bubble[] => {
        const event = this.store.unseal<{ content?: string; media?: unknown }>(
          String(row.payload_json),
        );
        return !event.media && event.content
          ? [
              {
                id: String(row.id),
                role: "user",
                text: event.content,
                createdAt: Number(row.received_at),
              },
            ]
          : [];
      });
    const recorded = this.store
      .all(
        `WITH sources AS (
         SELECT e.id,e.run_id,e.received_at,
           MAX(e.received_at) OVER(PARTITION BY e.run_id) AS last_received_at,
           ROW_NUMBER() OVER(PARTITION BY e.run_id ORDER BY e.received_at,e.rowid) AS position
         FROM channel_events e
         WHERE e.source_key=? AND e.owner_id=? AND e.channel=? AND e.account_id=?
           AND e.credential_ref=? AND e.status='completed' AND e.run_id IS NOT NULL
       ), eligible AS (
         SELECT r.id,s.id AS event_id,s.received_at,s.last_received_at
         FROM runs r JOIN run_targets t ON t.run_id=r.id
         JOIN sources s ON s.run_id=r.id AND s.position=1
         WHERE r.conversation_id=? AND r.owner_id=? AND t.channel=? AND t.account_id=? AND t.credential_ref=?
           AND NOT EXISTS(SELECT 1 FROM local_runs l WHERE l.run_id=r.id)
           AND NOT EXISTS(SELECT 1 FROM automatic_runs a WHERE a.run_id=r.id)
       ), visible AS (
         SELECT e.event_id AS id,e.id AS run_id,'user' AS role,m.content,e.received_at AS timestamp,
           0 AS first_text,0 AS sequence,NULL AS confirmed_at
         FROM eligible e JOIN messages m ON m.run_id=e.id AND m.role='user'
         WHERE NOT EXISTS(SELECT 1 FROM channel_media_sources q WHERE q.run_id=e.id)
         UNION ALL
         SELECT p.id,e.id,'assistant',p.content,max(p.updated_at*1000,e.last_received_at+1),
           NOT EXISTS(SELECT 1 FROM delivery_parts older WHERE older.run_id=p.run_id
             AND older.kind='text' AND older.status='sent' AND older.sequence<p.sequence),
           p.sequence,p.updated_at*1000
         FROM eligible e JOIN delivery_parts p ON p.run_id=e.id
         WHERE p.kind='text' AND p.status='sent'
       ) SELECT * FROM visible WHERE timestamp>? ORDER BY timestamp DESC, sequence DESC, id DESC LIMIT ?`,
        scope.sourceKey,
        scope.sourceKey,
        scope.channel,
        scope.account,
        scope.credential,
        conversation,
        scope.sourceKey,
        scope.channel,
        scope.account,
        scope.credential,
        replay ? -1 : firstLive,
        replay ? 4 : 8,
      )
      .reverse()
      .map((row): Bubble => ({
        id: String(row.id),
        runId: String(row.run_id),
        role: row.role as "user" | "assistant",
        text: String(row.content),
        createdAt: Number(row.timestamp),
        firstText: !!row.first_text,
        confirmedAt:
          row.confirmed_at === null ? undefined : Number(row.confirmed_at),
      }));
    return this.present(
      [...pending.reverse(), ...recorded]
        .sort((a, b) => a.createdAt - b.createdAt)
        .slice(replay ? -4 : -8),
      replay,
      currentTime,
      scope.version,
    );
  }

  private present(
    rows: Bubble[],
    replay: boolean,
    currentTime: number,
    grant?: string,
  ) {
    const redact = rows.length ? this.store.redactor() : (text: string) => text;
    return {
      now: currentTime,
      items: rows.map((row) => {
        const { id, role, createdAt } = row;
        const text = Array.from(redact(row.text).trim());
        const action =
          role === "assistant" && row.runId
            ? this.actions.get(row.runId)
            : undefined;
        const expressionExpires = grant
          ? (row.confirmedAt ?? 0) + expressionLifetime
          : (action?.expiresAt ?? 0);
        const expression =
          action &&
          action.grant === grant &&
          action.expiresAt > currentTime &&
          expressionExpires > currentTime &&
          (!grant || (row.firstText && !channelFallbackText(row.text)))
            ? { id: action.id, expiresAt: expressionExpires }
            : undefined;
        return {
          id,
          role,
          text:
            text.length > 400
              ? `${text.slice(0, 400).join("")}…`
              : text.join(""),
          createdAt,
          expiresAt: (replay ? currentTime : createdAt) + bubbleLifetime,
          ...(expression ? { expression } : {}),
        };
      }),
    };
  }
}
