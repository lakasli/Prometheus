import { randomUUID } from "node:crypto";
import {
  ApiError,
  bad,
  conflict,
  fields,
  integer,
  missing,
  string,
} from "./errors.ts";
import { now } from "./security.ts";
import { profileValue } from "./profile-values.ts";
import { ProfileSources } from "./profile-sources.ts";
import type { ProfileSource, ProfileRef } from "./profile-sources.ts";
import type { Store, Row } from "./store.ts";
import type { Runtime } from "./runtime.ts";
import type { Knowledge } from "./knowledge.ts";
import type { Diary } from "./diary.ts";
const parse = (v: unknown) => JSON.parse(String(v));
export type ProfileUse = {
  person_id: string;
  person_revision: number;
  assertion_id?: string;
  assertion_revision?: number;
  edge_id?: string;
  edge_revision?: number;
};
export class Profiles {
  onModelInvalidated?: () => void;
  onAffinitySourceInvalidated?: (
    type: string,
    id: string,
    revision: number,
  ) => void;
  onIdentityChanged?: (person: string) => void;
  readonly sources: ProfileSources;
  private refreshing = false;
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
    readonly knowledge: Knowledge,
    readonly diary: Diary,
  ) {
    this.sources = new ProfileSources(store, knowledge, diary);
    for (const p of store.all(
      "SELECT * FROM records WHERE kind='people' AND deleted_at IS NULL AND owner_id IN (SELECT id FROM admins)",
    ))
      this.adopt(p, String(p.owner_id));
  }
  private clean(value: unknown, label: string, max = 2000, min = 1) {
    const text = string(value, label, max, min);
    if (
      this.store.redact(text) !== text ||
      text.includes("\0") ||
      Buffer.from(text).toString("utf8") !== text ||
      /(?:sk-[A-Za-z0-9_-]{16,}|(?:api[_ -]?key|password|密码|验证码|access[_ -]?token)\s*[:=：]\s*\S{4,})/i.test(
        text,
      )
    )
      throw bad(`${label}含无效字符或疑似秘密`);
    return text;
  }
  private adopt(
    p: Row,
    manager: string,
    conversation: string | null = null,
    reason: string | null = null,
  ) {
    this.store.run(
      "INSERT OR IGNORE INTO person_profiles VALUES(?,?,?,?,?,?)",
      String(p.id),
      this.store.assistantId,
      manager,
      conversation,
      reason,
      now(),
    );
    this.store.run(
      "INSERT OR IGNORE INTO person_name_versions VALUES(?,?,?,?,?,?)",
      String(p.id),
      Number(p.revision),
      String(parse(p.payload).name),
      reason,
      manager,
      now(),
    );
    this.store.run(
      "INSERT OR IGNORE INTO profile_contact_permissions VALUES(?,1,'not_granted',NULL,NULL,NULL,?)",
      String(p.id),
      now(),
    );
  }
  person(actor: string, id: string, write = false): Row {
    const p = this.store.get(
      "SELECT r.*,p.manager_id,p.source_conversation_id FROM records r LEFT JOIN person_profiles p ON p.person_id=r.id WHERE r.id=? AND r.kind='people' AND r.deleted_at IS NULL",
      id,
    );
    if (
      !p ||
      (write
        ? p.manager_id !== actor && p.owner_id !== actor
        : p.manager_id !== actor &&
          p.owner_id !== actor &&
          p.visibility !== "shared")
    )
      throw missing();
    if (!p.manager_id && p.owner_id === actor) {
      this.adopt(p, actor);
      p.manager_id = actor;
    }
    return p;
  }
  private public(p: Row) {
    return {
      id: p.id,
      revision: p.revision,
      owner_id: p.owner_id,
      visibility: p.visibility,
      data: parse(p.payload),
      manager_id: p.manager_id,
      source_conversation_id: p.source_conversation_id,
      created_at: p.created_at,
      updated_at: p.updated_at,
      merged_into:
        this.store.get(
          "SELECT person_id FROM person_aliases WHERE alias_id=?",
          String(p.id),
        )?.person_id ?? null,
    };
  }
  list(actor: string, query: URLSearchParams) {
    const limit = integer(Number(query.get("limit") ?? 50), "数量", 1, 100),
      offset = integer(Number(query.get("offset") ?? 0), "偏移", 0, 1000000),
      q = this.clean(query.get("q") ?? "", "搜索", 128, 0);
    const sql =
      "FROM records r LEFT JOIN person_profiles p ON p.person_id=r.id WHERE r.kind='people' AND r.deleted_at IS NULL AND (r.owner_id=? OR p.manager_id=? OR r.visibility='shared') AND (?='' OR instr(lower(r.payload),lower(?))>0)";
    return {
      items: this.store
        .all(
          `SELECT r.*,p.manager_id,p.source_conversation_id ${sql} ORDER BY r.updated_at DESC,r.id LIMIT ? OFFSET ?`,
          actor,
          actor,
          q,
          q,
          limit,
          offset,
        )
        .map((p) => this.public(p)),
      total: this.store.get(`SELECT count(*) AS n ${sql}`, actor, actor, q, q)!
        .n,
      limit,
      offset,
    };
  }
  create(actor: string, value: unknown) {
    const b = fields(value, [
        "name",
        "notes",
        "source_conversation_id",
        "visibility",
        "confirmed",
        "reason",
      ]),
      name = this.clean(b.name, "人物名称", 128),
      notes = this.clean(b.notes ?? "", "人工备注", 8000, 0),
      reason = this.clean(b.reason, "建立依据");
    if (b.confirmed !== true) throw bad("建立人物须明确确认");
    const visibility = b.visibility ?? "private";
    if (visibility !== "private" && visibility !== "shared")
      throw bad("可见范围无效");
    let owner = actor,
      conversation: string | null = null;
    if (b.source_conversation_id) {
      conversation = string(b.source_conversation_id, "来源会话", 36, 36);
      const c = this.store.get(
        "SELECT * FROM conversations WHERE id=?",
        conversation,
      );
      if (!c) throw missing();
      this.knowledge.source(actor, String(c.source_key ?? c.id));
      owner = String(c.owner_id);
    }
    if (visibility === "shared" && owner !== actor)
      throw bad("跨账号私人来源不能直接建立共享档案");
    return this.store.transaction(() => {
      if (
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM records WHERE kind='people' AND deleted_at IS NULL AND owner_id=?",
            owner,
          )!.n,
        ) >= 5000
      )
        throw bad("人物数量上限5000");
      const id = randomUUID(),
        at = now();
      this.store.run(
        "INSERT INTO records VALUES(?,'people',?,?,1,?,?,?,NULL)",
        id,
        owner,
        visibility,
        JSON.stringify({ name, notes, reviewed: true }),
        at,
        at,
      );
      const p = this.store.get("SELECT * FROM records WHERE id=?", id)!;
      this.adopt(p, actor, conversation, reason);
      this.runtime.onInvalidation?.(owner, { action: "created", id });
      this.store.audit(
        actor,
        "relationships",
        "completed",
        "已建立来源域人物，未绑定账号或授予联系权限",
        id,
      );
      return this.detail(actor, id);
    });
  }
  private bump(id: string) {
    this.store.run(
      "UPDATE records SET revision=revision+1,updated_at=? WHERE id=?",
      now(),
      id,
    );
  }
  private check(p: Row, expected: number) {
    if (p.revision !== expected) throw conflict();
  }
  private scope(p: Row, s: ProfileSource) {
    if (s.ref.type === "manual") return;
    if (
      (p.visibility === "shared" && s.visibility !== "shared") ||
      (p.visibility !== "shared" &&
        s.visibility !== "shared" &&
        s.owner !== p.owner_id)
    )
      throw bad("人物与来源的私有域或共享范围不一致", "profile_scope_conflict");
  }
  private invalidate(...ids: string[]) {
    const convs = new Set<string>();
    for (const id of ids) {
      for (const row of this.store.all(
        "SELECT conversation_id FROM profile_identities WHERE person_id=? AND revoked_at IS NULL UNION SELECT r.conversation_id FROM profile_context_uses u JOIN runs r ON r.id=u.run_id WHERE u.person_id=?",
        id,
        id,
      ))
        convs.add(String(row.conversation_id));
    }
    for (const c of convs) this.runtime.invalidateConversation(c);
  }
  changed(id: string) {
    const p = this.store.get(
      "SELECT * FROM records WHERE id=? AND kind='people' AND deleted_at IS NULL",
      id,
    );
    if (p) {
      if (
        !this.store.get(
          "SELECT person_id FROM person_profiles WHERE person_id=?",
          id,
        ) &&
        this.store.get("SELECT id FROM admins WHERE id=?", String(p.owner_id))
      )
        this.adopt(p, String(p.owner_id));
      const m = this.store.get(
        "SELECT manager_id FROM person_profiles WHERE person_id=?",
        id,
      );
      if (m)
        this.store.run(
          "INSERT OR IGNORE INTO person_name_versions VALUES(?,?,?,?,?,?)",
          id,
          Number(p.revision),
          String(parse(p.payload).name),
          null,
          String(m.manager_id),
          now(),
        );
      this.invalidate(id);
    }
  }
  rename(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, ["name", "notes", "reason"]),
      name = this.clean(b.name, "人物名称", 128),
      notes = this.clean(b.notes ?? "", "人工备注", 8000, 0),
      reason = this.clean(b.reason, "更正依据");
    return this.store.transaction(() => {
      const p = this.person(actor, id, true);
      this.check(p, expected);
      this.store.run(
        "UPDATE records SET payload=?,revision=revision+1,updated_at=? WHERE id=?",
        JSON.stringify({ name, notes, reviewed: true }),
        now(),
        id,
      );
      this.store.run(
        "INSERT INTO person_name_versions VALUES(?,?,?,?,?,?)",
        id,
        expected + 1,
        name,
        reason,
        actor,
        now(),
      );
      this.invalidate(id);
      return this.detail(actor, id);
    });
  }
  private assertionOut(a: Row) {
    return {
      ...a,
      value: a.value_json ? parse(a.value_json) : null,
      value_json: undefined,
      ref_json: undefined,
      source: parse(a.ref_json),
      expired: a.valid_until !== null && Number(a.valid_until) <= now(),
      not_started: a.valid_from !== null && Number(a.valid_from) > now(),
    };
  }
  detail(actor: string, id: string) {
    this.refresh();
    const p = this.person(actor, id),
      managed = p.manager_id === actor || p.owner_id === actor;
    const assertions = this.store
      .all(
        "SELECT * FROM profile_assertions WHERE person_id=? AND deleted_at IS NULL AND (?=1 OR status='approved') ORDER BY created_at,id LIMIT 500",
        id,
        managed ? 1 : 0,
      )
      .map((a) => this.assertionOut(a));
    const edges = this.store
      .all(
        "SELECT * FROM profile_edges WHERE (from_person_id=? OR to_person_id=?) AND deleted_at IS NULL AND (?=1 OR status='approved') ORDER BY created_at,id LIMIT 500",
        id,
        id,
        managed ? 1 : 0,
      )
      .filter((e) => {
        try {
          this.person(actor, String(e.from_person_id));
          this.person(actor, String(e.to_person_id));
          return true;
        } catch {
          return false;
        }
      })
      .map((e) => ({ ...e, ref_json: undefined, source: parse(e.ref_json) }));
    const group = this.store.get(
      "SELECT g.* FROM profile_identity_groups g JOIN profile_group_members m ON m.group_id=g.id WHERE m.person_id=?",
      id,
    );
    const members = group
      ? this.store
          .all(
            "SELECT person_id FROM profile_group_members WHERE group_id=?",
            String(group.id),
          )
          .flatMap((m) => {
            try {
              return [this.public(this.person(actor, String(m.person_id)))];
            } catch {
              return [];
            }
          })
      : [];
    return {
      ...this.public(p),
      can_manage: managed,
      assertions,
      edges,
      identities: managed
        ? this.store.all(
            "SELECT * FROM profile_identities WHERE person_id=? ORDER BY verified_at DESC LIMIT 100",
            id,
          )
        : [],
      name_history: managed
        ? this.store.all(
            "SELECT * FROM person_name_versions WHERE person_id=? ORDER BY revision DESC LIMIT 100",
            id,
          )
        : [],
      group: group
        ? {
            id: group.id,
            revision: group.revision,
            members,
            permissions_combined: false,
            affinity_combined: false,
          }
        : null,
      contact: managed
        ? (this.store.get(
            "SELECT * FROM profile_contact_permissions WHERE person_id=?",
            id,
          ) ?? { revision: 0, status: "not_granted" })
        : { status: "not_granted" },
      delivery_available: false,
    };
  }
  history(actor: string, kind: "assertion" | "edge", id: string) {
    this.refresh();
    const table = kind === "assertion" ? "profile_assertions" : "profile_edges",
      a = this.store.get(`SELECT * FROM ${table} WHERE id=?`, id);
    if (!a) throw missing();
    this.person(
      actor,
      String(kind === "assertion" ? a.person_id : a.from_person_id),
      true,
    );
    const items =
      kind === "assertion"
        ? this.store
            .all(
              "SELECT * FROM profile_assertion_versions WHERE assertion_id=? ORDER BY revision DESC LIMIT 100",
              id,
            )
            .map((v) => ({
              ...v,
              value: v.value_json ? parse(v.value_json) : null,
              value_json: undefined,
            }))
        : this.store.all(
            "SELECT * FROM profile_edge_versions WHERE edge_id=? ORDER BY revision DESC LIMIT 100",
            id,
          );
    return { items };
  }
  propose(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, [
        "field",
        "value",
        "statement_kind",
        "source",
        "reason",
        "valid_from",
        "valid_until",
      ]),
      field = string(b.field, "字段", 24, 1),
      v = profileValue(field, b.value),
      reason = this.clean(b.reason, "来源说明"),
      statement = this.statement(b.statement_kind);
    this.clean(JSON.stringify(v), "字段值", 4000);
    const from =
        b.valid_from == null
          ? null
          : integer(b.valid_from, "生效时间", 0, 4102444800),
      until =
        b.valid_until == null
          ? null
          : integer(b.valid_until, "失效时间", 1, 4102444800);
    if (until !== null && until <= (from ?? now()))
      throw bad("失效时间须晚于生效时间");
    return this.store.transaction(() => {
      const p = this.person(actor, id, true);
      this.check(p, expected);
      const source = this.sources.resolve(actor, b.source);
      this.scope(p, source);
      if (
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM profile_assertions WHERE person_id=? AND deleted_at IS NULL",
            id,
          )!.n,
        ) >= 500
      )
        throw bad("单个人物最多500条现存字段陈述");
      const aid = randomUUID(),
        at = now();
      this.store.run(
        "INSERT INTO profile_assertions VALUES(?,?,?,?,1,'pending',?,?,?,?,?,?,?,?,?,?,?,?,?,NULL)",
        aid,
        id,
        field,
        JSON.stringify(v),
        statement,
        source.ref.type,
        source.ref.id,
        source.ref.revision,
        source.ref.policy_id,
        JSON.stringify(source.ref),
        reason,
        String(p.owner_id),
        String(p.visibility),
        from,
        until,
        at,
        at,
      );
      this.store.run(
        "INSERT INTO profile_assertion_versions VALUES(?,1,?,'pending',?,?,?)",
        aid,
        JSON.stringify(v),
        reason,
        actor,
        at,
      );
      this.bump(id);
      return this.detail(actor, id);
    });
  }
  private statement(value: unknown) {
    const s = string(value, "陈述类型", 24, 1);
    if (!["self_report", "third_party", "inference", "confirmed"].includes(s))
      throw bad("须标明本人陈述、转述、推测或已核实");
    return s;
  }
  review(
    actor: string,
    kind: "assertion" | "edge",
    id: string,
    expected: number,
    value: unknown,
  ) {
    const b = fields(value, ["action", "reason", "supersedes"]),
      action = string(b.action, "审核操作", 16, 1),
      reason = this.clean(b.reason, "审核依据");
    if (!["approve", "reject", "forget"].includes(action))
      throw bad("审核操作无效");
    if (
      !Array.isArray(b.supersedes ?? []) ||
      ((b.supersedes as unknown[] | undefined) ?? []).length > 20
    )
      throw bad("替代项最多20条");
    const supersedes = ((b.supersedes as unknown[] | undefined) ?? []).map(
      (v) => {
        const r = fields(v, ["id", "revision"]);
        return {
          id: string(r.id, "替代ID", 36, 36),
          revision: integer(r.revision, "替代版本", 1, Number.MAX_SAFE_INTEGER),
        };
      },
    );
    const table = kind === "assertion" ? "profile_assertions" : "profile_edges",
      key = kind === "assertion" ? "person_id" : "from_person_id";
    return this.store.transaction(() => {
      const a = this.store.get(
        `SELECT * FROM ${table} WHERE id=? AND deleted_at IS NULL`,
        id,
      );
      if (!a) throw missing();
      const p = this.person(actor, String(a[key]), true);
      if (a.revision !== expected) throw conflict();
      if (kind === "edge") this.person(actor, String(a.to_person_id), true);
      if (action === "approve") {
        if (a.status !== "pending") throw bad("只能批准待审核陈述");
        this.scope(p, this.sources.read(actor, parse(a.ref_json)));
        if (
          a.valid_until !== undefined &&
          a.valid_until !== null &&
          Number(a.valid_until) <= now()
        )
          throw bad("不能批准已到期陈述");
        const others =
          kind === "assertion"
            ? this.store.all(
                "SELECT * FROM profile_assertions WHERE person_id=? AND field=? AND status='approved' AND deleted_at IS NULL",
                String(p.id),
                String(a.field),
              )
            : this.store.all(
                "SELECT * FROM profile_edges WHERE from_person_id=? AND to_person_id=? AND status='approved' AND deleted_at IS NULL",
                String(a.from_person_id),
                String(a.to_person_id),
              );
        if (
          others.length !== supersedes.length ||
          others.some(
            (o) =>
              !supersedes.some(
                (s) => s.id === o.id && s.revision === o.revision,
              ),
          )
        )
          throw new ApiError(
            409,
            "profile_conflict",
            "已有批准陈述，须逐项核对替代ID和当前版本",
          );
        for (const other of others)
          this.setStatus(kind, other, "superseded", reason, actor);
      } else if (supersedes.length) throw bad("此操作不使用替代列表");
      this.setStatus(
        kind,
        a,
        action === "approve"
          ? "approved"
          : action === "reject"
            ? "rejected"
            : "forgotten",
        reason,
        actor,
        action === "forget",
      );
      this.bump(String(p.id));
      if (kind === "edge") this.bump(String(a.to_person_id));
      this.invalidate(
        String(p.id),
        ...(kind === "edge" ? [String(a.to_person_id)] : []),
      );
      this.store.audit(
        actor,
        "relationships",
        "completed",
        "人物陈述审核状态已更新，陈述类型保持原标记",
        id,
      );
      return this.detail(actor, String(p.id));
    });
  }
  private setStatus(
    kind: "assertion" | "edge",
    a: Row,
    status: string,
    reason: string | null,
    actor: string,
    erase = false,
  ) {
    const table = kind === "assertion" ? "profile_assertions" : "profile_edges",
      versions =
        kind === "assertion"
          ? "profile_assertion_versions"
          : "profile_edge_versions",
      key = kind === "assertion" ? "assertion_id" : "edge_id",
      column = kind === "assertion" ? "value_json" : "relationship_type",
      rev = Number(a.revision) + 1,
      at = now();
    this.store.run(
      `UPDATE ${table} SET status=?,revision=?,${column}=?,reason=?,deleted_at=?,updated_at=? WHERE id=?`,
      status,
      rev,
      erase ? null : (a[column] ?? null),
      erase ? null : reason,
      erase ? at : null,
      at,
      String(a.id),
    );
    if (erase)
      this.store.run(
        `UPDATE ${versions} SET ${column}=NULL,reason=NULL WHERE ${key}=?`,
        String(a.id),
      );
    this.store.run(
      `INSERT INTO ${versions} VALUES(?,?,?,?,?,?,?)`,
      String(a.id),
      rev,
      erase ? null : (a[column] ?? null),
      status,
      erase ? null : reason,
      actor,
      at,
    );
  }
  edge(actor: string, expected: number, value: unknown) {
    const b = fields(value, [
        "from_id",
        "to_id",
        "expected_to_revision",
        "relationship_type",
        "statement_kind",
        "source",
        "reason",
      ]),
      from = string(b.from_id, "起点", 36, 36),
      to = string(b.to_id, "终点", 36, 36),
      type = this.clean(b.relationship_type, "有向关系", 200),
      reason = this.clean(b.reason, "关系依据"),
      statement = this.statement(b.statement_kind);
    if (from === to) throw bad("起终点须不同");
    return this.store.transaction(() => {
      const p = this.person(actor, from, true),
        q = this.person(actor, to, true);
      this.check(p, expected);
      this.check(
        q,
        integer(b.expected_to_revision, "终点版本", 1, Number.MAX_SAFE_INTEGER),
      );
      if (p.owner_id !== q.owner_id || p.visibility !== q.visibility)
        throw bad("关系端点须在同一可见域，跨账号请建立身份关联组");
      if (
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM profile_edges WHERE from_person_id=? AND deleted_at IS NULL",
            from,
          )!.n,
        ) >= 500
      )
        throw bad("单人物最多500条有向关系");
      const s = this.sources.resolve(actor, b.source);
      this.scope(p, s);
      const id = randomUUID(),
        at = now();
      this.store.run(
        "INSERT INTO profile_edges VALUES(?,?,?,?,1,'pending',?,?,?,?,?,?,?,?,?,?,?,NULL,NULL)",
        id,
        from,
        to,
        type,
        statement,
        s.ref.type,
        s.ref.id,
        s.ref.revision,
        s.ref.policy_id,
        JSON.stringify(s.ref),
        reason,
        String(p.owner_id),
        String(p.visibility),
        at,
        at,
      );
      this.store.run(
        "INSERT INTO profile_edge_versions VALUES(?,1,?,'pending',?,?,?)",
        id,
        type,
        reason,
        actor,
        at,
      );
      this.bump(from);
      this.bump(to);
      return this.detail(actor, from);
    });
  }
  bind(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, [
        "conversation_id",
        "alias",
        "reason",
        "confirmed",
      ]),
      conversation = string(b.conversation_id, "会话", 36, 36),
      alias = this.clean(b.alias ?? "", "账号别名", 128, 0),
      reason = this.clean(b.reason, "绑定依据");
    if (b.confirmed !== true) throw bad("账号绑定须明确确认");
    return this.store.transaction(() => {
      const p = this.person(actor, id, true);
      this.check(p, expected);
      const c = this.store.get(
        "SELECT * FROM conversations WHERE id=?",
        conversation,
      );
      if (!c) throw missing();
      this.knowledge.source(actor, String(c.source_key ?? c.id));
      if (
        p.visibility !== "private" ||
        c.owner_id !== p.owner_id ||
        /(?:^|:)(?:group|guild|channel)(?::|$)/i.test(
          String(c.owner_id) + " " + String(c.source_key ?? ""),
        )
      )
        throw bad("只能绑定同域的私聊/本人会话，群或频道不能代表一个人物");
      if (
        this.store.get(
          "SELECT id FROM profile_identities WHERE conversation_id=? AND revoked_at IS NULL",
          conversation,
        )
      )
        throw bad("此会话已有绑定，请先撤销");
      this.store.run(
        "INSERT INTO profile_identities VALUES(?,?,?,?,1,?,?,?,NULL)",
        randomUUID(),
        id,
        conversation,
        String(p.owner_id),
        alias,
        reason,
        now(),
      );
      this.bump(id);
      this.runtime.invalidateConversation(conversation);
      this.store.audit(
        actor,
        "relationships",
        "completed",
        "已独立核对人物账号身份，未授予联系权限",
        id,
      );
      return this.detail(actor, id);
    });
  }
  unbind(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, [
        "identity_id",
        "expected_identity_revision",
        "reason",
      ]),
      identity = string(b.identity_id, "绑定ID", 36, 36);
    this.clean(b.reason, "撤销依据");
    return this.store.transaction(() => {
      const p = this.person(actor, id, true);
      this.check(p, expected);
      const binding = this.store.get(
        "SELECT * FROM profile_identities WHERE id=? AND person_id=? AND revoked_at IS NULL",
        identity,
        id,
      );
      if (!binding) throw missing();
      if (binding.revision !== b.expected_identity_revision) throw conflict();
      this.store.run(
        "UPDATE profile_identities SET revoked_at=?,revision=revision+1,alias=NULL,reason=NULL WHERE id=?",
        now(),
        identity,
      );
      this.onIdentityChanged?.(id);
      this.store.run(
        "UPDATE profile_contact_permissions SET status='revoked',revision=revision+1,evidence=NULL,updated_at=? WHERE person_id=?",
        now(),
        id,
      );
      this.bump(id);
      this.runtime.invalidateConversation(String(binding.conversation_id));
      this.store.audit(
        actor,
        "relationships",
        "completed",
        "已撤销人物身份绑定及其直接计分归因",
        id,
      );
      return this.detail(actor, id);
    });
  }
  group(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, [
        "other_id",
        "expected_other_revision",
        "expected_group_revision",
        "reason",
        "confirmed",
      ]),
      other = string(b.other_id, "关联人物", 36, 36),
      reason = this.clean(b.reason, "关联依据");
    if (b.confirmed !== true || id === other)
      throw bad("请选择不同人物并明确确认关联");
    return this.store.transaction(() => {
      const p = this.person(actor, id, true),
        q = this.person(actor, other, true);
      this.check(p, expected);
      this.check(
        q,
        integer(
          b.expected_other_revision,
          "另一人物版本",
          1,
          Number.MAX_SAFE_INTEGER,
        ),
      );
      if (
        this.store.get(
          "SELECT alias_id FROM person_aliases WHERE alias_id IN (?,?) OR person_id IN (?,?)",
          id,
          other,
          id,
          other,
        )
      )
        throw bad("请先拆开人物合并映射");
      const a = this.store.get(
          "SELECT group_id FROM profile_group_members WHERE person_id=?",
          id,
        ),
        c = this.store.get(
          "SELECT group_id FROM profile_group_members WHERE person_id=?",
          other,
        );
      if (a && c) throw bad("不自动合并两个已有身份组");
      let gid: string;
      if (a || c) {
        gid = String((a ?? c)!.group_id);
        const g = this.store.get(
          "SELECT revision FROM profile_identity_groups WHERE id=?",
          gid,
        )!;
        if (g.revision !== b.expected_group_revision) throw conflict();
        if (
          Number(
            this.store.get(
              "SELECT count(*) AS n FROM profile_group_members WHERE group_id=?",
              gid,
            )!.n,
          ) >= 10
        )
          throw bad("身份组最多10人");
        this.store.run(
          "UPDATE profile_identity_groups SET revision=revision+1,updated_at=? WHERE id=?",
          now(),
          gid,
        );
      } else {
        if (b.expected_group_revision !== 0) throw conflict();
        gid = randomUUID();
        this.store.run(
          "INSERT INTO profile_identity_groups VALUES(?,1,?,?)",
          gid,
          now(),
          now(),
        );
      }
      for (const pid of [id, other])
        this.store.run(
          "INSERT OR IGNORE INTO profile_group_members VALUES(?,?,?)",
          pid,
          gid,
          now(),
        );
      this.store.run(
        "INSERT INTO profile_group_history(group_id,person_id,other_person_id,action,reason,actor_id,created_at) VALUES(?,?,?,'link',?,?,?)",
        gid,
        id,
        other,
        reason,
        actor,
        now(),
      );
      this.bump(id);
      this.bump(other);
      this.invalidate(id, other);
      return this.detail(actor, id);
    });
  }
  ungroup(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, ["expected_group_revision", "reason", "confirmed"]),
      reason = this.clean(b.reason, "拆分依据");
    if (b.confirmed !== true) throw bad("拆分身份组须确认");
    return this.store.transaction(() => {
      const p = this.person(actor, id, true);
      this.check(p, expected);
      const g = this.store.get(
        "SELECT g.* FROM profile_identity_groups g JOIN profile_group_members m ON m.group_id=g.id WHERE m.person_id=?",
        id,
      );
      if (!g) throw missing();
      if (g.revision !== b.expected_group_revision) throw conflict();
      const members = this.store.all(
        "SELECT person_id FROM profile_group_members WHERE group_id=?",
        String(g.id),
      );
      this.store.run("DELETE FROM profile_group_members WHERE person_id=?", id);
      if (members.length === 2)
        this.store.run(
          "DELETE FROM profile_group_members WHERE group_id=?",
          String(g.id),
        );
      this.store.run(
        "UPDATE profile_identity_groups SET revision=revision+1,updated_at=? WHERE id=?",
        now(),
        String(g.id),
      );
      this.store.run(
        "INSERT INTO profile_group_history(group_id,person_id,action,reason,actor_id,created_at) VALUES(?,?,'unlink',?,?,?)",
        String(g.id),
        id,
        reason,
        actor,
        now(),
      );
      for (const m of members) this.bump(String(m.person_id));
      this.invalidate(...members.map((m) => String(m.person_id)));
      return this.detail(actor, id);
    });
  }
  contact(actor: string, id: string, expected: number, value: unknown) {
    const b = fields(value, [
        "status",
        "conversation_id",
        "evidence",
        "expires_at",
      ]),
      status = string(b.status, "联系状态", 24, 1);
    if (!["not_granted", "pending_verification", "revoked"].includes(status))
      throw bad("此渠道尚无可验证的主动联系许可，只能记录待验证或撤销");
    return this.store.transaction(() => {
      this.person(actor, id, true);
      const old = this.store.get(
        "SELECT * FROM profile_contact_permissions WHERE person_id=?",
        id,
      );
      if (Number(old?.revision ?? 0) !== expected) throw conflict();
      const conversation =
        b.conversation_id == null
          ? null
          : string(b.conversation_id, "会话", 36, 36);
      if (
        status === "pending_verification" &&
        (!conversation ||
          !this.store.get(
            "SELECT id FROM profile_identities WHERE person_id=? AND conversation_id=? AND revoked_at IS NULL",
            id,
            conversation,
          ))
      )
        throw bad("待验证联系须指向当前已核对的绑定会话");
      const evidence =
          status === "pending_verification"
            ? this.clean(b.evidence, "许可证据")
            : null,
        expires =
          b.expires_at == null
            ? null
            : integer(b.expires_at, "到期时间", now() + 1, 4102444800);
      this.store.run(
        "INSERT INTO profile_contact_permissions VALUES(?,?,?,?,?,?,?) ON CONFLICT(person_id) DO UPDATE SET revision=excluded.revision,status=excluded.status,conversation_id=excluded.conversation_id,evidence=excluded.evidence,expires_at=excluded.expires_at,updated_at=excluded.updated_at",
        id,
        expected + 1,
        status,
        conversation,
        evidence,
        expires,
        now(),
      );
      return this.detail(actor, id);
    });
  }
  invalidateSource(type: string, id: string, revision: number) {
    this.advanceDeletionEpoch();
    this.store.run(
      "INSERT INTO profile_source_tombstones VALUES(?,?,?,?) ON CONFLICT(source_type,source_id) DO UPDATE SET source_revision=max(source_revision,excluded.source_revision),invalidated_at=excluded.invalidated_at",
      type,
      id,
      revision,
      now(),
    );
    this.onAffinitySourceInvalidated?.(type, id, revision);
    this.eraseWhere("source_type=? AND source_id=? AND source_revision<=?", [
      type,
      id,
      revision,
    ]);
  }
  invalidatePolicy(id: string) {
    this.advanceDeletionEpoch();
    this.eraseWhere("source_policy_id=?", [id]);
  }
  private eraseWhere(where: string, args: (string | number)[]) {
    const people = new Set<string>();
    for (const [kind, table] of [
      ["assertion", "profile_assertions"],
      ["edge", "profile_edges"],
    ] as const) {
      for (const a of this.store.all(
        `SELECT * FROM ${table} WHERE deleted_at IS NULL AND (${where})`,
        ...args,
      )) {
        this.setStatus(kind, a, "invalidated", null, "system", true);
        people.add(
          String(kind === "assertion" ? a.person_id : a.from_person_id),
        );
        if (kind === "edge") people.add(String(a.to_person_id));
      }
    }
    for (const p of people) this.bump(p);
    this.invalidate(...people);
  }
  refresh() {
    if (this.refreshing) return;
    this.refreshing = true;
    try {
      for (const [kind, table] of [
        ["assertion", "profile_assertions"],
        ["edge", "profile_edges"],
      ] as const) {
        for (const a of this.store.all(
          `SELECT a.*,p.manager_id FROM ${table} a JOIN person_profiles p ON p.person_id=a.${kind === "assertion" ? "person_id" : "from_person_id"} WHERE a.deleted_at IS NULL AND a.status IN ('approved','pending','superseded','rejected')`,
        )) {
          if (a.source_type !== "manual") {
            try {
              this.sources.read(String(a.manager_id), parse(a.ref_json));
            } catch {
              this.eraseWhere("id=?", [String(a.id)]);
              continue;
            }
          }
          if (
            kind === "assertion" &&
            a.status === "approved" &&
            ((a.valid_until !== null && Number(a.valid_until) <= now()) ||
              (a.valid_from !== null && Number(a.valid_from) <= now()))
          ) {
            const marker =
              a.valid_until !== null && Number(a.valid_until) <= now()
                ? "expired"
                : "started";
            if (
              !this.store.get(
                "SELECT id FROM profile_expiry_marks WHERE kind=? AND id=? AND revision=?",
                marker,
                String(a.id),
                Number(a.revision),
              )
            ) {
              this.store.run(
                "INSERT INTO profile_expiry_marks VALUES(?,?,?)",
                marker,
                String(a.id),
                Number(a.revision),
              );
              this.invalidate(String(a.person_id));
            }
          }
        }
      }
      this.store.run(
        "UPDATE profile_contact_permissions SET status='revoked',revision=revision+1,evidence=NULL,updated_at=? WHERE expires_at IS NOT NULL AND expires_at<=? AND status='pending_verification'",
        now(),
        now(),
      );
    } finally {
      this.refreshing = false;
    }
  }
  sourceChanged(change?: { action: string; id: string }) {
    if (!change || change.action === "created") return;
    const r = this.store.get(
      "SELECT kind,revision FROM records WHERE id=?",
      change.id,
    );
    if (r?.kind === "memory")
      this.invalidateSource(
        "memory",
        change.id,
        Number(r.revision) - (change.action === "corrected" ? 1 : 0),
      );
    if (r?.kind === "people") {
      if (change.action === "forgotten") this.forget(change.id);
      else this.changed(change.id);
    }
  }
  forget(id: string) {
    const p = this.store.get(
      "SELECT person_id FROM person_profiles WHERE person_id=?",
      id,
    );
    if (!p) return;
    this.advanceDeletionEpoch();
    this.invalidate(id);
    for (const a of this.store.all(
      "SELECT * FROM profile_assertions WHERE person_id=? AND deleted_at IS NULL",
      id,
    ))
      this.setStatus("assertion", a, "forgotten", null, "system", true);
    for (const e of this.store.all(
      "SELECT * FROM profile_edges WHERE deleted_at IS NULL AND (from_person_id=? OR to_person_id=?)",
      id,
      id,
    )) {
      this.setStatus("edge", e, "forgotten", null, "system", true);
      this.invalidate(String(e.from_person_id), String(e.to_person_id));
    }
    this.store.run(
      "UPDATE person_name_versions SET name=NULL,reason=NULL WHERE person_id=?",
      id,
    );
    this.store.run(
      "UPDATE person_profiles SET reason=NULL WHERE person_id=?",
      id,
    );
    this.store.run(
      "UPDATE profile_identities SET revoked_at=coalesce(revoked_at,?),revision=revision+1,alias=NULL,reason=NULL WHERE person_id=?",
      now(),
      id,
    );
    this.store.run(
      "UPDATE profile_contact_permissions SET status='revoked',revision=revision+1,evidence=NULL,conversation_id=NULL WHERE person_id=?",
      id,
    );
    this.store.run(
      "UPDATE profile_group_history SET reason=NULL WHERE person_id=? OR other_person_id=?",
      id,
      id,
    );
    const g = this.store.get(
      "SELECT group_id FROM profile_group_members WHERE person_id=?",
      id,
    );
    if (g) {
      const members = this.store.all(
        "SELECT person_id FROM profile_group_members WHERE group_id=?",
        String(g.group_id),
      );
      this.store.run("DELETE FROM profile_group_members WHERE person_id=?", id);
      if (members.length === 2)
        this.store.run(
          "DELETE FROM profile_group_members WHERE group_id=?",
          String(g.group_id),
        );
      this.store.run(
        "UPDATE profile_identity_groups SET revision=revision+1,updated_at=? WHERE id=?",
        now(),
        String(g.group_id),
      );
      this.invalidate(...members.map((m) => String(m.person_id)));
    }
  }
  native(
    conversation: string,
    owner: string,
  ): { text: string; uses: ProfileUse[] } {
    this.refresh();
    const binding = this.store.get(
      "SELECT * FROM profile_identities WHERE conversation_id=? AND owner_id=? AND revoked_at IS NULL",
      conversation,
      owner,
    );
    if (!binding) return { text: "", uses: [] };
    const visible = (id: string) =>
      this.store.get(
        "SELECT r.*,p.manager_id FROM records r JOIN person_profiles p ON p.person_id=r.id WHERE r.id=? AND r.kind='people' AND r.deleted_at IS NULL AND (r.owner_id=? OR r.visibility='shared')",
        id,
        owner,
      );
    const root = visible(String(binding.person_id));
    if (!root) return { text: "", uses: [] };
    const ids = new Set<string>([String(root.id)]),
      group = this.store.get(
        "SELECT group_id FROM profile_group_members WHERE person_id=?",
        String(root.id),
      );
    const mapping = this.store.get(
        "SELECT person_id FROM person_aliases WHERE alias_id=?",
        String(root.id),
      ),
      primary = String(mapping?.person_id ?? root.id);
    if (visible(primary)) ids.add(primary);
    for (const m of this.store.all(
      "SELECT alias_id FROM person_aliases WHERE person_id=?",
      primary,
    ))
      if (visible(String(m.alias_id))) ids.add(String(m.alias_id));
    if (group)
      for (const m of this.store.all(
        "SELECT person_id FROM profile_group_members WHERE group_id=?",
        String(group.group_id),
      ))
        if (visible(String(m.person_id))) ids.add(String(m.person_id));
    const uses: ProfileUse[] = [],
      people: unknown[] = [],
      edges: unknown[] = [];
    let count = 0;
    const permitted = (a: Row, p: Row) => {
      if (a.source_type === "manual") return true;
      try {
        const s = this.sources.read(
          String(p.manager_id),
          parse(a.ref_json),
          true,
        );
        return s.visibility === "shared" || s.owner === owner;
      } catch {
        return false;
      }
    };
    for (const id of ids) {
      const p = visible(id)!;
      const assertions = [];
      uses.push({ person_id: id, person_revision: Number(p.revision) });
      for (const a of this.store.all(
        "SELECT * FROM profile_assertions WHERE person_id=? AND status='approved' AND deleted_at IS NULL AND (valid_from IS NULL OR valid_from<=?) AND (valid_until IS NULL OR valid_until>?) ORDER BY updated_at DESC,id",
        id,
        now(),
        now(),
      )) {
        if (count >= 30) break;
        if (!permitted(a, p)) continue;
        const v = {
          id: a.id,
          revision: a.revision,
          field: a.field,
          value: parse(a.value_json),
          statement_kind: a.statement_kind,
          source: {
            type: a.source_type,
            id: a.source_id,
            revision: a.source_revision,
          },
        };
        if (
          Buffer.byteLength(
            JSON.stringify({
              people: [
                ...people,
                {
                  id,
                  name: parse(p.payload).name,
                  assertions: [...assertions, v],
                },
              ],
              edges,
            }),
          ) > 11000
        )
          continue;
        assertions.push(v);
        count++;
        uses.push({
          person_id: id,
          person_revision: Number(p.revision),
          assertion_id: String(a.id),
          assertion_revision: Number(a.revision),
        });
      }
      people.push({
        id,
        revision: p.revision,
        name: parse(p.payload).name,
        assertions,
      });
      for (const e of this.store.all(
        "SELECT * FROM profile_edges WHERE (from_person_id=? OR to_person_id=?) AND status='approved' AND deleted_at IS NULL ORDER BY updated_at DESC,id",
        id,
        id,
      )) {
        if (edges.length >= 20) break;
        const from = visible(String(e.from_person_id)),
          to = visible(String(e.to_person_id));
        if (
          !from ||
          !to ||
          !permitted(e, p) ||
          uses.some((u) => u.edge_id === e.id)
        )
          continue;
        const item = {
          id: e.id,
          revision: e.revision,
          from: { id: from.id, name: parse(from.payload).name },
          to: { id: to.id, name: parse(to.payload).name },
          relationship_type: e.relationship_type,
          statement_kind: e.statement_kind,
          source: {
            type: e.source_type,
            id: e.source_id,
            revision: e.source_revision,
          },
        };
        if (
          Buffer.byteLength(
            JSON.stringify({ people, edges: [...edges, item] }),
          ) > 11000
        )
          continue;
        edges.push(item);
        for (const endpoint of [from, to])
          uses.push({
            person_id: String(endpoint.id),
            person_revision: Number(endpoint.revision),
            edge_id: String(e.id),
            edge_revision: Number(e.revision),
          });
      }
    }
    const text = JSON.stringify({
      bound_person_id: root.id,
      people,
      edges,
      statement_labels: {
        self_report: "本人陈述，未独立核实",
        third_party: "第三方转述",
        inference: "推测，人工审核不使其成为事实",
        confirmed: "人工明确核实",
      },
      permissions_granted: false,
    });
    return { text, uses };
  }
  recordUses(run: string, uses: ProfileUse[]) {
    for (const u of uses)
      this.store.run(
        "INSERT OR IGNORE INTO profile_context_uses VALUES(?,?,?,?,?,?,?)",
        run,
        u.person_id,
        u.person_revision,
        u.assertion_id ?? "",
        u.assertion_revision ?? 0,
        u.edge_id ?? "",
        u.edge_revision ?? 0,
      );
  }
  private advanceDeletionEpoch() {
    this.store.run(
      "UPDATE metadata SET value=CAST(value AS INTEGER)+1 WHERE key='profile.model.deletion_epoch'",
    );
    this.onModelInvalidated?.();
  }
  generated(
    actor: string,
    id: string,
    ref: ProfileRef,
    output: {
      assertions: {
        field: string;
        value: unknown;
        statement_kind: string;
        reason: string;
      }[];
      edges: {
        to_person_id: string;
        relationship_type: string;
        statement_kind: string;
        reason: string;
      }[];
    },
  ) {
    const p = this.person(actor, id, true),
      assertions: string[] = [],
      edges: string[] = [],
      changed = new Set<string>(),
      at = now();
    for (const a of output.assertions) {
      const value = profileValue(a.field, a.value),
        reason = this.clean(a.reason, "模型字段依据"),
        statement = this.statement(a.statement_kind);
      if (statement === "confirmed") throw bad("模型不能自行核实事实");
      this.clean(JSON.stringify(value), "模型字段", 4000);
      const duplicate = this.store.get(
        "SELECT id FROM profile_assertions WHERE person_id=? AND field=? AND value_json=? AND statement_kind=? AND source_type=? AND source_id=? AND source_revision=? AND status IN ('pending','approved') AND deleted_at IS NULL",
        id,
        a.field,
        JSON.stringify(value),
        statement,
        ref.type,
        ref.id,
        ref.revision,
      );
      if (duplicate) {
        assertions.push(String(duplicate.id));
        continue;
      }
      if (
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM profile_assertions WHERE person_id=? AND deleted_at IS NULL",
            id,
          )!.n,
        ) >= 500
      )
        throw bad("单个人物最多500条现存字段陈述");
      const aid = randomUUID();
      this.store.run(
        "INSERT INTO profile_assertions VALUES(?,?,?,?,1,'pending',?,?,?,?,?,?,?,?,?,NULL,NULL,?,?,NULL)",
        aid,
        id,
        a.field,
        JSON.stringify(value),
        statement,
        ref.type,
        ref.id,
        ref.revision,
        ref.policy_id,
        JSON.stringify(ref),
        reason,
        String(p.owner_id),
        String(p.visibility),
        at,
        at,
      );
      this.store.run(
        "INSERT INTO profile_assertion_versions VALUES(?,1,?,'pending',?,?,?)",
        aid,
        JSON.stringify(value),
        reason,
        actor,
        at,
      );
      assertions.push(aid);
      changed.add(id);
    }
    for (const e of output.edges) {
      const q = this.person(actor, e.to_person_id, true);
      if (
        q.owner_id !== p.owner_id ||
        q.visibility !== p.visibility ||
        q.id === p.id
      )
        throw bad("模型目标范围已变化");
      const type = this.clean(e.relationship_type, "模型关系", 200),
        reason = this.clean(e.reason, "模型关系依据"),
        statement = this.statement(e.statement_kind);
      if (statement === "confirmed") throw bad("模型不能自行核实关系");
      const duplicate = this.store.get(
        "SELECT id FROM profile_edges WHERE from_person_id=? AND to_person_id=? AND relationship_type=? AND statement_kind=? AND source_type=? AND source_id=? AND source_revision=? AND status IN ('pending','approved') AND deleted_at IS NULL",
        id,
        String(q.id),
        type,
        statement,
        ref.type,
        ref.id,
        ref.revision,
      );
      if (duplicate) {
        edges.push(String(duplicate.id));
        continue;
      }
      if (
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM profile_edges WHERE from_person_id=? AND deleted_at IS NULL",
            id,
          )!.n,
        ) >= 500
      )
        throw bad("单人物最多500条有向关系");
      const eid = randomUUID();
      this.store.run(
        "INSERT INTO profile_edges VALUES(?,?,?,?,1,'pending',?,?,?,?,?,?,?,?,?,?,?,NULL,NULL)",
        eid,
        id,
        String(q.id),
        type,
        statement,
        ref.type,
        ref.id,
        ref.revision,
        ref.policy_id,
        JSON.stringify(ref),
        reason,
        String(p.owner_id),
        String(p.visibility),
        at,
        at,
      );
      this.store.run(
        "INSERT INTO profile_edge_versions VALUES(?,1,?,'pending',?,?,?)",
        eid,
        type,
        reason,
        actor,
        at,
      );
      edges.push(eid);
      changed.add(id);
      changed.add(String(q.id));
    }
    for (const id of changed) this.bump(id);
    return {
      assertion_ids: [...new Set(assertions)],
      edge_ids: [...new Set(edges)],
      review_required: true,
    };
  }
  graph(actor: string, query: URLSearchParams) {
    this.refresh();
    const list = this.list(
        actor,
        new URLSearchParams({
          limit: "50",
          offset: query.get("offset") ?? "0",
          q: query.get("q") ?? "",
        }),
      ),
      ids = new Set(list.items.filter((p) => !p.merged_into).map((p) => p.id));
    return {
      ...list,
      items: list.items.filter((p) => ids.has(p.id)),
      edges: this.store
        .all(
          "SELECT * FROM profile_edges WHERE status='approved' AND deleted_at IS NULL ORDER BY updated_at DESC,id",
        )
        .filter((e) => ids.has(e.from_person_id) && ids.has(e.to_person_id))
        .map((e) => ({
          id: e.id,
          revision: e.revision,
          from_id: e.from_person_id,
          to_id: e.to_person_id,
          relationship_type: e.relationship_type,
          statement_kind: e.statement_kind,
        })),
      page_only: true,
      max_nodes: 50,
    };
  }
}
