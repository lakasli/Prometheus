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
import { hash, now } from "./security.ts";
import {
  activity,
  calendarRange,
  DAY,
  expand,
  localText,
  localInstant,
  MAX_ITEMS,
  MAX_TIME,
  zone,
  zoned,
} from "./schedule-time.ts";
import type { Activity } from "./schedule-time.ts";
import type { Row, Store } from "./store.ts";
import type { Runtime } from "./runtime.ts";
import { nextOccurrence } from "./automation.ts";
export type State = "enabled" | "paused" | "cancelled" | "pending_approval";
export interface Exception {
  nominal_at: number;
  definition: Activity;
  state: State;
  revision: number;
}
export interface ScheduleView {
  id: string;
  owner: string;
  revision: number;
  creator: "manual" | "autonomous";
  state: State;
  definition: Activity;
  exceptions: Exception[];
  grant_revision: number | null;
  approval: Record<string, unknown> | null;
  created_at: number;
}
export interface Expanded {
  nominal_at: number;
  definition: Activity;
  state: State;
}
const parse = (value: unknown) => JSON.parse(String(value));
const immutable = new Set(["running", "partial", "completed", "unknown"]);
export const block = (a: Activity): string | null =>
  ({
    reading: null,
    video: "video_provider_unavailable",
    chat: "contact_consent_and_verified_delivery_required",
    action: "registered_action_broker_required",
    consolidation: "consolidation_system_job_only",
    writing: "registered_text_activity_required",
  })[a.type] ?? null;
