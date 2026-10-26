// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import AssistantPersonality from "../src/components/AssistantPersonality.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  clearCsrfToken();
});
const personality = {
  name: "刘嘉卉",
  description: "身份背景",
  personality: "温和坦诚",
  scenario: "日常相处",
  system_prompt: "认真回应",
  mes_example: "",
};
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
const snapshot = () => ({
  etag: "personality-7",
  revision: 7,
  personality: { ...personality },
  saved: true,
  applied: true,
  effective_from: "next_run",
});
it("edits the one personality with CSRF and revision, applying it independently of model configuration", async () => {
  setCsrfToken("test-csrf");
  const fetch = vi.fn(async (_url: string, options?: RequestInit) =>
    response(
      options?.method === "PUT"
        ? {
            ...snapshot(),
            etag: "personality-8",
            revision: 8,
            personality: JSON.parse(String(options.body)).personality,
          }
        : snapshot(),
    ),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(AssistantPersonality);
  await flushPromises();
  expect(wrapper.text()).toContain("已生效 v7");
  await wrapper.get("textarea").setValue("新的身份说明");
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  const call = fetch.mock.calls[1]!;
  expect(call[0]).toBe("/api/admin/v1/assistant/personality");
  expect(call[1]?.headers).toMatchObject({
    "If-Match": '"personality-7"',
    "X-CSRF-Token": "test-csrf",
  });
  expect(JSON.parse(String(call[1]?.body)).personality.description).toBe(
    "新的身份说明",
  );
  expect(wrapper.text()).toContain("已保存并生效");
  expect(wrapper.text()).toContain("已生效 v8");
  expect(
    fetch.mock.calls.some(
      ([url]) => url.includes("character-cards") || url.includes("config"),
    ),
  ).toBe(false);
});
it("preserves local text on conflict and requires explicit reload before retry", async () => {
  const fetch = vi.fn(async (_url: string, options?: RequestInit) =>
    options?.method === "PUT"
      ? response(
          { error: { code: "personality_conflict", message: "conflict" } },
          409,
        )
      : response(snapshot()),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(AssistantPersonality);
  await flushPromises();
  await wrapper.get("textarea").setValue("保留本地修改");
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  expect((wrapper.get("textarea").element as HTMLTextAreaElement).value).toBe(
    "保留本地修改",
  );
  expect(wrapper.get('[type="submit"]').attributes("disabled")).toBeDefined();
  expect(wrapper.get('[role="alert"]').text()).toContain("你的修改仍保留");
  vi.stubGlobal(
    "confirm",
    vi.fn(() => false),
  );
  await wrapper
    .findAll("button")
    .find((button) => button.text() === "重新载入性格")!
    .trigger("click");
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("restores a historical personality using the current version as a new update", async () => {
  const old = { ...personality, personality: "旧版性格" };
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    if (url.endsWith("/history"))
      return response({
        items: [{ revision: 3, personality: old, created_at: 1792830000 }],
      });
    return response(
      options?.method === "PUT"
        ? {
            ...snapshot(),
            revision: 8,
            etag: "personality-8",
            personality: old,
          }
        : snapshot(),
    );
  });
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(AssistantPersonality);
  await flushPromises();
  const details = wrapper.get("details");
  (details.element as HTMLDetailsElement).open = true;
  await details.trigger("toggle");
  await flushPromises();
  await wrapper
    .findAll("button")
    .find((button) => button.text() === "恢复此版本")!
    .trigger("click");
  await flushPromises();
  const update = fetch.mock.calls.find(
    ([, options]) => options?.method === "PUT",
  )!;
  expect(update[1]?.headers).toMatchObject({ "If-Match": '"personality-7"' });
  expect(JSON.parse(String(update[1]?.body)).personality.personality).toBe(
    "旧版性格",
  );
  expect(wrapper.text()).toContain("已生效 v8");
  expect(wrapper.text()).toContain("不回退模型配置或历史运行记录");
});
