// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import RuntimeLogs from "../src/components/RuntimeLogs.vue";
import { selectValue } from "./select-helpers";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
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
const event = (id: number) => ({
  id,
  kind: "tool",
  status: "completed",
  source: "web",
  run_id: "run-one",
  conversation_id: "conversation-one",
  title: "工具执行",
  content: "<script>unsafe()</script> 工具输出",
  details: { tool: "test-tool", elapsed_ms: 12 },
  created_at: 1792830000,
});
const page = (items = [event(10)], next_cursor: string | null = "10") => ({
  items,
  next_cursor,
  capabilities: { subagent: false, asr: false, tts: true },
  retention: { days: 30, max_events: 20000 },
});
it("renders real event content as text and pages with the same filters", async () => {
  const fetch = vi.fn(async (url: string) =>
    response(url.includes("cursor=") ? page([event(9)], null) : page()),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(RuntimeLogs);
  await flushPromises();
  expect(wrapper.text()).toContain("语音识别尚未接入");
  expect(wrapper.text()).toContain("协作分工尚未接入");
  expect(wrapper.find("script").exists()).toBe(false);
  await wrapper.get(".runtime-log-summary").trigger("click");
  expect(wrapper.get(".runtime-log-detail").text()).toContain("run-one");
  expect(wrapper.get(".runtime-log-detail pre").text()).toContain(
    "<script>unsafe()</script>",
  );
  await selectValue(wrapper.get('[aria-label="记录类型"]'), "tool");
  await wrapper.get('input[placeholder="按运行编号筛选"]').setValue("run-one");
  await wrapper.get('input[placeholder="标题或日志内容"]').setValue("工具");
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  const filtered = new URL(fetch.mock.calls[1]![0], "http://localhost");
  expect(filtered.searchParams.get("kind")).toBe("tool");
  expect(filtered.searchParams.get("run_id")).toBe("run-one");
  expect(filtered.searchParams.get("q")).toBe("工具");
  await wrapper
    .findAll("button")
    .find((button) => button.text() === "加载更早记录")!
    .trigger("click");
  await flushPromises();
  const earlier = new URL(fetch.mock.calls[2]![0], "http://localhost");
  expect(earlier.searchParams.get("cursor")).toBe("10");
  expect(earlier.searchParams.get("kind")).toBe("tool");
  expect(wrapper.findAll(".runtime-log-entry")).toHaveLength(2);
  expect(
    fetch.mock.calls.every(([url]) =>
      url.startsWith("/api/admin/v1/runtime-logs?"),
    ),
  ).toBe(true);
});
it("pauses automatic refresh while details are expanded and clears its timer on unmount", async () => {
  vi.useFakeTimers();
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  const fetch = vi.fn(async () => response(page()));
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(RuntimeLogs);
  await flushPromises();
  await wrapper.get('input[type="checkbox"]').setValue(true);
  await wrapper.get(".runtime-log-summary").trigger("click");
  await vi.advanceTimersByTimeAsync(5000);
  expect(fetch).toHaveBeenCalledTimes(1);
  await wrapper.get(".runtime-log-summary").trigger("click");
  await vi.advanceTimersByTimeAsync(5000);
  expect(fetch).toHaveBeenCalledTimes(2);
  wrapper.unmount();
  wrapper = undefined;
  await vi.advanceTimersByTimeAsync(15000);
  expect(fetch).toHaveBeenCalledTimes(2);
});
it("reports authentication expiry and never requests raw filesystem logs", async () => {
  const fetch = vi.fn(async () =>
    response({ error: { code: "unauthorized", message: "请重新登录" } }, 401),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(RuntimeLogs);
  await flushPromises();
  expect(wrapper.emitted("unauthorized")).toHaveLength(1);
  expect(wrapper.get('[role="alert"]').text()).toContain("请重新登录");
  expect(fetch).toHaveBeenCalledTimes(1);
});
