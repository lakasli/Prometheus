export interface ContextBudgetPolicy {
  schema_version: number;
  revision: number;
  model_id: string;
  provider_context_window: number | null;
  provider_fingerprint: string | null;
  window_basis: "unknown" | "administrator_declared";
  estimated_input_limit: number;
  max_input_bytes: number;
  response_reserve_tokens: number;
  protocol_reserve_tokens: number;
  safety_reserve_tokens: number;
  estimator_version: string;
}
export interface ContextPolicyView {
  policy: ContextBudgetPolicy;
  etag: string;
  current_model_id: string;
  current_provider_fingerprint: string;
  provider_binding_matches: boolean;
  window_status: string;
  applied: boolean;
  saved: boolean;
  token_counts_exact: false;
  host_limits: { max_input_bytes: number; max_response_tokens: number };
}
export interface ContextSourceReference {
  kind: string;
  id: string;
  revision: number;
  epoch?: number | null;
  digest?: string | null;
}
export interface ContextSelectionView {
  run_id: string;
  available: boolean;
  reason?: string;
  selection?: {
    accepted: boolean;
    rejection_reason: string | null;
    model_id: string;
    policy: ContextBudgetPolicy;
    actual_response_tokens: number;
    serialized_bytes: number;
    estimated_input_tokens: number;
    estimated_total_tokens: number;
    token_counts_exact: false;
    blocks: {
      id: string;
      kind: string;
      selected: boolean;
      required: boolean;
      reason: string;
      source_refs: ContextSourceReference[];
    }[];
  } | null;
}
export interface CommitmentScope {
  conversation_id: string;
  run_id: string;
  source_run_id: string;
  title: string;
  source_label: string;
  channel: string;
  source_kind: string;
  scope_ref: string;
  updated_at: number;
}
export type CommitmentSubject =
  "speaker" | "assistant" | "joint" | "unresolved";
export type CommitmentCounterparty = "speaker" | "assistant" | "unresolved";
export interface ConversationCommitment {
  id: string;
  revision: number;
  confirmation_state: string;
  confirmation_evidence?: {
    basis?: string | null;
    event_revision?: number | null;
    source_assent_verified: boolean;
    recipient_permission?: boolean;
  };
  intent: string;
  kind: string;
  subject_kind: CommitmentSubject;
  counterparty_kind: CommitmentCounterparty;
  time: {
    original_text?: string | null;
    precision?: string;
    time_zone?: string;
  };
  sources: ContextSourceReference[];
  source_run_id: string;
  execution: {
    state: string;
    activity_id?: string;
    current_activity_revision?: number;
    completed_at?: number | null;
    result_digest?: string | null;
    fulfillment_verified?: boolean;
    recipient_permission?: boolean;
  };
  recipient_permission: boolean;
}
export interface CommitmentActivity {
  activity_id: string;
  revision: number;
  title: string;
  kind: string;
  state: string;
  start_at: number;
  end_at: number | null;
  time_zone: string;
  binding_supported: boolean;
  block_reason: string | null;
}
export const contextBlockLabels: Record<string, string> = {
  host_policy: "宿主规则与性格",
  current_input: "当前完整问题",
  correction: "当前更正",
  delivery: "投递证据",
  focus: "会话焦点",
  commitment: "会话约定",
  history: "近期对话",
  summary: "会话摘要",
  memory: "记忆来源",
  diary: "日记",
  person: "人物资料",
  style: "完整风格示例",
  tool_result: "执行结果",
};
export const selectionReasonLabels: Record<string, string> = {
  required: "必须保留",
  selected_priority: "按优先级选入",
  duplicate_source: "来源重复，已省略",
  recently_presented: "近期已呈现，本次省略",
  input_bytes_exceeded: "达到字节预算",
  estimated_input_budget_exceeded: "达到保守估算额度",
  required_context_rejected: "必留内容未进入请求",
  request_rejected: "请求未发出",
  mandatory_input_bytes_exceeded: "必留内容超过字节上限",
  mandatory_estimated_input_budget_exceeded: "必留内容超过保守估算额度",
  provider_binding_changed: "模型服务绑定已变化",
  response_reservation_exceeded: "响应预留不足",
};
