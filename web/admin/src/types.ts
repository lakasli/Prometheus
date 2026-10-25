export type Timestamp = string | number;

export interface RuntimeConfig {
  reply?: { min_interval_ms: number; max_interval_ms: number; max_items: number };
  speech?: { enabled: boolean; voice: string; timeout_seconds: number; max_text_chars: number };
  assistant: {
    name: string;
    system_prompt: string;
    max_concurrent_runs: number;
    timeout_seconds: number;
  };
  model: {
    base_url: string;
    model: string;
    credential_ref: string | null;
    image_generation?: boolean;
  };
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
  character?: CharacterIdentity | null;
  legacy?: boolean;
  test?: boolean;
  is_test?: boolean;
  greeting?: string | null;
}
export interface ChatMessage {
  reply_plan?: { version: number; normalized: boolean; items: ReplyItem[] } | null;
  generated_image_url?: string | null;
  id: string;
  role: string;
  content: string;
  created_at: Timestamp;
  image?: {
    width: number;
    height: number;
    sha256: string;
    status:
      | "queued"
      | "running"
      | "described"
      | "failed"
      | "cancelled"
      | "interrupted"
      | "invalidated";
    original_retained: false;
  } | null;
  basis?: "vision_description" | null;
}
export interface ReplyItem {
  type: "text" | "voice" | "image" | "video";
  text: string;
  delay_ms: number;
  asset_id?: string;
}
export interface ConversationImageCapabilities {
  available: boolean;
  provider: "deepseek";
  model: "deepseek-flash";
  basis: "vision_description";
  max_image_bytes: number;
  max_width: number;
  max_height: number;
  max_pixels: number;
  formats: string[];
  max_images: number;
  availability_code?: string | null;
}
export interface ChatSubmission {
  run_id: string;
  conversation_id: string;
}
export interface SkillDraft {
  name: string;
  description: string;
  instructions: string;
  kind: "instruction" | "drawing";
  enabled: boolean;
}
export interface Skill extends SkillDraft {
  id: string;
  revision: number;
  builtin: boolean;
  updated_at: Timestamp;
}
export interface SkillList {
  items: Skill[];
  drawing: { native_available: boolean };
}
export interface Run {
  id: string;
  status: string;
  response?: string | null;
  error?: string | null;
  character?: CharacterIdentity | null;
}

export interface CharacterIdentity {
  card_id: string;
  revision: number;
  name: string;
  digest?: string;
  is_test?: boolean;
  greeting?: string | null;
}
export interface CharacterCard {
  spec: "chara_card_v3";
  spec_version: "3.0";
  data: {
    name: string;
    nickname?: string;
    description: string;
    personality: string;
    scenario: string;
    system_prompt: string;
    first_mes: string;
    mes_example: string;
    post_history_instructions: string;
    alternate_greetings: string[];
    group_only_greetings: string[];
    creator_notes: string;
    tags: string[];
    creator: string;
    character_version: string;
    extensions: {
      dsh: {
        schema_version: 1;
        modules: Record<
          "chat" | "vision" | "speech",
          {
            mode: string;
            provider_hint?: string;
            model_hint?: string;
            voice_hint?: string;
          }
        >;
      };
    };
  };
}
export interface CharacterDraft {
  card_id: string;
  draft_id: string;
  draft_etag: string;
  card: CharacterCard;
}
export interface CharacterCardSummary {
  id: string;
  name: string;
  archived_at: Timestamp | null;
  latest_revision: number | null;
  qq_selectable?: boolean;
  draft?: CharacterDraft | null;
}
export interface CharacterCardList {
  items: CharacterCardSummary[];
  default: CharacterIdentity | null;
  default_etag: string;
}
export interface CharacterRevision extends CharacterIdentity {
  created_at?: Timestamp;
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
