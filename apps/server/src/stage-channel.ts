import { randomBytes, randomUUID } from "node:crypto";
import { ApiError } from "./errors.ts";
import { hash, now } from "./security.ts";
import type { Store } from "./store.ts";

const qqLinkPrefix = "stage:qq:";
const challengeLifetime = 10 * 60_000;

export type StageChannel = {
  channel: "qq" | "weixin";
  sourceKey: string;
  account: string;
  credential: string;
  version: string;
  generation?: string;
};

type Challenge = {
  actor: string;
  account: string;
  credential: string;
  expiresAt: number;
};

export class StageChannelAccess {
  private readonly pending = new Map<string, Challenge>();

  constructor(private readonly store: Store) {}

  private qqConfig() {
    const qq = this.store.config().config.qq;
    return qq.enabled && qq.app_id && qq.credential_ref
      ? { account: qq.app_id, credential: qq.credential_ref }
      : undefined;
  }

  private qq(actor: string): StageChannel | undefined {
    const config = this.qqConfig();
    if (!config) return;
    const row = this.store.get(
      "SELECT value_json FROM feature_policies WHERE name=?",
      qqLinkPrefix + actor,
    );
    if (!row) return;
    const link = JSON.parse(String(row.value_json)) as {
      sourceKey: string;
      account: string;
      credential: string;
      version: string;
    };
    if (
      link.account === config.account &&
      link.credential === config.credential
    )
      return { channel: "qq", ...link };
  }

  private weixin(): (StageChannel & { actor: string }) | undefined {
    if (!this.store.config().config.weixin.enabled) return;
    const state = this.store.get(
      `SELECT s.generation,s.value_json,j.owner_id AS actor
       FROM channel_state s JOIN login_jobs j ON j.channel='weixin'
         AND j.status='confirmed' AND j.account_revision=s.revision-1
       JOIN admins a ON a.id=j.owner_id WHERE s.channel='weixin'`,
    );
    if (!state) return;
    const value = this.store.unseal<{
      account_id: string;
      owner_id: string;
      credential_ref: string;
    }>(String(state.value_json));
    const generation = String(state.generation);
    return {
      channel: "weixin",
      actor: String(state.actor),
      sourceKey: `weixin:${value.account_id}:${value.owner_id}`,
      account: value.account_id,
      credential: value.credential_ref,
      generation,
      version: generation,
    };
  }

  channels(actor: string): StageChannel[] {
    const qq = this.qq(actor),
      weixin = this.weixin();
    return [...(qq ? [qq] : []), ...(weixin?.actor === actor ? [weixin] : [])];
  }

  private clearChallenges(actor?: string) {
    const time = Date.now();
    for (const [key, challenge] of this.pending)
      if (challenge.expiresAt <= time || challenge.actor === actor)
        this.pending.delete(key);
  }

  qqStatus(actor: string) {
    this.clearChallenges();
    const config = this.qqConfig();
    return {
      configured: !!config,
      linked: !!this.qq(actor),
      pending:
        !!config &&
        Array.from(this.pending.values()).some(
          (entry) =>
            entry.actor === actor &&
            entry.account === config.account &&
            entry.credential === config.credential,
        ),
    };
  }

  createQqChallenge(actor: string) {
    const config = this.qqConfig();
    if (!config)
      throw new ApiError(409, "qq_not_configured", "请先启用 QQ 渠道");
    this.clearChallenges(actor);
    if (this.pending.size >= 100)
      throw new ApiError(
        429,
        "stage_link_busy",
        "等待中的舞台绑定过多，请稍后再试",
      );
    const code = randomBytes(10).toString("hex");
    const expiresAt = Date.now() + challengeLifetime;
    this.pending.set(hash(code), { ...config, actor, expiresAt });
    return { command: `/舞台绑定 ${code}`, expiresAt };
  }

  revokeQq(actor: string) {
    this.clearChallenges(actor);
    this.store.run(
      "DELETE FROM feature_policies WHERE name=?",
      qqLinkPrefix + actor,
    );
    return { ok: true };
  }

  linkQq(sourceKey: string, account: string, credential: string, code: string) {
    this.clearChallenges();
    const digest = hash(code),
      challenge = this.pending.get(digest),
      config = this.qqConfig();
    if (
      !challenge ||
      !config ||
      challenge.account !== account ||
      challenge.credential !== credential ||
      config.account !== account ||
      config.credential !== credential ||
      !sourceKey.startsWith(`qq:${account}:c2c:`) ||
      !this.store.get("SELECT id FROM admins WHERE id=?", challenge.actor)
    )
      return;
    const name = qqLinkPrefix + challenge.actor;
    this.store.transaction(() => {
      // A signed private chat can have only one stage owner at a time.
      this.store.run(
        "DELETE FROM feature_policies WHERE name GLOB 'stage:qq:*' AND name<>? AND json_extract(value_json,'$.sourceKey')=?",
        name,
        sourceKey,
      );
      this.store.run(
        `INSERT INTO feature_policies VALUES(?,1,?,?) ON CONFLICT(name) DO UPDATE SET
         revision=feature_policies.revision+1,value_json=excluded.value_json,updated_at=excluded.updated_at`,
        name,
        JSON.stringify({
          sourceKey,
          account,
          credential,
          version: randomUUID(),
        }),
        now(),
      );
    });
    this.pending.delete(digest);
  }

  grantForRun(runId: string): string | undefined {
    const run = this.store.get(
      `SELECT c.source_key,t.channel,t.account_id,t.credential_ref,t.target_json
       FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id
       JOIN run_targets t ON t.run_id=r.id WHERE r.id=? AND c.source_key=c.owner_id
       AND EXISTS(SELECT 1 FROM channel_events e WHERE e.run_id=r.id AND e.source_key=c.source_key
         AND e.owner_id=r.owner_id AND e.channel=t.channel AND e.account_id=t.account_id
         AND e.credential_ref=t.credential_ref AND e.status='completed')`,
      runId,
    );
    if (!run) return;
    const sourceKey = String(run.source_key);
    const link =
      run.channel === "qq"
        ? this.store.get(
            `SELECT a.id FROM feature_policies p JOIN admins a ON p.name=?||a.id
           WHERE json_extract(p.value_json,'$.sourceKey')=?`,
            qqLinkPrefix,
            sourceKey,
          )
        : undefined;
    const scope =
      run.channel === "qq" && link ? this.qq(String(link.id)) : this.weixin();
    if (
      !scope ||
      scope.channel !== run.channel ||
      scope.sourceKey !== sourceKey ||
      scope.account !== run.account_id ||
      scope.credential !== run.credential_ref
    )
      return;
    const target = this.store.unseal<{
      kind: string;
      account: string;
      sender: string;
      target: string;
      generation?: string;
    }>(String(run.target_json));
    const prefix =
      scope.channel === "qq"
        ? `qq:${scope.account}:c2c:`
        : `weixin:${scope.account}:`;
    if (
      target.kind === "c2c" &&
      target.account === scope.account &&
      target.sender === target.target &&
      sourceKey === `${prefix}${target.sender}` &&
      (!scope.generation || target.generation === scope.generation)
    )
      return scope.version;
  }
}
