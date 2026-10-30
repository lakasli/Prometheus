import {
  bad,
  boolean,
  fields,
  integer,
  object,
  string,
  unavailable,
} from "./errors.ts";

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
export const defaultConfig = (): Config => ({
  model: {
    base_url: "https://api.deepseek.com",
    model: "deepseek-chat",
    credential_ref: null,
  },
  runtime: {
    max_concurrent_runs: 2,
    timeout_seconds: 120,
    max_history_messages: 24,
  },
  qq: {
    enabled: false,
    app_id: "",
    credential_ref: null,
    transport: "webhook",
  },
  weixin: { enabled: false },
  speech: { enabled: false },
});
export function modelUrl(value: string, allowLoopback = false): URL {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw bad("模型地址无效");
  }
  const loopback = ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname);
  if (
    url.username ||
    url.password ||
    url.hash ||
    url.search ||
    (url.protocol !== "https:" &&
      !(allowLoopback && loopback && url.protocol === "http:"))
  )
    throw bad("模型服务须使用无凭据、无查询的 HTTPS 地址");
  if (!url.hostname || (url.port && !/^[0-9]+$/.test(url.port)))
    throw bad("模型地址无效");
  return url;
}
export function validateConfig(value: unknown, allowLoopback = false): Config {
  const root = fields(value, ["model", "runtime", "qq", "weixin", "speech"]);
  const model = fields(root.model, ["base_url", "model", "credential_ref"]);
  const runtime = fields(root.runtime, [
    "max_concurrent_runs",
    "timeout_seconds",
    "max_history_messages",
  ]);
  const qq = fields(root.qq, [
    "enabled",
    "app_id",
    "credential_ref",
    "transport",
  ]);
  const weixin = fields(root.weixin, ["enabled"]),
    speech = fields(root.speech, ["enabled"]);
  const ref = (v: unknown) =>
    v === null ? null : string(v, "凭据引用", 36, 36);
  const baseUrl = string(model.base_url, "模型地址", 2048, 1);
  modelUrl(baseUrl, allowLoopback);
  const qqEnabled = boolean(qq.enabled, "QQ 开关");
  if (qqEnabled) throw unavailable("QQ 接收与投递");
  if (boolean(weixin.enabled, "微信开关")) throw unavailable("微信接收与投递");
  if (boolean(speech.enabled, "语音开关")) throw unavailable("隔离语音 worker");
  const transport = string(qq.transport, "QQ 连接方式", 20);
  if (transport !== "webhook" && transport !== "websocket")
    throw bad("QQ 连接方式无效");
  return {
    model: {
      base_url: baseUrl.replace(/\/+$/, ""),
      model: string(model.model, "模型名称", 128, 1),
      credential_ref: ref(model.credential_ref),
    },
    runtime: {
      max_concurrent_runs: integer(
        runtime.max_concurrent_runs,
        "并发上限",
        1,
        2,
      ),
      timeout_seconds: integer(runtime.timeout_seconds, "请求超时", 5, 120),
      max_history_messages: integer(
        runtime.max_history_messages,
        "历史消息数量",
        2,
        40,
      ),
    },
    qq: {
      enabled: false,
      app_id: string(qq.app_id, "QQ App ID", 128),
      credential_ref: ref(qq.credential_ref),
      transport,
    },
    weixin: { enabled: false },
    speech: { enabled: false },
  };
}
export interface Personality {
  name: string;
  description: string;
  personality: string;
  scenario: string;
  system_prompt: string;
  mes_example: string;
}
export function validatePersonality(value: unknown): Personality {
  const keys = [
    "name",
    "description",
    "personality",
    "scenario",
    "system_prompt",
    "mes_example",
  ] as const;
  const input = fields(value, keys);
  const result = Object.fromEntries(
    keys.map((key) => [
      key,
      string(
        input[key],
        key,
        key === "name" ? 128 : 8000,
        key === "name" ? 1 : 0,
      ),
    ]),
  );
  return result as unknown as Personality;
}
export function personalityPrompt(value: Personality): string {
  return [
    value.system_prompt,
    `姓名：${value.name}`,
    value.description,
    value.personality,
    value.scenario,
    value.mes_example &&
      `以下为虚构表达示例，不是历史或事实：\n${value.mes_example}`,
  ]
    .filter(Boolean)
    .join("\n\n");
}
export function cloneJson<T>(input: T): T {
  return JSON.parse(JSON.stringify(input)) as T;
}
export const configObject = (value: unknown) =>
  object(value) as unknown as Config;
