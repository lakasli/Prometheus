import { missing } from "./errors.ts";
import type { StageExpressionId } from "./stage-expression.ts";
import type { Store } from "./store.ts";

const bubbleLifetime = 12_000;
const expressionLifetime = 5_000;
const maxActions = 256;

type Action = { id: StageExpressionId; expiresAt: number };

export class StageFeed {
  private readonly actions = new Map<string, Action>();

  constructor(private readonly store: Store) {}

  publish(runId: string, id: StageExpressionId): void {
    const currentTime = Date.now();
    for (const [key, action] of this.actions)
      if (action.expiresAt <= currentTime) this.actions.delete(key);
    if (this.actions.size >= maxActions)
      this.actions.delete(this.actions.keys().next().value!);
    this.actions.set(runId, {
      id,
      expiresAt: currentTime + expressionLifetime,
    });
  }

  conversations(owner: string) {
    const redact = this.store.redactor();
    return {
      items: this.store
        .all(
          "SELECT id,title FROM conversations WHERE owner_id=? AND source_key IS NULL ORDER BY rowid DESC LIMIT 100",
          owner,
        )
        .map((row) => ({
          id: String(row.id),
          title: redact(String(row.title)),
        })),
    };
  }

  read(conversation: string, owner: string, replay: boolean) {
    if (
      !this.store.get(
        "SELECT id FROM conversations WHERE id=? AND owner_id=? AND source_key IS NULL",
        conversation,
        owner,
      )
    )
      throw missing();

    const currentTime = Date.now();
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
    const redact = rows.length ? this.store.redactor() : (text: string) => text;
    return {
      now: currentTime,
      items: rows.reverse().map((row) => {
        const createdAt = Number(row.created_at) * 1_000;
        const text = Array.from(redact(String(row.content)).trim());
        const expression =
          row.role === "assistant"
            ? this.actions.get(String(row.run_id))
            : undefined;
        return {
          id: String(row.id),
          role: row.role as "user" | "assistant",
          text:
            text.length > 400
              ? `${text.slice(0, 400).join("")}…`
              : text.join(""),
          createdAt,
          expiresAt: (replay ? currentTime : createdAt) + bubbleLifetime,
          ...(expression && expression.expiresAt > currentTime
            ? { expression }
            : {}),
        };
      }),
    };
  }
}
