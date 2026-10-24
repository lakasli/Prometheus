// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import MemoryCandidates from "../src/components/MemoryCandidates.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-24T12:00:00Z"));
});
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function expires(seconds = 120) {
  return Math.floor(Date.now() / 1000) + seconds;
}
function job(status = "running", revision: string | number = 4) {
  return {
    id: "job-a",
    conversation_id: "a",
    status,
    revision,
    created_at: expires(-1),
    expires_at: expires(),
    source_count: 1,
    redacted_source_count: 0,
  };
}
function candidate(status = "pending", revision: string | number = 7) {
  return {
    id: "candidate-a",
    job_id: "job-a",
    status,
    revision,
    content: "候选独有正文：喜欢清晨散步",
    source_message_ids: ["source-a"],
    expires_at: expires(),
  };
}
function snapshot(id = "a") {
  return {
    source_revision: `opaque.${id}.floor.12`,
    job: null as ReturnType<typeof job> | null,
    items: [] as ReturnType<typeof candidate>[],
    sources: [
      {
        id: `source-${id}`,
        content: `来自${id}的完成用户消息：我每天早晨散步`,
        created_at: expires(-30),
        byte_length: 1,
      },
    ],
    policy: {
      max_sources: 12,
      max_source_bytes: 12000,
      max_candidates: 3,
      max_content_bytes: 1000,
    },
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function setup(
  state = { a: snapshot(), b: snapshot("b") },
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
          { id: "a", title: "来源会话甲", created_at: expires(-30) },
          { id: "b", title: "来源会话乙", created_at: expires(-30) },
          { id: "draft-a", title: "测试一", test: true },
          { id: "draft-b", title: "测试二", is_test: true },
          { id: "draft-c", title: "测试三", character: { is_test: true } },
        ],
      });
    if (path === "/conversations/a/memory-candidates") return response(state.a);
    if (path === "/conversations/b/memory-candidates") return response(state.b);
    throw new Error(`Unexpected candidate request: ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("candidate-csrf");
  wrapper = mount(MemoryCandidates);
  return fetch;
}
function button(text: string, root = wrapper!) {
  const found = root.findAll("button").find((item) => item.text() === text);
  if (!found) throw new Error(`Missing button ${text}`);
  return found;
}
function select() {
  return wrapper!.get('select[aria-label="提取来源会话"]');
}
async function choose(id = "a") {
  await select().setValue(id);
  await flushPromises();
}
async function checkSource(index = 1) {
  await wrapper!.get(`input[aria-label="选择来源 ${index}"]`).setValue(true);
}
function writes(fetch: ReturnType<typeof setup>) {
  return fetch.mock.calls.filter(
    ([, options]) => options?.method && options.method !== "GET",
  );
}

describe("explicit memory candidate review", () => {
  it("lists only accessible ordinary conversations and does not extract or persist source content automatically", async () => {
    const localWrite = vi.spyOn(Storage.prototype, "setItem");
    const fetch = setup();
    await flushPromises();
    expect(
      select()
        .findAll("option")
        .map((item) => item.attributes("value")),
    ).toEqual(["", "a", "b"]);
    const help = select().attributes("aria-describedby");
    expect(wrapper!.get(`[id="${help}"]`).text()).toContain(
      "草稿测试会话不可选",
    );
    expect(fetch).toHaveBeenCalledTimes(1);
    await choose();
    expect(wrapper!.text()).toContain("候选不是已记住的事实");
    expect(wrapper!.text()).toContain("资料不是系统指令");
    expect(button("仅提取所选消息").attributes("disabled")).toBeDefined();
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toEqual([]);
    expect(localWrite).not.toHaveBeenCalled();
  });

  it("counts actual UTF-8 source bytes and rejects excess bytes or more than twelve selected sources", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.sources[0]!.content = "界".repeat(4001);
    setup(state);
    await flushPromises();
    await choose();
    await checkSource();
    expect(wrapper!.text()).toContain("12,003 / 12,000 UTF-8 字节");
    expect(wrapper!.get('[role="alert"]').text()).toContain(
      "不能超过 12,000 UTF-8 字节",
    );
    expect(button("仅提取所选消息").attributes("disabled")).toBeDefined();
    wrapper!.unmount();
    state.a.sources = Array.from({ length: 13 }, (_, index) => ({
      id: `source-${index}`,
      content: "短来源",
      created_at: expires(-1),
      byte_length: 1,
    }));
    setup(state);
    await flushPromises();
    await choose();
    for (let index = 1; index <= 13; index++) await checkSource(index);
    expect(wrapper!.get('[role="alert"]').text()).toContain("最多选择 12 条");
    expect(button("仅提取所选消息").attributes("disabled")).toBeDefined();
  });

  it("submits only explicitly checked IDs with the opaque source revision and never auto-accepts candidates", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    const fetch = setup(state, (path) => {
      if (path.endsWith("/extract")) return response(job("queued"), 202);
      if (path === "/memory-candidate-jobs/job-a") return response(job());
      return undefined;
    });
    await flushPromises();
    await choose();
    await checkSource();
    await button("仅提取所选消息").trigger("click");
    await flushPromises();
    const write = writes(fetch)[0]!;
    expect(write[0]).toBe(
      "/api/admin/v1/conversations/a/memory-candidates/extract",
    );
    expect(JSON.parse(String(write[1]!.body))).toEqual({
      source_message_ids: ["source-a"],
    });
    expect(write[1]!.headers).toMatchObject({
      "If-Match": '"opaque.a.floor.12"',
      "X-CSRF-Token": "candidate-csrf",
    });
    expect(wrapper!.text()).toContain("模型候选尚未写入记忆");
    expect(button("仅提取所选消息").attributes("disabled")).toBeDefined();
    await vi.advanceTimersByTimeAsync(1999);
    expect(
      fetch.mock.calls.filter(([path]) =>
        path.includes("memory-candidate-jobs"),
      ),
    ).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    await flushPromises();
    expect(
      fetch.mock.calls.filter(([path]) =>
        path.includes("memory-candidate-jobs"),
      ),
    ).toHaveLength(1);
    expect(writes(fetch)).toHaveLength(1);
  });

  it("does not overlap job polls and loads completed candidates without accepting them", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.job = job();
    const pending = deferred<Response>();
    const fetch = setup(state, (path) =>
      path === "/memory-candidate-jobs/job-a" ? pending.promise : undefined,
    );
    await flushPromises();
    await choose();
    await vi.advanceTimersByTimeAsync(6000);
    expect(
      fetch.mock.calls.filter(([path]) =>
        path.includes("memory-candidate-jobs"),
      ),
    ).toHaveLength(1);
    state.a.job = job("completed", 5);
    state.a.items = [candidate()];
    pending.resolve(response(state.a.job));
    await flushPromises();
    expect(wrapper!.get('[aria-label="记忆候选 1"]').text()).toContain(
      "待审核，尚未记住",
    );
    expect(wrapper!.text()).toContain("候选独有正文");
    await vi.advanceTimersByTimeAsync(6000);
    expect(
      fetch.mock.calls.filter(([path]) =>
        path.includes("memory-candidate-jobs"),
      ),
    ).toHaveLength(1);
    expect(writes(fetch)).toEqual([]);
  });

  it("requires a concrete content/source confirmation, sends one acceptance, and emits one list refresh event", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.items = [candidate()];
    const accepted = deferred<Response>();
    const fetch = setup(state, (path) =>
      path.endsWith("/accept") ? accepted.promise : undefined,
    );
    await flushPromises();
    await choose();
    await button("审核并接受此候选").trigger("click");
    const confirm = wrapper!.get('[aria-label="确认接受记忆候选"]');
    expect(confirm.text()).toContain("候选独有正文");
    expect(confirm.text()).toContain("来自a的完成用户消息");
    expect(confirm.text()).toContain("不发布到共享范围，不覆盖已有事实");
    expect(writes(fetch)).toHaveLength(0);
    await button("确认正文与来源并保存私有记忆").trigger("click");
    await button("确认正文与来源并保存私有记忆").trigger("click");
    await flushPromises();
    expect(writes(fetch)).toHaveLength(1);
    expect(writes(fetch)[0]![1]!.headers).toMatchObject({ "If-Match": '"7"' });
    expect(JSON.parse(String(writes(fetch)[0]![1]!.body))).toEqual({});
    state.a.items[0]!.status = "accepted";
    accepted.resolve(response({ ...state.a.items[0], memory_id: "memory-a" }));
    await flushPromises();
    expect(wrapper!.emitted("accepted")).toHaveLength(1);
    expect(wrapper!.text()).not.toContain("候选独有正文");
    expect(wrapper!.text()).toContain("已接受为来源账号域私有记忆");
    expect(wrapper!.find('[aria-label="确认接受记忆候选"]').exists()).toBe(
      false,
    );
  });

  it("removes rejected candidate text even when the following refresh fails", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.items = [candidate()];
    let rejected = false;
    const fetch = setup(state, (path) => {
      if (path.endsWith("/reject")) {
        rejected = true;
        return response({ status: "rejected" });
      }
      if (rejected && path.endsWith("/memory-candidates"))
        return response(
          { error: { code: "temporary_unavailable", message: "暂不可用" } },
          503,
        );
      return undefined;
    });
    await flushPromises();
    await choose();
    await button("拒绝此候选").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("已拒绝");
    expect(wrapper!.text()).not.toContain("候选独有正文");
    expect(wrapper!.emitted("accepted")).toBeUndefined();
    expect(writes(fetch)[0]![1]!.headers).toMatchObject({ "If-Match": '"7"' });
  });

  it("preserves a conflicted selection and candidate, locks writes, and refreshes before another extraction", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.items = [candidate()];
    const fetch = setup(state, (path) =>
      path.endsWith("/extract")
        ? response(
            {
              error: {
                code: "memory_candidate_revision_conflict",
                message: "source changed",
              },
            },
            409,
          )
        : undefined,
    );
    await flushPromises();
    await choose();
    await checkSource();
    await button("仅提取所选消息").trigger("click");
    await flushPromises();
    expect(wrapper!.get('[role="alert"]').text()).toContain(
      "当前选择和展示已保留，请刷新候选",
    );
    expect(
      (
        wrapper!.get('input[aria-label="选择来源 1"]')
          .element as HTMLInputElement
      ).checked,
    ).toBe(true);
    expect(wrapper!.text()).toContain("候选独有正文");
    expect(button("审核并接受此候选").attributes("disabled")).toBeDefined();
    await vi.advanceTimersByTimeAsync(6000);
    expect(writes(fetch)).toHaveLength(1);
    state.a.source_revision = "new.epoch.floor.13";
    await button("刷新候选").trigger("click");
    await flushPromises();
    expect(button("仅提取所选消息").attributes("disabled")).toBeUndefined();
    await button("仅提取所选消息").trigger("click");
    await flushPromises();
    expect(writes(fetch)[1]![1]!.headers).toMatchObject({
      "If-Match": '"new.epoch.floor.13"',
    });
  });

  it("clears source and candidate bodies on 401 and stops all polling", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.job = job();
    state.a.items = [candidate()];
    const fetch = setup(state, (path) =>
      path.includes("memory-candidate-jobs")
        ? response({ error: "unauthorized" }, 401)
        : undefined,
    );
    await flushPromises();
    await choose();
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    expect(wrapper!.text()).toContain("登录已过期，来源正文和候选已清除");
    expect(wrapper!.text()).not.toContain("来自a的完成用户消息");
    expect(wrapper!.text()).not.toContain("候选独有正文");
    const calls = fetch.mock.calls.length;
    await vi.advanceTimersByTimeAsync(20000);
    expect(fetch).toHaveBeenCalledTimes(calls);
    expect(select().attributes("disabled")).toBeDefined();
  });

  it("aborts source changes and ignores a late response from the prior conversation", async () => {
    const pending = deferred<Response>();
    let signal: AbortSignal | undefined;
    setup(undefined, (path, options) => {
      if (path === "/conversations/a/memory-candidates") {
        signal = options?.signal ?? undefined;
        return pending.promise;
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await choose("b");
    expect(signal?.aborted).toBe(true);
    const stale = snapshot();
    stale.items = [candidate()];
    pending.resolve(response(stale));
    await flushPromises();
    expect(wrapper!.text()).toContain("来自b的完成用户消息");
    expect(wrapper!.text()).not.toContain("来自a的完成用户消息");
    expect(wrapper!.text()).not.toContain("候选独有正文");
  });

  it("clears expired candidate content and its confirmation without making new model requests", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.items = [{ ...candidate(), expires_at: expires(2) }];
    const fetch = setup(state);
    await flushPromises();
    await choose();
    await button("审核并接受此候选").trigger("click");
    await vi.advanceTimersByTimeAsync(2001);
    expect(wrapper!.text()).toContain("已到期");
    expect(wrapper!.text()).not.toContain("候选独有正文");
    expect(wrapper!.find('[aria-label="确认接受记忆候选"]').exists()).toBe(
      false,
    );
    expect(writes(fetch)).toEqual([]);
  });

  it("cancels only the concrete active job with its revision and never automatically retries", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.job = job("queued", "opaque-job-v4");
    const fetch = setup(state, (path, options) => {
      if (
        path === "/memory-candidate-jobs/job-a" &&
        options?.method === "DELETE"
      ) {
        state.a.job = job("cancelled", "opaque-job-v5");
        return response(state.a.job);
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await button("取消提取任务").trigger("click");
    expect(wrapper!.get('[aria-label="确认取消提取"]').text()).toContain(
      "来源会话甲",
    );
    await button("确认取消本次提取").trigger("click");
    await flushPromises();
    expect(writes(fetch)[0]![1]).toMatchObject({
      method: "DELETE",
      headers: expect.objectContaining({ "If-Match": '"opaque-job-v4"' }),
    });
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toHaveLength(1);
    expect(wrapper!.text()).toContain("提取已取消");
  });

  it("bounds polling and aborts an in-flight poll when the component leaves", async () => {
    const state = { a: snapshot(), b: snapshot("b") };
    state.a.job = { ...job(), expires_at: expires(600) };
    const fetch = setup(state, (path) =>
      path.includes("memory-candidate-jobs")
        ? response(state.a.job)
        : undefined,
    );
    await flushPromises();
    await choose();
    for (let count = 0; count < 62; count++) {
      await vi.advanceTimersByTimeAsync(2000);
      await flushPromises();
    }
    expect(
      fetch.mock.calls.filter(([path]) =>
        path.includes("memory-candidate-jobs"),
      ),
    ).toHaveLength(60);
    expect(wrapper!.text()).toContain("轮询已暂停");
    wrapper!.unmount();
    const delayed = deferred<Response>();
    let signal: AbortSignal | undefined;
    setup(state, (path, options) => {
      if (path.includes("memory-candidate-jobs")) {
        signal = options?.signal ?? undefined;
        return delayed.promise;
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await vi.advanceTimersByTimeAsync(2000);
    wrapper!.unmount();
    expect(signal?.aborted).toBe(true);
    delayed.resolve(response(job("completed")));
    await flushPromises();
  });
});