export class ScheduleCalendar {
  onChanged?: (ids: string[]) => void;
  registeredText?: (id: string, definition: Activity) => boolean;
  routineItems?: (
    owner: string,
    start: number,
    end: number,
  ) => Record<string, any>[];
  protected capability(view: ScheduleView, definition?: Activity) {
    definition ??=
      view.exceptions.find(
        (e) =>
          e.nominal_at === view.definition.start_at && e.state === "enabled",
      )?.definition ?? view.definition;
    return definition.type === "writing" &&
      this.registeredText?.(view.id, definition)
      ? null
      : block(definition);
  }
  constructor(
    readonly store: Store,
    readonly runtime: Runtime,
  ) {}
  protected load(owner: string, id: string): ScheduleView {
    const row = this.store.get(
      "SELECT * FROM schedule_activities WHERE id=? AND owner_id=?",
      id,
      owner,
    );
    if (!row) throw missing();
    return this.view(row);
  }
  protected view(row: Row): ScheduleView {
    return {
      id: String(row.id),
      owner: String(row.owner_id),
      revision: Number(row.revision),
      creator: String(row.creator) as ScheduleView["creator"],
      state: String(row.state) as State,
      definition: parse(row.definition_json),
      exceptions: this.store
        .all(
          "SELECT * FROM schedule_exceptions WHERE activity_id=? ORDER BY nominal_at LIMIT 501",
          String(row.id),
        )
        .map((item) => ({
          nominal_at: Number(item.nominal_at),
          definition: parse(item.definition_json),
          state: String(item.state) as State,
          revision: Number(item.revision),
        })),
      grant_revision:
        row.grant_revision === null ? null : Number(row.grant_revision),
      approval: row.approved_json ? parse(row.approved_json) : null,
      created_at: Number(row.created_at),
    };
  }
  protected references(owner: string, a: Activity) {
    if (
      a.resource_id &&
      !this.store.get(
        "SELECT id FROM schedule_resources WHERE id=? AND owner_id=? AND revoked_at IS NULL AND content IS NOT NULL",
        a.resource_id,
        owner,
      )
    )
      throw bad("资料不存在、已撤销或不属于当前账户", "resource_unavailable");
    if (
      a.grant_id &&
      !this.store.get(
        "SELECT id FROM schedule_grants WHERE id=? AND owner_id=? AND revoked_at IS NULL",
        a.grant_id,
        owner,
      )
    )
      throw bad("自主授权不存在、已撤销或不属于当前账户", "grant_unavailable");
  }
  protected permission(
    owner: string,
    a: Activity,
    creator: ScheduleView["creator"],
    _exclude?: string,
  ): { allowed: boolean; revision: number | null; reason: string | null } {
    this.references(owner, a);
    return {
      allowed: creator === "manual",
      revision: null,
      reason: creator === "manual" ? null : "autonomy_grant_required",
    };
  }
  protected chargeMutation(
    _view: ScheduleView,
    _kind: "create" | "change",
    _autonomous = false,
  ) {}
  protected digest(view: ScheduleView) {
    return hash(
      JSON.stringify({
        definition: view.definition,
        exceptions: view.exceptions.map(
          ({ nominal_at, definition, state }) => ({
            nominal_at,
            definition,
            state,
          }),
        ),
      }),
    );
  }
  protected approved(view: ScheduleView) {
    return view.approval?.definition_hash === this.digest(view);
  }
  protected expanded(
    view: ScheduleView,
    start: number,
    end: number,
  ): Expanded[] {
    if (view.exceptions.length > MAX_ITEMS)
      throw new ApiError(
        409,
        "calendar_expansion_limit",
        "例外数量超过上限，请分段整理",
      );
    const map = new Map<number, Expanded>();
    const effectiveState = (state: State) =>
      view.state === "enabled" ? state : view.state;
    for (const slot of expand(view.definition, start, end)) {
      const override = view.exceptions.find(
        (item) => item.nominal_at === slot.nominal_at,
      );
      const definition = override?.definition ?? {
        ...view.definition,
        start_at: slot.start_at,
        end_at: slot.end_at,
        repeat: null,
      };
      if (definition.start_at < end && definition.end_at > start)
        map.set(slot.nominal_at, {
          nominal_at: slot.nominal_at,
          definition,
          state: effectiveState(override?.state ?? view.state),
        });
    }
    // A moved occurrence is selected by its effective interval, preserving its original nominal key.
    for (const item of view.exceptions)
      if (item.definition.start_at < end && item.definition.end_at > start)
        map.set(item.nominal_at, {
          nominal_at: item.nominal_at,
          definition: item.definition,
          state: effectiveState(item.state),
        });
    return [...map.values()].sort(
      (a, b) =>
        a.definition.start_at - b.definition.start_at ||
        a.nominal_at - b.nominal_at,
    );
  }
  private activeViews(exclude: Set<string>): ScheduleView[] {
    const rows = this.store.all(
      "SELECT * FROM schedule_activities WHERE state='enabled' ORDER BY created_at,id LIMIT 2001",
    );
    if (rows.length > 2000)
      throw new ApiError(
        409,
        "schedule_capacity",
        "活动超过首版协调上限，请先归档历史活动",
      );
    return rows
      .filter((row) => !exclude.has(String(row.id)))
      .map((row) => this.view(row));
  }
  protected conflicts(owner: string, changed: ScheduleView[], at = now()) {
    const exclude = new Set(changed.map((view) => view.id)),
      rivals = [
        ...this.activeViews(exclude),
        ...changed.filter((view) => view.state === "enabled"),
      ];
    const found: Record<string, unknown>[] = [],
      seen = new Set<string>();
    let steps = 0;
    const add = (
      otherOwner: string,
      id: string,
      title: string,
      start: number,
      end: number,
      nominal: number,
    ) => {
      const key = `${id}:${nominal}:${start}:${end}`;
      if (seen.has(key)) return;
      seen.add(key);
      found.push(
        otherOwner === owner
          ? {
              origin: id.startsWith("record:") ? "manual_record" : "schedule",
              activity_id: id.replace(/^record:/, ""),
              title,
              start_at: start,
              end_at: end,
              nominal_at: nominal,
            }
          : { origin: "private_busy", start_at: start, end_at: end },
      );
      if (found.length > MAX_ITEMS)
        throw new ApiError(
          409,
          "schedule_conflict_limit",
          "冲突项过多，请缩小规划范围",
        );
    };
    const records = this.store.all(
      "SELECT id,owner_id,payload FROM records WHERE kind='schedule' AND deleted_at IS NULL LIMIT 5001",
    );
    if (records.length > 5000)
      throw new ApiError(409, "schedule_capacity", "人工日程超过首版协调上限");
    for (const candidate of changed.filter(
      (view) => view.state === "enabled",
    )) {
      const begin = Math.max(at, candidate.definition.start_at),
        finish = Math.min(MAX_TIME, begin + 90 * DAY);
      const ours = this.expanded(candidate, begin, finish);
      for (const exception of candidate.exceptions)
        if (
          exception.state === "enabled" &&
          exception.definition.end_at > at &&
          !ours.some((item) => item.nominal_at === exception.nominal_at)
        )
          ours.push(exception);
      for (const item of ours) {
        if (item.state !== "enabled") continue;
        const a = item.definition;
        for (const rival of rivals) {
          if (++steps > 200000)
            throw new ApiError(
              409,
              "schedule_capacity",
              "本次冲突检查量过大，请缩小系列范围",
            );
          for (const other of this.expanded(rival, a.start_at, a.end_at))
            if (
              other.state === "enabled" &&
              !(
                candidate.id === rival.id &&
                item.nominal_at === other.nominal_at
              )
            )
              add(
                rival.owner,
                rival.id,
                other.definition.title,
                other.definition.start_at,
                other.definition.end_at,
                other.nominal_at,
              );
        }
        for (const row of records) {
          const data = parse(row.payload);
          if (
            data.status !== "cancelled" &&
            data.start_at < a.end_at &&
            data.end_at > a.start_at
          )
            add(
              String(row.owner_id),
              `record:${row.id}`,
              data.title,
              data.start_at,
              data.end_at,
              data.start_at,
            );
        }
      }
    }
    return found;
  }
  checkSimple(owner: string, data: Record<string, unknown>, exclude?: string) {
    if (data.status === "cancelled") return;
    const start = Number(data.start_at),
      end = Number(data.end_at);
    for (const view of this.activeViews(new Set()))
      if (
        this.expanded(view, start, end).some((item) => item.state === "enabled")
      )
        throw new ApiError(
          409,
          "schedule_conflict",
          "与已有活动时间冲突，请先核对日历",
        );
    // Legacy simple records remain ordinary manually reviewed notes; their old overlaps are not rewritten.
    void owner;
    void exclude;
  }
  private write(view: ScheduleView, insert: boolean) {
    if (insert)
      this.store.run(
        "INSERT INTO schedule_activities VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        view.id,
        view.owner,
        this.store.assistantId,
        view.revision,
        view.creator,
        view.state,
        JSON.stringify(view.definition),
        view.grant_revision,
        view.approval ? JSON.stringify(view.approval) : null,
        view.created_at,
        now(),
      );
    else
      this.store.run(
        "UPDATE schedule_activities SET revision=?,state=?,definition_json=?,grant_revision=?,approved_json=?,updated_at=? WHERE id=?",
        view.revision,
        view.state,
        JSON.stringify(view.definition),
        view.grant_revision,
        view.approval ? JSON.stringify(view.approval) : null,
        now(),
        view.id,
      );
    this.store.run(
      "DELETE FROM schedule_exceptions WHERE activity_id=?",
      view.id,
    );
    for (const item of view.exceptions)
      this.store.run(
        "INSERT INTO schedule_exceptions VALUES(?,?,?,?,?)",
        view.id,
        item.nominal_at,
        JSON.stringify(item.definition),
        item.state,
        item.revision,
      );
    this.store.run(
      "INSERT INTO schedule_revisions VALUES(?,?,?,?,?,?)",
      view.id,
      view.revision,
      JSON.stringify(view.definition),
      JSON.stringify(view.exceptions),
      view.state,
      now(),
    );
  }
  create(owner: string, value: unknown, key: unknown, autonomous = false) {
    const body = fields(value, ["definition", "state"]),
      definition = activity(body.definition),
      requestKey = string(key, "日程幂等键", 160, 1),
      fingerprint = hash(
        JSON.stringify({
          definition,
          state: body.state ?? "enabled",
          autonomous,
        }),
      );
    const requested = body.state ?? "enabled";
    if (requested !== "enabled" && requested !== "paused")
      throw bad("新活动状态无效");
    let result: unknown;
    this.store.transaction(() => {
      const previous = this.store.get(
        "SELECT * FROM schedule_idempotency WHERE owner_id=? AND request_key=?",
        owner,
        requestKey,
      );
      if (previous) {
        if (previous.request_hash !== fingerprint) throw conflict();
        result = parse(previous.response_json);
        return;
      }
      if (
        Number(
          this.store.get(
            "SELECT count(*) AS n FROM schedule_activities WHERE owner_id=?",
            owner,
          )!.n,
        ) >= 5000
      )
        throw new ApiError(
          409,
          "schedule_capacity",
          "活动数量已达上限，请先整理历史活动",
        );
      const permission = this.permission(
        owner,
        definition,
        autonomous ? "autonomous" : "manual",
      );
      const view: ScheduleView = {
        id: randomUUID(),
        owner,
        revision: 1,
        creator: autonomous ? "autonomous" : "manual",
        state:
          requested === "paused"
            ? "paused"
            : permission.allowed
              ? "enabled"
              : "pending_approval",
        definition,
        exceptions: [],
        grant_revision: permission.revision,
        approval: null,
        created_at: now(),
      };
      if (this.conflicts(owner, [view]).length)
        throw new ApiError(
          409,
          "schedule_conflict",
          "与已有活动时间冲突，请先预演核对",
        );
      this.chargeMutation(view, "create", autonomous);
      this.write(view, true);
      result = {
        id: view.id,
        revision: 1,
        state: view.state,
        creator: view.creator,
        definition: view.definition,
      };
      this.store.run(
        "INSERT INTO schedule_idempotency VALUES(?,?,?,?,?)",
        owner,
        requestKey,
        fingerprint,
        JSON.stringify(result),
        now(),
      );
      this.store.audit(
        owner,
        "schedule",
        "completed",
        view.state === "pending_approval"
          ? "活动已保存待单独批准"
          : "活动已保存，实际执行与投递另行记账",
        view.id,
      );
    });
    return result;
  }
  list(owner: string, query: URLSearchParams) {
    const offset = integer(Number(query.get("offset") ?? 0), "偏移", 0, 100000),
      limit = integer(Number(query.get("limit") ?? 50), "分页大小", 1, 100);
    return {
      items: this.store
        .all(
          "SELECT * FROM schedule_activities WHERE owner_id=? ORDER BY updated_at DESC,rowid DESC LIMIT ? OFFSET ?",
          owner,
          limit,
          offset,
        )
        .map((row) => this.summary(this.view(row))),
      total: Number(
        this.store.get(
          "SELECT count(*) AS n FROM schedule_activities WHERE owner_id=?",
          owner,
        )!.n,
      ),
      offset,
      limit,
    };
  }
  private summary(view: ScheduleView) {
    return {
      id: view.id,
      revision: view.revision,
      creator: view.creator,
      state: view.state,
      definition: view.definition,
      local_start: localText(
        view.definition.start_at,
        view.definition.time_zone,
      ),
      local_end: localText(view.definition.end_at, view.definition.time_zone),
      local_until:
        view.definition.repeat?.until != null
          ? localText(view.definition.repeat.until, view.definition.time_zone)
          : "",
      capability_block: this.capability(view),
      grant_revision: view.grant_revision,
      admin_approval: this.approved(view) ? view.approval : null,
    };
  }
  detail(owner: string, id: string) {
    const view = this.load(owner, id);
    return {
      ...this.summary(view),
      exceptions: view.exceptions,
      occurrences: this.store
        .all(
          "SELECT id,nominal_at,start_at,end_at,status,block_reason,checkpoint,total_chars,resource_hash,reading_generation,result_json,started_at,completed_at FROM schedule_occurrences WHERE activity_id=? AND owner_id=? ORDER BY start_at DESC LIMIT 100",
          id,
          owner,
        )
        .map((row) => ({
          ...row,
          result: row.result_json ? parse(row.result_json) : null,
          result_json: undefined,
        })),
    };
  }
  history(owner: string, id: string) {
    this.load(owner, id);
    return {
      items: this.store
        .all(
          "SELECT * FROM schedule_revisions WHERE activity_id=? ORDER BY revision DESC LIMIT 100",
          id,
        )
        .map((row) => ({
          revision: row.revision,
          definition: parse(row.definition_json),
          exceptions: parse(row.exceptions_json),
          state: row.state,
          created_at: row.created_at,
        })),
    };
  }
  private prepareChange(
    owner: string,
    id: string,
    expected: number,
    value: unknown,
    autonomous = false,
  ) {
    const current = this.load(owner, id);
    if (current.revision !== expected) throw conflict();
    const body = fields(value, [
        "definition",
        "state",
        "scope",
        "nominal_at",
        "reset_future_exceptions",
      ]),
      scope = body.scope ?? "series";
    if (!["series", "single", "following"].includes(String(scope)))
      throw bad("修改范围须为单次、此后或整个系列");
    if (
      autonomous &&
      (current.creator !== "autonomous" || !current.definition.movable)
    )
      throw bad("自主修改不能覆盖人工或锁定活动");
    const next = structuredClone(current);
    next.revision++;
    let requested =
      body.state === undefined
        ? current.state
        : (string(body.state, "活动状态", 30, 1) as State);
    if (
      !["enabled", "paused", "cancelled", "pending_approval"].includes(
        requested,
      )
    )
      throw bad("活动状态无效");
    if (
      current.state === "pending_approval" &&
      body.state !== undefined &&
      requested !== "cancelled"
    )
      throw bad("待审核活动须使用明确批准入口，不能用恢复代替");
    if (body.state === "pending_approval")
      throw bad("待审核状态由宿主权限复核决定");
    const definition =
      body.definition === undefined
        ? structuredClone(current.definition)
        : activity(body.definition);
    this.references(owner, definition);
    const changed: ScheduleView[] = [next];
    let following: ScheduleView | undefined;
    if (scope === "series") {
      if (
        body.definition !== undefined &&
        JSON.stringify(definition) !== JSON.stringify(current.definition)
      ) {
        const future = current.exceptions.filter(
          (item) =>
            item.nominal_at >= now() || item.definition.start_at >= now(),
        );
        if (future.length && body.reset_future_exceptions !== true)
          throw new ApiError(
            409,
            "future_exceptions_confirmation_required",
            "整个系列改期会重置未来例外，请先核对并明确确认",
          );
        next.exceptions = next.exceptions.filter(
          (item) => item.nominal_at < now() && item.definition.start_at < now(),
        );
      }
      next.definition = definition;
      next.state = requested;
    } else {
      const nominal = integer(body.nominal_at, "原计划时刻", 0, MAX_TIME),
        slot = expand(current.definition, nominal, nominal + 1).find(
          (item) => item.nominal_at === nominal,
        );
      if (!slot) throw bad("原计划时刻不属于此系列");
      const oldException = current.exceptions.find(
          (item) => item.nominal_at === nominal,
        ),
        effective = oldException?.definition ?? {
          ...current.definition,
          start_at: slot.start_at,
          end_at: slot.end_at,
          repeat: null,
        };
      if ((scope === "following" ? nominal : effective.start_at) < now())
        throw bad("只可修改尚未开始的机会");
      const rows = this.store.all(
        "SELECT nominal_at,status FROM schedule_occurrences WHERE activity_id=? AND nominal_at>=?",
        id,
        nominal,
      );
      if (
        rows.some(
          (row) =>
            (scope === "following" || row.nominal_at === nominal) &&
            immutable.has(String(row.status)),
        )
      )
        throw new ApiError(
          409,
          "occurrence_immutable",
          "已有实际执行或检查点的机会不能改写",
        );
      if (scope === "single") {
        const changedDefinition =
          body.definition === undefined
            ? effective
            : { ...definition, repeat: null };
        next.exceptions = next.exceptions.filter(
          (item) => item.nominal_at !== nominal,
        );
        next.exceptions.push({
          nominal_at: nominal,
          definition: changedDefinition,
          state:
            body.state === undefined
              ? (oldException?.state ?? current.state)
              : requested,
          revision: next.revision,
        });
        next.exceptions.sort((a, b) => a.nominal_at - b.nominal_at);
      } else {
        if (!current.definition.repeat) throw bad("此后修改需要重复系列");
        const future = current.exceptions.filter(
          (item) => item.nominal_at >= nominal,
        );
        if (
          body.definition !== undefined &&
          future.length &&
          body.reset_future_exceptions !== true
        )
          throw new ApiError(
            409,
            "future_exceptions_confirmation_required",
            "此后改期将重置后续例外，请核对并明确确认",
          );
        next.definition = {
          ...current.definition,
          repeat: {
            ...current.definition.repeat,
            until: Math.min(
              current.definition.repeat.until ?? MAX_TIME,
              nominal - 1,
            ),
          },
        };
        next.exceptions = next.exceptions.filter(
          (item) => item.nominal_at < nominal,
        );
        const followingDefinition =
          body.definition === undefined
            ? {
                ...current.definition,
                start_at: nominal,
                end_at:
                  nominal +
                  (current.definition.end_at - current.definition.start_at),
                repeat: {
                  ...current.definition.repeat,
                  count:
                    current.definition.repeat.count === null
                      ? null
                      : current.definition.repeat.count - slot.ordinal,
                },
              }
            : definition;
        following = {
          ...structuredClone(current),
          id: randomUUID(),
          revision: 1,
          definition: followingDefinition,
          state: requested,
          exceptions: body.definition === undefined ? future : [],
          approval: null,
          created_at: now(),
        };
        changed.push(following);
      }
    }
    for (const view of changed) {
      if (view.approval && !this.approved(view)) view.approval = null;
      if (view.state === "enabled") {
        const permission = this.permission(
          owner,
          scope === "single" && view.id === current.id
            ? view.exceptions.find((e) => e.nominal_at === body.nominal_at)!
                .definition
            : view.definition,
          view.creator,
          view.id,
        );
        view.grant_revision = permission.revision;
        if (!permission.allowed && !this.approved(view))
          view.state = "pending_approval";
        for (const item of view.exceptions)
          if (item.state === "enabled") {
            this.references(owner, item.definition);
            if (view.creator === "autonomous" && !this.approved(view)) {
              const allowed = this.permission(
                owner,
                item.definition,
                "autonomous",
                view.id,
              );
              if (
                !allowed.allowed ||
                item.definition.grant_id !== view.definition.grant_id ||
                allowed.revision !== view.grant_revision
              )
                view.state = "pending_approval";
            }
          }
      }
    }
    return {
      changed,
      following,
      scope,
      conflicts: this.conflicts(owner, changed),
    };
  }
  change(
    owner: string,
    id: string,
    expected: number,
    value: unknown,
    autonomous = false,
    beforeChanged?: () => void,
  ) {
    let result: unknown;
    let affected: string[] = [];
    this.store.transaction(() => {
      const plan = this.prepareChange(owner, id, expected, value, autonomous);
      if (plan.conflicts.length)
        throw new ApiError(
          409,
          "schedule_conflict",
          "改期与已有活动冲突，原计划保持不变",
        );
      for (const view of plan.changed) {
        const effective =
          plan.scope === "single"
            ? view.exceptions.find(
                (e) =>
                  e.nominal_at ===
                  (value as Record<string, unknown>).nominal_at,
              )?.definition
            : undefined;
        this.chargeMutation(
          effective ? { ...view, definition: effective } : view,
          "change",
          autonomous,
        );
        this.write(view, view.id !== id);
        affected.push(view.id);
      }
      // Real history stays immutable; only unstarted future materializations are recalculated.
      this.store.run(
        "DELETE FROM schedule_occurrences WHERE activity_id=? AND start_at>=? AND status IN ('pending','blocked','skipped')",
        id,
        now(),
      );
      this.store.audit(
        owner,
        "schedule",
        "completed",
        plan.following
          ? "系列已截断并创建后续系列"
          : "活动修改已保存，既有执行历史保留",
        id,
      );
      beforeChanged?.();
      result = {
        ...this.detail(owner, id),
        following_id: plan.following?.id ?? null,
      };
    });
    this.onChanged?.(affected);
    return result && typeof result === "object" && "id" in result
      ? { ...result, ...this.detail(owner, String(result.id)) }
      : result;
  }
  preview(owner: string, value: unknown, id?: string, expected?: number) {
    if (id) {
      const result = this.prepareChange(owner, id, expected!, value);
      return {
        read_only: true,
        conflicts: result.conflicts,
        activities: result.changed.map((view) => this.summary(view)),
        occurrences: result.changed.flatMap((view) =>
          this.expanded(
            view,
            view.definition.start_at,
            Math.min(MAX_TIME, view.definition.start_at + 31 * DAY),
          ).slice(0, 100),
        ),
      };
    }
    const body = fields(value, ["definition"]),
      definition = activity(body.definition),
      permission = this.permission(owner, definition, "manual");
    const view: ScheduleView = {
      id: "preview",
      owner,
      revision: 0,
      creator: "manual",
      state: permission.allowed ? "enabled" : "pending_approval",
      definition,
      exceptions: [],
      grant_revision: permission.revision,
      approval: null,
      created_at: now(),
    };
    return {
      read_only: true,
      conflicts: this.conflicts(owner, [view]),
      occurrences: this.expanded(
        view,
        definition.start_at,
        Math.min(MAX_TIME, definition.start_at + 31 * DAY),
      ),
      capability_block: block(definition),
    };
  }
  movePreview(owner: string, id: string, expected: number, value: unknown) {
    const view = this.load(owner, id);
    if (view.revision !== expected) throw conflict();
    const body = fields(value, ["nominal_at", "target_date", "time_zone"]),
      nominal = integer(body.nominal_at, "原计划时刻", 0, MAX_TIME),
      targetDate = string(body.target_date, "目标日期", 10, 10),
      timeZone = zone(body.time_zone);
    const slot = expand(view.definition, nominal, nominal + 1).find(
      (item) => item.nominal_at === nominal,
    );
    if (!slot) throw bad("原计划时刻不属于此系列");
    const original = view.exceptions.find((item) => item.nominal_at === nominal)
      ?.definition ?? {
      ...view.definition,
      start_at: slot.start_at,
      end_at: slot.end_at,
      repeat: null,
    };
    const start = localInstant(
        `${targetDate}T${zoned(original.start_at, timeZone).toPlainTime().toString({ smallestUnit: "second" })}`,
        timeZone,
      ),
      definition = activity({
        ...original,
        start_at: start,
        end_at: start + (original.end_at - original.start_at),
        repeat: null,
      });
    const change = { scope: "single", nominal_at: nominal, definition };
    return {
      read_only: true,
      change,
      local_start: localText(definition.start_at, definition.time_zone),
      local_end: localText(definition.end_at, definition.time_zone),
      preview: this.preview(owner, change, id, expected),
    };
  }
  approve(owner: string, id: string, expected: number, value: unknown) {
    const body = fields(value, ["confirmed", "scope"]);
    if (body.confirmed !== true || body.scope !== "series")
      throw bad("须明确确认当前保存的整个系列及例外");
    this.store.transaction(() => {
      const view = this.load(owner, id);
      if (view.revision !== expected) throw conflict();
      if (view.state !== "pending_approval") throw bad("仅可批准待审核活动");
      view.definition.grant_id = null;
      for (const item of view.exceptions) {
        item.definition.grant_id = null;
        this.references(owner, item.definition);
      }
      this.references(owner, view.definition);
      view.state = "enabled";
      view.grant_revision = null;
      if (this.conflicts(owner, [view]).length)
        throw new ApiError(
          409,
          "schedule_conflict",
          "当前系列存在时间冲突，不能批准",
        );
      if (
        !this.expanded(view, now(), Math.min(MAX_TIME, now() + 90 * DAY)).length
      )
        throw bad("未来90天内没有有效机会");
      view.revision++;
      view.approval = {
        id: randomUUID(),
        actor: owner,
        scope: "series",
        definition_hash: this.digest(view),
        approved_revision: view.revision,
        approved_at: now(),
      };
      this.write(view, false);
      this.store.audit(
        owner,
        "schedule",
        "completed",
        "当前保存系列已明确批准，执行和投递仍受能力门禁",
        id,
      );
    });
    this.onChanged?.([id]);
    return this.detail(owner, id);
  }
  calendar(owner: string, query: URLSearchParams) {
    const timeZone = zone(query.get("time_zone") ?? "Asia/Shanghai"),
      range = query.has("date")
        ? calendarRange(
            query.get("date")!,
            query.get("view") ?? "month",
            timeZone,
          )
        : null;
    const start =
        range?.start ??
        integer(Number(query.get("start")), "范围开始", 0, MAX_TIME),
      end =
        range?.end ??
        integer(
          Number(query.get("end")),
          "范围结束",
          start + 1,
          Math.min(MAX_TIME, start + 93 * DAY),
        );
    const limit = integer(
        Number(query.get("limit") ?? 500),
        "分页大小",
        1,
        500,
      ),
      offset = integer(Number(query.get("offset") ?? 0), "分页偏移", 0, 100000),
      types = query.get("type"),
      creator = query.get("creator"),
      state = query.get("state");
    const items: Record<string, any>[] = [],
      unscheduled: Record<string, unknown>[] = [],
      blocked: Record<string, unknown>[] = [];
    const add = (item: Record<string, any>) => {
      if (types && item.type !== types) return;
      if (creator && item.creator !== creator) return;
      if (state && item.state !== state) return;
      items.push(item);
      if (items.length > MAX_ITEMS)
        throw new ApiError(
          409,
          "calendar_expansion_limit",
          "范围内项目超过500，请缩短范围或按类型筛选",
        );
    };
    for (const item of this.routineItems?.(owner, start, end) ?? []) add(item);
    const rows = this.store.all(
      "SELECT * FROM schedule_activities WHERE owner_id=? ORDER BY created_at,id LIMIT 5001",
      owner,
    );
    if (rows.length > 5000)
      throw new ApiError(409, "schedule_capacity", "活动数量超过范围查询上限");
    for (const row of rows) {
      const view = this.view(row),
        stored = this.store.all(
          "SELECT * FROM schedule_occurrences WHERE activity_id=? AND owner_id=? AND start_at<? AND end_at>?",
          view.id,
          owner,
          end,
          start,
        ),
        map = new Map<number, Record<string, any>>();
      for (const item of this.expanded(view, start, end))
        map.set(item.nominal_at, {
          origin: "schedule",
          id: `${view.id}:${item.nominal_at}`,
          activity_id: view.id,
          revision: view.revision,
          nominal_at: item.nominal_at,
          following_repeat: view.definition.repeat
            ? {
                ...view.definition.repeat,
                count:
                  view.definition.repeat.count === null
                    ? null
                    : view.definition.repeat.count -
                      (expand(
                        view.definition,
                        item.nominal_at,
                        item.nominal_at + 1,
                      ).find((slot) => slot.nominal_at === item.nominal_at)
                        ?.ordinal ?? 0),
              }
            : null,
          title: item.definition.title,
          type: item.definition.type,
          creator: view.creator,
          state: item.state,
          status:
            item.state === "enabled"
              ? item.definition.end_at <= now()
                ? "skipped"
                : "planned"
              : item.state,
          execution_record: false,
          start_at: item.definition.start_at,
          end_at: item.definition.end_at,
          time_zone: item.definition.time_zone,
          definition: item.definition,
          local_start: localText(
            item.definition.start_at,
            item.definition.time_zone,
          ),
          local_end: localText(
            item.definition.end_at,
            item.definition.time_zone,
          ),
          block_reason:
            item.state === "enabled" && item.definition.end_at <= now()
              ? "missed_execution_window"
              : this.capability(view, item.definition),
          read_only: false,
        });
      for (const actual of stored) {
        const definition = parse(actual.definition_json);
        map.set(Number(actual.nominal_at), {
          origin: "schedule",
          id: actual.id,
          activity_id: view.id,
          revision: view.revision,
          activity_revision: actual.activity_revision,
          nominal_at: actual.nominal_at,
          following_repeat: view.definition.repeat
            ? {
                ...view.definition.repeat,
                count:
                  view.definition.repeat.count === null
                    ? null
                    : view.definition.repeat.count -
                      (expand(
                        view.definition,
                        Number(actual.nominal_at),
                        Number(actual.nominal_at) + 1,
                      ).find((slot) => slot.nominal_at === actual.nominal_at)
                        ?.ordinal ?? 0),
              }
            : null,
          title: definition.title,
          type: definition.type,
          creator: view.creator,
          state: view.state,
          status: actual.status,
          execution_record: true,
          start_at: actual.start_at,
          end_at: actual.end_at,
          time_zone: definition.time_zone,
          definition,
          local_start: localText(definition.start_at, definition.time_zone),
          local_end: localText(definition.end_at, definition.time_zone),
          block_reason: actual.block_reason,
          checkpoint: actual.checkpoint,
          total_chars: actual.total_chars,
          read_only: immutable.has(String(actual.status)),
        });
      }
      for (const item of map.values()) add(item);
    }
    for (const row of this.store.all(
      "SELECT * FROM records WHERE owner_id=? AND kind='schedule' AND deleted_at IS NULL",
      owner,
    )) {
      const data = parse(row.payload);
      if (data.start_at < end && data.end_at > start)
        add({
          origin: "manual_record",
          id: row.id,
          revision: row.revision,
          ...data,
          type: "manual",
          creator: "manual",
          state: data.status,
          read_only: true,
        });
    }
    const triggers = this.store.all(
      "SELECT * FROM trigger_rules WHERE owner_id=? ORDER BY created_at,id LIMIT 501",
      owner,
    );
    if (triggers.length > 500)
      throw new ApiError(
        409,
        "calendar_expansion_limit",
        "触发规则过多，请先整理历史规则",
      );
    for (const row of triggers) {
      const rule = parse(row.rule_json),
        action = parse(row.action_json);
      if (rule.type === "event" || row.next_at === null) {
        unscheduled.push({
          origin: "trigger",
          id: row.id,
          title: action.title,
          type: rule.type,
          enabled: row.enabled === 1,
          read_only: true,
        });
        continue;
      }
      let at = Number(row.next_at),
        count = 0;
      if (rule.type === "interval" && at < start)
        at +=
          Math.ceil((start - at) / Number(rule.seconds)) * Number(rule.seconds);
      try {
        while (at < end && count++ <= 500) {
          if (at >= start)
            add({
              origin: "trigger",
              id: `trigger:${row.id}:${at}`,
              trigger_id: row.id,
              revision: row.revision,
              title: action.title,
              type: "trigger",
              creator: "manual",
              state: row.enabled === 1 ? "enabled" : "paused",
              status: "projected",
              start_at: at,
              end_at: null,
              time_zone: "Asia/Shanghai",
              read_only: true,
            });
          const next =
            rule.type === "once"
              ? null
              : nextOccurrence(
                  rule,
                  Math.max(at, start - 1),
                  Number(row.created_at),
                );
          if (next === null || next <= at) break;
          at = next;
        }
        if (count > 500)
          throw new ApiError(
            409,
            "calendar_expansion_limit",
            "触发投影过多，请缩短范围",
          );
      } catch (error) {
        if (error instanceof ApiError && error.code === "calendar_unknown")
          blocked.push({
            origin: "trigger",
            id: row.id,
            title: action.title,
            reason: "calendar_unknown",
          });
        else throw error;
      }
    }
    for (const row of this.store.all(
      "SELECT id,status,created_at,updated_at FROM model_jobs WHERE owner_id=? AND kind='diary' AND created_at>=? AND created_at<?",
      owner,
      start,
      end,
    ))
      add({
        origin: "diary_system",
        id: `diary:${row.id}`,
        title: "日记整理记录",
        type: "consolidation",
        creator: "system",
        state: row.status,
        status: row.status,
        start_at: row.created_at,
        end_at: Math.max(Number(row.created_at) + 1, Number(row.updated_at)),
        read_only: true,
      });
    items.sort(
      (a, b) =>
        a.start_at - b.start_at || String(a.id).localeCompare(String(b.id)),
    );
    return {
      items: items.slice(offset, offset + limit),
      total: items.length,
      offset,
      limit,
      time_zone: timeZone,
      range: { start, end },
      cells: range?.cells,
      unscheduled_rules: unscheduled,
      blocked_rules: blocked,
    };
  }
}
