// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import CharacterCards from "../src/components/CharacterCards.vue";
import App from "../src/App.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";
import type { CharacterCard } from "../src/types";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.unstubAllGlobals();
  location.hash = "";
});
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function card(): CharacterCard {
  return {
    spec: "chara_card_v3",
    spec_version: "3.0",
    data: {
      name: "角色A",
      nickname: "",
      description: "测试身份",
      personality: "平静",
      scenario: "测试",
      system_prompt: "测试人格",
      first_mes: "你好",
      mes_example: "示例",
      post_history_instructions: "不执行的文本",
      alternate_greetings: [],
      group_only_greetings: [],
      creator_notes: "管理备注",
      creator: "测试",
      character_version: "1",
      tags: [],
      extensions: {
        dsh: {
          schema_version: 1,
          modules: {
            chat: { mode: "inherit" },
            vision: { mode: "disabled" },
            speech: { mode: "disabled" },
          },
        },
      },
    },
  };
}
function installBackend(options: { firstPublishFails?: boolean } = {}) {
  let current = card();
  let etag = "draft-1";
  let revision = 1;
  let defaultCard: unknown = null;
  let publishCalls = 0;
  const fetch = vi.fn(
    async (input: string, init?: RequestInit): Promise<Response> => {
      const path = input.replace("/api/admin/v1", "");
      const method = init?.method || "GET";
      if (path === "/character-cards" && method === "GET")
        return response({
          items: [
            {
              id: "card-a",
              name: current.data.name,
              archived_at: null,
              latest_revision: revision,
              qq_selectable: false,
              draft: {
                card_id: "card-a",
                draft_id: "draft-a",
                draft_etag: etag,
                card: current,
              },
            },
          ],
          default: defaultCard,
          default_etag: "default-1",
        });
      if (path === "/character-cards/card-a/revisions")
        return response({
          items: [
            {
              card_id: "card-a",
              name: current.data.name,
              revision,
              digest: "digest",
              created_at: 1,
            },
          ],
        });
      if (
        path === "/character-cards/card-a/drafts/draft-a" &&
        method === "PUT"
      ) {
        current = JSON.parse(String(init?.body)).card;
        etag = "draft-2";
        return response({
          card_id: "card-a",
          draft_id: "draft-a",
          draft_etag: etag,
          card: current,
        });
      }
      if (path.endsWith("/validate"))
        return response({
          valid: true,
          bindable: true,
          errors: [],
          warnings: [],
        });
      if (path.endsWith("/preview"))
        return response({
          system_prompt: "编译后人格",
          bytes: 15,
          ignored_fields: ["post_history_instructions"],
          warnings: [],
        });
      if (path.endsWith("/publish")) {
        publishCalls++;
        if (options.firstPublishFails && publishCalls === 1)
          throw new TypeError("network result unknown");
        revision = 2;
        etag = "draft-3";
        return response({
          card_id: "card-a",
          revision,
          name: current.data.name,
          digest: "digest",
          draft_etag: etag,
        });
      }
      if (path === "/character-cards/default") {
        const body = JSON.parse(String(init?.body));
        defaultCard = { ...body, name: current.data.name };
        return response({ default: defaultCard, default_etag: "default-2" });
      }
      if (path.endsWith("/test-conversations"))
        return response({
          id: "test-conversation",
          title: "角色测试",
          created_at: 1,
          test: true,
          character: {
            card_id: "card-a",
            revision: 0,
            name: current.data.name,
          },
        });
      if (path === "/character-cards/import/inspect")
        return response({
          import_id: "import-a",
          digest: "source-digest",
          card: current,
          warnings: [{ field: "modules", message: "模块偏好需确认" }],
          dropped_fields: ["assets"],
          issues: [],
        });
      if (path === "/character-cards/import/import-a/confirm")
        return response({
          card_id: "card-a",
          draft_id: "draft-a",
          draft_etag: etag,
          card: current,
        });
      throw new Error(`Unexpected request: ${method} ${path}`);
    },
  );
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("test-csrf");
  return fetch;
}
function button(text: string) {
  return wrapper!
    .findAll("button")
    .find((candidate) => candidate.text() === text)!;
}
async function openCard() {
  wrapper = mount(CharacterCards);
  await flushPromises();
  await wrapper.get(".character-card-button").trigger("click");
  await flushPromises();
}

