// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import ImageAnalysis from "../src/components/ImageAnalysis.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
let decodeSize = { width: 16, height: 12 };
let decodeFails = false;
let decodeAutomatically = true;
let imageInstances: FakeImage[] = [];
let urlCount = 0;
class FakeImage {
  naturalWidth = decodeSize.width;
  naturalHeight = decodeSize.height;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  decoding = "";
  constructor() {
    imageInstances.push(this);
  }
  set src(_value: string) {
    if (decodeAutomatically)
      queueMicrotask(() => (decodeFails ? this.onerror?.() : this.onload?.()));
  }
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-24T12:00:00Z"));
  decodeSize = { width: 16, height: 12 };
  decodeFails = false;
  decodeAutomatically = true;
  imageInstances = [];
  urlCount = 0;
  vi.stubGlobal("Image", FakeImage);
  vi.spyOn(URL, "createObjectURL").mockImplementation(
    () => `blob:synthetic-${++urlCount}`,
  );
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
  vi.spyOn(crypto, "randomUUID").mockReturnValue(
    "c07b1bba-dbbc-4c8e-911c-e8db4639ede9",
  );
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
function job(status = "queued", revision = 1) {
  return {
    id: "image-job-a",
    status,
    revision,
    created_at: expires(-1),
    expires_at: expires(),
    model: "deepseek-flash",
    question: "只属于这张图片的问题",
    result: undefined as string | undefined,
    width: 16,
    height: 12,
    image_sha256: "synthetic-image-hash",
  };
}
function service() {
  return {
    available: true,
    provider: "DeepSeek",
    model: "deepseek-flash",
    credential_configured: true,
    items: [] as ReturnType<typeof job>[],
    policy: {
      max_image_bytes: 1048576,
      max_width: 2048,
      max_height: 2048,
      max_pixels: 2097152,
      max_question_bytes: 2000,
      result_ttl_seconds: 86400,
    },
  };
}
function png(width = 16, height = 12, size = 33, type = "image/png") {
  // The component's Image decoder is controlled separately. This helper makes
  // header fixtures, not full CRC-valid PNGs or claims about provider decoding.
  const values = new Uint8Array(size);
  if (size >= 33) {
    values.set([137, 80, 78, 71, 13, 10, 26, 10]);
    const data = new DataView(values.buffer);
    data.setUint32(8, 13);
    data.setUint32(12, 0x49484452);
    data.setUint32(16, width);
    data.setUint32(20, height);
  }
  return new File([values], "synthetic-local.png", { type });
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function setup(
  state = service(),
  handler?: (
    path: string,
    options?: RequestInit,
  ) => Response | Promise<Response> | undefined,
) {
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1", "");
    const custom = await handler?.(path, options);
    if (custom) return custom;
    if (path === "/image-analysis") return response(state);
    if (path === "/image-analysis/jobs" && options?.method === "POST") {
      state.items = [job()];
      return response(state.items[0], 202);
    }
    if (path === "/image-analysis/jobs/image-job-a")
      return response(state.items[0]);
    throw new Error(`Unexpected image request: ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("image-csrf");
  wrapper = mount(ImageAnalysis);
  return fetch;
}
function button(label: string) {
  const found = wrapper!
    .findAll("button")
    .find((item) => item.text() === label);
  if (!found) throw new Error(`Missing button ${label}`);
  return found;
}
async function choose(chosen = png()) {
  const input = wrapper!.get('input[aria-label="选择本地 PNG 图片"]');
  Object.defineProperty(input.element, "files", {
    configurable: true,
    value: [chosen],
  });
  await input.trigger("change");
  await flushPromises();
}
async function submit() {
  await wrapper!.get("form").trigger("submit");
  await flushPromises();
}
function writes(fetch: ReturnType<typeof setup>) {
  return fetch.mock.calls.filter(
    ([, options]) => options?.method && options.method !== "GET",
  );
}

describe("independent explicit image analysis", () => {
  it("states the destination and retention boundaries and never sends a local preview automatically", async () => {
    const store = vi.spyOn(Storage.prototype, "setItem");
    const state = service();
    state.available = false;
    state.credential_configured = false;
    const fetch = setup(state);
    await flushPromises();
    await choose();
    expect(wrapper!.text()).toContain("发送至 DeepSeek");
    expect(wrapper!.text()).toContain("结果最多保留 24 小时");
    expect(wrapper!.text()).toContain("原数据库/WAL及离线备份采用独立保留策略");
    expect(wrapper!.text()).toContain("不自动接入普通聊天或记忆");
    expect(
      wrapper!.get('img[alt="待发送 PNG 的本地预览"]').attributes("src"),
    ).toBe("blob:synthetic-1");
    expect(
      button("发送至 DeepSeek 并分析").attributes("disabled"),
    ).toBeDefined();
    const help = wrapper!
      .get('input[type="file"]')
      .attributes("aria-describedby");
    expect(wrapper!.get(`[id="${help}"]`).text()).toContain("本地预览不会上传");
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toEqual([]);
    expect(store).not.toHaveBeenCalled();
  });

  it("rejects wrong MIME, empty/oversized data, bad signatures, oversized dimensions and excessive pixels before upload", async () => {
    const fetch = setup();
    await flushPromises();
    for (const [chosen, message] of [
      [png(16, 12, 33, "image/jpeg"), "只接受一张 PNG"],
      [png(16, 12, 0), "不能为空"],
      [png(16, 12, 1048577), "不能超过 1 MiB"],
      [
        new File([new Uint8Array(33)], "not-png.png", { type: "image/png" }),
        "有效的 PNG",
      ],
      [png(2049, 1), "宽高均需"],
      [png(0, 12), "宽高均需"],
      [png(2048, 2048), "总像素不能超过"],
    ] as const) {
      await choose(chosen);
      expect(wrapper!.get('[role="alert"]').text()).toContain(message);
      expect(wrapper!.find("img").exists()).toBe(false);
      expect(
        button("发送至 DeepSeek 并分析").attributes("disabled"),
      ).toBeDefined();
    }
    expect(URL.createObjectURL).not.toHaveBeenCalled();
    expect(writes(fetch)).toEqual([]);
  });

  it("requires successful local decode and rejects mismatched decoded dimensions", async () => {
    const fetch = setup();
    await flushPromises();
    decodeFails = true;
    await choose();
    expect(wrapper!.get('[role="alert"]').text()).toContain("无法在本地解码");
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-1");
    decodeFails = false;
    decodeSize = { width: 17, height: 12 };
    await choose();
    expect(wrapper!.get('[role="alert"]').text()).toContain(
      "解码尺寸与文件头不一致",
    );
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-2");
    expect(writes(fetch)).toEqual([]);
  });

  it("releases replaced, removed and unmounted previews and ignores an older decode", async () => {
    setup();
    await flushPromises();
    decodeAutomatically = false;
    await choose();
    const older = imageInstances[0]!;
    await choose();
    expect(older.onload).toBeNull();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-1");
    imageInstances[1]!.onload?.();
    await flushPromises();
    expect(wrapper!.get("img").attributes("src")).toBe("blob:synthetic-2");
    await button("移除本地图片").trigger("click");
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-2");
    decodeAutomatically = true;
    await choose();
    wrapper!.unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-3");
  });

  it("enforces UTF-8 question size and permits an exact pixel-limit PNG only after decode", async () => {
    setup();
    await flushPromises();
    decodeSize = { width: 2048, height: 1024 };
    await choose(png(2048, 1024));
    expect(wrapper!.text()).toContain("2048 × 1024");
    await wrapper!
      .get('textarea[aria-label="要分析的问题"]')
      .setValue("界".repeat(667));
    expect(wrapper!.get('[role="alert"]').text()).toContain(
      "不能超过 2,000 UTF-8 字节",
    );
    expect(
      button("发送至 DeepSeek 并分析").attributes("disabled"),
    ).toBeDefined();
    await wrapper!.get("textarea").setValue("分析颜色");
    expect(
      button("发送至 DeepSeek 并分析").attributes("disabled"),
    ).toBeUndefined();
  });

  it("sends one explicit UUID/base64 request, excludes data prefixes, and removes the local image after acceptance", async () => {
    const pending = deferred<Response>();
    const fetch = setup(undefined, (path) =>
      path === "/image-analysis/jobs" ? pending.promise : undefined,
    );
    await flushPromises();
    await choose();
    await submit();
    await submit();
    expect(writes(fetch)).toHaveLength(1);
    const sent = JSON.parse(String(writes(fetch)[0]![1]!.body));
    expect(sent.client_request_id).toBe("c07b1bba-dbbc-4c8e-911c-e8db4639ede9");
    expect(Object.keys(sent).sort()).toEqual([
      "client_request_id",
      "image_base64",
      "question",
    ]);
    expect(sent.image_base64).not.toContain("data:");
    expect(
      [...atob(sent.image_base64)]
        .slice(0, 8)
        .map((character) => character.charCodeAt(0)),
    ).toEqual([137, 80, 78, 71, 13, 10, 26, 10]);
    expect(writes(fetch)[0]![1]!.headers).toMatchObject({
      "X-CSRF-Token": "image-csrf",
    });
    expect(wrapper!.text()).not.toContain(sent.image_base64);
    pending.resolve(response(job(), 202));
    await flushPromises();
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-1");
    expect(
      (wrapper!.get("textarea").element as HTMLTextAreaElement).value,
    ).toBe("");
    expect(wrapper!.text()).toContain("结果不会自动进入普通聊天或记忆");
  });

  it("retains the same idempotency ID after an uncertain submission and never retries without an explicit refreshed action", async () => {
    let first = true;
    const fetch = setup(undefined, (path) => {
      if (path === "/image-analysis/jobs" && first) {
        first = false;
        throw new Error("synthetic network failure");
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await submit();
    expect(wrapper!.get('[role="alert"]').text()).toContain("不会自动重发图片");
    expect(
      button("发送至 DeepSeek 并分析").attributes("disabled"),
    ).toBeDefined();
    expect(wrapper!.find("img").exists()).toBe(true);
    expect(wrapper!.get("textarea").attributes("disabled")).toBeDefined();
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toHaveLength(1);
    await button("刷新图片任务").trigger("click");
    await flushPromises();
    expect(wrapper!.get("textarea").attributes("disabled")).toBeUndefined();
    await submit();
    expect(writes(fetch)).toHaveLength(2);
    expect(
      writes(fetch).map(
        ([, options]) => JSON.parse(String(options!.body)).client_request_id,
      ),
    ).toEqual([
      "c07b1bba-dbbc-4c8e-911c-e8db4639ede9",
      "c07b1bba-dbbc-4c8e-911c-e8db4639ede9",
    ]);
    expect(crypto.randomUUID).toHaveBeenCalledTimes(1);
  });

  it("starts a new UUID only on explicit submission of a changed question after refreshing an unknown attempt", async () => {
    vi.mocked(crypto.randomUUID)
      .mockReturnValueOnce("c07b1bba-dbbc-4c8e-911c-e8db4639ede9")
      .mockReturnValueOnce("4985cf8e-6129-45a1-b29b-8587c1711d9a");
    let first = true;
    const fetch = setup(undefined, (path) => {
      if (path === "/image-analysis/jobs" && first) {
        first = false;
        throw new Error("synthetic network failure");
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await wrapper!.get("textarea").setValue("原始问题，重试不得改变正文");
    await submit();
    expect(wrapper!.get("textarea").attributes("disabled")).toBeDefined();
    expect(wrapper!.text()).toContain("本次未确认请求的问题已暂时锁定");
    await button("刷新图片任务").trigger("click");
    await flushPromises();
    await wrapper!.get("textarea").setValue("用户明确修改后的新问题");
    expect(writes(fetch)).toHaveLength(1);
    expect(crypto.randomUUID).toHaveBeenCalledTimes(1);
    await submit();
    const bodies = writes(fetch).map(([, options]) =>
      JSON.parse(String(options!.body)),
    );
    expect(bodies[0]).toMatchObject({
      client_request_id: "c07b1bba-dbbc-4c8e-911c-e8db4639ede9",
      question: "原始问题，重试不得改变正文",
    });
    expect(bodies[1]).toMatchObject({
      client_request_id: "4985cf8e-6129-45a1-b29b-8587c1711d9a",
      question: "用户明确修改后的新问题",
    });
    expect(bodies[1].image_base64).toBe(bodies[0].image_base64);
  });

  it("serializes polls and renders completed output as text without HTML or a memory mutation", async () => {
    const state = service();
    state.items = [job("running")];
    const pending = deferred<Response>();
    const fetch = setup(state, (path) =>
      path.endsWith("/image-job-a") ? pending.promise : undefined,
    );
    await flushPromises();
    await vi.advanceTimersByTimeAsync(6000);
    expect(
      fetch.mock.calls.filter(([path]) => path.endsWith("/image-job-a")),
    ).toHaveLength(1);
    pending.resolve(
      response({
        ...job("completed", 2),
        result: '<img src="https://example.invalid/never"> 合成图片分析结果',
      }),
    );
    await flushPromises();
    expect(wrapper!.get('[aria-label="图片分析结果"]').text()).toContain(
      '<img src="https://example.invalid/never">',
    );
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(wrapper!.find("a").exists()).toBe(false);
    await vi.advanceTimersByTimeAsync(6000);
    expect(
      fetch.mock.calls.filter(([path]) => path.endsWith("/image-job-a")),
    ).toHaveLength(1);
    expect(writes(fetch)).toEqual([]);
  });

  it("requires concrete deletion confirmation and keeps results cleared when the refresh fails", async () => {
    const state = service();
    state.items = [{ ...job("completed", 9), result: "待清除的合成结果" }];
    let removed = false;
    const fetch = setup(state, (path, options) => {
      if (options?.method === "DELETE") {
        removed = true;
        return response({ status: "deleted" });
      }
      if (removed && path === "/image-analysis")
        return response(
          { error: { code: "unavailable", message: "暂不可用" } },
          503,
        );
      return undefined;
    });
    await flushPromises();
    await button("清除此任务").trigger("click");
    expect(wrapper!.get('[aria-label="确认清除图片任务"]').text()).toContain(
      "只属于这张图片的问题",
    );
    expect(wrapper!.get('[aria-label="确认清除图片任务"]').text()).toContain(
      "已经发出的请求可能已被 DeepSeek 接收",
    );
    expect(writes(fetch)).toEqual([]);
    await button("确认取消或清除本任务").trigger("click");
    await flushPromises();
    expect(writes(fetch)[0]![1]).toMatchObject({
      method: "DELETE",
      headers: expect.objectContaining({ "If-Match": '"9"' }),
    });
    expect(wrapper!.text()).not.toContain("待清除的合成结果");
    expect(wrapper!.text()).not.toContain("只属于这张图片的问题");
    expect(wrapper!.text()).toContain("本任务已清除");
  });

  it("retains a conflicted result and requires refresh before another clear action", async () => {
    const state = service();
    state.items = [{ ...job("completed", 2), result: "并发保护的合成结果" }];
    const fetch = setup(state, (_path, options) =>
      options?.method === "DELETE"
        ? response(
            {
              error: { code: "image_revision_conflict", message: "任务已更新" },
            },
            409,
          )
        : undefined,
    );
    await flushPromises();
    await button("清除此任务").trigger("click");
    await button("确认取消或清除本任务").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("当前展示已保留，请刷新图片任务");
    expect(wrapper!.text()).toContain("并发保护的合成结果");
    expect(button("清除此任务").attributes("disabled")).toBeDefined();
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toHaveLength(1);
    state.items[0]!.revision = 3;
    await button("刷新图片任务").trigger("click");
    await flushPromises();
    await button("清除此任务").trigger("click");
    await button("确认取消或清除本任务").trigger("click");
    await flushPromises();
    expect(writes(fetch)[1]![1]!.headers).toMatchObject({ "If-Match": '"3"' });
  });

  it("scrubs result/question bodies at expiry and suppresses bodies returned for already terminal jobs", async () => {
    const state = service();
    state.items = [
      {
        ...job("completed"),
        expires_at: expires(2),
        result: "即将到期的合成结果",
      },
      {
        ...job("deleted"),
        id: "deleted-job",
        result: "不得复现的旧结果",
        question: "不得复现的旧问题",
      },
    ];
    const fetch = setup(state);
    await flushPromises();
    expect(wrapper!.text()).toContain("即将到期的合成结果");
    expect(wrapper!.text()).not.toContain("不得复现");
    await vi.advanceTimersByTimeAsync(2001);
    expect(wrapper!.text()).not.toContain("即将到期的合成结果");
    expect(wrapper!.text()).not.toContain("只属于这张图片的问题");
    expect(wrapper!.text()).toContain("已到期");
    expect(writes(fetch)).toEqual([]);
  });

  it("clears local previews, question drafts and results on 401, then stops polling", async () => {
    const state = service();
    state.items = [
      job("running"),
      {
        ...job("completed"),
        id: "completed-job",
        result: "会话失效必须清除的结果",
      },
    ];
    const fetch = setup(state, (path) =>
      path.endsWith("/image-job-a")
        ? response({ error: "unauthorized" }, 401)
        : undefined,
    );
    await flushPromises();
    await choose();
    await wrapper!.get("textarea").setValue("会话失效必须清除的问题");
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    expect(wrapper!.text()).toContain(
      "登录已过期，本地图片预览、问题和分析结果已清除",
    );
    expect(wrapper!.text()).not.toContain("会话失效必须清除的结果");
    expect(
      (wrapper!.get("textarea").element as HTMLTextAreaElement).value,
    ).toBe("");
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-1");
    const count = fetch.mock.calls.length;
    await vi.advanceTimersByTimeAsync(20000);
    expect(fetch).toHaveBeenCalledTimes(count);
  });

  it("bounds polling and aborts a pending image request when leaving the component", async () => {
    const state = service();
    state.items = [{ ...job("running"), expires_at: expires(600) }];
    const fetch = setup(state);
    await flushPromises();
    for (let index = 0; index < 62; index++) {
      await vi.advanceTimersByTimeAsync(2000);
      await flushPromises();
    }
    expect(
      fetch.mock.calls.filter(([path]) => path.endsWith("/image-job-a")),
    ).toHaveLength(60);
    expect(wrapper!.text()).toContain("轮询已暂停");
    wrapper!.unmount();
    const pending = deferred<Response>();
    let signal: AbortSignal | undefined;
    const second = setup(undefined, (path, options) => {
      if (path === "/image-analysis/jobs") {
        signal = options?.signal ?? undefined;
        return pending.promise;
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await submit();
    wrapper!.unmount();
    expect(signal?.aborted).toBe(true);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:synthetic-1");
    pending.resolve(response(job(), 202));
    await flushPromises();
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(second)).toHaveLength(1);
    expect(
      second.mock.calls.filter(([path]) => path.includes("/image-job-a")),
    ).toHaveLength(0);
  });
});
