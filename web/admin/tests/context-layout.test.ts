// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import App from "../src/App.vue";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  location.hash = "";
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function response(value: unknown) {
  return new Response(JSON.stringify(value), {
    headers: { "Content-Type": "application/json" },
  });
}
it("keeps five configuration drawers and loads the independent budget only inside work rhythm", async () => {
  location.hash = "#config";
  const config = {
    assistant: {
      name: "刘嘉卉",
      system_prompt: "说明",
      max_concurrent_runs: 2,
      timeout_seconds: 120,
    },
    model: {
      base_url: "https://api.example.invalid",
      model: "test-model",
      credential_ref: null,
    },
    qq: {
      enabled: false,
      app_id: "",
      credential_ref: null,
      transport: "websocket",
    },
  };
  const fetch = vi.fn(async (url: string) => {
    const path = url.replace("/api/admin/v1", "");
    if (path === "/auth/status") return response({ initialized: true });
    if (path === "/auth/me")
      return response({
        user: { username: "admin", role: "admin" },
        csrf_token: "layout-csrf",
      });
    if (path === "/config")
      return response({ revision: 1, applied_revision: 1, config });
    if (path === "/status")
      return response({
        version: "test",
        queue: {},
        model: {},
        config: { desired_revision: 1, applied_revision: 1 },
      });
    if (path === "/context-policy")
      return response({
        policy: {
          schema_version: 1,
          revision: 0,
          model_id: "test-model",
          provider_context_window: null,
          provider_fingerprint: null,
          window_basis: "unknown",
          estimated_input_limit: 96000,
          max_input_bytes: 96000,
          response_reserve_tokens: 4096,
          protocol_reserve_tokens: 1024,
          safety_reserve_tokens: 4096,
          estimator_version: "utf8_serialized_bytes_v1",
        },
        etag: '"0"',
        current_model_id: "test-model",
        current_provider_fingerprint: "a".repeat(64),
        provider_binding_matches: true,
        window_status: "unknown",
        applied: true,
        saved: false,
        token_counts_exact: false,
        host_limits: { max_input_bytes: 96000, max_response_tokens: 4096 },
      });
    return response({ items: [] });
  });
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(App);
  await flushPromises();
  expect(
    wrapper
      .findAll("details.config-drawer > summary")
      .map((item) => item.text()),
  ).toEqual(["模型配置", "工作节奏", "绘画能力", "好感规则配置", "配置历史"]);
  expect(fetch.mock.calls.some(([url]) => url.includes("context-policy"))).toBe(
    false,
  );
  const rhythm = wrapper
    .findAll("details.config-drawer")
    .find((item) => item.get("summary").text() === "工作节奏")!;
  (rhythm.element as HTMLDetailsElement).open = true;
  await rhythm.trigger("toggle");
  await flushPromises();
  expect(rhythm.get('[aria-label="上下文预算"]').text()).toContain(
    "未知，使用本地估算限制",
  );
  expect(wrapper.findAll("details.config-drawer")).toHaveLength(5);
  expect(
    wrapper
      .findAll(".sidebar-nav button")
      .some((item) => item.text().includes("上下文预算")),
  ).toBe(false);
  expect(
    fetch.mock.calls.filter(([url]) => url.includes("context-policy")),
  ).toHaveLength(1);
});
