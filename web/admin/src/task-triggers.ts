export type TriggerType = "once" | "interval" | "calendar" | "event";
export interface TriggerRule {
  type: TriggerType;
  mode?: "at" | "after";
  at?: string;
  after_seconds?: number;
  every_seconds?: number;
  anchor?: "first_enable";
  first_fire?: "after_interval";
  frequency?: "daily" | "weekly" | "monthly" | "yearly" | "dates";
  at_times?: string[];
  day_class?: "any" | "workday" | "restday";
  calendar_id?: "cn-official" | null;
  weekdays?: number[];
  month_days?: (number | "last")[];
  month_dates?: { month: number; day: number }[];
  dates?: string[];
  source_id?: string;
  event_type?: string;
  filters?: {
    field: string;
    operator: "eq" | "neq" | "in";
    value?: unknown;
    values?: unknown[];
  }[];
  cooldown_seconds?: number;
}
export interface TriggerDefinition {
  spec: "dsh.task-trigger";
  spec_version: "1";
  title: string;
  time_zone: string;
  trigger: TriggerRule;
  action: {
    type: "notify" | "assistant_run";
    content?: string;
    prompt?: string;
    skill?: { id: string; revision: number } | null;
    persona?: {
      mode: "instance_snapshot" | "character_revision";
      card_id?: string;
      revision?: number;
    };
    output_mode?: "text" | "html_drawing" | "native_image";
    context_policy?: "isolated";
  };
  delivery: { kind: "web_inbox" };
  active_window: { start_at: string | null; end_at: string | null };
  policy: {
    misfire: "skip" | "coalesce_one" | "bounded_catch_up";
    grace_seconds: number;
    max_catch_up: number;
    overlap: "skip" | "coalesce_one";
    catch_up_window_seconds: number;
  };
  budget: {
    daily_occurrences: number;
    daily_model_runs: number;
    max_output_tokens: number;
    run_timeout_seconds: number;
  };
}
export interface TriggerOccurrence {
  id: string;
  job_id: string;
  job_revision: number;
  origin: string;
  time_zone?: string;
  nominal_at: number;
  status: string;
  response?: string | null;
  error?: string | null;
  delivery_status: string;
  generated_image_url?: string | null;
  run_id?: string | null;
  created_at: number;
  started_at?: number | null;
  completed_at?: number | null;
  result_expired?: boolean;
}
export interface TriggerJob {
  id: string;
  title: string;
  revision: number;
  draft_revision: number;
  active_revision: number | null;
  lifecycle: string;
  block_reason: string | null;
  next_fire_at: number | null;
  created_at: number;
  updated_at: number;
  task_trigger_draft: TriggerDefinition;
  active_definition: TriggerDefinition | null;
  last_occurrence?: TriggerOccurrence | null;
}
export interface TriggerCapabilities {
  available: boolean;
  spec_version: string;
  now: number;
  time_zone: string;
  calendars: {
    calendar_id?: string;
    id?: string;
    revision?: number;
    year?: number;
    coverage_start?: string;
    coverage_end?: string;
    source_url?: string;
    active?: boolean;
    document?: CalendarRevision["document"];
  }[];
  event_types: { source_id: string; event_type: string; fields: unknown[] }[];
  limits: Record<string, unknown>;
  retention?: { result_days: number; event_body_days: number };
  skills: { id: string; name: string; revision: number; kind: string }[];
  characters: { card_id: string; revision: number; name: string }[];
  clock?: {
    trusted: boolean;
    source: string;
    reason: string | null;
    expires_in_seconds?: number | null;
  };
}
export interface CalendarRevision {
  id: string;
  calendar_id: string;
  revision: number;
  document: {
    year: number;
    time_zone: string;
    holidays: string[];
    working_days: string[];
    source: {
      authority: string;
      document_number: string;
      published_at: string;
      url: string;
    };
  };
  sha256: string;
  active: boolean;
  verified_by: string | null;
  created_at: number;
}
export interface TriggerPreview {
  valid: boolean;
  errors: { field: string; message: string }[];
  now: number;
  next_times: { at: number; local: string; key: string }[];
  preview_token?: string | null;
  estimated: boolean;
  calendar_coverage?: unknown[];
  blocked_reason?: string | null;
}
export function newTriggerDefinition(): TriggerDefinition {
  return {
    spec: "dsh.task-trigger",
    spec_version: "1",
    title: "",
    time_zone: "Asia/Shanghai",
    trigger: { type: "once", mode: "after", after_seconds: 30 },
    action: { type: "notify", content: "" },
    delivery: { kind: "web_inbox" },
    active_window: { start_at: null, end_at: null },
    policy: {
      misfire: "skip",
      grace_seconds: 300,
      max_catch_up: 1,
      overlap: "coalesce_one",
      catch_up_window_seconds: 0,
    },
    budget: {
      daily_occurrences: 1000,
      daily_model_runs: 0,
      max_output_tokens: 1024,
      run_timeout_seconds: 120,
    },
  };
}
export function triggerSummary(rule: TriggerRule): string {
  if (rule.type === "once")
    return rule.mode === "after"
      ? `首次启用后 ${rule.after_seconds} 秒`
      : `一次 · ${rule.at || "待设置"}`;
  if (rule.type === "interval") return `每隔 ${rule.every_seconds} 秒`;
  if (rule.type === "event") return `事件 · ${rule.event_type || "待选择"}`;
  const days =
    rule.day_class === "workday"
      ? "中国工作日"
      : rule.day_class === "restday"
        ? "中国休息日"
        : {
            daily: "每日",
            weekly: "每周",
            monthly: "每月",
            yearly: "每年",
            dates: "指定日期",
          }[rule.frequency || "daily"];
  return `${days} · ${(rule.at_times || []).join("、")}`;
}
export function triggerState(value: string): string {
  return (
    (
      {
        draft: "草稿 / 停用",
        enabled: "已启用",
        paused: "已暂停",
        completed: "已完成",
        archived: "已归档",
        waiting_capacity: "等待容量",
        queued: "已入队",
        running: "执行中",
        failed: "失败",
        cancelled: "已取消",
        expired: "已过期",
        needs_attention: "需要人工处理",
        stored: "已保存到结果中心",
        pending: "待保存",
        skipped: "已跳过",
        coalesced: "已合并",
        interrupted: "中断，结果未确认",
        unknown: "结果未知",
      } as Record<string, string>
    )[value] || value
  );
}
export function triggerDate(
  value: number | null | undefined,
  zone = "Asia/Shanghai",
): string {
  if (value === null || value === undefined || !Number.isFinite(value))
    return "—";
  const date = new Date(value * 1000);
  try {
    return `${new Intl.DateTimeFormat("zh-CN", { dateStyle: "medium", timeStyle: "medium", timeZone: zone }).format(date)} · ${zone}`;
  } catch {
    return date.toISOString();
  }
}
