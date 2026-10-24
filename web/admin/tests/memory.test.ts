// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import MemoryPanel from "../src/components/MemoryPanel.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const item = {
  id: "memory-1",
  assistant_id: "assistant-stable-identity",
  content: "已确认的共同经历",
  visibility: "private",
  source_conversation_id: "conversation-1",
  source_message_id: "message-1",
  revision: 4,
  created_at: 1792830000,
  updated_at: 1792830060,
};
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function list(items: unknown[] = [item], total = items.length) {
  return response({
    assistant_id: item.assistant_id,
    items,
    total,
    limit: 25,
    offset: 0,
  });
}
function setup(
  handler?: (
    path: string,
    options?: RequestInit,
  ) => Response | Promise<Response> | undefined,
) {
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1", "");
    const custom = await handler?.(path, options);
    if (custom) return custom;
    if (path === "/conversations")
      return response({
        items: [
          {
            id: "conversation-1",
            title: "私人来源会话",
            created_at: 1792830000,
          },
        ],
      });
    if (path.startsWith("/memory?") && options?.method === "GET") return list();
    throw new Error(`Unexpected request: ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("memory-test-csrf");
  wrapper = mount(MemoryPanel);
  return fetch;
}
function button(text: string) {
  const found = wrapper!
    .findAll("button")
    .find((candidate) => candidate.text() === text);
  if (!found) throw new Error(`Button missing: ${text}`);
  return found;
}
function createForm() {
  return wrapper!.get('section[aria-label="新增记忆"] form');
}
function editForm() {
  return wrapper!.get('section[aria-label="编辑记忆"] form');
}

describe("assistant memory management", () => {
  it("keeps the assistant identity visible and applies filters to every page request", async () => {
    const fetch = setup((path, options) => {
      if (path.startsWith("/memory?") && options?.method === "GET")
        return list([item], 51);
    });
    await flushPromises();
    expect(wrapper!.text()).toContain(
      "记忆属于助手，不随渠道、会话或角色卡版本丢失",
    );
    expect(wrapper!.get("code").attributes("title")).toBe(item.assistant_id);
    expect(wrapper!.text()).toContain("message-1");
    const filters = wrapper!.get('form[aria-label="筛选记忆"]');
    expect(filters.find('input[aria-label="查询内容"]').exists()).toBe(true);
    expect(filters.find('select[aria-label="可见范围"]').exists()).toBe(true);
    expect(filters.find('select[aria-label="来源会话"]').exists()).toBe(true);
    const create = createForm();
    expect(create.find('select[aria-label="使用范围"]').exists()).toBe(true);
    const content = create.get('textarea[aria-label="记忆内容"]');
    expect(
      create.get(`[id="${content.attributes("aria-describedby")}"]`).text(),
    ).toContain("4,000 UTF-8 字节");
    const source = create.get('select[aria-label="来源会话"]');
    expect(source.attributes("aria-required")).toBe("true");
    expect(
      create.get(`[id="${source.attributes("aria-describedby")}"]`).text(),
    ).toContain("不自动进入其他账号或群聊");
    expect(create.find('input[aria-label="来源消息 ID（可选）"]').exists()).toBe(
      true,
    );
    await filters.get('input[type="search"]').setValue("关键词 & OR");
    const selects = filters.findAll("select");
    await selects[0]!.setValue("private");
    await selects[1]!.setValue("conversation-1");
    await filters.trigger("submit");
    await flushPromises();
    await button("下一页").trigger("click");
    await flushPromises();
    const url = new URL(
      String(fetch.mock.calls.at(-1)![0]),
      "http://localhost",
    );
    expect(url.pathname).toBe("/api/admin/v1/memory");
    expect(Object.fromEntries(url.searchParams)).toMatchObject({
      q: "关键词 & OR",
      visibility: "private",
      source_conversation_id: "conversation-1",
      limit: "25",
      offset: "25",
    });
  });

  it("requires the private source and counts UTF-8 bytes before saving", async () => {
    const fetch = setup((path, options) => {
      if (path === "/memory" && options?.method === "POST")
        return response({ ...item, revision: 1 });
    });
    await flushPromises();
    const form = createForm();
    await form.get("textarea").setValue("中".repeat(1334));
    expect(button("保存记忆").attributes()).toHaveProperty("disabled");
    await form.trigger("submit");
    expect(wrapper!.text()).toContain("不能超过 4,000 UTF-8 字节");
    await form.get("textarea").setValue("  已确认的事实  ");
    await form.trigger("submit");
    expect(wrapper!.text()).toContain("私有记忆必须选择来源会话");
    expect(
      fetch.mock.calls.filter(([, options]) => options?.method === "POST"),
    ).toHaveLength(0);
    await form.findAll("select")[1]!.setValue("conversation-1");
    await form.get("input").setValue("message-1");
    await form.trigger("submit");
    await flushPromises();
    const mutation = fetch.mock.calls.find(
      ([, options]) => options?.method === "POST",
    )!;
    expect(JSON.parse(String(mutation[1]?.body))).toEqual({
      content: "已确认的事实",
      visibility: "private",
      source_conversation_id: "conversation-1",
      source_message_id: "message-1",
    });
    expect(mutation[1]?.headers).toMatchObject({
      "X-CSRF-Token": "memory-test-csrf",
    });
  });

  it("requires an explicit cross-channel confirmation before publishing shared memory", async () => {
    const fetch = setup((path, options) => {
      if (path === "/memory" && options?.method === "POST")
        return response({ ...item, visibility: "shared", revision: 1 });
    });
    await flushPromises();
    const form = createForm();
    await form.get("textarea").setValue("可公开使用的项目事实");
    await form.findAll("select")[0]!.setValue("shared");
    await form.trigger("submit");
    expect(wrapper!.text()).toContain("请明确确认允许助手在所有渠道使用");
    expect(
      fetch.mock.calls.filter(([, options]) => options?.method === "POST"),
    ).toHaveLength(0);
    await form.get('input[type="checkbox"]').setValue(true);
    await form.trigger("submit");
    await flushPromises();
    const mutation = fetch.mock.calls.find(
      ([, options]) => options?.method === "POST",
    )!;
    expect(JSON.parse(String(mutation[1]?.body))).toEqual({
      content: "可公开使用的项目事实",
      visibility: "shared",
      publish: true,
    });
  });

  it("preserves an edit after a conflict and merges against the explicitly reloaded version", async () => {
    let patchCount = 0;
    const fetch = setup((path, options) => {
      if (path === "/memory/memory-1" && options?.method === "GET")
        return response({
          ...item,
          revision: 5,
          content: "另一处刚更新的内容",
        });
      if (path === "/memory/memory-1" && options?.method === "PATCH") {
        patchCount++;
        return patchCount === 1
          ? response({ error: { code: "conflict", message: "stale" } }, 409)
          : response({ ...item, revision: 6 });
      }
    });
    await flushPromises();
    await button("编辑纠正").trigger("click");
    expect(editForm().find('textarea[aria-label="纠正后的内容"]').exists()).toBe(
      true,
    );
    expect(
      editForm().find('select[aria-label="纠正后的可见范围"]').exists(),
    ).toBe(true);
    await editForm().get("textarea").setValue("我正在编辑的纠正草稿");
    await editForm().trigger("submit");
    await flushPromises();
    expect(wrapper!.text()).toContain("你的编辑草稿仍保留");
    expect(
      (editForm().get("textarea").element as HTMLTextAreaElement).value,
    ).toBe("我正在编辑的纠正草稿");
    expect(button("保存纠正").attributes()).toHaveProperty("disabled");
    await button("载入最新版本").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("另一处刚更新的内容");
    await button("已对照，采用最新版本号并保留草稿").trigger("click");
    expect(
      (editForm().get("textarea").element as HTMLTextAreaElement).value,
    ).toBe("我正在编辑的纠正草稿");
    await editForm().trigger("submit");
    await flushPromises();
    const mutations = fetch.mock.calls.filter(
      ([, options]) => options?.method === "PATCH",
    );
    expect(mutations[0]![1]?.headers).toMatchObject({ "If-Match": '"4"' });
    expect(mutations[1]![1]?.headers).toMatchObject({ "If-Match": '"5"' });
    expect(JSON.parse(String(mutations[1]![1]?.body))).toEqual({
      content: "我正在编辑的纠正草稿",
    });
    expect(wrapper!.text()).toContain("纠正已保存，版本 6");
  });

  it("requires a separate publication confirmation when changing private memory to shared", async () => {
    const fetch = setup((path, options) => {
      if (path === "/memory/memory-1" && options?.method === "PATCH")
        return response({ ...item, visibility: "shared", revision: 5 });
    });
    await flushPromises();
    await button("编辑纠正").trigger("click");
    await editForm().get("select").setValue("shared");
    await editForm().trigger("submit");
    expect(
      fetch.mock.calls.filter(([, options]) => options?.method === "PATCH"),
    ).toHaveLength(0);
    expect(wrapper!.text()).toContain("将私有记忆改为共享前");
    await editForm().get('input[type="checkbox"]').setValue(true);
    await editForm().trigger("submit");
    await flushPromises();
    const mutation = fetch.mock.calls.find(
      ([, options]) => options?.method === "PATCH",
    )!;
    expect(JSON.parse(String(mutation[1]?.body))).toEqual({
      content: item.content,
      visibility: "shared",
      publish: true,
    });
  });

  it("requires specific forgetting confirmation and reports the retained copies accurately", async () => {
    let deleted = false;
    const fetch = setup((path, options) => {
      if (path === "/memory/memory-1" && options?.method === "DELETE") {
        deleted = true;
        return response({
          id: item.id,
          revision: 5,
          deleted: true,
          deleted_at: 1792830120,
          cleanup: {
            memory_content: "removed",
            conversation_history: "retained",
            offline_backups: "retained",
          },
        });
      }
      if (path.startsWith("/memory?") && deleted) return list([]);
    });
    await flushPromises();
    await button("遗忘此条").trigger("click");
    const confirmation = wrapper!.get('[aria-label="确认遗忘"]');
    expect(confirmation.text()).toContain(item.content);
    expect(confirmation.text()).toContain("版本 4");
    expect(
      fetch.mock.calls.filter(([, options]) => options?.method === "DELETE"),
    ).toHaveLength(0);
    await button("确认遗忘此条").trigger("click");
    await flushPromises();
    const mutation = fetch.mock.calls.find(
      ([, options]) => options?.method === "DELETE",
    )!;
    expect(mutation[1]?.headers).toMatchObject({ "If-Match": '"4"' });
    expect(wrapper!.text()).toContain("原始消息与离线备份仍独立保留");
    expect(wrapper!.find('[data-memory-id="memory-1"]').exists()).toBe(false);
  });

  it("exports through the authenticated API to a download without browser storage", async () => {
    const payload = {
      schema: "dsh.assistant-memory.v1",
      assistant_id: item.assistant_id,
      items: [item],
    };
    const fetch = setup((path) =>
      path === "/memory/export" ? response(payload) : undefined,
    );
    const createUrl = vi.fn((_blob: Blob) => "blob:memory-export");
    vi.stubGlobal(
      "URL",
      class extends URL {
        static createObjectURL = createUrl;
        static revokeObjectURL = vi.fn();
      },
    );
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    const localStorageWrite = vi.spyOn(Storage.prototype, "setItem");
    await flushPromises();
    await button("导出记忆").trigger("click");
    await flushPromises();
    expect(
      fetch.mock.calls.some(([url]) => url === "/api/admin/v1/memory/export"),
    ).toBe(true);
    expect(createUrl).toHaveBeenCalledTimes(1);
    expect(
      JSON.parse(await (createUrl.mock.calls[0]![0] as Blob).text()),
    ).toEqual(payload);
    expect(click).toHaveBeenCalledTimes(1);
    expect(localStorageWrite).not.toHaveBeenCalled();
    expect(wrapper!.text()).toContain("文件包含私有内容");
  });

  it("removes forgotten content from the view even when the subsequent refresh fails", async () => {
    let deleted = false;
    setup((path, options) => {
      if (path === "/memory/memory-1" && options?.method === "DELETE") {
        deleted = true;
        return response({ id: item.id, revision: 5, deleted: true });
      }
      if (path.startsWith("/memory?") && deleted)
        return response({ error: "temporarily_unavailable" }, 503);
    });
    await flushPromises();
    await button("遗忘此条").trigger("click");
    await button("确认遗忘此条").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("记忆列表未能更新");
    expect(wrapper!.text()).toContain("该记忆库条目及其旧版本已清除");
    expect(wrapper!.text()).not.toContain(item.content);
    expect(wrapper!.find('[data-memory-id="memory-1"]').exists()).toBe(false);
  });
});
