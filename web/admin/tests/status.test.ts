// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import App from "../src/App.vue";
import { describeQqConnection } from "../src/connection";
import type { RuntimeConfig, SystemStatus } from "../src/types";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  location.hash = "";
});

function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function server() {
  const config: RuntimeConfig = {
    assistant: {
      name: "Original assistant",
      system_prompt: "Test",
      max_concurrent_runs: 2,
      timeout_seconds: 120,
    },
    model: {
      base_url: "https://api.deepseek.com",
      model: "test-model",
      credential_ref: "model-test",
    },
    qq: {
      enabled: true,
      app_id: "123456",
      credential_ref: "qq-test",
      transport: "websocket",
    },
  };
  const state = {
    connection: "connecting",
    statusCalls: 0,
    configCalls: 0,
    statusHandler: null as
      null | ((call: number, options?: RequestInit) => Promise<Response>),
  };
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const url = input.replace("/api/admin/v1", "");
    if (url === "/auth/status") return response({ initialized: true });
    if (url === "/auth/me")
      return response({
        user: { username: "admin", role: "admin" },
        csrf_token: "test-csrf",
      });
    if (url === "/status") {
      state.statusCalls++;
      if (state.statusHandler)
        return state.statusHandler(state.statusCalls, options);
      return response({
        version: "test",
        channels: {
          qq: {
            enabled: true,
            configured: true,
            connection_status: state.connection,
          },
        },
      });
    }
    if (url === "/config") {
      state.configCalls++;
      return response({ revision: 1, applied_revision: 1, config });
    }
    if (url === "/secrets" || url === "/conversations")
      return response({ items: [] });
    if (url === "/config/drafts")
      return response({ id: "test-draft", base_revision: 1, config });
    if (url.endsWith("/checks"))
      return response({
        ok: true,
        checks: [{ name: "qq", ok: true, message: "凭证鉴权通过" }],
      });
    throw new Error(`Unexpected test request ${url}`);
  });
  return { state, fetch };
}

describe("QQ live status", () => {
  it("does not treat a configured credential as a connected gateway", () => {
    const sample = {
      channels: {
        qq: {
          enabled: true,
          configured: true,
          connection_status: "connecting",
        },
      },
    } as SystemStatus;
    expect(describeQqConnection(sample, false).connected).toBe(false);
    sample.channels!.qq!.connection_status = "connected";
    expect(describeQqConnection(sample, false).connected).toBe(true);
    expect(describeQqConnection(sample, true).connected).toBe(false);
  });

  it("polls only status and preserves unsaved configuration, with manual refresh and cleanup", async () => {
    vi.useFakeTimers();
    vi.spyOn(document, "hidden", "get").mockReturnValue(false);
    location.hash = "#assistant";
    const { state, fetch } = server();
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(App);
    await flushPromises();
    await wrapper
      .get('input[placeholder="例如：我的日常助手"]')
      .setValue("Unsaved change");
    state.connection = "connected";
    await vi.advanceTimersByTimeAsync(5000);
    await flushPromises();
    expect(state.statusCalls).toBe(2);
    expect(state.configCalls).toBe(1);
    expect(
      (
        wrapper.get('input[placeholder="例如：我的日常助手"]')
          .element as HTMLInputElement
      ).value,
    ).toBe("Unsaved change");
    await wrapper
      .findAll("button")
      .find((button) => button.text() === "QQ 连接")!
      .trigger("click");
    expect(wrapper.get('[data-testid="qq-connection-status"]').text()).toBe(
      "WebSocket 已连接",
    );
    expect(wrapper.get('[data-testid="status-sampled-at"]').text()).not.toBe(
      "—",
    );
    await wrapper
      .findAll("button")
      .find((button) => button.text() === "刷新状态")!
      .trigger("click");
    await flushPromises();
    expect(state.statusCalls).toBe(3);
    expect(state.configCalls).toBe(1);
    wrapper.unmount();
    wrapper = undefined;
    await vi.advanceTimersByTimeAsync(15000);
    expect(state.statusCalls).toBe(3);
  });

  it("never overlaps slow status requests and returns to login when polling sees 401", async () => {
    vi.useFakeTimers();
    vi.spyOn(document, "hidden", "get").mockReturnValue(false);
    const { state, fetch } = server();
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(App);
    await flushPromises();
    let release: (response: Response) => void = () => {};
    state.statusHandler = (call) =>
      call === 2
        ? new Promise<Response>((resolve) => {
            release = resolve;
          })
        : Promise.resolve(
            response(
              { error: { code: "unauthorized", message: "Expired" } },
              401,
            ),
          );
    await vi.advanceTimersByTimeAsync(5000);
    expect(state.statusCalls).toBe(2);
    await vi.advanceTimersByTimeAsync(4000);
    expect(state.statusCalls).toBe(2);
    release(response({ version: "test" }));
    await flushPromises();
    await vi.advanceTimersByTimeAsync(5000);
    await flushPromises();
    expect(wrapper.text()).toContain("欢迎回来");
    await vi.advanceTimersByTimeAsync(15000);
    expect(state.statusCalls).toBe(3);
  });

  it("skips hidden pages and does not promote an authentication probe to connected", async () => {
    vi.useFakeTimers();
    let hidden = true;
    vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
    location.hash = "#qq";
    const { state, fetch } = server();
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(App);
    await flushPromises();
    await vi.advanceTimersByTimeAsync(5000);
    expect(state.statusCalls).toBe(1);
    hidden = false;
    await vi.advanceTimersByTimeAsync(5000);
    expect(state.statusCalls).toBe(2);
    await wrapper
      .findAll("button")
      .find((button) => button.text() === "保存草稿")!
      .trigger("click");
    await flushPromises();
    await wrapper
      .findAll("button")
      .find((button) => button.text() === "测试连接")!
      .trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain(
      "探测通过，仅覆盖列出的模型调用或凭证鉴权",
    );
    expect(wrapper.get('[data-testid="qq-connection-status"]').text()).toBe(
      "正在连接 WebSocket",
    );
  });
});
