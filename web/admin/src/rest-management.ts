export type RestPhase =
  "awake" | "sleep_pending" | "consolidating" | "sleeping" | "waking";
export interface RestPolicy {
  anticipation_enabled?: boolean;
  reset_context_on_sleep?: boolean;
  natural_enabled: boolean;
  schedule_enabled: boolean;
  time_zone: string;
  sleep_local: string | null;
  wake_local: string | null;
  default_sleep_seconds: number | null;
  max_sleep_seconds: number;
  drain_seconds: number;
  consolidation_seconds: number;
  queue_limit: number;
  max_job_attempts: number;
  max_cycle_calls: number;
  max_cycle_reserved_tokens: number;
  ack_seconds: number;
}
export interface RestPolicyView {
  revision: number;
  policy: RestPolicy;
  independent_of_runtime_config_and_personality: true;
}
export interface RestState {
  generation: number;
  revision: number;
  phase: RestPhase;
  cycle_id: string | null;
  wake_at: number | null;
  drain_at: number | null;
  consolidate_at: number | null;
  logical_at: number;
  reason: string | null;
}
export interface RestStatus {
  state: RestState;
  policy_revision: number;
  clock: {
    host_sample_available: boolean;
    client_clock_accepted: false;
    trusted_for_new_sleep?: boolean;
    sampled_wall_at?: number | null;
  };
  admission: Record<
    string,
    { decision: string; generation: number; code?: string }
  >;
  physical_requests: number;
}
export interface RestProgress {
  cycle_id: string;
  generation: number;
  wake_at: number;
  partial: boolean;
  error_code: string | null;
  metadata_only: true;
  steps: {
    kind: string;
    status: string;
    checkpoint: {
      cursor_digest: string | null;
      processed_refs: number;
      result_digest: string | null;
    };
    result_digest: string | null;
    error_code: string | null;
  }[];
  jobs: {
    id: string;
    cycle_id: string;
    status: string;
    fence: number;
    attempts: number;
    error_code: string | null;
  }[];
}
export interface RestCycleSummary {
  cycle_id: string;
  generation: number;
  trigger_kind: string;
  policy_revision: number;
  wake_at: number;
  created_at: number;
  finished_at: number | null;
  partial: boolean;
  model_calls: number;
  reserved_tokens: number;
}
export interface RestGrant {
  source_run_id: string;
  source_ref: string;
  revision: number;
  permissions: { can_sleep: boolean; can_wake: boolean; valid_until: number };
  natural_control_enabled: boolean;
  grant_present: boolean;
  grant_expired: boolean;
  current_source_and_policy_still_required: true;
  recipient_or_model_permission: false;
}
export interface RestControlReceipt {
  state: RestState;
  operation: {
    id: string;
    generation: number;
    phase: RestPhase;
    wake_at: number | null;
  };
  idempotent_replay: boolean;
  operation_is_original_receipt: true;
  physical_leases_released: false;
}
export const restPhaseLabels: Record<RestPhase, string> = {
  awake: "清醒",
  sleep_pending: "等待排空",
  consolidating: "正在整理",
  sleeping: "睡眠中",
  waking: "正在唤醒",
};
export const restResultLabels: Record<string, string> = {
  pending: "待处理",
  queued: "待领取",
  running: "进行中",
  success: "已完成",
  partial: "部分完成",
  failure: "失败",
  failed: "失败",
  unknown: "结果未知",
  cancelled: "已取消",
  skipped: "已跳过",
};
export const restAdmissionLabels: Record<string, string> = {
  allow: "允许处理",
  hold: "等待唤醒后复核",
  reject: "已拒绝",
  preserve_in_flight: "保留在途",
};
export const restStepLabels: Record<string, string> = {
  freeze_sources: "冻结来源",
  diary_day: "当日日记",
  closed_periods: "已结束周期",
  relationships: "人物关系",
  affinity_candidates: "好感候选",
  commit_index: "版本与索引",
  checkpoint: "保存检查点",
};
