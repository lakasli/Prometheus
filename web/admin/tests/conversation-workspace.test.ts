// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import App from "../src/App.vue";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  location.hash = "";
});
function response(value: unknown) {
  return new Response(JSON.stringify(value), {
    headers: { "Content-Type": "application/json" },
  });
}
function setup(hash = "") {
  location.hash = hash;
  const fetch = vi.fn(async (input: string) => {
    const path = input.replace("/api/admin/v1", "");
    if (path === "/auth/status") return response({ initialized: true });
    if (path === "/auth/me")
      return response({
        user: { username: "owner", role: "admin" },
        csrf_token: "workspace-csrf",
      });
    if (path === "/status")
      return response({
        version: "test",
        queue: {},
        platform: { os: "linux", arch: "aarch64" },
        model: { configured: true, model: "test" },
      });
    if (path === "/config")
      return response({
        revision: 1,
        applied_revision: 1,
        config: {
          assistant: {
            name: "DSH",
            system_prompt: "",
            max_concurrent_runs: 2,
            timeout_seconds: 120,
          },
          model: {
            base_url: "https://api.deepseek.com",
            model: "test",
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
    if (path === "/secrets" || path === "/conversations")
      return response({ items: [] });
    if (path === "/assistant/personality")
      return response({
        etag: "one",
        revision: 1,
        personality: {
          name: "刘嘉卉",
          description: "",
          personality: "温和",
          scenario: "",
          system_prompt: "",
          mes_example: "",
        },
        saved: true,
        applied: true,
        effective_from: "next_run",
      });
    if (path.startsWith("/runtime-logs?"))
      return response({
        items: [],
        next_cursor: null,
        capabilities: { subagent: false, asr: false, tts: true },
        retention: { days: 30, max_events: 20000 },
      });
    throw new Error(`Unexpected request ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(App);
  return fetch;
}
it("shows a single assistant settings page and a read-only log page", async () => {
  const fetch = setup();
  await flushPromises();
  const navigation = wrapper!.get("nav");
  expect(navigation.text()).toContain("性格与模型");
  expect(navigation.text()).toContain("日志记录");
  expect(navigation.text()).not.toContain("角色卡");
  expect(navigation.text()).not.toContain("对话工作台");
  expect(navigation.text()).not.toContain("任务与诊断");
  expect(wrapper!.text()).not.toMatch(/助手|\bAI\b|工具|assistant/i);
  await navigation
    .findAll("button")
    .find((button) => button.text() === "日志记录")!
    .trigger("click");
  await flushPromises();
  expect(wrapper!.find('[aria-label="运行日志"]').exists()).toBe(true);
  expect(wrapper!.text()).not.toContain("新建对话");
  expect(wrapper!.find(".conversation-composer").exists()).toBe(false);
  await navigation
    .findAll("button")
    .find((button) => button.text() === "性格与模型")!
    .trigger("click");
  await flushPromises();
  expect(wrapper!.get('[aria-label="她的性格"]').text()).toContain(
    "在这里写下她的性情、说话方式与相处习惯。",
  );
  expect(wrapper!.text()).not.toMatch(/助手|\bAI\b|工具|assistant/i);
  expect(
    wrapper!
      .findAll("input")
      .filter(
        (input) => (input.element as HTMLInputElement).value === "刘嘉卉",
      ),
  ).toHaveLength(1);
  expect(
    fetch.mock.calls.some(([url]) => url.includes("character-cards")),
  ).toBe(false);
  expect(
    fetch.mock.calls.some(
      ([url]) =>
        url.includes("image-analysis") || url.includes("wasm-workbench"),
    ),
  ).toBe(false);
});
for (const legacy of ["chat", "diagnostics", "images", "wasm"]) {
  it(`maps the old ${legacy} link to logs without mounting development tools`, async () => {
    const fetch = setup(`#${legacy}`);
    await flushPromises();
    expect(location.hash).toBe("#logs");
    expect(wrapper!.find('[aria-label="运行日志"]').exists()).toBe(true);
    expect(
      fetch.mock.calls.every(
        ([url]) =>
          !url.includes("wasm-workbench") && !url.includes("image-analysis"),
      ),
    ).toBe(true);
  });
}
it("maps old character links to the single personality and keeps unsaved text on cancelled navigation", async () => {
  setup("#characters");
  await flushPromises();
  expect(location.hash).toBe("#assistant");
  await wrapper!.get('[aria-label="她的性格"] textarea').setValue("未保存文本");
  vi.stubGlobal(
    "confirm",
    vi.fn(() => false),
  );
  await wrapper!
    .get("nav")
    .findAll("button")
    .find((button) => button.text() === "日志记录")!
    .trigger("click");
  expect(location.hash).toBe("#assistant");
  expect(
    (
      wrapper!.get('[aria-label="她的性格"] textarea')
        .element as HTMLTextAreaElement
    ).value,
  ).toBe("未保存文本");
});
