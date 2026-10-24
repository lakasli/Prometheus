// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import ConversationSummary from "../src/components/ConversationSummary.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";
import { selectValue, selectValues } from "./select-helpers";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.unstubAllGlobals();
});
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function summary(id: string, content = `仅属于${id}的摘要`, revision = "4.8") {
  return {
    conversation_id: id,
    revision,
    summary: {
      content,
      revision: 8,
      source_count: 8,
      through_run_rowid: 32,
      epoch: 4,
      source_run_ids: ["source-run-1", "source-run-2"],
      source_digest: "digest-123",
    },
    last_job: { status: "completed", created_at: 1792830000, error_code: null },
    policy: {
      keep_recent_turns: 4,
      max_batch_turns: 8,
      max_source_bytes: 24000,
      max_summary_bytes: 6000,
      timeout_seconds: 20,
    },
  };
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
          { id: "a", title: "普通会话甲", created_at: 1792830000 },
          { id: "b", title: "普通会话乙", created_at: 1792830000 },
          {
            id: "draft-a",
            title: "顶层测试",
            test: true,
            created_at: 1792830000,
          },
          {
            id: "draft-b",
            title: "顶层草稿",
            is_test: true,
            created_at: 1792830000,
          },
          {
            id: "draft-c",
            title: "角色测试",
            character: { is_test: true },
            created_at: 1792830000,
          },
        ],
      });
    if (path === "/conversations/a/summary") return response(summary("a"));
    if (path === "/conversations/b/summary") return response(summary("b"));
    throw new Error(`Unexpected summary request: ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("summary-csrf");
  wrapper = mount(ConversationSummary);
  return fetch;
}
function button(label: string) {
  const found = wrapper!
    .findAll("button")
    .find((candidate) => candidate.text() === label);
  if (!found) throw new Error(`Missing button ${label}`);
  return found;
}
function select() {
  return wrapper!.get('[aria-label="查看摘要的会话"]');
}
async function choose(id = "a") {
  await selectValue(select(), id);
  await flushPromises();
}
async function confirmRebuild() {
  await button("重建摘要").trigger("click");
  await button("确认清除派生摘要并安排重建").trigger("click");
  await flushPromises();
}

describe("private conversation summaries", () => {
  it("shows only ordinary accessible conversations and traces the selected summary", async () => {
    const fetch = setup();
    await flushPromises();
    expect(await selectValues(select())).toEqual(["", "a", "b"]);
    expect(fetch).toHaveBeenCalledTimes(1);
    const help = select().attributes("aria-describedby");
    expect(wrapper!.get(`[id="${help}"]`).text()).toContain(
      "当前已认证账户可访问",
    );
    await choose();
    expect(wrapper!.text()).toContain(
      "仅用于所选来源会话，不共享到其他会话或群聊",
    );
    expect(wrapper!.text()).toContain("每次普通任务最多整理一次、最长 20 秒");
    expect(wrapper!.text()).toContain("不是系统指令");
    expect(wrapper!.get('[aria-label="当前派生摘要"]').text()).toContain(
      "仅属于a的摘要",
    );
    expect(wrapper!.text()).toContain("来源 8 轮");
    expect(wrapper!.text()).toContain("source-run-1");
    expect(wrapper!.text()).toContain("digest-123");
    expect(wrapper!.text()).not.toContain("仅属于b的摘要");
    expect(wrapper!.find(".summary-source-limits").exists()).toBe(false);
  });

  it("explains omitted and excerpted source counts without claiming a complete transcript", async () => {
    setup((path) =>
      path === "/conversations/a/summary"
        ? response({
            ...summary("a"),
            summary: {
              ...summary("a").summary,
              redacted_source_count: 2,
              truncated_source_count: 3,
            },
          })
        : undefined,
    );
    await flushPromises();
    await choose();
    const limits = wrapper!.get(".summary-source-limits");
    expect(limits.text()).toContain("已省略敏感来源 2 轮");
    expect(limits.text()).toContain("超长来源摘录 3 轮");
    expect(limits.text()).toContain("摘要不代表完整原文");
  });

  it("aborts a previous selection and ignores its late response", async () => {
    let resolveFirst: (value: Response) => void = () => {};
    const first = new Promise<Response>((resolve) => {
      resolveFirst = resolve;
    });
    const fetch = setup((path) =>
      path === "/conversations/a/summary" ? first : undefined,
    );
    await flushPromises();
    await choose("a");
    await choose("b");
    const firstCall = fetch.mock.calls.find(([url]) =>
      url.endsWith("/a/summary"),
    )!;
    expect(firstCall[1]?.signal?.aborted).toBe(true);
    expect(wrapper!.text()).toContain("仅属于b的摘要");
    resolveFirst(response(summary("a", "迟到的私人甲摘要")));
    await flushPromises();
    expect(wrapper!.text()).toContain("仅属于b的摘要");
    expect(wrapper!.text()).not.toContain("迟到的私人甲摘要");
  });

  it("requires specific confirmation and preserves the opaque version when scheduling a rebuild", async () => {
    let cleared = false;
    const fetch = setup((path, options) => {
      if (
        path === "/conversations/a/summary/rebuild" &&
        options?.method === "POST"
      ) {
        cleared = true;
        return response({ status: "pending_next_run", revision: "5.8" });
      }
      if (path === "/conversations/a/summary" && cleared)
        return response({
          ...summary("a", "", "5.8"),
          summary: null,
          last_job: { status: "pending_next_run", created_at: 1792830060 },
        });
    });
    await flushPromises();
    await choose();
    await button("重建摘要").trigger("click");
    const confirmation = wrapper!.get('[aria-label="确认重建摘要"]');
    expect(confirmation.text()).toContain("普通会话甲");
    expect(confirmation.text()).toContain("仅属于a的摘要");
    expect(confirmation.text()).toContain("原始消息保留");
    expect(
      fetch.mock.calls.filter(([, options]) => options?.method === "POST"),
    ).toHaveLength(0);
    await button("确认清除派生摘要并安排重建").trigger("click");
    await flushPromises();
    const mutation = fetch.mock.calls.find(
      ([, options]) => options?.method === "POST",
    )!;
    expect(mutation[0]).toBe("/api/admin/v1/conversations/a/summary/rebuild");
    expect(mutation[1]?.headers).toMatchObject({
      "If-Match": '"4.8"',
      "X-CSRF-Token": "summary-csrf",
    });
    expect(JSON.parse(String(mutation[1]?.body))).toEqual({});
    expect(wrapper!.text()).toContain("下次普通消息时重建；原始消息保留");
    expect(wrapper!.text()).not.toContain("仅属于a的摘要");
    expect(
      fetch.mock.calls.some(([path]) => /messages.*(delete|forget)/.test(path)),
    ).toBe(false);
  });

  it("retains the displayed summary after a conflict and requires a successful refresh before retry", async () => {
    let conflictSeen = false;
    const fetch = setup((path, options) => {
      if (path.endsWith("/summary/rebuild")) {
        conflictSeen = true;
        return response(
          { error: { code: "summary_revision_conflict", message: "stale" } },
          409,
        );
      }
      if (
        path === "/conversations/a/summary" &&
        conflictSeen &&
        options?.method === "GET"
      )
        return response(summary("a", "另一处更新后的甲摘要", "4.9"));
    });
    await flushPromises();
    await choose();
    await confirmRebuild();
    expect(wrapper!.text()).toContain("当前展示已保留，请刷新摘要");
    expect(wrapper!.get('[aria-label="当前派生摘要"]').text()).toContain(
      "仅属于a的摘要",
    );
    expect(button("重建摘要").attributes()).toHaveProperty("disabled");
    await button("刷新摘要").trigger("click");
    await flushPromises();
    expect(wrapper!.get('[aria-label="当前派生摘要"]').text()).toContain(
      "另一处更新后的甲摘要",
    );
    expect(button("重建摘要").attributes()).not.toHaveProperty("disabled");
    await confirmRebuild();
    const mutations = fetch.mock.calls.filter(
      ([, options]) => options?.method === "POST",
    );
    expect(mutations[1]![1]?.headers).toMatchObject({ "If-Match": '"4.9"' });
  });

  it("keeps the last successful view on refresh failure and rejects a response from the wrong conversation", async () => {
    let refreshes = 0;
    setup((path) => {
      if (path === "/conversations/a/summary" && ++refreshes > 1)
        return response({ error: "unavailable" }, 503);
      if (path === "/conversations/b/summary")
        return response(summary("a", "错误来源的私人摘要"));
    });
    await flushPromises();
    await choose();
    await button("刷新摘要").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("以下保留上次读取的内容");
    expect(wrapper!.text()).toContain("仅属于a的摘要");
    await choose("b");
    expect(wrapper!.text()).toContain("摘要响应与所选会话");
    expect(wrapper!.text()).not.toContain("错误来源的私人摘要");
    expect(wrapper!.text()).not.toContain("仅属于a的摘要");
  });

  it("cancels the selected conversation request when the component is closed", async () => {
    const fetch = setup((path, options) => {
      if (path.endsWith("/summary"))
        return new Promise<Response>((_resolve, reject) =>
          options?.signal?.addEventListener(
            "abort",
            () => reject(new DOMException("aborted", "AbortError")),
            { once: true },
          ),
        );
    });
    await flushPromises();
    await choose();
    const request = fetch.mock.calls.find(([url]) =>
      url.endsWith("/a/summary"),
    )!;
    expect(request[1]?.signal?.aborted).toBe(false);
    wrapper!.unmount();
    wrapper = undefined;
    expect(request[1]?.signal?.aborted).toBe(true);
    await flushPromises();
  });
});
