import { randomUUID } from "node:crypto";
import {
  ApiError,
  bad,
  boolean,
  conflict,
  fields,
  integer,
  missing,
  object,
  string,
} from "./errors.ts";
import { now } from "./security.ts";
import type { Row, Store } from "./store.ts";

export const recordKinds = [
  "memory",
  "diary",
  "schedule",
  "people",
  "relationships",
  "skills",
] as const;
export type RecordKind = (typeof recordKinds)[number];
export interface RecordItem {
  id: string;
  kind: RecordKind;
  owner_id: string;
  visibility: "private" | "shared";
  revision: number;
  data: Record<string, unknown>;
  created_at: number;
  updated_at: number;
}
export function kind(value: string): RecordKind {
  if (!recordKinds.includes(value as RecordKind)) throw missing();
  return value as RecordKind;
}
function payload(
  type: RecordKind,
  value: unknown,
  store: Store,
  owner: string,
): Record<string, unknown> {
  const common = (names: string[]) => fields(value, names);
  const text = (input: unknown, name: string, max = 8000, min = 0) =>
    string(input, name, max, min);
  switch (type) {
    case "memory": {
      const data = common(["content"]);
      return { content: text(data.content, "记忆内容", 4000, 1) };
    }
    case "diary": {
      const data = common([
        "title",
        "content",
        "feelings",
        "date",
        "period",
        "sources",
      ]);
      const date = text(data.date, "日期", 10, 10),
        period = data.period ?? "day";
      if (
        !/^\d{4}-\d{2}-\d{2}$/.test(date) ||
        Number.isNaN(Date.parse(`${date}T00:00:00Z`)) ||
        new Date(`${date}T00:00:00Z`).toISOString().slice(0, 10) !== date
      )
        throw bad("日记日期无效");
      if (!["day", "week", "month"].includes(String(period)))
        throw bad("日记周期无效");
      const sources = data.sources ?? [];
      if (!Array.isArray(sources) || sources.length > 128)
        throw bad("日记来源最多128条");
      const ids = sources.map((id) => text(id, "来源 ID", 36, 36));
      for (const id of ids)
        if (
          !store.get(
            "SELECT id FROM records WHERE id=? AND owner_id=? AND kind IN ('memory','diary') AND deleted_at IS NULL",
            id,
            owner,
          )
        )
          throw bad("日记引用包含不可访问的来源");
      return {
        title: text(data.title, "日记标题", 200, 1),
        content: text(data.content, "日记内容", 16000, 1),
        feelings: text(data.feelings ?? "", "感受", 4000),
        date,
        period,
        sources: ids,
        generator: "manual",
        timezone: "Asia/Shanghai",
      };
    }
    case "schedule": {
      const data = common(["title", "content", "start_at", "end_at", "status"]);
      const start = integer(data.start_at, "开始时间", 0, 4102444800),
        end = integer(data.end_at, "结束时间", 0, 4102444800);
      if (end <= start) throw bad("结束时间须晚于开始时间");
      const status = data.status ?? "planned";
      if (
        !["planned", "in_progress", "completed", "cancelled"].includes(
          String(status),
        )
      )
        throw bad("日程状态无效");
      return {
        title: text(data.title, "日程标题", 200, 1),
        content: text(data.content ?? "", "安排", 8000),
        start_at: start,
        end_at: end,
        status,
        execution: "manual",
      };
    }
    case "people": {
      const data = common(["name", "notes"]);
      return {
        name: text(data.name, "姓名", 128, 1),
        notes: text(data.notes ?? "", "人物资料", 8000),
        reviewed: true,
      };
    }
    case "relationships": {
      const data = common(["from_id", "to_id", "description"]);
      const from = text(data.from_id, "起点人物", 36, 36),
        to = text(data.to_id, "终点人物", 36, 36);
      if (from === to) throw bad("有向关系的起点和终点须不同");
      for (const id of [from, to])
        if (
          !store.get(
            "SELECT id FROM records WHERE id=? AND kind='people' AND owner_id=? AND deleted_at IS NULL",
            id,
            owner,
          )
        )
          throw bad("关系只能引用当前账户的有效人物");
      return {
        from_id: from,
        to_id: to,
        description: text(data.description, "关系说明", 4000, 1),
        reviewed: true,
      };
    }
    case "skills": {
      const data = common(["name", "description", "instructions", "enabled"]);
      return {
        name: text(data.name, "Skill 名称", 128, 1),
        description: text(data.description ?? "", "说明", 1000),
        instructions: text(data.instructions, "方法", 16000, 1),
        enabled: boolean(data.enabled ?? true, "启用状态"),
        grants_tool_permissions: false,
      };
    }
  }
}
const item = (row: Row): RecordItem => ({
  id: String(row.id),
  kind: row.kind as RecordKind,
  owner_id: String(row.owner_id),
  visibility: row.visibility as "private" | "shared",
  revision: Number(row.revision),
  data: JSON.parse(String(row.payload)) as Record<string, unknown>,
  created_at: Number(row.created_at),
  updated_at: Number(row.updated_at),
});
export class Records {
  constructor(
    private store: Store,
    private invalidate: (
      owner: string,
      change?: { action: string; id: string },
    ) => void,
  ) {}
  get(type: RecordKind, id: string, owner: string): RecordItem {
    const row = this.store.get(
      "SELECT * FROM records WHERE kind=? AND id=? AND owner_id=? AND deleted_at IS NULL",
      type,
      id,
      owner,
    );
    if (!row) throw missing();
    return item(row);
  }
  list(type: RecordKind, owner: string, query: URLSearchParams) {
    const q = string(query.get("q") ?? "", "查询", 200),
      limit = integer(Number(query.get("limit") ?? 50), "分页大小", 1, 100),
      offset = integer(Number(query.get("offset") ?? 0), "分页偏移", 0, 100000);
    if (!q || [...q].length >= 3) {
      // FTS phrases treat operators, quotes and punctuation as literal data.
      // Trigram indexes cannot match one/two characters; retain a bounded
      // substring fallback for these searches below.
      const search = q
        ? '"' + JSON.stringify(q).slice(1, -1).replaceAll('"', '""') + '"'
        : "";
      const where =
        "kind=? AND owner_id=? AND deleted_at IS NULL" +
        (q
          ? " AND rowid IN (SELECT rowid FROM record_fts WHERE record_fts MATCH ?)"
          : "");
      const parameters = q ? [type, owner, search] : [type, owner];
      return {
        items: this.store
          .all(
            `SELECT * FROM records WHERE ${where} ORDER BY updated_at DESC,rowid DESC LIMIT ? OFFSET ?`,
            ...parameters,
            limit,
            offset,
          )
          .map(item),
        total: Number(
          this.store.get(
            `SELECT count(*) AS n FROM records WHERE ${where}`,
            ...parameters,
          )!.n,
        ),
        limit,
        offset,
        assistant_id: this.store.assistantId,
      };
    }
    const all = this.store.all(
      "SELECT * FROM records WHERE kind=? AND owner_id=? AND deleted_at IS NULL ORDER BY updated_at DESC,rowid DESC LIMIT 5001",
      type,
      owner,
    );
    const matches = all.filter(
      (row) =>
        !q ||
        JSON.stringify(JSON.parse(String(row.payload)))
          .toLocaleLowerCase()
          .includes(q.toLocaleLowerCase()),
    );
    return {
      items: matches.slice(offset, offset + limit).map(item),
      total: matches.length,
      limit,
      offset,
      assistant_id: this.store.assistantId,
    };
  }
  save(
    type: RecordKind,
    owner: string,
    value: unknown,
    id?: string,
    expected?: number,
    provenance: "manual" | "model_reviewed" = "manual",
  ) {
    const body = fields(value, ["data", "visibility", "publish"]);
    const visibility = body.visibility ?? "private";
    if (!["private", "shared"].includes(String(visibility)))
      throw bad("可见范围无效");
    if (visibility === "shared" && body.publish !== true)
      throw bad("共享资料须显式确认发布");
    const data = payload(type, body.data, this.store, owner);
    if (type === "diary") data.generator = provenance;
    if (
      visibility === "shared" &&
      ["diary", "relationships", "skills", "schedule", "people"].includes(type)
    )
      throw bad("此类资料当前仅支持私有保存");
    return this.store.transaction(() => {
      const current = id ? this.get(type, id, owner) : null;
      if (current && current.revision !== expected) throw conflict();
      const count = Number(
        this.store.get(
          "SELECT count(*) AS count FROM records WHERE owner_id=? AND kind=? AND deleted_at IS NULL",
          owner,
          type,
        )!.count,
      );
      if (!current && count >= (type === "skills" ? 64 : 5000))
        throw new ApiError(409, "record_limit", "已达到资料数量上限");
      const recordId = id ?? randomUUID(),
        timestamp = now();
      if (current) this.clearDependents(recordId, false);
      if (current)
        this.store.run(
          "UPDATE records SET payload=?,visibility=?,revision=revision+1,updated_at=? WHERE id=?",
          JSON.stringify(data),
          String(visibility),
          timestamp,
          recordId,
        );
      else
        this.store.run(
          "INSERT INTO records VALUES(?,?,?,?,1,?,?,?,NULL)",
          recordId,
          type,
          owner,
          String(visibility),
          JSON.stringify(data),
          timestamp,
          timestamp,
        );
      this.store.audit(
        owner,
        type,
        "completed",
        current ? "资料已更新" : "资料已保存",
        recordId,
      );
      this.invalidate(
        visibility === "shared" || current?.visibility === "shared"
          ? "*"
          : owner,
        { action: current ? "corrected" : "created", id: recordId },
      );
      return this.get(type, recordId, owner);
    });
  }
  forget(type: RecordKind, id: string, owner: string, expected: number) {
    return this.store.transaction(() => {
      const current = this.get(type, id, owner);
      if (current.revision !== expected) throw conflict();
      if (
        type === "people" &&
        this.store.get(
          "SELECT a.alias_id FROM person_aliases a JOIN records r ON r.id=a.alias_id WHERE a.person_id=? AND r.deleted_at IS NULL",
          id,
        )
      )
        throw bad("请先拆分该档案的合并来源，再遗忘，避免连带删除其他人物来源");
      this.clearDependents(id, true);
      this.store.audit(
        owner,
        type,
        "completed",
        "资料已遗忘，依赖的派生资料已失效",
        id,
      );
      this.invalidate("*", { action: "forgotten", id });
      return {
        id,
        deleted: true,
        revision: expected + 1,
        cleanup: {
          live_content: "removed",
          derived_records: "invalidated",
          conversation_history: "retained",
          backups: "retained",
        },
      };
    });
  }
  private clearDependents(id: string, includeSource: boolean): void {
    // Both correction and forgetting invalidate the entire dependency chain.
    const dependents = this.store
      .all(
        "SELECT * FROM records WHERE kind IN ('diary','relationships') AND deleted_at IS NULL",
      )
      .map((row) => ({
        id: String(row.id),
        data: object(JSON.parse(String(row.payload))),
      }));
    const invalid = new Set([id]);
    let added = true;
    while (added) {
      added = false;
      for (const row of dependents) {
        if (invalid.has(row.id)) continue;
        const references = [
          ...(Array.isArray(row.data.sources) ? row.data.sources : []),
          row.data.from_id,
          row.data.to_id,
        ];
        if (
          references.some(
            (reference) =>
              typeof reference === "string" && invalid.has(reference),
          )
        ) {
          invalid.add(row.id);
          added = true;
        }
      }
    }
    for (const invalidId of invalid)
      if (includeSource || invalidId !== id)
        this.store.run(
          "UPDATE records SET payload=NULL,deleted_at=?,revision=revision+1,updated_at=? WHERE id=?",
          now(),
          now(),
          invalidId,
        );
  }
  context(owner: string): string {
    const rows = this.store.all(
      "SELECT * FROM records WHERE deleted_at IS NULL AND kind='memory' AND (owner_id=? OR visibility='shared') ORDER BY updated_at DESC LIMIT 20",
      owner,
    );
    const selected: string[] = [];
    let bytes = 0;
    for (const row of rows) {
      const line = `[来源 ${String(row.id)}；${String(row.visibility)}] ${String(object(JSON.parse(String(row.payload))).content)}`;
      if (bytes + Buffer.byteLength(line) > 20000) break;
      selected.push(line);
      bytes += Buffer.byteLength(line);
    }
    return selected.join("\n");
  }
}
