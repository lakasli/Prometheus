// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import App from "../src/App.vue";
import { restPolicy, restStatus } from "./rest-fixtures";
let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  location.hash = "";
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});
function response(value: unknown) {
  return new Response(JSON.stringify(value), {
    headers: { "Content-Type": "application/json" },
  });
}
function setup() {
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
        csrf_token: "rest-layout-csrf",
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
    if (path === "/rest") return response(restStatus());
    if (path === "/rest/policy") return response(restPolicy());
    if (path === "/rest/cycles/current") return response({ current: null });
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
  const confirm = vi.fn().mockReturnValue(false);
  vi.stubGlobal("confirm", confirm);
  return { fetch, confirm };
}
async function rhythm() {
  const drawer = wrapper!
    .findAll("details.config-drawer")
    .find((item) => item.get("summary").text() === "工作节奏")!;
  (drawer.element as HTMLDetailsElement).open = true;
  await drawer.trigger("toggle");
  await flushPromises();
  return drawer;
}
it("loads rest alongside budget only in work rhythm and preserves five drawers without a new navigation page", async () => {
  const { fetch } = setup();
  wrapper = mount(App);
  await flushPromises();
  expect(fetch.mock.calls.some(([url]) => url.includes("/rest"))).toBe(false);
  const drawer = await rhythm();
  expect(drawer.find('[aria-label="作息与整理"]').exists()).toBe(true);
  expect(drawer.find('[aria-label="上下文预算"]').exists()).toBe(true);
  expect(
    wrapper
      .findAll("details.config-drawer > summary")
      .map((item) => item.text()),
  ).toEqual(["模型配置", "工作节奏", "绘画能力", "好感规则配置", "配置历史"]);
  expect(
    wrapper
      .findAll('[aria-label="主导航"] button')
      .some((item) => /作息|整理|睡眠/.test(item.text())),
  ).toBe(false);
  expect(
    fetch.mock.calls
      .filter(([url]) => url.includes("/rest"))
      .map(([url]) => url),
  ).toEqual(
    expect.arrayContaining([
      "/api/admin/v1/rest",
      "/api/admin/v1/rest/policy",
      "/api/admin/v1/rest/cycles/current",
    ]),
  );
});
it("protects unsaved rest edits on navigation and logout independently of RuntimeConfig", async () => {
  const { fetch, confirm } = setup();
  wrapper = mount(App);
  await flushPromises();
  await rhythm();
  await wrapper.get('[aria-label="排空窗口"]').setValue(40);
  const nav = wrapper
    .findAll('[aria-label="主导航"] button')
    .find((item) => item.text().includes("日志记录"))!;
  await nav.trigger("click");
  await flushPromises();
  expect(confirm).toHaveBeenCalledWith(
    "作息有未保存的填写或结果未确认的操作，确定离开？",
  );
  expect(wrapper.find('[aria-label="作息与整理"]').exists()).toBe(true);
  const logout = wrapper.get('button[aria-label="退出登录"]');
  await logout.trigger("click");
  await flushPromises();
  expect(confirm).toHaveBeenCalledWith(
    "作息有未保存的填写或结果未确认的操作，确定退出登录？",
  );
  expect(fetch.mock.calls.some(([url]) => url.endsWith("/auth/logout"))).toBe(
    false,
  );
  expect(wrapper.get('[aria-label="排空窗口"]').element).toHaveProperty(
    "value",
    "40",
  );
});
