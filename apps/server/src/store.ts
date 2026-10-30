import {
  randomUUID,
  createCipheriv,
  createDecipheriv,
  randomBytes,
} from "node:crypto";
import { existsSync, readFileSync, writeFileSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import type { SQLInputValue } from "node:sqlite";
import { defaultConfig, validatePersonality } from "./config.ts";
import type { Config, Personality } from "./config.ts";
import { ApiError, bad, conflict, missing } from "./errors.ts";
import { now, privateDirectory } from "./security.ts";

export type Row = Record<string, string | number | null>;
export class Store {
  readonly database: DatabaseSync;
  readonly dataDir: string;
  readonly assistantId: string;
  private readonly key: Buffer;
  constructor(path: string) {
    this.dataDir = privateDirectory(path);
    if (existsSync(join(path, "dsh.db")))
      throw new Error(
        "Legacy DSH data is read-only for this rewrite; use a new Prometheus data directory",
      );
    const secrets = privateDirectory(join(path, "secrets"));
    const keyPath = join(secrets, "master.key");
    if (!existsSync(keyPath))
      writeFileSync(keyPath, randomBytes(32), { flag: "wx", mode: 0o600 });
    this.key = readFileSync(keyPath);
    if (this.key.length !== 32) throw new Error("Invalid secret-store key");
    this.database = new DatabaseSync(join(path, "prometheus.db"), {
      timeout: 3000,
    });
    const tables = this.database
      .prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
      )
      .all();
    if (tables.length && !tables.some((row) => row.name === "metadata")) {
      this.database.close();
      throw new Error("Unrecognized database; refusing schema changes");
    }
    const previousSchema = tables.length
      ? this.database
          .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
          .get()?.value
      : undefined;
    if (tables.length) {
      if (!["1", "2", "3", "4", "5"].includes(String(previousSchema))) {
        this.database.close();
        throw new Error("Unsupported Prometheus schema; do not downgrade");
      }
    }
    if (tables.length && previousSchema !== "5") {
      const backupDir = privateDirectory(join(path, "backups"));
      const target = join(
        backupDir,
        `schema${previousSchema}-${Date.now()}-${randomUUID()}.db`,
      );
      this.database.exec(`VACUUM INTO '${target.replace(/'/g, "''")}'`);
    }
    const schema = readFileSync(
      new URL("./assets/schema.sql", import.meta.url),
      "utf8",
    ).split(/\r?\n/);
    this.database.exec(
      schema
        .filter((line) => line.trimStart().startsWith("PRAGMA "))
        .join("\n"),
    );
    this.transaction(() => {
      this.database.exec(
        schema
          .filter((line) => !line.trimStart().startsWith("PRAGMA "))
          .join("\n"),
      );
      const draftColumns = new Set(
        this.all("PRAGMA table_info(config_drafts)").map((row) =>
          String(row.name),
        ),
      );
      for (const [name, definition] of [
        ["revision", "INTEGER NOT NULL DEFAULT 1"],
        ["updated_at", "INTEGER NOT NULL DEFAULT 0"],
        ["state", "TEXT NOT NULL DEFAULT 'draft'"],
        ["validation_json", "TEXT"],
        ["last_check_id", "TEXT"],
        ["applied_operation_id", "TEXT"],
      ])
        if (!draftColumns.has(name!))
          this.database.exec(
            `ALTER TABLE config_drafts ADD COLUMN ${name} ${definition}`,
          );
      this.run(
        "UPDATE config_drafts SET updated_at=created_at WHERE updated_at=0",
      );
      this.run(
        "UPDATE config_operations SET status='interrupted',config_json=NULL,error='连接检查随服务退出，未自动重试',updated_at=? WHERE status IN ('running','cancelling')",
        now(),
      );
      this.run(
        "INSERT INTO metadata(key,value) VALUES('prometheus.schema','5') ON CONFLICT(key) DO UPDATE SET value='5'",
      );
      if (previousSchema && previousSchema !== "5")
        this.run(
          "INSERT OR IGNORE INTO metadata VALUES('affinity.legacy_cutoff',?)",
          String(now()),
        );
      this.run(
        "INSERT OR IGNORE INTO metadata(key,value) VALUES('assistant.identity',?)",
        randomUUID(),
      );
      if (!this.get("SELECT revision FROM config_revisions LIMIT 1"))
        this.run(
          "INSERT INTO config_revisions VALUES(1,?,?)",
          JSON.stringify(defaultConfig()),
          now(),
        );
      if (!this.get("SELECT revision FROM personality_versions LIMIT 1")) {
        const personality = validatePersonality(
          JSON.parse(
            readFileSync(
              new URL("./assets/personality.json", import.meta.url),
              "utf8",
            ),
          ),
        );
        this.run(
          "INSERT INTO personality_versions VALUES(1,?,?)",
          JSON.stringify(personality),
          now(),
        );
      }
      if (
        !this.get("SELECT value FROM metadata WHERE key='journal.backfill.v1'")
      ) {
        this.run(
          "INSERT INTO audit_events(owner_id,kind,status,title,resource_id,created_at) SELECT r.owner_id,CASE m.role WHEN 'user' THEN 'input' ELSE 'output' END,'recorded',CASE m.role WHEN 'user' THEN '历史消息输入' ELSE '历史生成候选' END,m.id,m.created_at FROM (SELECT * FROM messages WHERE created_at>=? ORDER BY rowid DESC LIMIT 20000) m JOIN runs r ON r.id=m.run_id ORDER BY m.created_at",
          now() - 30 * 86400,
        );
        this.run("INSERT INTO metadata VALUES('journal.backfill.v1','1')");
      }
      this.run(
        "UPDATE runs SET status='interrupted',error='服务在请求完成前退出，未自动重放',updated_at=? WHERE status='running'",
        now(),
      );
      this.run(
        "UPDATE channel_outbox SET status='unknown',error='发送过程中退出，结果未知',updated_at=? WHERE status='sending'",
        now(),
      );
      this.run("DELETE FROM sessions WHERE expires_at<=?", now());
      this.run(
        "UPDATE operator_calls SET outcome='unknown' WHERE outcome='requesting'",
      );
      this.run(
        "UPDATE delivery_parts SET status='unknown',error='进程退出时发送结果未知',updated_at=? WHERE status='sending'",
        now(),
      );
      this.run(
        "UPDATE model_jobs SET status='interrupted',error='请求中断，需人工重试',updated_at=? WHERE status='running'",
        now(),
      );
      this.run(
        "DELETE FROM prompt_snapshots WHERE created_at<?",
        now() - 30 * 86400,
      );
    });
    this.assistantId = String(
      this.get("SELECT value FROM metadata WHERE key='assistant.identity'")!
        .value,
    );
  }
  get(sql: string, ...params: SQLInputValue[]): Row | undefined {
    return this.database.prepare(sql).get(...params) as Row | undefined;
  }
  all(sql: string, ...params: SQLInputValue[]): Row[] {
    return this.database.prepare(sql).all(...params) as Row[];
  }
  run(sql: string, ...params: SQLInputValue[]) {
    return this.database.prepare(sql).run(...params);
  }
  private transactionDepth = 0;
  private transactionSequence = 0;
  transaction<T>(operation: () => T): T {
    const nested = this.transactionDepth > 0,
      name = `prometheus_${++this.transactionSequence}`;
    this.database.exec(nested ? `SAVEPOINT ${name}` : "BEGIN IMMEDIATE");
    this.transactionDepth++;
    try {
      const value = operation();
      if (value instanceof Promise)
        throw new Error(
          "Database transactions must not cross asynchronous work",
        );
      this.database.exec(nested ? `RELEASE SAVEPOINT ${name}` : "COMMIT");
      return value;
    } catch (error) {
      this.database.exec(
        nested
          ? `ROLLBACK TO SAVEPOINT ${name}; RELEASE SAVEPOINT ${name}`
          : "ROLLBACK",
      );
      throw error;
    } finally {
      this.transactionDepth--;
    }
  }
  seal(value: unknown): string {
    const nonce = randomBytes(12),
      cipher = createCipheriv("aes-256-gcm", this.key, nonce);
    const encrypted = Buffer.concat([
      cipher.update(JSON.stringify(value), "utf8"),
      cipher.final(),
    ]);
    return Buffer.concat([nonce, cipher.getAuthTag(), encrypted]).toString(
      "base64",
    );
  }
  unseal<T>(value: string): T {
    const bytes = Buffer.from(value, "base64"),
      cipher = createDecipheriv("aes-256-gcm", this.key, bytes.subarray(0, 12));
    cipher.setAuthTag(bytes.subarray(12, 28));
    return JSON.parse(
      Buffer.concat([
        cipher.update(bytes.subarray(28)),
        cipher.final(),
      ]).toString("utf8"),
    ) as T;
  }
  config(): { revision: number; applied_revision: number; config: Config } {
    const row = this.get(
      "SELECT revision,config_json FROM config_revisions ORDER BY revision DESC LIMIT 1",
    )!;
    return {
      revision: Number(row.revision),
      applied_revision: Number(row.revision),
      config: JSON.parse(String(row.config_json)) as Config,
    };
  }
  applyConfig(expected: number, config: Config, owner: string) {
    return this.transaction(() => {
      if (this.config().revision !== expected) throw conflict();
      this.checkCredentials(config);
      this.run(
        "INSERT INTO config_revisions VALUES(?,?,?)",
        expected + 1,
        JSON.stringify(config),
        now(),
      );
      this.audit(
        owner,
        "config",
        "completed",
        "配置已保存并生效",
        String(expected + 1),
      );
      return this.config();
    });
  }
  checkCredentials(config: Config): void {
    if (
      config.qq.enabled &&
      (!/^[a-zA-Z0-9_]{1,128}$/.test(config.qq.app_id) ||
        !config.qq.credential_ref)
    )
      throw bad("启用 QQ 前须配置有效 App ID 和凭据");
    if (
      config.weixin.enabled &&
      !this.get("SELECT 1 FROM channel_state WHERE channel='weixin'")
    )
      throw bad("请先完成微信扫码绑定");
    for (const [reference, kind] of [
      [config.model.credential_ref, "model_api_key"],
      [config.qq.credential_ref, "qq_app_secret"],
    ] as const) {
      if (reference) this.secret(reference, kind);
    }
  }
  personality() {
    const row = this.get(
      "SELECT * FROM personality_versions ORDER BY revision DESC LIMIT 1",
    )!;
    return {
      revision: Number(row.revision),
      personality: JSON.parse(String(row.personality_json)) as Personality,
      saved: true,
      applied: true,
      effective_from: "next_run",
    };
  }
  savePersonality(expected: number, personality: Personality, owner: string) {
    return this.transaction(() => {
      if (this.personality().revision !== expected) throw conflict();
      this.run(
        "INSERT INTO personality_versions VALUES(?,?,?)",
        expected + 1,
        JSON.stringify(personality),
        now(),
      );
      this.audit(
        owner,
        "personality",
        "completed",
        "性格已保存并生效",
        String(expected + 1),
      );
      return this.personality();
    });
  }
  audit(
    owner: string,
    kind: string,
    status: string,
    title: string,
    resource: string | null = null,
  ): void {
    this.run(
      "INSERT INTO audit_events(owner_id,kind,status,title,resource_id,created_at) VALUES(?,?,?,?,?,?)",
      owner,
      kind,
      status,
      title,
      resource,
      now(),
    );
    this.run(
      "DELETE FROM audit_events WHERE created_at<? OR id<=(SELECT coalesce(max(id),0)-20000 FROM audit_events)",
      now() - 30 * 86400,
    );
  }
  addSecret(kind: string, value: string, owner: string) {
    if (
      !["model_api_key", "qq_app_secret", "weixin_token"].includes(kind) ||
      !value ||
      value.length > 8192 ||
      /[\r\n\0]/.test(value)
    )
      throw bad("凭据类型或值无效");
    const id = randomUUID(),
      nonce = randomBytes(12),
      cipher = createCipheriv("aes-256-gcm", this.key, nonce);
    cipher.setAAD(Buffer.from(`${id}:${kind}`));
    const data = Buffer.concat([cipher.update(value, "utf8"), cipher.final()]);
    const path = join(this.dataDir, "secrets", id);
    writeFileSync(path, Buffer.concat([nonce, cipher.getAuthTag(), data]), {
      flag: "wx",
      mode: 0o600,
    });
    try {
      this.transaction(() => {
        this.run("INSERT INTO secrets VALUES(?,?,?)", id, kind, now());
        this.audit(owner, "secret", "completed", "凭据已保存", id);
      });
    } catch (error) {
      unlinkSync(path);
      throw error;
    }
    return { id, kind, configured: true, created_at: now() };
  }
  secret(id: string, kind: string): string {
    if (
      !/^[0-9a-f-]{36}$/.test(id) ||
      !this.get("SELECT 1 FROM secrets WHERE id=? AND kind=?", id, kind)
    )
      throw bad("凭据引用不存在或类型不符", "invalid_credential");
    try {
      const data = readFileSync(join(this.dataDir, "secrets", id));
      const cipher = createDecipheriv(
        "aes-256-gcm",
        this.key,
        data.subarray(0, 12),
      );
      cipher.setAAD(Buffer.from(`${id}:${kind}`));
      cipher.setAuthTag(data.subarray(12, 28));
      return Buffer.concat([
        cipher.update(data.subarray(28)),
        cipher.final(),
      ]).toString("utf8");
    } catch {
      throw new ApiError(503, "secret_unavailable", "凭据存储无法读取");
    }
  }
  removeSecret(id: string, owner: string) {
    if (!this.get("SELECT id FROM secrets WHERE id=?", id)) throw missing();
    if (
      this.get(
        "SELECT id FROM channel_events WHERE credential_ref=? AND status='queued'",
        id,
      ) ||
      this.all("SELECT value_json FROM channel_state").some((row) => {
        try {
          return (
            this.unseal<Record<string, unknown>>(String(row.value_json))
              .credential_ref === id
          );
        } catch {
          return true;
        }
      })
    )
      throw new ApiError(409, "credential_in_use", "凭据仍被渠道引用");
    if (
      this.all("SELECT config_json FROM config_revisions").some((row) =>
        String(row.config_json).includes(id),
      ) ||
      this.get(
        "SELECT id FROM runs WHERE status IN ('queued','running') AND instr(config_json,?)>0",
        id,
      ) ||
      this.get(
        "SELECT id FROM config_drafts WHERE instr(config_json,?)>0",
        id,
      ) ||
      this.get(
        "SELECT id FROM config_operations WHERE status IN ('running','cancelling') AND instr(config_json,?)>0",
        id,
      )
    )
      throw new ApiError(
        409,
        "credential_in_use",
        "凭据仍被配置、草稿、回退版本或在途请求引用",
      );
    this.transaction(() => {
      this.run("DELETE FROM secrets WHERE id=?", id);
      this.audit(owner, "secret", "completed", "凭据已删除", id);
    });
    unlinkSync(join(this.dataDir, "secrets", id));
    return { ok: true };
  }
  redact(text: string): string {
    return this.redactor()(text);
  }
  redactor(): (text: string) => string {
    const known: string[] = [];
    for (const row of this.all("SELECT id,kind FROM secrets")) {
      try {
        const secret = this.secret(String(row.id), String(row.kind));
        known.push(secret);
      } catch {
        /* Never expose secret-store errors. */
      }
    }
    return (text) =>
      known.reduce(
        (value, secret) => value.split(secret).join("[凭据已隐藏]"),
        text,
      );
  }
  close() {
    this.database.exec("PRAGMA wal_checkpoint(TRUNCATE)");
    this.database.close();
    this.key.fill(0);
  }
}