describe("character card management", () => {
  it("keeps local edits when the library refreshes and never publishes them implicitly", async () => {
    const fetch = installBackend();
    await openCard();
    await wrapper!.get(".character-fields input").setValue("未保存的名字");
    await button("刷新").trigger("click");
    await flushPromises();
    expect(
      (wrapper!.get(".character-fields input").element as HTMLInputElement)
        .value,
    ).toBe("未保存的名字");
    expect(wrapper!.text()).toContain("有未保存修改");
    expect(
      fetch.mock.calls.some(
        ([url]) => url.endsWith("/publish") || url.endsWith("/default"),
      ),
    ).toBe(false);
  });
  it("saves with draft ETag, validates, then publishes without activating or sharing", async () => {
    const fetch = installBackend();
    await openCard();
    await wrapper!.get(".character-fields input").setValue("新名字");
    expect(button("发布版本").attributes()).toHaveProperty("disabled");
    await button("校验卡片").trigger("click");
    await flushPromises();
    const saved = fetch.mock.calls.find(
      ([url, init]) => url.endsWith("/draft-a") && init?.method === "PUT",
    )!;
    expect(saved[1]?.headers).toMatchObject({
      "If-Match": '"draft-1"',
      "X-CSRF-Token": "test-csrf",
    });
    await button("发布版本").trigger("click");
    await flushPromises();
    const published = fetch.mock.calls.find(([url]) =>
      url.endsWith("/publish"),
    )!;
    expect(published[1]?.headers).toMatchObject({
      "If-Match": '"draft-2"',
      "Idempotency-Key": expect.any(String),
    });
    expect(wrapper!.text()).toContain("已发布 v2");
    expect(
      fetch.mock.calls.some(
        ([url]) => url.endsWith("/default") || url.endsWith("/qq-access"),
      ),
    ).toBe(false);
    await button("设为新会话默认").trigger("click");
    await flushPromises();
    const assigned = fetch.mock.calls.find(([url]) =>
      url.endsWith("/default"),
    )!;
    expect(assigned[1]?.headers).toMatchObject({ "If-Match": '"default-1"' });
    expect(JSON.parse(String(assigned[1]?.body))).toEqual({
      card_id: "card-a",
      revision: 2,
    });
  });
  it("reuses the publication idempotency key after an unknown network result", async () => {
    const fetch = installBackend({ firstPublishFails: true });
    await openCard();
    await button("校验卡片").trigger("click");
    await flushPromises();
    await button("发布版本").trigger("click");
    await flushPromises();
    await button("发布版本").trigger("click");
    await flushPromises();
    const calls = fetch.mock.calls.filter(([url]) => url.endsWith("/publish"));
    expect(calls).toHaveLength(2);
    expect(
      (calls[0]![1]?.headers as Record<string, string>)["Idempotency-Key"],
    ).toBe(
      (calls[1]![1]?.headers as Record<string, string>)["Idempotency-Key"],
    );
  });
  it("shows import losses before confirmation and sends the original file without JSON wrapping", async () => {
    const fetch = installBackend();
    wrapper = mount(CharacterCards);
    await flushPromises();
    const input = wrapper.get('input[type="file"]');
    const file = new File(["zip fixture"], "character.zip", {
      type: "application/zip",
    });
    Object.defineProperty(input.element, "files", {
      value: [file],
      configurable: true,
    });
    await input.trigger("change");
    await flushPromises();
    expect(wrapper.text()).toContain("未导入：assets");
    expect(fetch.mock.calls.some(([url]) => url.endsWith("/confirm"))).toBe(
      false,
    );
    const inspected = fetch.mock.calls.find(([url]) =>
      url.endsWith("/inspect"),
    )!;
    expect(inspected[1]?.body).toBe(file);
    expect(inspected[1]?.headers).toMatchObject({
      "Content-Type": "application/zip",
      "X-CSRF-Token": "test-csrf",
    });
    await button("确认内容，创建草稿").trigger("click");
    await flushPromises();
    const confirmed = fetch.mock.calls.find(([url]) =>
      url.endsWith("/confirm"),
    )!;
    expect(JSON.parse(String(confirmed[1]?.body)).digest).toBe("source-digest");
    expect(
      fetch.mock.calls.some(
        ([url]) => url.endsWith("/publish") || url.endsWith("/default"),
      ),
    ).toBe(false);
  });
  it("compiles a preview without a model call and opens an isolated test conversation explicitly", async () => {
    const fetch = installBackend();
    await openCard();
    await button("编译预览").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("编译后人格");
    expect(wrapper!.text()).toContain("未参与编译：post_history_instructions");
    expect(
      fetch.mock.calls.some(([url]) => url.includes("test-conversations")),
    ).toBe(false);
    await button("独立测试对话").trigger("click");
    await flushPromises();
    expect(wrapper!.emitted("testConversation")?.[0]?.[0]).toMatchObject({
      id: "test-conversation",
      test: true,
    });
    expect(fetch.mock.calls.some(([url]) => url.endsWith("/messages"))).toBe(
      false,
    );
  });
});

