// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import App from "../src/App.vue";

let wrapper: VueWrapper | undefined;
beforeEach(() => {
  location.hash = "";
  vi.useFakeTimers();
  vi.spyOn(crypto, "randomUUID").mockReturnValue(
    "ebc61bf2-b07d-409e-a29d-13e70657c696",
  );
});
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  vi.clearAllTimers();
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
function setup(
  hash = "",
  handler?: (
    path: string,
    options?: RequestInit,
  ) => Response | Promise<Response> | undefined,
) {
  location.hash = hash;
  const server = {
    messages: [
      {
        id: "message-user",
        role: "user",
        content: "请看这张图片",
        created_at: 1792830000,
        image: {
          width: 20,
          height: 12,
          sha256: "a".repeat(64),
          status: "queued",
          original_retained: false,
        },
      },
    ] as unknown[],
    runs: 0,
  };
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1", "");
    const custom = await handler?.(path, options);
    if (custom) return custom;
    if (path === "/auth/status") return response({ initialized: true });
    if (path === "/auth/me")
      return response({
        user: { username: "owner", role: "admin" },
        csrf_token: "workspace-csrf",
      });
    if (path === "/status")
      return response({
        version: "test",
        queue: { queued: 0, running: 0, failed: 0 },
        platform: { os: "linux", arch: "aarch64" },
        model: { configured: true, model: "current-text-assistant" },
      });
    if (path === "/config")
      return response({
        revision: 1,
        applied_revision: 1,
        config: {
          assistant: {
            name: "当前助手",
            system_prompt: "",
            max_concurrent_runs: 2,
            timeout_seconds: 120,
          },
          model: {
            base_url: "https://api.deepseek.com",
            model: "current-text-assistant",
            credential_ref: "configured",
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
    if (path === "/conversations")
      return response({
        items: [
          {
            id: "a",
            title: "原有普通会话",
            created_at: 1792830000,
            character: { name: "发布角色", revision: 1, is_test: false },
          },
        ],
      });
    if (path === "/character-cards")
      return response({ items: [], default: null, default_etag: "0" });
    if (path === "/conversation-images/capabilities")
      return response({
        available: true,
        provider: "deepseek",
        model: "deepseek-flash",
        basis: "vision_description",
        max_image_bytes: 1048576,
        max_width: 2048,
        max_height: 2048,
        max_pixels: 2097152,
        formats: ["png"],
        max_images: 1,
      });
    if (path === "/conversations/a/messages" && options?.method !== "POST")
      return response({ items: server.messages });
    if (
      path === "/conversations/a/image-messages" ||
      path === "/conversations/a/messages"
    )
      return response({ run_id: "run-a", conversation_id: "a" }, 202);
    if (path === "/runs/run-a") {
      server.runs++;
      server.messages = [
        {
          id: "message-user",
          role: "user",
          content: "请看这张图片",
          created_at: 1792830000,
          image: {
            width: 20,
            height: 12,
            sha256: "a".repeat(64),
            status: "described",
            original_retained: false,
          },
        },
        {
          id: "message-assistant",
          role: "assistant",
          content: "基于视觉描述作出的当前助手回答",
          basis: "vision_description",
          created_at: 1792830001,
        },
      ];
      return response({ id: "run-a", status: "completed" });
    }
    throw new Error(`Unexpected workspace request ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(App, {
    global: {
      stubs: {
        ImageAnalysis: {
          template: '<div data-test="legacy-image">legacy image</div>',
        },
        WasmWorkbench: {
          template: '<div data-test="legacy-wasm">legacy wasm</div>',
        },
      },
    },
  });
  return { server, fetch };
}
function button(label: string) {
  const found = wrapper!
    .findAll("button")
    .find((item) => item.text().trim() === label);
  if (!found) throw new Error(`Missing button ${label}`);
  return found;
}
async function openChat() {
  await button("对话工作台").trigger("click");
  await flushPromises();
  await wrapper!.get(".conversation-button").trigger("click");
  await flushPromises();
}

describe("formal chat images and diagnostic navigation", () => {
  it("removes experimental workspaces from main navigation and defaults the development area to unmounted and closed", async () => {
    const { fetch } = setup();
    await flushPromises();
    const nav = wrapper!.get('nav[aria-label="主导航"]');
    expect(nav.text()).toContain("任务与诊断");
    expect(nav.text()).not.toContain("图片分析");
    expect(nav.text()).not.toContain("计算工坊");
    expect(wrapper!.text()).toContain("Ubuntu 24.04 LTS 64 位 / aarch64");
    expect(wrapper!.text()).not.toContain("Lite");
    await button("任务与诊断").trigger("click");
    await flushPromises();
    expect(
      (wrapper!.get(".development-diagnostics").element as HTMLDetailsElement)
        .open,
    ).toBe(false);
    expect(wrapper!.find('[data-test="legacy-image"]').exists()).toBe(false);
    expect(wrapper!.find('[data-test="legacy-wasm"]').exists()).toBe(false);
    expect(wrapper!.get('[aria-label="任务与诊断概览"]').text()).toContain(
      "脱敏状态",
    );
    expect(
      fetch.mock.calls.some(
        ([path]) =>
          path.includes("/wasm-workbench") || path.includes("/image-analysis"),
      ),
    ).toBe(false);
  });
  it.each(["images", "wasm"])(
    "normalizes the old #%s link into diagnostics and opens only its explicit compatibility region",
    async (legacy) => {
      setup(legacy);
      await flushPromises();
      expect(location.hash).toBe("#diagnostics");
      expect(
        (wrapper!.get(".development-diagnostics").element as HTMLDetailsElement)
          .open,
      ).toBe(true);
      expect(
        wrapper!
          .find(
            `[data-test="legacy-${legacy === "images" ? "image" : "wasm"}"]`,
          )
          .exists(),
      ).toBe(true);
      expect(
        wrapper!
          .find(
            `[data-test="legacy-${legacy === "images" ? "wasm" : "image"}"]`,
          )
          .exists(),
      ).toBe(false);
      expect(wrapper!.text()).toContain("非正式插件能力");
    },
  );
  it("shows processing status without falsely saying canonical images are already cleaned and records visual grounding in the same history", async () => {
    const { server } = setup();
    await flushPromises();
    await openChat();
    expect(wrapper!.get('[aria-label="历史图片信息"]').text()).toContain(
      "图片处理中",
    );
    expect(wrapper!.get('[aria-label="历史图片信息"]').text()).not.toContain(
      "处理图片已清理",
    );
    server.messages = [
      {
        id: "m",
        role: "user",
        content: "图片问题",
        created_at: 1792830000,
        image: {
          width: 20,
          height: 12,
          sha256: "a".repeat(64),
          status: "described",
          original_retained: false,
        },
      },
      {
        id: "r",
        role: "assistant",
        content: "当前助手的文字回答",
        created_at: 1792830001,
        basis: "vision_description",
      },
    ];
    await button("刷新会话与任务").trigger("click");
    await flushPromises();
    expect(wrapper!.get('[aria-label="历史图片信息"]').text()).toContain(
      "视觉描述已生成，处理图片已清理",
    );
    expect(wrapper!.text()).toContain("基于图片视觉描述的回答");
  });
  it("submits a real conversation image route and follows its ordinary run into the same history", async () => {
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:workspace");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    vi.stubGlobal(
      "Image",
      class {
        naturalWidth = 20;
        naturalHeight = 12;
        onload: (() => void) | null = null;
        onerror: (() => void) | null = null;
        decoding = "";
        set src(_value: string) {
          queueMicrotask(() => this.onload?.());
        }
      },
    );
    const { fetch } = setup();
    await flushPromises();
    await openChat();
    const bytes = new Uint8Array(33);
    bytes.set([137, 80, 78, 71, 13, 10, 26, 10]);
    const header = new DataView(bytes.buffer);
    header.setUint32(8, 13);
    header.setUint32(12, 0x49484452);
    header.setUint32(16, 20);
    header.setUint32(20, 12);
    const input = wrapper!.get('input[type="file"]');
    Object.defineProperty(input.element, "files", {
      configurable: true,
      value: [new File([bytes], "synthetic.png", { type: "image/png" })],
    });
    await input.trigger("change");
    await flushPromises();
    await wrapper!
      .get('textarea[aria-label="对话消息"]')
      .setValue("请看图并回答");
    await wrapper!.get('form[aria-label="发送对话消息"]').trigger("submit");
    await flushPromises();
    expect(
      fetch.mock.calls
        .filter(([, options]) => options?.method === "POST")
        .map(([path]) => path),
    ).toEqual(["/api/admin/v1/conversations/a/image-messages"]);
    expect(
      fetch.mock.calls.some(([path]) => path.endsWith("/runs/run-a")),
    ).toBe(true);
    expect(wrapper!.text()).toContain("基于视觉描述作出的当前助手回答");
    expect(wrapper!.text()).toContain("基于图片视觉描述的回答");
  });
  it("queries an already accepted run again after a status failure without posting the message twice", async () => {
    let failRun = true;
    const { fetch } = setup("", (path) => {
      if (path === "/runs/run-a" && failRun)
        return response({ error: "temporary_failure" }, 503);
      return undefined;
    });
    await flushPromises();
    await openChat();
    await wrapper!
      .get('textarea[aria-label="对话消息"]')
      .setValue("普通已提交消息");
    await wrapper!.get('form[aria-label="发送对话消息"]').trigger("submit");
    await flushPromises();
    expect(wrapper!.text()).toContain("消息任务已创建，状态查询未完成");
    failRun = false;
    await button("刷新会话与任务").trigger("click");
    await flushPromises();
    expect(
      fetch.mock.calls.filter(([, options]) => options?.method === "POST"),
    ).toHaveLength(1);
    expect(wrapper!.text()).toContain("基于视觉描述作出的当前助手回答");
  });
});
