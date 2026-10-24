export type Timestamp = string | number;

export interface RuntimeConfig {
  assistant: {
    name: string;
    system_prompt: string;
    max_concurrent_runs: number;
    timeout_seconds: number;
  };
  model: { base_url: string; model: string; credential_ref: string | null };
  qq: {
    enabled: boolean;
    app_id: string;
    credential_ref: string | null;
    transport: "websocket" | "webhook";
  };
}

export interface AuthUser {
  username: string;
  role: string;
}
export interface AuthResponse {
  user: AuthUser;
  csrf_token: string;
}
export interface ConfigResponse {
  revision: number;
  applied_revision: number;
  config: RuntimeConfig;
}
export interface SecretMetadata {
  id: string;
  kind: string;
  configured: boolean;
  created_at?: Timestamp;
}
export interface Draft {
  id: string;
  base_revision: number;
  config: RuntimeConfig;
}
export interface Check {
  name: string;
  ok: boolean;
  message: string;
}
export interface FieldError {
  field: string;
  message: string;
}
export interface Operation {
  id: string;
  status: string;
  desired_revision: number;
  applied_revision: number;
  error?: string;
}
export interface Revision {
  revision: number;
  created_at: Timestamp;
  current: boolean;
  config?: RuntimeConfig;
}
export interface Conversation {
  id: string;
  title: string;
  created_at: Timestamp;
}
export interface ChatMessage {
  id: string;
  role: string;
  content: string;
  created_at: Timestamp;
}
export interface Run {
  id: string;
  status: string;
  response?: string | null;
  error?: string | null;
}
export interface SystemStatus {
  version: string;
  commit?: string;
  uptime_seconds?: number;
  platform?: {
    os?: string;
    arch?: string;
    memory_total_bytes?: number | null;
    memory_available_bytes?: number | null;
  };
  queue?: {
    queued?: number;
    running?: number;
    failed?: number;
    outbox_pending?: number;
    outbox_unknown?: number;
  };
  config?: { desired_revision: number; applied_revision: number };
  channels?: {
    qq?: {
      enabled: boolean;
      configured: boolean;
      transport?: string;
      connection_status?: string;
      webhook_path?: string;
      last_event_at?: Timestamp | null;
      last_error?: string | null;
    };
  };
  model?: { configured: boolean; model: string };
}
