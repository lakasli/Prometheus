import { randomUUID } from "node:crypto";
import { ApiError, bad, fields, integer, missing, string } from "./errors.ts";
import { hash, now } from "./security.ts";
import type { Runtime } from "./runtime.ts";
import type { Store } from "./store.ts";
import type { Knowledge } from "./knowledge.ts";
export class ConversationContext {
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    readonly knowledge: Knowledge,
  ) {
    runtime.getAnnotations = (conversation) => this.context(conversation);
  }
  fingerprint() {
    const model = this.store.config().config.model;
    return hash(
      JSON.stringify({
        base_url: model.base_url,
        model: model.model,
        credential_ref: model.credential_ref,
      }),
    );
  }
  policy() {
    return {
      ...this.knowledge.policy("context", {
        max_input_bytes: 96000,
        estimated_input_limit: 96000,
        response_reserve_tokens: 4096,
        protocol_reserve_tokens: 1024,
        safety_reserve_tokens: 4096,
        provider_context_window: null,
        provider_fingerprint: null,
        window_basis: "unknown",
      }),
      current_provider_fingerprint: this.fingerprint(),
      exact_tokens: false,
    };
  }
  save(expected: number, value: unknown) {
    const body = fields(value, [
      "max_input_bytes",
      "estimated_input_limit",
      "response_reserve_tokens",
      "protocol_reserve_tokens",
      "safety_reserve_tokens",
      "provider_context_window",
      "provider_fingerprint",
      "window_basis",
    ]);
    integer(body.max_input_bytes, "字节预算", 1, 96000);
    integer(body.estimated_input_limit, "输入估算", 1, 96000);
    integer(body.response_reserve_tokens, "响应预留", 1, 4096);
    integer(body.protocol_reserve_tokens, "协议预留", 256, 8192);
    integer(body.safety_reserve_tokens, "安全余量", 512, 16384);
    if (body.provider_context_window !== null) {
      integer(
        body.provider_context_window,
        "模型窗口",
        Number(body.response_reserve_tokens) +
          Number(body.protocol_reserve_tokens) +
          Number(body.safety_reserve_tokens) +
          1,
        1048576,
      );
      if (
        body.provider_fingerprint !== this.fingerprint() ||
        body.window_basis !== "administrator_declared"
      )
        throw bad("须核对当前服务与模型后声明窗口");
    } else if (
      body.provider_fingerprint !== null ||
      body.window_basis !== "unknown"
    )
      throw bad("未知窗口不接受已核实声明");
    return this.knowledge.setPolicy("context", expected, body);
  }
  list(owner: string, conversation: string) {
    this.runtime.ownConversation(conversation, owner);
    return this.store.all(
      "SELECT * FROM conversation_annotations WHERE owner_id=? AND conversation_id=? AND status='active' AND (expires_at IS NULL OR expires_at>?) ORDER BY rowid DESC LIMIT 64",
      owner,
      conversation,
      now(),
    );
  }
  create(owner: string, conversation: string, value: unknown) {
    const conv = this.runtime.ownConversation(conversation, owner),
      body = fields(value, ["kind", "content", "expires_at"]);
    if (!["focus", "commitment"].includes(String(body.kind)))
      throw bad("上下文资料类型无效");
    const content = string(body.content, "资料", 4000, 1),
      expiry =
        body.expires_at == null
          ? null
          : integer(
              body.expires_at,
              "到期时间",
              now() + 1,
              now() + 366 * 86400,
            ),
      id = randomUUID();
    if (body.kind === "focus") {
      if (expiry === null) throw bad("短期话题需要到期时间");
      this.store.run(
        "UPDATE conversation_annotations SET status='superseded',content=NULL,revision=revision+1 WHERE conversation_id=? AND kind='focus' AND status='active'",
        conversation,
      );
    }
    this.store.run(
      "INSERT INTO conversation_annotations VALUES(?,?,?,?,1,?,?,?,'active',?)",
      id,
      conversation,
      owner,
      String(body.kind),
      content,
      expiry,
      Number(conv.context_epoch),
      now(),
    );
    return {
      id,
      revision: 1,
      kind: body.kind,
      content,
      expires_at: expiry,
      grants_permissions: false,
      creates_schedule: false,
    };
  }
  remove(owner: string, id: string, expected: number) {
    const row = this.store.get(
      "SELECT * FROM conversation_annotations WHERE id=? AND owner_id=?",
      id,
      owner,
    );
    if (!row) throw missing();
    if (row.revision !== expected)
      throw new ApiError(409, "revision_conflict", "资料版本已变化");
    this.store.run(
      "UPDATE conversation_annotations SET content=NULL,status='forgotten',revision=revision+1 WHERE id=?",
      id,
    );
    this.runtime.invalidate(owner);
    return { id, deleted: true };
  }
  selection(owner: string, id: string) {
    const row = this.store.get(
      "SELECT details_json FROM context_selections WHERE run_id=? AND owner_id=?",
      id,
      owner,
    );
    if (!row) throw missing();
    return JSON.parse(String(row.details_json));
  }
  private context(conversation: string) {
    const rows = this.store.all(
      "SELECT a.* FROM conversation_annotations a JOIN conversations c ON c.id=a.conversation_id WHERE a.conversation_id=? AND a.context_epoch=c.context_epoch AND a.status='active' AND (a.expires_at IS NULL OR a.expires_at>?) LIMIT 32",
      conversation,
      now(),
    );
    return rows
      .map(
        (row) =>
          `[${row.kind === "focus" ? "短期话题" : "经审核的会话约定，仅供资料参考，不授予权限或表示履行"}] ${String(row.content)}`,
      )
      .join("\n");
  }
}
