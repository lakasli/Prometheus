// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import ConversationMedia from "../src/components/ConversationMedia.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
const asset = (kind = "video") => ({
  id: "60fcd0a4-aa5c-4edc-a519-7d9bca09c197",
  kind,
  bytes: 400,
  mime: "video/mp4",
  expires_at: 1793000000,
});
function response(data: unknown) {
  return new Response(JSON.stringify(data), { status: 200 });
}
async function open() {
  (wrapper!.get("details").element as HTMLDetailsElement).open = true;
  await wrapper!.get("details").trigger("toggle");
  await flushPromises();
}
async function choose(file: File) {
  Object.defineProperty(wrapper!.get("input").element, "files", {
    value: [file],
    configurable: true,
  });
  await wrapper!.get("input").trigger("change");
  await flushPromises();
}
describe("conversation media boundaries", () => {
  it("loads only on opening and keeps preview authenticated and scoped", async () => {
    const fetch = vi.fn(async () => response({ items: [asset()] }));
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(ConversationMedia, {
      props: { conversationId: "private-conversation" },
    });
    expect(fetch).not.toHaveBeenCalled();
    await open();
    expect(wrapper.get("video").attributes("src")).toBe(
      "/api/admin/v1/conversations/private-conversation/media/60fcd0a4-aa5c-4edc-a519-7d9bca09c197",
    );
    expect(wrapper.get("video").attributes("preload")).toBe("none");
    expect(wrapper.text()).toContain("视频仅支持保存和回发");
  });
  it("rejects oversized or unsupported files before reading or sending them", async () => {
    const fetch = vi.fn(async () => response({ items: [] }));
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(ConversationMedia, {
      props: { conversationId: "private-conversation" },
    });
    await open();
    fetch.mockClear();
    const file = new File(["fixture"], "huge.mp4");
    Object.defineProperty(file, "size", { value: 20 * 1048576 + 1 });
    await choose(file);
    expect(wrapper.get('[role="alert"]').text()).toContain("20MiB");
    await choose(new File(["fixture"], "untrusted.html"));
    expect(fetch).not.toHaveBeenCalled();
  });
  it("posts only the private bytes and type with CSRF, without triggering chat sends", async () => {
    setCsrfToken("fixture-csrf");
    const fetch = vi.fn(async (_url: string, options?: RequestInit) =>
      response(options?.method === "POST" ? asset("image") : { items: [] }),
    );
    vi.stubGlobal("fetch", fetch);
    // Deterministic FileReader event, without waiting for platform file I/O.
    vi.stubGlobal(
      "FileReader",
      class {
        result = "data:image/png;base64,c3ludGhldGlj";
        onload?: () => void;
        readAsDataURL() {
          this.onload?.();
        }
        abort() {}
      },
    );
    wrapper = mount(ConversationMedia, {
      props: { conversationId: "private-conversation" },
    });
    await open();
    await choose(new File(["synthetic"], "fixture.png", { type: "image/png" }));
    const post = fetch.mock.calls.find((call) => call[1]?.method === "POST")!;
    expect(post[0]).toBe(
      "/api/admin/v1/conversations/private-conversation/media",
    );
    expect(JSON.parse(post[1]!.body as string)).toEqual({
      kind: "image",
      data_base64: "c3ludGhldGlj",
    });
    expect((post[1]!.headers as Record<string, string>)["X-CSRF-Token"]).toBe(
      "fixture-csrf",
    );
    expect(wrapper.get('[role="status"]').text()).toContain(
      "尚未发送到聊天平台",
    );
    expect(
      fetch.mock.calls.every((call) => !call[0].includes("/messages")),
    ).toBe(true);
  });
  it("discards stale results when switching private conversations", async () => {
    let resolveOld!: (value: Response) => void;
    const fetch = vi.fn((url: string) =>
      url.includes("/old/")
        ? new Promise<Response>((resolve) => {
            resolveOld = resolve;
          })
        : Promise.resolve(response({ items: [] })),
    );
    vi.stubGlobal("fetch", fetch);
    wrapper = mount(ConversationMedia, { props: { conversationId: "old" } });
    await open();
    await wrapper.setProps({ conversationId: "new" });
    await flushPromises();
    resolveOld(response({ items: [asset()] }));
    await flushPromises();
    expect(wrapper.find("video").exists()).toBe(false);
    expect(wrapper.text()).toContain("当前没有可用的媒体附件");
  });
});
