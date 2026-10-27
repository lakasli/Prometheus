// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import ConversationComposer from "../src/components/ConversationComposer.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
let decodeFails = false;
let decodeWidth = 20;
let decodeHeight = 12;
let urlCount = 0;
beforeEach(() => {
  vi.useFakeTimers();
  decodeFails = false;
  decodeWidth = 20;
  decodeHeight = 12;
  urlCount = 0;
  vi.spyOn(URL, "createObjectURL").mockImplementation(
    () => `blob:conversation-${++urlCount}`,
  );
  vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
  vi.spyOn(crypto, "randomUUID").mockReturnValue(
    "ebc61bf2-b07d-409e-a29d-13e70657c696",
  );
  vi.stubGlobal(
    "Image",
    class {
      naturalWidth = decodeWidth;
      naturalHeight = decodeHeight;
      onload: (() => void) | null = null;
      onerror: (() => void) | null = null;
      decoding = "";
      set src(_value: string) {
        queueMicrotask(() =>
          decodeFails ? this.onerror?.() : this.onload?.(),
        );
      }
    },
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
function caps(available = true) {
  return {
    available,
    provider: "deepseek",
    model: "deepseek-flash",
    basis: "vision_description",
    max_image_bytes: 8*1048576,
    max_width: 12000,
    max_height: 12000,
    max_pixels: 24000000,
    formats: ["png","jpeg","webp","gif","bmp"],
    max_images: 1,
  };
}
function png(width = 20, height = 12, size = 33, type = "image/png") {
  // Header fixtures plus a separately controlled Image decoder; real PNG
  // bytes and native browser decoding are covered by the browser test.
  const bytes = new Uint8Array(size);
  if (size >= 33) {
    bytes.set([137, 80, 78, 71, 13, 10, 26, 10]);
    const data = new DataView(bytes.buffer);
    data.setUint32(8, 13);
    data.setUint32(12, 0x49484452);
    data.setUint32(16, width);
    data.setUint32(20, height);
  }
  return new File([bytes], "synthetic-conversation.png", { type });
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function setup(
  handler?: (
    path: string,
    options?: RequestInit,
  ) => Response | Promise<Response> | undefined,
  overrides: Record<string, unknown> = {},
) {
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1", "");
    const custom = await handler?.(path, options);
    if (custom) return custom;
    if (path === "/conversation-images/capabilities") return response(caps());
    if (
      path === "/conversations/a/messages" ||
      path === "/conversations/a/image-messages"
    )
      return response({ run_id: "run-a", conversation_id: "a" }, 202);
    throw new Error(`Unexpected composer request ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("conversation-csrf");
  wrapper = mount(ConversationComposer, {
    props: {
      conversationId: "a",
      isTest: false,
      modelConfigured: true,
      running: false,
      blocked: false,
      prepareConversation: async () => "a",
      ...overrides,
    },
  });
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
  const input = wrapper!.get('input[type="file"]');
  Object.defineProperty(input.element, "files", {
    configurable: true,
    value: [chosen],
  });
  await input.trigger("change");
  await flushPromises();
}
async function write(value = "请结合我的问题看这张图片") {
  await wrapper!.get('textarea[aria-label="对话消息"]').setValue(value);
}
async function submit() {
  await wrapper!.get("form").trigger("submit");
  await flushPromises();
}
function writes(fetch: ReturnType<typeof setup>) {
  return fetch.mock.calls.filter(([, options]) => options?.method === "POST");
}

describe("normal conversation PNG composer", () => {
  it("previews locally, explains Flash description grounding and never uploads until explicit send", async () => {
    const store = vi.spyOn(Storage.prototype, "setItem");
    const fetch = setup();
    await flushPromises();
    await choose();
    await write();
    expect(wrapper!.get("img").attributes("src")).toBe("blob:conversation-1");
    expect(wrapper!.text()).toContain("再结合当前对话回答");
    expect(wrapper!.text()).toContain("独立的视觉偏好尚未接入");
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toEqual([]);
    expect(store).not.toHaveBeenCalled();
  });
  it("uses the conversation image message endpoint, existing history/run identity and no old independent analysis endpoint", async () => {
    const fetch = setup();
    await flushPromises();
    await choose();
    await write();
    await submit();
    const request = writes(fetch)[0]!;
    expect(request[0]).toBe("/api/admin/v1/conversations/a/image-messages");
    const body = JSON.parse(String(request[1]!.body));
    expect(Object.keys(body).sort()).toEqual([
      "client_message_id",
      "content",
      "image_base64",
    ]);
    expect(body).toMatchObject({
      content: "请结合我的问题看这张图片",
      client_message_id: "ebc61bf2-b07d-409e-a29d-13e70657c696",
    });
    expect(body.image_base64).not.toContain("data:");
    expect(
      [...atob(body.image_base64)]
        .slice(0, 8)
        .map((value) => value.charCodeAt(0)),
    ).toEqual([137, 80, 78, 71, 13, 10, 26, 10]);
    expect(request[1]!.headers).toMatchObject({
      "X-CSRF-Token": "conversation-csrf",
    });
    expect(wrapper!.emitted("accepted")).toEqual([
      [{ run_id: "run-a", conversation_id: "a" }],
    ]);
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:conversation-1");
    expect(wrapper!.text()).not.toContain(body.image_base64);
    expect(
      fetch.mock.calls.some(([path]) => path.includes("/image-analysis")),
    ).toBe(false);
  });
  it("preserves ordinary text and model-free explicit memory commands, and rejects all slash commands when an image is present", async () => {
    const fetch = setup(undefined, { modelConfigured: false });
    await flushPromises();
    await write("/memory list");
    expect(button("发送").attributes("disabled")).toBeUndefined();
    await submit();
    expect(writes(fetch)[0]![0]).toBe("/api/admin/v1/conversations/a/messages");
    expect(JSON.parse(String(writes(fetch)[0]![1]!.body))).not.toHaveProperty(
      "image_base64",
    );
    await wrapper!.setProps({ modelConfigured: true });
    await choose();
    for (const command of [
      "/memory save x",
      " /persona abc",
      "\n/角色 切换",
      "/anything",
    ]) {
      await write(command);
      expect(wrapper!.text()).toContain("不能发送任何 / 开头的控制命令");
      expect(button("发送").attributes("disabled")).toBeDefined();
    }
    expect(writes(fetch)).toHaveLength(1);
  });
  it("keeps draft image uploads closed while ordinary text remains available", async () => {
    const fetch = setup(undefined, { isTest: true });
    await flushPromises();
    expect(
      wrapper!.get('input[type="file"]').attributes("disabled"),
    ).toBeDefined();
    expect(wrapper!.text()).toContain("草稿测试会话暂不支持图片");
    await write("继续普通草稿文本测试");
    await submit();
    expect(writes(fetch)[0]![0]).toBe("/api/admin/v1/conversations/a/messages");
  });
  it("handles unavailable image capabilities without disabling text or inventing support", async () => {
    const fetch = setup((path) =>
      path.endsWith("/capabilities") ? response(caps(false)) : undefined,
    );
    await flushPromises();
    expect(
      wrapper!.get('input[type="file"]').attributes("disabled"),
    ).toBeDefined();
    expect(wrapper!.text()).toContain("当前图片理解不可用");
    await write("文字仍可发送");
    await submit();
    expect(writes(fetch)).toHaveLength(1);
  });
  it("checks actual PNG bytes, decode, dimensions, pixels, byte size and UTF-8 message size before sending", async () => {
    const fetch = setup();
    await flushPromises();
    for (const [chosen, text] of [
      [new File(["<svg/>"],"image.svg",{type:"image/svg+xml"}),"请选择一张"],
      [png(20, 12, 8*1048576+1), "不能超过 8 MiB"],
      [png(12001, 12), "尺寸太大"],
      [png(6000, 6000), "尺寸太大"],
      [
        new File([new Uint8Array(33)], "not-png.png", { type: "image/png" }),
        "图片内容没有识别出来",
      ],
    ] as const) {
      await choose(chosen);
      expect(wrapper!.get('[role="alert"]').text()).toContain(text);
      expect(wrapper!.find("img").exists()).toBe(false);
    }
    decodeFails = true;
    await choose();
    expect(wrapper!.get('[role="alert"]').text()).toContain("没有完整打开");
    decodeFails = false;
    decodeWidth = 21;
    await choose();
    expect(wrapper!.text()).toContain("图片尺寸与文件内容不一致");
    await write("界".repeat(5334));
    expect(button("发送").attributes("disabled")).toBeDefined();
    expect(writes(fetch)).toEqual([]);
  });
  it("freezes the exact unknown image request and UUID, only retries manually and emits history inspection without a POST", async () => {
    let first = true;
    const fetch = setup((path) => {
      if (path.endsWith("/image-messages") && first) {
        first = false;
        throw new Error("synthetic disconnect");
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await write();
    await submit();
    expect(wrapper!.get("textarea").attributes("disabled")).toBeDefined();
    expect(wrapper!.text()).toContain("提交结果未确认，仅供核对");
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toHaveLength(1);
    await button("查看会话历史").trigger("click");
    expect(wrapper!.emitted("inspectHistory")).toHaveLength(1);
    expect(writes(fetch)).toHaveLength(1);
    await button("原样重试此消息").trigger("click");
    await flushPromises();
    expect(writes(fetch)).toHaveLength(2);
    expect(JSON.parse(String(writes(fetch)[1]![1]!.body))).toEqual(
      JSON.parse(String(writes(fetch)[0]![1]!.body)),
    );
    expect(crypto.randomUUID).toHaveBeenCalledTimes(1);
    expect(wrapper!.emitted("accepted")).toHaveLength(1);
  });
  it("applies the same uncertain idempotency protection to existing ordinary text sends", async () => {
    let first = true;
    const fetch = setup((path) => {
      if (path.endsWith("/messages") && first) {
        first = false;
        throw new Error("synthetic disconnect");
      }
      return undefined;
    });
    await flushPromises();
    await write("仅此一条普通消息");
    await submit();
    await button("原样重试此消息").trigger("click");
    await flushPromises();
    expect(writes(fetch)).toHaveLength(2);
    expect(writes(fetch)[1]![1]!.body).toBe(writes(fetch)[0]![1]!.body);
    expect(wrapper!.emitted("accepted")).toHaveLength(1);
  });
  it("unlocks definite 403 validation errors without emitting a fabricated run", async () => {
    const fetch = setup((path) =>
      path.endsWith("/image-messages")
        ? response(
            {
              error: {
                code: "conversation_image_draft_test",
                message: "草稿不允许图片",
              },
            },
            403,
          )
        : undefined,
    );
    await flushPromises();
    await choose();
    await write();
    await submit();
    expect(wrapper!.text()).toContain("草稿不允许图片");
    expect(wrapper!.get("textarea").attributes("disabled")).toBeUndefined();
    expect(wrapper!.emitted("accepted")).toBeUndefined();
    expect(writes(fetch)).toHaveLength(1);
  });
  it("clears image/body on 401, leaves no persistent storage and requests parent reauthentication", async () => {
    setup((path) =>
      path.endsWith("/image-messages")
        ? response({ error: "unauthorized" }, 401)
        : undefined,
    );
    await flushPromises();
    await choose();
    await write();
    await submit();
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(
      (wrapper!.get("textarea").element as HTMLTextAreaElement).value,
    ).toBe("");
    expect(wrapper!.emitted("unauthorized")).toHaveLength(1);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:conversation-1");
  });
  it("revokes previews on replacement, removal and unmount and ignores a late send after a source switch", async () => {
    const pending = deferred<Response>();
    let signal: AbortSignal | undefined;
    const fetch = setup((path, options) => {
      if (path.endsWith("/image-messages")) {
        signal = options?.signal ?? undefined;
        return pending.promise;
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await choose();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:conversation-1");
    await button("移除对话图片").trigger("click");
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:conversation-2");
    await choose();
    await write();
    await submit();
    await wrapper!.setProps({ conversationId: "b" });
    expect(signal?.aborted).toBe(true);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:conversation-3");
    pending.resolve(
      response({ run_id: "stale-run", conversation_id: "a" }, 202),
    );
    await flushPromises();
    expect(wrapper!.emitted("accepted")).toBeUndefined();
    expect(writes(fetch)).toHaveLength(1);
    await choose();
    wrapper!.unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:conversation-4");
  });
  it("keeps the image while preparing a brand-new conversation instead of treating its assigned ID as a user source switch", async () => {
    const prepare = vi.fn(async () => {
      await wrapper!.setProps({ conversationId: "a" });
      return "a";
    });
    const fetch = setup(undefined, {
      conversationId: null,
      prepareConversation: prepare,
    });
    await flushPromises();
    await choose();
    await write();
    await submit();
    expect(prepare).toHaveBeenCalledTimes(1);
    expect(writes(fetch)[0]![0]).toBe(
      "/api/admin/v1/conversations/a/image-messages",
    );
    expect(wrapper!.emitted("accepted")).toHaveLength(1);
  });
  it("requires a specific abandonment confirmation and never turns abandonment into another message", async () => {
    const fetch = setup((path) => {
      if (path.endsWith("/image-messages"))
        throw new Error("synthetic unknown");
      return undefined;
    });
    await flushPromises();
    await choose();
    await write();
    await submit();
    await button("放弃本地草稿").trigger("click");
    expect(wrapper!.find("img").exists()).toBe(true);
    expect(wrapper!.get('[aria-label="确认放弃未确认消息"]').text()).toContain(
      "不会重发或取消可能已创建的任务",
    );
    await button("确认放弃本地草稿").trigger("click");
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(writes(fetch)).toHaveLength(1);
  });
});
