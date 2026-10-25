// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import TaskTriggers from "../src/components/TaskTriggers.vue";
import SkillOutput from "../src/components/SkillOutput.vue";
import { newTriggerDefinition } from "../src/task-triggers";
import type { TriggerDefinition, TriggerJob } from "../src/task-triggers";
import { taskTriggerCandidate } from "../src/task-output";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  clearCsrfToken();
});
const response = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
function proposal(): TriggerDefinition {
  const value = newTriggerDefinition();
  value.title = "测试提醒";
  value.action.content = "独立提醒内容";
  return value;
}
function envelope(value = proposal()) {
  return "```json\n" + JSON.stringify({ task_trigger_draft: value }) + "\n```";
}
const caps = {
  available: true,
  spec_version: "1",
  now: 1792900000,
  time_zone: "Asia/Shanghai",
  skills: [],
  characters: [],
  calendars: [],
  event_types: [],
  limits: {},
  clock: { trusted: true, source: "test", reason: null },
};
function mock(
  options: { conflict?: boolean; uncertain?: boolean; clock?: boolean } = {},
) {
  let job: TriggerJob | null = null;
  let failed = false;
  let acknowledged = false;
  const fetch = vi.fn(async (url: string, opts?: RequestInit) => {
    const path = url.replace("/api/admin/v1/task-triggers", "");
    const method = opts?.method || "GET";
    const body = opts?.body ? JSON.parse(opts.body as string) : null;
    if (path === "/capabilities")
      return response({
        ...caps,
        clock: { ...caps.clock, trusted: !options.clock || acknowledged },
      });
    if (path === "/clock/acknowledge") {
      acknowledged = true;
      return response({ trusted: true });
    }
    if (path.startsWith("/jobs?") && method === "GET")
      return response({ items: job ? [job] : [], total: job ? 1 : 0 });
    if (path === "/jobs" && method === "POST") {
      if (options.uncertain && !failed) {
        failed = true;
        throw new TypeError("network pending");
      }
      job = {
        id: "one",
        title: body.task_trigger_draft.title,
        revision: 1,
        draft_revision: 1,
        active_revision: null,
        lifecycle: "draft",
        block_reason: null,
        next_fire_at: null,
        created_at: 1,
        updated_at: 1,
        task_trigger_draft: body.task_trigger_draft,
        active_definition: null,
      };
      return response(job, 201);
    }
    if (path === "/preview")
      return response({
        valid: true,
        errors: [],
        now: caps.now,
        next_times: [
          {
            at: caps.now + 30,
            local: "2026-10-25 18:00:30 Asia/Shanghai",
            key: "once",
          },
        ],
        preview_token: body.job_id ? "scoped-preview" : null,
        estimated: true,
        blocked_reason: null,
      });
    if (path.endsWith("/activate")) {
      job = {
        ...job!,
        revision: 2,
        active_revision: 1,
        lifecycle: "enabled",
        active_definition: job!.task_trigger_draft,
      };
      return response(job);
    }
    if (path.endsWith("/drafts")) {
      if (options.conflict)
        return response(
          { error: { code: "revision_conflict", message: "reload" } },
          409,
        );
      job = {
        ...job!,
        revision: job!.revision + 1,
        draft_revision: job!.draft_revision + 1,
        task_trigger_draft: body.task_trigger_draft,
      };
      return response(job);
    }
    if (path.includes("/occurrences")) return response({ items: [], total: 0 });
    if (path === "/jobs/one") return response(job);
    throw new Error("Unexpected fixture route " + path);
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}
function button(name: string) {
  const found = wrapper!.findAll("button").find((item) => item.text() === name);
  if (!found) throw new Error(name);
  return found;
}
function field(name: string, selector = "input") {
  const found = wrapper!
    .findAll("label")
    .find(
      (item) => item.find("span").exists() && item.find("span").text() === name,
    );
  if (!found) throw new Error(name);
  return found.get(selector);
}
async function createAndFill() {
  await flushPromises();
  await button("新建触发任务").trigger("click");
  await field("任务名称").setValue("测试提醒");
  await field("固定提醒内容", "textarea").setValue("独立提醒内容");
}
it("recognizes only bounded explicit task proposals, rejecting duplicate keys and forged management fields", () => {
  expect(taskTriggerCandidate(envelope())).toEqual(proposal());
  expect(
    taskTriggerCandidate(
      envelope().replace('"title":"测试提醒"', '"title":"原文","title":"改写"'),
    ),
  ).toBeNull();
  expect(
    taskTriggerCandidate(
      envelope().replace(
        '"title":"测试提醒"',
        '"title":"测试提醒","enabled":true',
      ),
    ),
  ).toBeNull();
  expect(
    taskTriggerCandidate(
      envelope().replace('"policy":{', '"policy":null,"unused":{'),
    ),
  ).toBeNull();
  expect(
    taskTriggerCandidate(
      envelope().replace(
        '"trigger":{"type":"once"',
        '"trigger":{"shell":"x","type":"once"',
      ),
    ),
  ).toBeNull();
  expect(
    taskTriggerCandidate(
      '```json\n{"name":"ordinary","description":"x","instructions":"x"}\n```',
    ),
  ).toBeNull();
  expect(taskTriggerCandidate("x".repeat(65537))).toBeNull();
});
it("task proposal review emits the draft without writing or activating anything", async () => {
  const fetch = mock();
  wrapper = mount(SkillOutput, { props: { content: envelope() } });
  await button("审核任务草稿").trigger("click");
  expect(wrapper.emitted("reviewTask")?.[0]?.[0]).toEqual(proposal());
  expect(fetch).not.toHaveBeenCalled();
});
it("saves disabled first, then binds activation to the saved job and current preview token", async () => {
  setCsrfToken("test-csrf");
  const fetch = mock();
  wrapper = mount(TaskTriggers);
  await createAndFill();
  await button("保存停用草稿").trigger("click");
  await flushPromises();
  expect(wrapper.text()).toContain("任务已保存为停用草稿");
  const create = fetch.mock.calls.find(
    (call) => call[0].endsWith("/jobs") && call[1]?.method === "POST",
  )!;
  expect(JSON.parse(create[1]!.body as string)).toEqual({
    task_trigger_draft: proposal(),
  });
  expect(create[1]!.headers).toMatchObject({ "X-CSRF-Token": "test-csrf" });
  expect(button("应用并启用").attributes("disabled")).toBeDefined();
  await button("校验并预演规则").trigger("click");
  await flushPromises();
  expect(button("应用并启用").attributes("disabled")).toBeUndefined();
  const preview = fetch.mock.calls.find((call) =>
    call[0].endsWith("/preview"),
  )!;
  expect(JSON.parse(preview[1]!.body as string)).toMatchObject({
    job_id: "one",
    expected_revision: 1,
    draft_revision: 1,
  });
  await button("应用并启用").trigger("click");
  await flushPromises();
  const activate = fetch.mock.calls.find((call) =>
    call[0].endsWith("/activate"),
  )!;
  expect(JSON.parse(activate[1]!.body as string)).toEqual({
    expected_revision: 1,
    draft_revision: 1,
    preview_token: "scoped-preview",
  });
  expect(wrapper.text()).toContain("任务版本已应用并启用");
});
it("keeps edits after a server version conflict", async () => {
  mock({ conflict: true });
  wrapper = mount(TaskTriggers);
  await createAndFill();
  await button("保存停用草稿").trigger("click");
  await flushPromises();
  await field("任务名称").setValue("本地尚未合并的修改");
  await button("保存停用草稿").trigger("click");
  await flushPromises();
  expect(field("任务名称").element).toHaveProperty(
    "value",
    "本地尚未合并的修改",
  );
  expect(wrapper.text()).toContain("重新载入后合并");
});
it("freezes uncertain mutation contents and idempotency key for an explicit retry", async () => {
  const fetch = mock({ uncertain: true });
  wrapper = mount(TaskTriggers);
  await createAndFill();
  await button("保存停用草稿").trigger("click");
  await flushPromises();
  expect(wrapper.text()).toContain("操作结果未确认");
  expect(field("任务名称").attributes("disabled")).toBeUndefined(); // inherited fieldset carries the disabled state
  expect(wrapper.get("fieldset").attributes("disabled")).toBeDefined();
  await button("原样重试上次操作").trigger("click");
  await flushPromises();
  const calls = fetch.mock.calls.filter(
    (call) => call[0].endsWith("/jobs") && call[1]?.method === "POST",
  );
  expect(calls).toHaveLength(2);
  expect(calls[0]![1]!.body).toBe(calls[1]![1]!.body);
  expect(calls[0]![1]!.headers).toMatchObject({
    "Idempotency-Key": (calls[1]![1]!.headers as Record<string, string>)[
      "Idempotency-Key"
    ],
  });
});
it("imports combined weekly/workday selectors without losing the intersection", async () => {
  const fetch = mock();
  const value = proposal();
  value.trigger = {
    type: "calendar",
    frequency: "weekly",
    weekdays: [1, 3],
    at_times: ["09:00:00"],
    day_class: "workday",
    calendar_id: "cn-official",
  };
  wrapper = mount(TaskTriggers, { props: { initialDraft: value } });
  await flushPromises();
  await button("校验并预演规则").trigger("click");
  await flushPromises();
  const request = fetch.mock.calls.find((call) =>
    call[0].endsWith("/preview"),
  )!;
  expect(
    JSON.parse(request[1]!.body as string).task_trigger_draft.trigger,
  ).toEqual(value.trigger);
  expect(
    fetch.mock.calls.every(
      (call) => !(call[1]?.method === "POST" && !call[0].endsWith("/preview")),
    ),
  ).toBe(true);
});
it("clock trust requires an explicit user confirmation and no host command", async () => {
  const fetch = mock({ clock: true });
  vi.stubGlobal(
    "confirm",
    vi.fn(() => true),
  );
  wrapper = mount(TaskTriggers);
  await flushPromises();
  await button("核对并确认服务器时间").trigger("click");
  await flushPromises();
  const request = fetch.mock.calls.find((call) =>
    call[0].endsWith("/clock/acknowledge"),
  )!;
  expect(JSON.parse(request[1]!.body as string)).toEqual({
    observed_now: caps.now,
    confirmed: true,
  });
  expect(wrapper.text()).toContain("服务器时钟已人工确认");
});
it("calendar import remains a draft until source verification and digest-bound application", async () => {
  const fallback = mock();
  const calendar = {
    id: "calendar-one",
    calendar_id: "cn-official",
    revision: 2,
    active: false,
    sha256: "synthetic-calendar-digest",
    verified_by: null,
    created_at: 1,
    document: {
      year: 2026,
      time_zone: "Asia/Shanghai",
      holidays: ["2026-01-01"],
      working_days: ["2026-01-04"],
      source: {
        authority: "synthetic fixture",
        document_number: "fixture",
        published_at: "2025-11-04",
        url: "https://www.example.invalid/fixture",
      },
    },
  };
  const fetch = vi.fn(async (url: string, opts?: RequestInit) => {
    if (url.endsWith("/calendars")) return response({ items: [] });
    if (url.endsWith("/calendars/drafts")) return response(calendar, 201);
    if (url.endsWith("/calendars/calendar-one/apply"))
      return response({ ...calendar, active: true });
    return fallback(url, opts);
  });
  vi.stubGlobal("fetch", fetch);
  vi.stubGlobal(
    "confirm",
    vi.fn(() => true),
  );
  wrapper = mount(TaskTriggers);
  await flushPromises();
  await field("官方年度日历 JSON", "textarea").setValue(
    JSON.stringify(calendar.document),
  );
  await button("保存日历导入草稿").trigger("click");
  await flushPromises();
  expect(wrapper.text()).toContain("尚未应用");
  expect(button("核验并应用日历版本").attributes("disabled")).toBeDefined();
  expect(fetch.mock.calls.some((call) => call[0].endsWith("/apply"))).toBe(
    false,
  );
  await wrapper.get('.trigger-verify input[type="checkbox"]').setValue(true);
  await button("核验并应用日历版本").trigger("click");
  await flushPromises();
  const apply = fetch.mock.calls.find((call) =>
    call[0].endsWith("/calendars/calendar-one/apply"),
  )!;
  expect(JSON.parse(apply[1]!.body as string)).toEqual({
    expected_digest: "synthetic-calendar-digest",
  });
});
it("session expiry is propagated and pending reads are aborted on unmount", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      response({ error: { code: "unauthorized", message: "login" } }, 401),
    ),
  );
  wrapper = mount(TaskTriggers);
  await flushPromises();
  expect(wrapper.emitted("unauthorized")).toHaveLength(1);
  wrapper.unmount();
  wrapper = undefined;
  let signal: AbortSignal | null | undefined;
  vi.stubGlobal(
    "fetch",
    vi.fn((_url: string, opts: RequestInit) => {
      signal = opts.signal;
      return new Promise(() => {});
    }),
  );
  wrapper = mount(TaskTriggers);
  await flushPromises();
  wrapper.unmount();
  wrapper = undefined;
  expect(signal?.aborted).toBe(true);
});
