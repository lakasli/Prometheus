import type {
  RestGrant,
  RestPolicyView,
  RestProgress,
  RestStatus,
} from "../src/rest-management";
export function restPolicy(): RestPolicyView {
  return {
    revision: 2,
    independent_of_runtime_config_and_personality: true,
    policy: {
      natural_enabled: false,
      anticipation_enabled: false,
      reset_context_on_sleep: false,
      schedule_enabled: false,
      time_zone: "Asia/Shanghai",
      sleep_local: null,
      wake_local: null,
      default_sleep_seconds: null,
      max_sleep_seconds: 86400,
      drain_seconds: 30,
      consolidation_seconds: 300,
      queue_limit: 128,
      max_job_attempts: 3,
      max_cycle_calls: 8,
      max_cycle_reserved_tokens: 24000,
      ack_seconds: 30,
    },
  };
}
export function restStatus(): RestStatus {
  return {
    state: {
      generation: 7,
      revision: 11,
      phase: "awake",
      cycle_id: null,
      wake_at: null,
      drain_at: null,
      consolidate_at: null,
      logical_at: 1793000000,
      reason: null,
    },
    policy_revision: 2,
    clock: {
      host_sample_available: true,
      trusted_for_new_sleep: true,
      sampled_wall_at: 1793000000,
      client_clock_accepted: false,
    },
    admission: {
      receiver: { decision: "allow", generation: 7 },
      wake: { decision: "allow", generation: 7 },
      automatic: { decision: "allow", generation: 7 },
    },
    physical_requests: 2,
  };
}
export function restProgress(): RestProgress {
  return {
    cycle_id: "cycle-one",
    generation: 7,
    wake_at: 1793003600,
    partial: true,
    error_code: "drain_deadline_with_inflight",
    metadata_only: true,
    steps: [
      {
        kind: "diary_day",
        status: "partial",
        checkpoint: {
          cursor_digest: "a".repeat(64),
          processed_refs: 5,
          result_digest: "b".repeat(64),
        },
        result_digest: "b".repeat(64),
        error_code: "source_snapshot_page_limit",
      },
      {
        kind: "relationships",
        status: "unknown",
        checkpoint: {
          cursor_digest: null,
          processed_refs: 1,
          result_digest: null,
        },
        result_digest: null,
        error_code: "request_unfinished",
      },
    ],
    jobs: [
      {
        id: "metadata-job-one",
        cycle_id: "cycle-one",
        status: "unknown",
        fence: 4,
        attempts: 2,
        error_code: "transport_outcome_unknown",
      },
    ],
  };
}
export function restGrant(run = "verified-run-one"): RestGrant {
  return {
    source_run_id: run,
    source_ref: `rest-source-${run}`,
    revision: 0,
    permissions: { can_sleep: false, can_wake: false, valid_until: 0 },
    natural_control_enabled: false,
    grant_present: false,
    grant_expired: false,
    current_source_and_policy_still_required: true,
    recipient_or_model_permission: false,
  };
}
export function restSources() {
  return ["one", "two"].map((suffix) => ({
    conversation_id: "same-group-conversation",
    run_id: `verified-run-${suffix}`,
    source_run_id: `verified-run-${suffix}`,
    title: "未命名来源",
    source_label: "QQ群会话",
    channel: "qq",
    source_kind: "group",
    scope_ref: `verified-speaker-${suffix}`,
    updated_at: 1793000000,
  }));
}
