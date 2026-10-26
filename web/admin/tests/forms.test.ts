// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import App from "../src/App.vue";
import type { RuntimeConfig } from "../src/types";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  vi.unstubAllGlobals();
  location.hash = "";
});

function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("management forms", () => {
  it("opens diary management without mounting or requesting retired memory and summary controls", async () => {
    location.hash = "#memory";
    const fetch = vi.fn(async (input: string) => {
      const url = input.replace("/api/admin/v1", "");
      if (url === "/auth/status") return response({ initialized: true });
      if (url === "/auth/me")
        return response({
          user: { username: "admin", role: "admin" },
          csrf_token: "test-csrf",
        });
      if (url === "/status")
        return response({ version: "test", model: {}, queue: {} });
      if (url === "/config")
        return response({
          revision: 1,
          applied_revision: 1,
          config: { assistant: {}, model: {}, qq: {} },
        });
      if (url === "/diary/status") return response({});
      if (
        url === "/secrets" ||
        url === "/conversations" ||
        url.startsWith("/diary/")
      )
        return response({ items: [], total: 0 });
      throw new Error(`Unexpected test request ${url}`);
    });
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(App);
    await flushPromises();
    expect(wrapper.text()).toContain("把经历慢慢记下来");
    expect(wrapper.text()).toContain("还没有这个范围的日记");
    expect(wrapper.text()).not.toContain("历史记忆与会话摘要兼容管理");
    for (const name of [
      "MemoryPanel",
      "MemoryCandidates",
      "ConversationSummary",
    ])
      expect(wrapper.findComponent({ name }).exists()).toBe(false);
    expect(
      fetch.mock.calls.some(([url]) =>
        /\/memory(?:[/?-]|$)|\/summary(?:[/?]|$)/.test(url),
      ),
    ).toBe(false);
  });

  it("rejects mismatched bootstrap passwords without creating an account", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(response({ initialized: false, version: "test" }));
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(App);
    await flushPromises();
    await wrapper.get('input[autocomplete="username"]').setValue("admin");
    await wrapper
      .get('input[autocomplete="off"]')
      .setValue("pairing-test-only");
    const passwords = wrapper.findAll('input[autocomplete="new-password"]');
    await passwords[0]!.setValue("long-password-one");
    await passwords[1]!.setValue("long-password-two");
    await wrapper.get("form").trigger("submit");
    await flushPromises();
    expect(wrapper.text()).toContain("两次输入的密码不一致");
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("keeps a replacement key out of configuration drafts and requires validation before apply", async () => {
    location.hash = "#assistant";
    let revision = 3;
    const config: RuntimeConfig = {
      assistant: {
        name: "Test assistant",
        system_prompt: "Test",
        max_concurrent_runs: 2,
        timeout_seconds: 120,
      },
      model: {
        base_url: "https://api.deepseek.com",
        model: "test-model",
        credential_ref: "secret-old",
      },
      qq: {
        enabled: false,
        app_id: "",
        credential_ref: null,
        transport: "websocket",
      },
    };
    const fetch = vi.fn(async (input: string, options?: RequestInit) => {
      const url = input.replace("/api/admin/v1", "");
      if (url === "/auth/status") return response({ initialized: true });
      if (url === "/auth/me")
        return response({
          user: { username: "admin", role: "admin" },
          csrf_token: "test-csrf",
        });
      if (url === "/status")
        return response({
          version: "test",
          model: { configured: true, model: "test-model" },
          queue: {},
          config: { desired_revision: revision, applied_revision: revision },
        });
      if (url === "/config")
        return response({ revision, applied_revision: revision, config });
      if (url === "/secrets" && options?.method === "POST")
        return response({
          id: "secret-new",
          kind: "model_api_key",
          configured: true,
        });
      if (url === "/secrets" || url === "/conversations")
        return response({ items: [] });
      if (url === "/config/drafts")
        return response({ id: "draft-1", base_revision: revision, config });
      if (url.endsWith("/validate"))
        return response({ valid: true, errors: [] });
      if (url.endsWith("/apply")) {
        revision = 4;
        return response(
          {
            id: "apply-1",
            status: "completed",
            desired_revision: 4,
            applied_revision: 4,
          },
          202,
        );
      }
      throw new Error(`Unexpected test request ${url}`);
    });
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(App);
    await flushPromises();
    const findButton = (text: string) =>
      wrapper!.findAll("button").find((button) => button.text() === text)!;
    expect(findButton("应用配置").attributes()).toHaveProperty("disabled");
    await wrapper
      .get('input[placeholder="输入新的 API Key"]')
      .setValue("not-a-real-provider-key");
    await findButton("保存草稿").trigger("click");
    await flushPromises();
    const draftRequest = fetch.mock.calls.find(([url]) =>
      url.endsWith("/config/drafts"),
    )!;
    expect(String(draftRequest[1]?.body)).not.toContain(
      "not-a-real-provider-key",
    );
    expect(
      JSON.parse(String(draftRequest[1]?.body)).config.model.credential_ref,
    ).toBe("secret-new");
    expect(findButton("应用配置").attributes()).toHaveProperty("disabled");
    await findButton("校验配置").trigger("click");
    await flushPromises();
    expect(findButton("应用配置").attributes()).not.toHaveProperty("disabled");
    await findButton("应用配置").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("配置已生效");
    const applyRequest = fetch.mock.calls.find(([url]) =>
      url.endsWith("/apply"),
    )!;
    expect(applyRequest[1]?.headers).toMatchObject({ "If-Match": '"3"' });
  });
});
