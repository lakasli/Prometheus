// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import AffinitySettings from "../src/components/AffinitySettings.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper<InstanceType<typeof AffinitySettings>> | undefined;
const policy = {
  revision: 3,
  initial: 50,
  minimum: 0,
  maximum: 100,
  daily_decay: 1,
  grace_seconds: 259200,
  downtime_cap_seconds: 604800,
  effective_at: 1,
};
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function setup(failSave = false) {
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1", "");
    if (path === "/relationships/affinity-policy" && options?.method === "GET")
      return response(policy);
    if (path === "/relationships/affinity-policy/preview")
      return response({ projected_from_initial: 47 });
    if (path === "/relationships/affinity-policy" && options?.method === "PUT")
      return failSave
        ? response({ error: { code: "revision_conflict" } }, 409)
        : response({ ...policy, revision: 4, daily_decay: 2 });
    throw new Error(`Unexpected request ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("affinity-csrf");
  wrapper = mount(AffinitySettings);
  return fetch;
}
function label(text: string) {
  const item = wrapper!.findAll("label").find((item) => item.text() === text);
  if (!item) throw new Error(`Missing label ${text}`);
  return item;
}
function button(text: string) {
  const item = wrapper!.findAll("button").find((item) => item.text() === text);
  if (!item) throw new Error(`Missing button ${text}`);
  return item;
}
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

it("loads the policy independently and saves with revision, CSRF and explicit confirmation", async () => {
  const fetch = setup();
  await flushPromises();
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(wrapper!.vm.hasUnsaved).toBe(false);
  await label("每日空闲衰减").get("input").setValue("2");
  await label("调整依据").get("textarea").setValue("根据实际互动节奏调整");
  expect(wrapper!.vm.hasUnsaved).toBe(true);
  await wrapper!.get("form").trigger("submit");
  await flushPromises();
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(wrapper!.get('[role="alert"]').text()).toContain("明确确认");
  await label("明确确认新规则从生效水位向后应用").get("input").setValue(true);
  await wrapper!.get("form").trigger("submit");
  await flushPromises();
  const save = fetch.mock.calls.find((call) => call[1]?.method === "PUT")!;
  expect(save[1]!.headers).toMatchObject({
    "If-Match": '"3"',
    "X-CSRF-Token": "affinity-csrf",
  });
  expect(JSON.parse(String(save[1]!.body))).toMatchObject({
    confirmed: true,
    daily_decay: 2,
    reason: "根据实际互动节奏调整",
  });
  expect(wrapper!.text()).toContain("当前版本 v4");
  expect(wrapper!.vm.hasUnsaved).toBe(false);
});

it("previews only through the preview endpoint and keeps changes pending", async () => {
  const fetch = setup();
  await flushPromises();
  await label("调整依据").get("textarea").setValue("检查停机后的结算");
  await label("预演空闲天数").get("input").setValue("12");
  await button("预演分数").trigger("click");
  await flushPromises();
  const preview = fetch.mock.calls.find((call) =>
    call[0].endsWith("/preview"),
  )!;
  expect(JSON.parse(String(preview[1]!.body))).toMatchObject({
    idle_seconds: 12 * 86400,
    policy: { confirmed: false, reason: "检查停机后的结算" },
  });
  expect(fetch.mock.calls.some((call) => call[1]?.method === "PUT")).toBe(
    false,
  );
  expect(wrapper!.text()).toContain("47.000");
  expect(wrapper!.vm.hasUnsaved).toBe(true);
});

it("retains pending edits after a revision conflict and refuses a cancelled reload", async () => {
  const fetch = setup(true);
  await flushPromises();
  await label("调整依据").get("textarea").setValue("保留这次调整");
  await label("明确确认新规则从生效水位向后应用").get("input").setValue(true);
  await wrapper!.get("form").trigger("submit");
  await flushPromises();
  expect(wrapper!.get('[role="alert"]').text()).toContain("配置已被更新");
  expect(wrapper!.vm.hasUnsaved).toBe(true);
  const confirm = vi.fn(() => false);
  vi.stubGlobal("confirm", confirm);
  await button("重新载入好感规则").trigger("click");
  await flushPromises();
  expect(confirm).toHaveBeenCalledOnce();
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(label("调整依据").get("textarea").element.value).toBe("保留这次调整");
});
