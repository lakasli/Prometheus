import { bad, integer, string } from "./errors.ts";
import { now } from "./security.ts";
import type { Row, Store } from "./store.ts";

const kinds = [
  "input",
  "output",
  "context",
  "run",
  "delivery",
  "system",
  "memory",
  "diary",
  "schedule",
  "people",
  "relationships",
  "skills",
  "personality",
  "config",
  "secret",
  "authentication",
  "rest",
  "memory_candidates",
  "summary",
  "drawing_html",
  "schedule_plan",
  "trigger_draft",
  "plugin",
  "tool",
  "tts",
  "asr",
  "subagent",
  "vision",
];
const maximumContent = 16384;
export class Journal {
  constructor(private store: Store) {}
  list(owner: string, query: URLSearchParams) {
    const allowed = ["limit", "cursor", "kind", "status", "run_id", "q"];
    for (const name of query.keys())
      if (!allowed.includes(name) || query.getAll(name).length !== 1)
        throw bad("日志筛选字段无效");
    const limit = integer(
      Number(query.get("limit") ?? 50),
      "日志分页大小",
      1,
      100,
    );
    let cursor = integer(
      Number(query.get("cursor") || Number.MAX_SAFE_INTEGER),
      "日志游标",
      1,
      Number.MAX_SAFE_INTEGER,
    );
    const kind = string(query.get("kind") ?? "", "日志类型", 40),
      status = string(query.get("status") ?? "", "日志状态", 32),
      runId = string(query.get("run_id") ?? "", "运行标识", 128),
      search = string(
        query.get("q") ?? "",
        "日志查询",
        200,
      ).toLocaleLowerCase();
    if (kind && !kinds.includes(kind)) throw bad("日志类型无效");
    const redact = this.store.redactor();
    const items: Record<string, unknown>[] = [];
    let more = false;
    for (let batch = 0; batch < 20; batch++) {
      const rows = this.store.all(
        "SELECT e.* FROM audit_events e WHERE e.id<? AND e.created_at>=? AND (e.owner_id=? OR ((e.owner_id LIKE 'qq:%' OR e.owner_id LIKE 'weixin:%') AND NOT EXISTS(SELECT 1 FROM admins a WHERE a.id=e.owner_id))) AND (?='' OR e.kind=?) AND (?='' OR e.status=?) ORDER BY e.id DESC LIMIT 200",
        cursor,
        now() - 30 * 86400,
        owner,
        kind,
        kind,
        status,
        status,
      );
      for (const row of rows) {
        const resolved = this.resolve(row, redact);
        const matches =
          (!runId || resolved.run_id === runId) &&
          (!search ||
            JSON.stringify(resolved).toLocaleLowerCase().includes(search));
        if (matches && items.length === limit) {
          more = true;
          break;
        }
        cursor = Number(row.id);
        if (matches) items.push(resolved);
      }
      if (more || rows.length < 200) break;
      if (batch === 19) more = true;
    }
    return {
      items,
      next_cursor: more ? cursor : null,
      retention: {
        days: 30,
        max_events: 20000,
        max_content_bytes: maximumContent,
      },
      filters: { kinds },
      scope: "own_web_and_instance_channels",
    };
  }
  private resolve(
    event: Row,
    redact: (text: string) => string,
  ): Record<string, unknown> {
    const reference = String(event.resource_id ?? "");
    const owner = String(event.owner_id);
    let run: Row | undefined,
      content = "",
      details: Record<string, unknown> = {};
    if (["input", "output"].includes(String(event.kind))) {
      run = this.store.get(
        "SELECT r.*,c.context_epoch AS current_epoch,m.content AS message_content FROM messages m JOIN runs r ON r.id=m.run_id JOIN conversations c ON c.id=r.conversation_id WHERE m.id=? AND r.owner_id=?",
        reference,
        owner,
      );
      if (run) content = String(run.message_content);
    } else if (["run", "delivery", "context"].includes(String(event.kind))) {
      run = this.store.get(
        "SELECT r.*,c.context_epoch AS current_epoch FROM runs r JOIN conversations c ON c.id=r.conversation_id WHERE r.id=? AND r.owner_id=?",
        reference,
        owner,
      );
      if (run && event.kind === "context") {
        const snapshot = this.store.get(
          "SELECT messages_json FROM prompt_snapshots WHERE run_id=? AND owner_id=? AND context_epoch=?",
          String(run.id),
          owner,
          Number(run.context_epoch),
        );
        content = snapshot
          ? String(snapshot.messages_json)
          : "[上下文已失效或清理，仅保留事件元数据]";
        const selection = this.store.get(
          "SELECT details_json FROM context_selections WHERE run_id=? AND owner_id=?",
          String(run.id),
          owner,
        );
        details = selection ? JSON.parse(String(selection.details_json)) : {};
      } else if (run && event.kind === "run") {
        // Resolve the original stores; never copy a reply or forgotten fact into an event.
        content = String(run.response ?? run.error ?? "");
      }
    }
    if (run) {
      details.run_status = run.status;
      details.publication =
        this.store.get(
          "SELECT state FROM run_publications WHERE run_id=?",
          String(run.id),
        )?.state ?? "active";
      if (run.context_epoch !== run.current_epoch)
        content = "[上下文已失效或清理，仅保留事件元数据]";
    }
    const safe = Buffer.from(redact(content)),
      truncated = safe.length > maximumContent;
    return {
      id: event.id,
      kind: event.kind,
      status: event.status,
      title: redact(String(event.title)),
      resource_id: event.resource_id,
      created_at: event.created_at,
      run_id: run?.id ?? null,
      conversation_id: run?.conversation_id ?? null,
      source: String(event.owner_id).startsWith("qq:")
        ? "qq"
        : String(event.owner_id).startsWith("weixin:")
          ? "weixin"
          : "web",
      content: safe.subarray(0, maximumContent).toString("utf8"),
      truncated,
      details,
    };
  }
}
