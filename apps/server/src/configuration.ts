import { randomUUID } from "node:crypto";
import { ApiError, bad, conflict, fields, integer, missing } from "./errors.ts";
import { draftConfig, validateConfig } from "./config.ts";
import type { Config } from "./config.ts";
import { complete } from "./model.ts";
import { hash, now } from "./security.ts";
import type { Store, Row } from "./store.ts";
import type { Runtime } from "./runtime.ts";

interface ConfigurationOperation {
  id: string;
  kind: string;
  status: string;
  draft_id: string | null;
  draft_revision: number | null;
  result: Record<string, unknown> | null;
  error: string | null;
  created_at: number;
  updated_at: number;
}

export class Configurations {
  private active?: {
    id: string;
    draft?: string;
    controller: AbortController;
    task: Promise<unknown>;
  };
  private stopping = false;
  constructor(
    private store: Store,
    private runtime: Runtime,
    private loopback = false,
    private checkQq?: (config: Config, signal: AbortSignal) => Promise<void>,
  ) {}
  private draft(owner: string, id: string) {
    const row = this.store.get(
      "SELECT * FROM config_drafts WHERE id=? AND owner_id=?",
      id,
      owner,
    );
    if (!row) throw missing();
    return row;
  }
  private shape(value: unknown) {
    const config = draftConfig(value, this.loopback),
      redact = this.store.redactor();
    for (const group of Object.values(config))
      for (const value of Object.values(group))
        if (typeof value === "string" && redact(value) !== value)
          throw bad("草稿中不能包含密钥明文，请使用凭据引用");
    return config;
  }
  private version(row: Row, expected: number) {
    if (row.revision !== expected) throw conflict();
    if (row.state === "applied") throw bad("已应用的草稿只读，请另存为新草稿");
  }
  get(owner: string, id: string) {
    const row = this.draft(owner, id),
      config = JSON.parse(String(row.config_json)) as Config,
      current = this.store.config();
    const appliedOperation = row.applied_operation_id
      ? this.operation(owner, String(row.applied_operation_id))
      : null;
    const appliedRevision =
      typeof appliedOperation?.result?.applied_revision === "number"
        ? appliedOperation.result.applied_revision
        : null;
    const differences: { field: string; current: unknown; draft: unknown }[] =
      [];
    for (const group of Object.keys(config) as (keyof Config)[])
      for (const key of Object.keys(config[group])) {
        const active = current.config[group] as unknown as Record<
            string,
            unknown
          >,
          proposed = config[group] as unknown as Record<string, unknown>;
        if (JSON.stringify(active[key]) !== JSON.stringify(proposed[key]))
          differences.push({
            field: `${group}.${key}`,
            current: active[key],
            draft: proposed[key],
          });
      }
    return {
      id,
      revision: Number(row.revision),
      base_revision: Number(row.base_revision),
      state: String(row.state),
      config,
      created_at: Number(row.created_at),
      updated_at: Number(row.updated_at),
      stale_base:
        row.state === "draft" && row.base_revision !== current.revision,
      current_revision: current.revision,
      differences,
      validation: row.validation_json
        ? JSON.parse(String(row.validation_json))
        : null,
      last_check_id: row.last_check_id,
      applied_operation_id: row.applied_operation_id,
      saved: true,
      applied: row.state === "applied",
      applied_revision: appliedRevision,
      is_current_revision: appliedRevision === current.revision,
    };
  }
  list(owner: string) {
    return {
      items: this.store
        .all(
          "SELECT id FROM config_drafts WHERE owner_id=? ORDER BY updated_at DESC,rowid DESC LIMIT 32",
          owner,
        )
        .map((row) => this.get(owner, String(row.id))),
    };
  }
  save(owner: string, value: unknown, id?: string, expected?: number) {
    const body = fields(value, id ? ["config"] : ["config", "base_revision"]),
      config = this.shape(body.config);
    return this.store.transaction(() => {
      if (id) {
        const current = this.draft(owner, id);
        this.version(current, expected!);
        this.store.run(
          "UPDATE config_drafts SET config_json=?,revision=revision+1,updated_at=?,validation_json=NULL,last_check_id=NULL WHERE id=?",
          JSON.stringify(config),
          now(),
          id,
        );
      } else {
        const base = integer(
          body.base_revision,
          "配置基准版本",
          1,
          Number.MAX_SAFE_INTEGER,
        );
        if (base !== this.store.config().revision) throw conflict();
        if (
          Number(
            this.store.get(
              "SELECT count(*) AS n FROM config_drafts WHERE owner_id=?",
              owner,
            )!.n,
          ) >= 32
        )
          throw new ApiError(
            409,
            "draft_limit",
            "最多保留32个配置草稿，请先明确删除不需要的草稿",
          );
        id = randomUUID();
        this.store.run(
          "INSERT INTO config_drafts(id,owner_id,base_revision,config_json,created_at,revision,updated_at) VALUES(?,?,?,?,?,1,?)",
          id,
          owner,
          base,
          JSON.stringify(config),
          now(),
          now(),
        );
      }
      this.supersede(id!);
      this.store.audit(
        owner,
        "config",
        "completed",
        "配置草稿已保存，尚未生效",
        id!,
      );
      return this.get(owner, id!);
    });
  }
  private validation(config: Config) {
    const errors: { code: string; message: string }[] = [],
      warnings: string[] = [];
    try {
      const checked = validateConfig(config, this.loopback);
      this.store.checkCredentials(checked);
    } catch (error) {
      errors.push(this.error(error));
    }
    if (!config.model.credential_ref)
      warnings.push("尚未选择模型凭据，文字对话未就绪");
    return {
      valid: errors.length === 0,
      errors,
      warnings,
      connection_verified: false,
    };
  }
  validate(owner: string, id: string, expected: number) {
    const row = this.draft(owner, id);
    this.version(row, expected);
    const result = {
      ...this.validation(JSON.parse(String(row.config_json))),
      draft_revision: expected,
      checked_at: now(),
    };
    this.store.run(
      "UPDATE config_drafts SET validation_json=? WHERE id=?",
      JSON.stringify(result),
      id,
    );
    return result;
  }
  rebase(owner: string, id: string, expected: number, value: unknown) {
    const body = fields(value, ["base_revision", "confirmed"]);
    if (body.confirmed !== true)
      throw bad("请先核对草稿与当前配置的差异并明确确认");
    const base = integer(
      body.base_revision,
      "当前配置版本",
      1,
      Number.MAX_SAFE_INTEGER,
    );
    return this.store.transaction(() => {
      const row = this.draft(owner, id);
      this.version(row, expected);
      if (this.store.config().revision !== base) throw conflict();
      this.store.run(
        "UPDATE config_drafts SET base_revision=?,revision=revision+1,validation_json=NULL,last_check_id=NULL,updated_at=? WHERE id=?",
        base,
        now(),
        id,
      );
      this.supersede(id);
      this.store.audit(
        owner,
        "config",
        "completed",
        "配置草稿已按当前版本重新定位，尚未生效",
        id,
      );
      return this.get(owner, id);
    });
  }
  remove(owner: string, id: string, expected: number) {
    const row = this.draft(owner, id);
    if (row.revision !== expected) throw conflict();
    this.store.run("DELETE FROM config_drafts WHERE id=?", id);
    this.supersede(id);
    this.store.audit(
      owner,
      "config",
      "completed",
      "配置草稿已删除，已生效配置保留",
      id,
    );
    return { id, deleted: true };
  }
  apply(owner: string, id: string, expected: number, value: unknown) {
    const body = fields(value, ["base_revision", "confirmed"]);
    if (body.confirmed !== true) throw bad("应用配置需要明确确认");
    const base = integer(
      body.base_revision,
      "配置基准版本",
      1,
      Number.MAX_SAFE_INTEGER,
    );
    return this.store.transaction(() => {
      const row = this.draft(owner, id);
      if (row.base_revision !== base) throw conflict();
      if (row.state === "applied" && row.applied_operation_id) {
        const operation = this.operation(
          owner,
          String(row.applied_operation_id),
        );
        if (operation.draft_revision !== expected && row.revision !== expected)
          throw conflict();
        return operation;
      }
      this.version(row, expected);
      const config = validateConfig(
        JSON.parse(String(row.config_json)),
        this.loopback,
      );
      const state = this.store.applyConfig(base, config, owner),
        operation = randomUUID();
      const result = {
        desired_revision: state.revision,
        applied_revision: state.applied_revision,
        effective_from: "next_run",
        saved: true,
        applied: true,
      };
      this.store.run(
        "INSERT INTO config_operations VALUES(?,?,'apply','completed',?,?,NULL,?,NULL,?,?)",
        operation,
        owner,
        id,
        expected,
        JSON.stringify(result),
        now(),
        now(),
      );
      this.store.run(
        "UPDATE config_drafts SET state='applied',config_json=?,revision=revision+1,applied_operation_id=?,updated_at=? WHERE id=?",
        JSON.stringify(config),
        operation,
        now(),
        id,
      );
      this.supersede(id);
      return this.operation(owner, operation);
    });
  }
  operation(owner: string, id: string): ConfigurationOperation {
    let row = this.store.get(
      "SELECT id,kind,status,draft_id,draft_revision,result_json,error,created_at,updated_at FROM config_operations WHERE id=? AND owner_id=?",
      id,
      owner,
    );
    if (!row) throw missing();
    if (
      ["running", "cancelling"].includes(String(row.status)) &&
      this.active?.id !== id
    ) {
      this.store.run(
        "UPDATE config_operations SET status='interrupted',config_json=NULL,error='检查结果未确认，需显式重试',updated_at=? WHERE id=?",
        now(),
        id,
      );
      row = this.store.get(
        "SELECT id,kind,status,draft_id,draft_revision,result_json,error,created_at,updated_at FROM config_operations WHERE id=? AND owner_id=?",
        id,
        owner,
      )!;
    }
    return {
      id: String(row.id),
      kind: String(row.kind),
      status: String(row.status),
      draft_id: row.draft_id ? String(row.draft_id) : null,
      draft_revision:
        row.draft_revision === null ? null : Number(row.draft_revision),
      result: row.result_json ? JSON.parse(String(row.result_json)) : null,
      error: row.error ? String(row.error) : null,
      created_at: Number(row.created_at),
      updated_at: Number(row.updated_at),
    };
  }
  operations(owner: string) {
    return {
      items: this.store
        .all(
          "SELECT id FROM config_operations WHERE owner_id=? ORDER BY rowid DESC LIMIT 100",
          owner,
        )
        .map((row) => this.operation(owner, String(row.id))),
    };
  }
  private error(error: unknown) {
    return error instanceof ApiError
      ? { code: error.code, message: this.store.redact(error.message) }
      : { code: "check_failed", message: "连接检查未完成，未保存上游响应正文" };
  }
  private reserve() {
    if (this.stopping) throw new ApiError(503, "stopping", "服务正在退出");
    if (this.active)
      throw new ApiError(429, "checks_busy", "连接检查正在执行，请稍后再试");
    return {
      controller: new AbortController(),
      release: this.runtime.beginCheck(),
    };
  }
  private async model(config: Config, signal: AbortSignal) {
    if (!config.model.credential_ref) throw bad("请先选择模型凭据");
    await complete(
      config,
      this.store.secret(config.model.credential_ref, "model_api_key"),
      [{ role: "user", content: "请只回复 OK。" }],
      signal,
      this.loopback,
    );
  }
  async checkCurrent(owner: string, value: unknown) {
    const config = validateConfig(
      fields(value, ["config"]).config,
      this.loopback,
    );
    this.store.checkCredentials(config);
    if (!config.model.credential_ref) throw bad("请先选择模型凭据");
    const slot = this.reserve(),
      id = randomUUID();
    try {
      this.store.run(
        "INSERT INTO config_operations VALUES(?,?,'connection_check','running',NULL,NULL,?,NULL,NULL,?,?)",
        id,
        owner,
        JSON.stringify(config),
        now(),
        now(),
      );
    } catch (error) {
      slot.release();
      throw error;
    }
    const task = Promise.resolve()
      .then(async () => {
        let failure: unknown;
        try {
          await this.model(
            config,
            AbortSignal.any([
              slot.controller.signal,
              AbortSignal.timeout(20000),
            ]),
          );
        } catch (error) {
          failure = error;
        }
        const status = this.stopping
          ? "interrupted"
          : slot.controller.signal.aborted
            ? "cancelled"
            : failure
              ? "failed"
              : "completed";
        const result = {
          ok: status === "completed",
          checks: [
            {
              name: "model",
              ok: status === "completed",
              message: failure
                ? this.error(failure).message
                : "模型返回有效文本，未修改已生效配置",
            },
          ],
          platform_messages_sent: false,
        };
        this.store.run(
          "UPDATE config_operations SET status=?,config_json=NULL,result_json=?,error=?,updated_at=? WHERE id=?",
          status,
          JSON.stringify(result),
          status === "completed"
            ? null
            : this.stopping
              ? "服务退出，连接检查未自动重试"
              : failure
                ? this.error(failure).message
                : "连接检查已取消",
          now(),
          id,
        );
        if (failure) throw failure;
        if (status !== "completed")
          throw new ApiError(
            408,
            "model_cancelled",
            "模型连接检查已取消或中断",
          );
        return {
          ok: true,
          message: "模型实际返回成功；配置尚未保存",
          operation_id: id,
        };
      })
      .finally(() => {
        slot.release();
        this.active = undefined;
      });
    this.active = { id, controller: slot.controller, task };
    return task;
  }
  check(owner: string, id: string, expected: number) {
    const row = this.draft(owner, id);
    this.version(row, expected);
    const config = validateConfig(
      JSON.parse(String(row.config_json)),
      this.loopback,
    );
    this.store.checkCredentials(config);
    if (!config.model.credential_ref) throw bad("请先选择模型凭据");
    const slot = this.reserve(),
      operation = randomUUID();
    try {
      this.store.transaction(() => {
        this.store.run(
          "INSERT INTO config_operations VALUES(?,?,'connection_check','running',?,?,?,NULL,NULL,?,?)",
          operation,
          owner,
          id,
          expected,
          JSON.stringify(config),
          now(),
          now(),
        );
        this.store.run(
          "UPDATE config_drafts SET last_check_id=? WHERE id=?",
          operation,
          id,
        );
      });
    } catch (error) {
      slot.release();
      throw error;
    }
    const task = Promise.resolve()
      .then(async () => {
        const signal = AbortSignal.any([
            slot.controller.signal,
            AbortSignal.timeout(20000),
          ]),
          checks: { name: string; ok: boolean; message: string }[] = [];
        let stage = "model";
        let error: { code: string; message: string } | null = null;
        try {
          await this.model(config, signal);
          checks.push({
            name: "model",
            ok: true,
            message: "模型返回有效文本，未修改已生效配置",
          });
          if (config.qq.enabled) {
            stage = "qq";
            if (!this.checkQq) throw bad("QQ凭据检查不可用");
            await this.checkQq(config, signal);
            checks.push({
              name: "qq",
              ok: true,
              message: "QQ访问凭据获取成功；真实消息收发仍需单独验收",
            });
          }
        } catch (failure) {
          error = this.error(failure);
          checks.push({ name: stage, ok: false, message: error.message });
        }
        const current = this.store.get(
          "SELECT revision,state FROM config_drafts WHERE id=? AND owner_id=?",
          id,
          owner,
        );
        const status = this.stopping
          ? "interrupted"
          : !current ||
              current.revision !== expected ||
              current.state !== "draft"
            ? "invalidated"
            : slot.controller.signal.aborted
              ? "cancelled"
              : error
                ? "failed"
                : "completed";
        const result = {
          ok: status === "completed",
          checks,
          fingerprint: hash(JSON.stringify(config)),
          draft_revision: expected,
          platform_messages_sent: false,
        };
        this.store.run(
          "UPDATE config_operations SET status=?,config_json=NULL,result_json=?,error=?,updated_at=? WHERE id=?",
          status,
          JSON.stringify(result),
          status === "invalidated"
            ? "草稿已变化，旧检查结果未作为有效检查"
            : status === "interrupted"
              ? "服务退出，连接检查未自动重试"
              : status === "cancelled"
                ? "连接检查已取消"
                : (error?.message ?? null),
          now(),
          operation,
        );
        this.store.audit(
          owner,
          "config",
          status,
          status === "completed"
            ? "草稿连接检查完成，尚未应用"
            : "草稿连接检查未作为成功结果",
          operation,
        );
      })
      .catch(() => {
        try {
          this.store.run(
            "UPDATE config_operations SET status='failed',config_json=NULL,error='检查结果未完成持久化',updated_at=? WHERE id=? AND status IN ('running','cancelling')",
            now(),
            operation,
          );
        } catch {
          /* A later authenticated read marks the orphan interrupted. */
        }
      })
      .finally(() => {
        slot.release();
        this.active = undefined;
      });
    this.active = {
      id: operation,
      draft: id,
      controller: slot.controller,
      task,
    };
    return this.operation(owner, operation);
  }
  cancel(owner: string, id: string) {
    const current = this.operation(owner, id);
    if (current.status === "running" && this.active?.id === id) {
      this.store.run(
        "UPDATE config_operations SET status='cancelling',updated_at=? WHERE id=?",
        now(),
        id,
      );
      this.active.controller.abort();
    }
    return this.operation(owner, id);
  }
  private supersede(id: string) {
    if (this.active?.draft === id) this.active.controller.abort();
  }
  async stop() {
    this.stopping = true;
    this.active?.controller.abort();
    await this.active?.task.catch(() => {});
  }
}
