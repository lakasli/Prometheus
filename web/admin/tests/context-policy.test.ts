// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import ContextPolicyPanel from "../src/components/ContextPolicyPanel.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";
import type { ContextPolicyView } from "../src/context-management";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function view(): ContextPolicyView {
  return {
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
  };
}

it("loads an explicitly unknown window and saves a separate version only after submit", async () => {
  setCsrfToken("budget-test-csrf");
  const local = vi.spyOn(Storage.prototype, "setItem");
  const fetch = vi.fn(async (_url: string, options?: RequestInit) => {
    const state = view();
    if (options?.method === "PUT") {
      state.policy = { ...JSON.parse(String(options.body)), revision: 1 };
      state.etag = '"1"';
    }
    return response(state);
  });
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(ContextPolicyPanel);
  await flushPromises();
  expect(wrapper.text()).toContain("未知，使用本地估算限制");
  expect(wrapper.text()).toContain("不是精确 tokenizer 或 token 计费");
  await wrapper.get('[aria-label="请求字节上限"]').setValue(80000);
  expect(fetch).toHaveBeenCalledTimes(1);
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  expect(fetch.mock.calls[1]![0]).toBe("/api/admin/v1/context-policy");
  expect(fetch.mock.calls[1]![1]?.headers).toMatchObject({
    "If-Match": '"0"',
    "X-CSRF-Token": "budget-test-csrf",
  });
  expect(JSON.parse(String(fetch.mock.calls[1]![1]?.body))).toMatchObject({
    revision: 0,
    model_id: "test-model",
    max_input_bytes: 80000,
    provider_context_window: null,
    window_basis: "unknown",
  });
  expect(wrapper.text()).toContain("用于新的请求");
  expect(wrapper.text()).toContain("预算 v1");
  expect(
    fetch.mock.calls.every(([url]) => url.endsWith("/context-policy")),
  ).toBe(true);
  expect(local).not.toHaveBeenCalled();
});

it("requires explicit current-service binding before declaring a model window and enforces host bounds", async () => {
  const fetch = vi.fn(async (_url: string, options?: RequestInit) =>
    response(
      options?.method === "PUT"
        ? {
            ...view(),
            policy: { ...JSON.parse(String(options.body)), revision: 1 },
            etag: '"1"',
          }
        : view(),
    ),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(ContextPolicyPanel);
  await flushPromises();
  await wrapper.get('[aria-label="请求字节上限"]').setValue(96001);
  expect(wrapper.get('[type="submit"]').attributes("disabled")).toBeDefined();
  await wrapper.get("form").trigger("submit");
  expect(fetch).toHaveBeenCalledTimes(1);
  await wrapper.get('[aria-label="请求字节上限"]').setValue(96000);
  await wrapper.get('[aria-label="声明当前服务窗口"]').setValue(true);
  await wrapper.get('[aria-label="服务窗口声明"]').setValue(32000);
  expect(wrapper.get('[type="submit"]').attributes("disabled")).toBeDefined();
  await wrapper.get('[aria-label="确认窗口绑定当前服务"]').setValue(true);
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  expect(JSON.parse(String(fetch.mock.calls[1]![1]?.body))).toMatchObject({
    provider_context_window: 32000,
    window_basis: "administrator_declared",
    provider_fingerprint: "a".repeat(64),
  });
});

it("keeps edits on conflict and requires an explicit confirmed reload before retry", async () => {
  const fetch = vi.fn(async (_url: string, options?: RequestInit) =>
    options?.method === "PUT"
      ? response(
          { error: { code: "context_policy_conflict", message: "changed" } },
          409,
        )
      : response(view()),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(ContextPolicyPanel);
  await flushPromises();
  await wrapper.get('[aria-label="输入估算额度"]').setValue(40000);
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  expect(wrapper.get('[role="alert"]').text()).toContain("你的修改仍保留");
  expect(
    (wrapper.get('[aria-label="输入估算额度"]').element as HTMLInputElement)
      .value,
  ).toBe("40000");
  expect(wrapper.get('[type="submit"]').attributes("disabled")).toBeDefined();
  vi.stubGlobal(
    "confirm",
    vi.fn(() => false),
  );
  await wrapper
    .findAll("button")
    .find((item) => item.text() === "重新载入预算")!
    .trigger("click");
  expect(fetch).toHaveBeenCalledTimes(2);
  vi.stubGlobal(
    "confirm",
    vi.fn(() => true),
  );
  await wrapper
    .findAll("button")
    .find((item) => item.text() === "重新载入预算")!
    .trigger("click");
  await flushPromises();
  expect(fetch).toHaveBeenCalledTimes(3);
  expect(
    (wrapper.get('[aria-label="输入估算额度"]').element as HTMLInputElement)
      .value,
  ).toBe("96000");
});

it("does not silently reuse a declared window after the endpoint binding changes", async () => {
  const changed = view();
  changed.policy.provider_context_window = 32000;
  changed.policy.window_basis = "administrator_declared";
  changed.policy.provider_fingerprint = "b".repeat(64);
  changed.provider_binding_matches = false;
  changed.window_status = "provider_binding_changed";
  changed.applied = false;
  const fetch = vi.fn(async (_url: string, options?: RequestInit) =>
    response(
      options?.method === "PUT"
        ? {
            ...changed,
            policy: { ...JSON.parse(String(options.body)), revision: 1 },
            etag: '"1"',
            provider_binding_matches: true,
            applied: true,
          }
        : changed,
    ),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(ContextPolicyPanel);
  await flushPromises();
  expect(wrapper.text()).toContain("服务绑定已变化");
  expect(wrapper.get('[type="submit"]').attributes("disabled")).toBeDefined();
  await wrapper.get('[aria-label="确认窗口绑定当前服务"]').setValue(true);
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  expect(
    JSON.parse(String(fetch.mock.calls[1]![1]?.body)).provider_fingerprint,
  ).toBe("a".repeat(64));
});

it("reports authentication expiry without attempting any mutation", async () => {
  const fetch = vi.fn(async () =>
    response({ error: { code: "unauthorized", message: "请重新登录" } }, 401),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(ContextPolicyPanel);
  await flushPromises();
  expect(wrapper.emitted("unauthorized")).toHaveLength(1);
  expect(wrapper.get('[role="alert"]').text()).toContain("请重新登录");
  expect(fetch).toHaveBeenCalledTimes(1);
});
