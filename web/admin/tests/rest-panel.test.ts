// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import RestPanel from "../src/components/RestPanel.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";
import {
  restGrant,
  restPolicy,
  restProgress,
  restSources,
  restStatus,
} from "./rest-fixtures";
import type { RestPolicyView, RestStatus } from "../src/rest-management";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function setup(
  options: {
    policy?: RestPolicyView;
    state?: RestStatus;
    intercept?: (
      path: string,
      init?: RequestInit,
    ) => Response | Promise<Response> | undefined;
  } = {},
) {
  let policy = options.policy || restPolicy(),
    state = options.state || restStatus();
  let grant = restGrant();
  setCsrfToken("rest-test-csrf");
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    const path = url.replace("/api/admin/v1", "");
    const intercepted = options.intercept?.(path, init);
    if (intercepted) return intercepted;
    if (path === "/rest") return response(state);
    if (path === "/rest/cycles/current")
      return response({
        current: state.state.cycle_id ? restProgress() : null,
      });
    if (path === "/rest/policy") {
      if (init?.method === "PUT")
        policy = {
          ...policy,
          revision: policy.revision + 1,
          policy: JSON.parse(String(init.body)),
        };
      return response(policy);
    }
    if (path.startsWith("/commitments/scopes?"))
      return response({ items: restSources(), next_cursor: null });
    if (path.startsWith("/rest/control-grants/")) {
      const run = path.split("/").at(-1)!;
      if (init?.method === "PUT")
        grant = {
          ...grant,
          source_run_id: run,
          revision: grant.revision + 1,
          permissions: JSON.parse(String(init.body)),
          grant_present: true,
        };
      return response({ ...grant, source_run_id: run });
    }
    if (path === "/rest/sleep" || path === "/rest/wake") {
      state = {
        ...state,
        state: {
          ...state.state,
          phase: path.endsWith("sleep") ? "sleep_pending" : "awake",
          generation: 8,
        },
      };
      return response({
        state: state.state,
        operation: {
          id: "original-control-receipt",
          generation: 6,
          phase: "sleeping",
          wake_at: 1793003600,
        },
        idempotent_replay: false,
        operation_is_original_receipt: true,
        physical_leases_released: false,
      });
    }
    if (path.startsWith("/rest/cycles?") && path.includes("&after="))
      return response({ items: [], next_cursor: null, metadata_only: true });
    if (path.startsWith("/rest/cycles?"))
      return response({
        items: [
          {
            cycle_id: "cycle-one",
            generation: 7,
            trigger_kind: "admin",
            policy_revision: 2,
            wake_at: 1793003600,
            created_at: 1793000000,
            finished_at: null,
            partial: true,
            model_calls: 2,
            reserved_tokens: 6000,
          },
        ],
        next_cursor: 4,
        metadata_only: true,
      });
    if (path === "/rest/cycles/cycle-one") return response(restProgress());
    throw new Error(`Unexpected rest request ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  const confirm = vi.fn().mockReturnValue(true);
  vi.stubGlobal("confirm", confirm);
  return { fetch, confirm };
}
function button(name: string) {
  return wrapper!.findAll("button").find((item) => item.text() === name)!;
}
async function mounted() {
  wrapper = mount(RestPanel, { attachTo: document.body });
  await flushPromises();
}
const writes = (fetch: ReturnType<typeof setup>["fetch"]) =>
  fetch.mock.calls.filter(([, init]) =>
    ["POST", "PUT"].includes(init?.method || ""),
  );

it("shows real phase, original deadlines, unknown checkpoints and held physical work without starting a job", async () => {
  const state = restStatus();
  state.state = {
    ...state.state,
    phase: "consolidating",
    cycle_id: "cycle-one",
    wake_at: 1793003600,
    drain_at: 1793000030,
    consolidate_at: 1793000300,
  };
  const { fetch } = setup({ state });
  await mounted();
  expect(wrapper!.text()).toContain("正在整理 · generation 7 · 状态 v11");
  expect(wrapper!.text()).toContain("2 项实际请求或投递尚未回收");
  expect(wrapper!.text()).toContain("2 个步骤 · 1 个作业 · 2 项结果未知");
  expect(wrapper!.text()).toContain("部分结果，保留未完成与未知");
  expect(wrapper!.text()).toContain("已处理 5 个引用");
  expect(wrapper!.text()).toContain("transport_outcome_unknown");
  expect(
    wrapper!.get('[aria-label="允许对话控制作息"]').element,
  ).toHaveProperty("checked", false);
  expect(wrapper!.get('[aria-label="启用定时作息"]').element).toHaveProperty(
    "checked",
    false,
  );
  expect(writes(fetch)).toHaveLength(0);
  expect(
    wrapper!
      .findAll("button")
      .some((item) => /完成步骤|重试作业|执行命令|释放请求/.test(item.text())),
  ).toBe(false);
});

it("saves a complete independent policy only on explicit submit, retaining the active cycle deadline", async () => {
  const state = restStatus();
  state.state.phase = "sleeping";
  state.state.wake_at = 1793003600;
  const { fetch } = setup({ state });
  const storage = vi.spyOn(Storage.prototype, "setItem");
  await mounted();
  await wrapper!.get('[aria-label="每周期最多整理调用"]').setValue(9);
  expect(writes(fetch)).toHaveLength(0);
  await wrapper!.get('form[aria-label="作息策略"]').trigger("submit");
  await flushPromises();
  const [url, init] = writes(fetch)[0]!;
  expect(url).toBe("/api/admin/v1/rest/policy");
  expect(init?.headers).toMatchObject({
    "If-Match": '"2"',
    "X-CSRF-Token": "rest-test-csrf",
    "Idempotency-Key": expect.any(String),
  });
  expect(JSON.parse(String(init?.body))).toEqual({
    ...restPolicy().policy,
    max_cycle_calls: 9,
  });
  expect(wrapper!.text()).toContain("当前周期沿用原策略与原唤醒期限");
  expect(wrapper!.text()).toContain("睡眠中");
  expect(
    fetch.mock.calls.some(([url]) =>
      /\/config|\/character|\/assistant/.test(url),
    ),
  ).toBe(false);
  expect(storage).not.toHaveBeenCalled();
});

it("blocks invalid resource bounds and requires a positive drain window for natural control", async () => {
  const { fetch } = setup();
  await mounted();
  await wrapper!.get('[aria-label="允许对话控制作息"]').setValue(true);
  await wrapper!.get('[aria-label="排空窗口"]').setValue(0);
  expect(wrapper!.text()).toContain("排空窗口必须大于零");
  await wrapper!.get('form[aria-label="作息策略"]').trigger("submit");
  await wrapper!.get('[aria-label="普通等待队列上限"]').setValue(513);
  expect(button("保存作息策略").attributes("disabled")).toBeDefined();
  expect(writes(fetch)).toHaveLength(0);
});

it("fails closed on missing clock trust but keeps wake available with the actual generation and no client clock", async () => {
  const state = restStatus();
  delete state.clock.trusted_for_new_sleep;
  const { fetch } = setup({ state });
  await mounted();
  expect(button("按已保存策略入睡").attributes("disabled")).toBeDefined();
  expect(button("请求唤醒").attributes("disabled")).toBeUndefined();
  await button("请求唤醒").trigger("click");
  await flushPromises();
  const [url, init] = writes(fetch)[0]!;
  expect(url).toBe("/api/admin/v1/rest/wake");
  expect(init?.headers).toMatchObject({
    "If-Match": '"7"',
    "Idempotency-Key": expect.any(String),
  });
  expect(JSON.parse(String(init?.body))).toEqual({});
  expect(wrapper!.text()).toContain("清醒 · generation 8");
  expect(wrapper!.text()).not.toContain("睡眠中 · generation 6");
  expect(wrapper!.text()).toContain("2 项实际请求或投递尚未回收");
});

it("requests sleep from the saved policy only after explicit confirmation and never extends the wake deadline locally", async () => {
  const policy = restPolicy();
  policy.policy.default_sleep_seconds = 600;
  const { fetch, confirm } = setup({ policy });
  await mounted();
  confirm.mockReturnValueOnce(false);
  await button("按已保存策略入睡").trigger("click");
  expect(writes(fetch)).toHaveLength(0);
  await button("按已保存策略入睡").trigger("click");
  await flushPromises();
  const [url, init] = writes(fetch)[0]!;
  expect(url).toBe("/api/admin/v1/rest/sleep");
  expect(JSON.parse(String(init?.body))).toEqual({ wake_at: null });
  expect(init?.headers).toMatchObject({
    "If-Match": '"7"',
    "X-CSRF-Token": "rest-test-csrf",
  });
  expect(wrapper!.text()).toContain("等待排空 · generation 8");
  expect(button("按已保存策略入睡").attributes("disabled")).toBeDefined();
  expect(writes(fetch)).toHaveLength(1);
});

it("retains policy edits on 409 and requires an explicit reload without retrying", async () => {
  let saves = 0;
  const { fetch, confirm } = setup({
    intercept: (path, init) =>
      path === "/rest/policy" && init?.method === "PUT"
        ? (saves++,
          response(
            {
              error: {
                code: "rest_revision_or_request_conflict",
                message: "conflict",
              },
            },
            409,
          ))
        : undefined,
  });
  await mounted();
  await wrapper!.get('[aria-label="每周期最多整理调用"]').setValue(10);
  await wrapper!.get('form[aria-label="作息策略"]').trigger("submit");
  await flushPromises();
  expect(
    wrapper!.get('[aria-label="每周期最多整理调用"]').element,
  ).toHaveProperty("value", "10");
  expect(wrapper!.text()).toContain("不会自动重试");
  expect(saves).toBe(1);
  expect(button("保存作息策略").attributes("disabled")).toBeDefined();
  confirm.mockReturnValueOnce(false);
  await button("重新载入作息策略").trigger("click");
  await flushPromises();
  expect(
    wrapper!.get('[aria-label="每周期最多整理调用"]').element,
  ).toHaveProperty("value", "10");
  await button("重新载入作息策略").trigger("click");
  await flushPromises();
  expect(
    wrapper!.get('[aria-label="每周期最多整理调用"]').element,
  ).toHaveProperty("value", "8");
  expect(writes(fetch)).toHaveLength(1);
});

it("a clock-trust rejection blocks further sleep while allowing wake without an automatic control retry", async () => {
  const policy = restPolicy();
  policy.policy.default_sleep_seconds = 600;
  const { fetch } = setup({
    policy,
    intercept: (path) =>
      path === "/rest/sleep"
        ? response(
            { error: { code: "rest_clock_untrusted", message: "blocked" } },
            409,
          )
        : undefined,
  });
  await mounted();
  await button("按已保存策略入睡").trigger("click");
  await flushPromises();
  expect(wrapper!.text()).toContain("主机时钟尚未获确认");
  expect(button("按已保存策略入睡").attributes("disabled")).toBeDefined();
  expect(button("请求唤醒").attributes("disabled")).toBeUndefined();
  expect(writes(fetch)).toHaveLength(1);
});

it("selects a real speaker scope and saves only separate sleep/wake permissions with an absolute expiry", async () => {
  const { fetch } = setup();
  await mounted();
  await button("读取可管理来源").trigger("click");
  await flushPromises();
  expect(
    wrapper!.get('[aria-label="作息控制来源"]').findAll("option"),
  ).toHaveLength(2);
  await wrapper!
    .get('[aria-label="作息控制来源"]')
    .setValue("verified-speaker-two");
  await flushPromises();
  await wrapper!.get('[aria-label="本来源可请求唤醒"]').setValue(true);
  await wrapper!
    .get('[aria-label="控制许可截止"]')
    .setValue("2026-11-15T10:30");
  await wrapper!.get('form[aria-label="来源控制许可"]').trigger("submit");
  await flushPromises();
  const [url, init] = writes(fetch)[0]!;
  expect(url).toBe("/api/admin/v1/rest/control-grants/verified-run-two");
  expect(init?.headers).toMatchObject({
    "If-Match": '"0"',
    "Idempotency-Key": expect.any(String),
    "X-CSRF-Token": "rest-test-csrf",
  });
  expect(JSON.parse(String(init?.body))).toEqual({
    can_sleep: false,
    can_wake: true,
    valid_until: Date.parse("2026-11-15T10:30:00+08:00") / 1000,
  });
  expect(wrapper!.text()).toContain("没有增加资料、模型出域或外发许可");
  expect(wrapper!.find('input[name="owner"]').exists()).toBe(false);
});

it("does not fall back to a guessed owner when a selected source is no longer controllable", async () => {
  const { fetch } = setup({
    intercept: (path) =>
      path.endsWith("/verified-run-two")
        ? response(
            { error: { code: "rest_source_not_available", message: "gone" } },
            404,
          )
        : undefined,
  });
  await mounted();
  await button("读取可管理来源").trigger("click");
  await flushPromises();
  await wrapper!
    .get('[aria-label="作息控制来源"]')
    .setValue("verified-speaker-two");
  await flushPromises();
  expect(wrapper!.text()).toContain("不会改用其他身份");
  expect(wrapper!.find('form[aria-label="来源控制许可"]').exists()).toBe(false);
  expect(writes(fetch)).toHaveLength(0);
});

it("preserves edits on Escape and requests the browser unsaved protection without writing", async () => {
  const { fetch } = setup();
  await mounted();
  const input = wrapper!.get('[aria-label="排空窗口"]');
  await input.setValue(45);
  (input.element as HTMLInputElement).focus();
  input.element.dispatchEvent(
    new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
  );
  expect(input.element).toHaveProperty("value", "45");
  const event = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(true);
  expect(writes(fetch)).toHaveLength(0);
});

it("loads cycle history and checkpoint metadata read-only using the server cursor", async () => {
  const { fetch } = setup();
  await mounted();
  await button("读取最近周期").trigger("click");
  await flushPromises();
  expect(wrapper!.text()).toContain("2 次整理调用 · 保守预留 6000 token");
  await button("更早周期").trigger("click");
  await flushPromises();
  expect(
    fetch.mock.calls.some(([url]) =>
      url.endsWith("/rest/cycles?limit=20&after=4"),
    ),
  ).toBe(true);
  await button("查看此周期进度").trigger("click");
  await flushPromises();
  expect(wrapper!.text()).toContain("结果未知");
  expect(writes(fetch)).toHaveLength(0);
});

it("aborts an in-flight write on unmount and does not emit or move focus when its late reply arrives", async () => {
  let settle!: (value: Response) => void;
  let signal: AbortSignal | undefined;
  setup({
    intercept: (path, init) =>
      path === "/rest/policy" && init?.method === "PUT"
        ? ((signal = init.signal!),
          new Promise<Response>((resolve) => {
            settle = resolve;
          }))
        : undefined,
  });
  await mounted();
  await wrapper!.get('[aria-label="排空窗口"]').setValue(45);
  await wrapper!.get('form[aria-label="作息策略"]').trigger("submit");
  const closed = wrapper!;
  wrapper!.unmount();
  wrapper = undefined;
  const next = document.createElement("button");
  document.body.append(next);
  next.focus();
  expect(signal?.aborted).toBe(true);
  settle(response(restPolicy()));
  await flushPromises();
  expect(document.activeElement).toBe(next);
  expect(closed.emitted("unauthorized")).toBeUndefined();
});

it("preserves an unchanged grant deadline to the second and does not reinterpret edited expiry when future policy changes zone", async () => {
  const loaded = restGrant();
  loaded.revision = 3;
  loaded.grant_present = true;
  loaded.permissions = {
    can_sleep: false,
    can_wake: false,
    valid_until: Date.parse("2026-11-15T10:30:47+08:00") / 1000,
  };
  const { fetch } = setup({
    intercept: (path, init) =>
      path.endsWith("/verified-run-one") && init?.method === "GET"
        ? response(loaded)
        : undefined,
  });
  await mounted();
  await button("读取可管理来源").trigger("click");
  await flushPromises();
  await wrapper!.get('[aria-label="本来源可请求唤醒"]').setValue(true);
  await wrapper!.get('form[aria-label="来源控制许可"]').trigger("submit");
  await flushPromises();
  const first = writes(fetch).find(([url]) => url.includes("control-grants"))!;
  expect(JSON.parse(String(first[1]?.body)).valid_until).toBe(
    loaded.permissions.valid_until,
  );
  await wrapper!
    .get('[aria-label="控制许可截止"]')
    .setValue("2026-11-15T11:30");
  await wrapper!.get('[aria-label="作息时区"]').setValue("UTC");
  await wrapper!.get('form[aria-label="作息策略"]').trigger("submit");
  await flushPromises();
  expect(wrapper!.text()).toContain("控制许可截止（Asia/Shanghai）");
  await wrapper!.get('form[aria-label="来源控制许可"]').trigger("submit");
  await flushPromises();
  const last = writes(fetch)
    .filter(([url]) => url.includes("control-grants"))
    .at(-1)!;
  expect(JSON.parse(String(last[1]?.body)).valid_until).toBe(
    Date.parse("2026-11-15T11:30:00+08:00") / 1000,
  );
});

it("preserves source grant edits on conflict until that grant is explicitly reloaded", async () => {
  const { fetch, confirm } = setup({
    intercept: (path, init) =>
      path.includes("/control-grants/") && init?.method === "PUT"
        ? response(
            {
              error: {
                code: "rest_revision_or_request_conflict",
                message: "conflict",
              },
            },
            409,
          )
        : undefined,
  });
  await mounted();
  await button("读取可管理来源").trigger("click");
  await flushPromises();
  await wrapper!.get('[aria-label="本来源可请求唤醒"]').setValue(true);
  await wrapper!
    .get('[aria-label="控制许可截止"]')
    .setValue("2026-11-15T10:30");
  await wrapper!.get('form[aria-label="来源控制许可"]').trigger("submit");
  await flushPromises();
  expect(
    wrapper!.get('[aria-label="本来源可请求唤醒"]').element,
  ).toHaveProperty("checked", true);
  expect(button("保存本来源控制许可").attributes("disabled")).toBeDefined();
  confirm.mockReturnValueOnce(false);
  await button("重新载入本来源许可").trigger("click");
  await flushPromises();
  expect(
    wrapper!.get('[aria-label="本来源可请求唤醒"]').element,
  ).toHaveProperty("checked", true);
  await button("重新载入本来源许可").trigger("click");
  await flushPromises();
  expect(
    wrapper!.get('[aria-label="本来源可请求唤醒"]').element,
  ).toHaveProperty("checked", false);
  expect(writes(fetch)).toHaveLength(1);
});

it("reports authentication expiry and emits the existing session boundary without any write", async () => {
  const { fetch } = setup({
    intercept: (path) =>
      path === "/rest"
        ? response(
            { error: { code: "unauthorized", message: "请重新登录" } },
            401,
          )
        : undefined,
  });
  await mounted();
  expect(wrapper!.emitted("unauthorized")).toHaveLength(1);
  expect(wrapper!.get('[role="alert"]').text()).toContain("请重新登录");
  expect(writes(fetch)).toHaveLength(0);
});