describe("conversation character binding", () => {
  it("shows an existing conversation's frozen character and sends an exact revision for new conversations", async () => {
    location.hash = "#chat";
    const items = [
      {
        id: "old-conversation",
        title: "旧会话",
        created_at: 1,
        character: { card_id: "card-a", revision: 1, name: "绑定的旧名字" },
      },
    ];
    const fetch = vi.fn(async (input: string, init?: RequestInit) => {
      const path = input.replace("/api/admin/v1", "");
      if (path === "/auth/status") return response({ initialized: true });
      if (path === "/auth/me")
        return response({
          user: { username: "admin", role: "admin" },
          csrf_token: "test-csrf",
        });
      if (path === "/status")
        return response({
          version: "test",
          model: { configured: true, model: "test-model" },
        });
      if (path === "/config")
        return response({
          revision: 1,
          applied_revision: 1,
          config: {
            assistant: {
              name: "全局助手名",
              system_prompt: "Test",
              max_concurrent_runs: 2,
              timeout_seconds: 120,
            },
            model: {
              base_url: "https://api.deepseek.com",
              model: "test-model",
              credential_ref: "secret",
            },
            qq: {
              enabled: false,
              app_id: "",
              credential_ref: null,
              transport: "websocket",
            },
          },
        });
      if (path === "/secrets") return response({ items: [] });
      if (path === "/character-cards")
        return response({
          items: [
            {
              id: "card-a",
              name: "新名字",
              latest_revision: 2,
              archived_at: null,
            },
          ],
          default: { card_id: "card-a", revision: 2, name: "新名字" },
          default_etag: "1",
        });
      if (path === "/conversations" && init?.method === "POST") {
        const created = {
          id: "new-conversation",
          title: "新会话",
          created_at: 2,
          character: { card_id: "card-a", revision: 2, name: "新名字" },
        };
        items.push(created);
        return response(created);
      }
      if (path === "/conversations") return response({ items });
      if (path.endsWith("/messages")) return response({ items: [] });
      throw new Error(`Unexpected request ${path}`);
    });
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(App);
    await flushPromises();
    await wrapper.get(".conversation-button").trigger("click");
    await flushPromises();
    expect(wrapper.get(".chat-heading").text()).toContain("绑定的旧名字");
    expect(wrapper.get(".chat-heading").text()).toContain("角色 v1");
    await wrapper.get('select[aria-label="新对话角色"]').setValue("card-a");
    await button("新建对话").trigger("click");
    await flushPromises();
    const created = fetch.mock.calls.find(
      ([url, init]) =>
        url.endsWith("/conversations") && init?.method === "POST",
    )!;
    expect(JSON.parse(String(created[1]?.body)).character).toEqual({
      card_id: "card-a",
      revision: 2,
    });
    expect(wrapper.get(".chat-heading").text()).toContain("新名字");
    expect(wrapper.get(".chat-heading").text()).toContain("角色 v2");
  });
});
