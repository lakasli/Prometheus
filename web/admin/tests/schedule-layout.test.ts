// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import SchedulePanel from "../src/components/SchedulePanel.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
function setup() {
  const resources = [
    {
      id: "book",
      title: "已有文字",
      revision: 1,
      characters: 12,
      revoked_at: null,
    },
  ];
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    const path = url.replace("/api/admin/v1", "");
    let value: unknown;
    if (path.startsWith("/schedule/calendar?"))
      value = { items: [], legacy_events: [], total: 0 };
    else if (path === "/schedule/capabilities")
      value = { clock: { trusted: true } };
    else if (path === "/schedule/resources") {
      if (options?.method === "POST") {
        const body = JSON.parse(options.body as string);
        resources.push({
          id: "new",
          title: body.title,
          revision: 1,
          characters: body.content.length,
          revoked_at: null,
        });
        value = resources.at(-1);
      } else value = { items: resources };
    } else value = { items: [] };
    return new Response(JSON.stringify(value), {
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("schedule-test");
  return fetch;
}

it("keeps reading as an activity without separate bookshelf or compatibility settings tabs", async () => {
  const fetch = setup();
  wrapper = mount(SchedulePanel);
  await flushPromises();
  expect(wrapper.findAll('[role="tab"]').map((tab) => tab.text())).toEqual([
    "日历",
    "工作目标与授权",
  ]);
  expect(wrapper.text()).not.toContain("授权书架");
  expect(wrapper.text()).not.toContain("兼容触发设置");
  expect(
    fetch.mock.calls.some(([url]) => url.includes("/task-triggers/jobs")),
  ).toBe(false);
  await wrapper
    .findAll("button")
    .find((button) => button.text() === "新建活动")!
    .trigger("click");
  const editor = wrapper.get('[aria-label="活动编辑"]');
  expect(editor.findAll("option").map((option) => option.text())).toContain(
    "阅读",
  );
  expect(editor.text()).toContain("已有文字");
  expect(editor.get("details summary").text()).toBe("阅读资料");
  expect(editor.get("details").attributes("open")).toBeUndefined();
  await editor.findAll("select")[0]!.setValue("consolidation");
  expect(editor.find("details").exists()).toBe(false);
});

it("allows text for a reading activity with explicit source authorization", async () => {
  const fetch = setup();
  wrapper = mount(SchedulePanel);
  await flushPromises();
  await wrapper
    .findAll("button")
    .find((button) => button.text() === "新建活动")!
    .trigger("click");
  const materials = wrapper.get(".reading-materials");
  expect(materials.get("button.primary").attributes("disabled")).toBeDefined();
  await materials.get('input[maxlength="120"]').setValue("日程中的阅读");
  await materials.get("textarea").setValue("有权提供的文字资料。");
  await materials.get('input[type="checkbox"]').setValue(true);
  expect(
    materials.get("button.primary").attributes("disabled"),
  ).toBeUndefined();
  await materials.get("form").trigger("submit");
  await flushPromises();
  const upload = fetch.mock.calls.find(
    ([url, options]) =>
      url.endsWith("/schedule/resources") && options?.method === "POST",
  );
  expect(JSON.parse(upload![1]!.body as string)).toEqual({
    title: "日程中的阅读",
    content: "有权提供的文字资料。",
    type: "text",
  });
  expect((upload![1]!.headers as Record<string, string>)["X-CSRF-Token"]).toBe(
    "schedule-test",
  );
  expect(materials.text()).toContain("日程中的阅读");
});

it("opens conversation commitments from the calendar toolbar without a new navigation panel", async () => {
  const fetch = setup();
  wrapper = mount(SchedulePanel);
  await flushPromises();
  const originalTabs = wrapper
    .findAll('[role="tab"]')
    .map((item) => item.text());
  expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
  expect(
    fetch.mock.calls.some(([url]) => url.includes("/commitments/scopes")),
  ).toBe(false);
  await wrapper
    .findAll("button")
    .find((item) => item.text() === "会话约定")!
    .trigger("click");
  await flushPromises();
  expect(wrapper.get('[role="dialog"]').text()).toContain("会话约定");
  expect(wrapper.findAll('[role="tab"]').map((item) => item.text())).toEqual(
    originalTabs,
  );
  expect(
    fetch.mock.calls.some(([url]) => url.includes("/commitments/scopes?")),
  ).toBe(true);
  await wrapper.get('[aria-label="关闭会话约定"]').trigger("click");
  expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
});
