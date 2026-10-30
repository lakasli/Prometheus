export interface Config {
  model: { base_url: string; model: string; credential_ref: string | null };
  runtime: {
    max_concurrent_runs: number;
    timeout_seconds: number;
    max_history_messages: number;
  };
  qq: {
    enabled: boolean;
    app_id: string;
    credential_ref: string | null;
    transport: "webhook" | "websocket";
  };
  weixin: { enabled: boolean };
  speech: { enabled: boolean };
}
export interface ConfigState {
  revision: number;
  applied_revision: number;
  config: Config;
}
export interface Personality {
  name: string;
  description: string;
  personality: string;
  scenario: string;
  system_prompt: string;
  mes_example: string;
}
export interface PersonalityState {
  revision: number;
  personality: Personality;
  saved: boolean;
  applied: boolean;
  effective_from: string;
}
export type RecordKind =
  "memory" | "diary" | "schedule" | "people" | "relationships" | "skills";
export interface RecordItem {
  id: string;
  kind: RecordKind;
  owner_id: string;
  visibility: "private" | "shared";
  revision: number;
  data: Record<string, any>;
  created_at: number;
  updated_at: number;
}
export interface Log {
  id: number;
  kind: string;
  status: string;
  title: string;
  resource_id: string | null;
  created_at: number;
}
export interface Status {
  name: string;
  version: string;
  commit: string;
  uptime_seconds: number;
  platform: {
    os: string;
    arch: string;
    node: string;
    memory_available_bytes: number;
    memory_total_bytes: number;
  };
  queue: { running: number; queued: number; failed: number };
  config: { desired_revision: number; applied_revision: number };
  assistant_id: string;
}
export interface Capability {
  available: boolean;
  description: string;
}
