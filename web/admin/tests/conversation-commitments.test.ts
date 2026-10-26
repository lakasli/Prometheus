// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import ConversationCommitments from "../src/components/ConversationCommitments.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";
import type { ConversationCommitment } from "../src/context-management";

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
function candidate(): ConversationCommitment {
  return {
    id: "commitment-one",
    revision: 1,
    confirmation_state: "proposed",
    intent: "明天读完已授权的文章",
    kind: "reading",
    subject_kind: "unresolved",
    counterparty_kind: "unresolved",
    time: { original_text: "明天", precision: "unresolved" },
    sources: [],
    source_run_id: "source-run",
    execution: {
      state: "unscheduled",
      fulfillment_verified: false,
      recipient_permission: false,
    },
    recipient_permission: false,
  };
}
function setup(
  record = candidate(),
  reject?: (operation: string) => Response | undefined,
) {
  setCsrfToken("commitment-csrf");
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    const address = new URL(url, "http://localhost");
    const path = address.pathname.replace("/api/admin/v1", "");
    if (path === "/commitments/scopes")
      return response({
        items: [
          {
            conversation_id: "qq-conversation",
            run_id: "trusted-source-run",
            source_run_id: "source-run",
            title: "未命名来源",
            source_label: "QQ私聊",
            channel: "qq",
            source_kind: "c2c",
            scope_ref: "verified-scope",
            updated_at: 1793000000,
          },
        ],
        next_cursor: null,
      });
    if (path === "/conversations/qq-conversation/commitment-activities")
      return response({
        items: [
          {
            activity_id: "real-reading-activity",
            revision: 3,
            title: "已授权阅读",
            kind: "reading",
            state: "enabled",
            start_at: 1793000100,
            end_at: 1793000400,
            time_zone: "Asia/Shanghai",
            binding_supported: true,
            block_reason: null,
          },
        ],
        next_cursor: null,
      });
    if (
      path === "/conversations/qq-conversation/commitments" &&
      (!options?.method || options.method === "GET")
    )
      return response({ items: [record], next_cursor: null });
    if (options?.method === "POST") {
      const operation = path.endsWith("/commitments")
        ? "propose"
        : path.split("/").at(-1)!;
      const failure = reject?.(operation);
      if (failure) return failure;
      const body = JSON.parse(String(options.body));
      record = { ...record, revision: record.revision + 1 };
      if (operation === "participants")
        record = {
          ...record,
          subject_kind: body.subject_kind,
          counterparty_kind: body.counterparty_kind,
        };
      if (operation === "confirm")
        record = {
          ...record,
          confirmation_state: "confirmed",
          confirmation_evidence: {
            basis: "authenticated_management_review",
            event_revision: record.revision,
            source_assent_verified: false,
            recipient_permission: false,
          },
        };
      if (operation === "activity")
        record = {
          ...record,
          execution: {
            state: "scheduled",
            activity_id: body.activity_id,
            current_activity_revision: 3,
            fulfillment_verified: false,
            recipient_permission: false,
          },
        };
      if (operation === "propose")
        record = {
          ...candidate(),
          id: "new-candidate",
          intent: body.intent,
          kind: body.kind,
          time: { original_text: body.time_text, precision: "unresolved" },
        };
      if (operation === "cancel")
        record = { ...record, confirmation_state: "cancelled" };
      return response(record);
    }
    throw new Error(`Unexpected commitment request ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}
function button(text: string) {
  return wrapper!.findAll("button").find((item) => item.text() === text)!;
}

it("selects server-verified sources without manual owner or run IDs and never confirms candidates on load", async () => {
  const fetch = setup();
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  expect(wrapper.get('[role="dialog"]').attributes("aria-modal")).toBe("true");
  expect(wrapper.get('[aria-label="约定来源"]').text()).toContain("QQ私聊");
  expect(wrapper.text()).toContain("主体待明确");
  expect(wrapper.text()).toContain("对象待明确");
  expect(wrapper.text()).toContain("尚未调度");
  expect(wrapper.find('input[name="owner"]').exists()).toBe(false);
  expect(fetch.mock.calls).toHaveLength(2);
  expect(
    fetch.mock.calls.every(
      ([, options]) => !options?.method || options.method === "GET",
    ),
  ).toBe(true);
  const list = new URL(fetch.mock.calls[1]![0], "http://localhost");
  expect(list.searchParams.get("run_id")).toBe("trusted-source-run");
  expect(button("确认约定").attributes("disabled")).toBeDefined();
});

it("reviews participants and confirms as separate versioned actions without creating a schedule", async () => {
  const fetch = setup();
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  await wrapper.get('[aria-label="履行主体"]').setValue("assistant");
  await wrapper.get('[aria-label="约定对象"]').setValue("speaker");
  await wrapper.get(".participant-review").trigger("submit");
  await flushPromises();
  const reviewed = fetch.mock.calls.find(([url]) =>
    url.includes("/participants?"),
  )!;
  expect(JSON.parse(String(reviewed[1]?.body))).toEqual({
    subject_kind: "assistant",
    counterparty_kind: "speaker",
  });
  expect(reviewed[1]?.headers).toMatchObject({
    "If-Match": '"1"',
    "X-CSRF-Token": "commitment-csrf",
    "Idempotency-Key": expect.any(String),
  });
  expect(wrapper.text()).toContain("约定仍待确认");
  await wrapper.get('.commitment-check input[type="checkbox"]').setValue(true);
  await button("确认约定").trigger("click");
  await flushPromises();
  const confirmed = fetch.mock.calls.find(([url]) =>
    url.includes("/confirm?"),
  )!;
  expect(confirmed[1]?.headers).toMatchObject({ "If-Match": '"2"' });
  expect(JSON.parse(String(confirmed[1]?.body))).toEqual({ confirmed: true });
  expect(wrapper.text()).toContain("资料已审核确认");
  expect(wrapper.text()).toContain(
    "这是当前管理员对约定资料的审核；未核验来源发言者或接收者同意。",
  );
  expect(wrapper.text()).toContain("资料确认不代表已建立日程或获得发送许可。");
  expect(wrapper.text()).not.toContain("用户已确认");
  expect(wrapper.text()).toContain("尚未调度");
  expect(fetch.mock.calls.some(([url]) => url.includes("/schedule/"))).toBe(
    false,
  );
});

it("binds only an existing activity from the selected source and keeps execution separate from delivery", async () => {
  const item = candidate();
  item.confirmation_state = "confirmed";
  item.subject_kind = "assistant";
  item.counterparty_kind = "speaker";
  const fetch = setup(item);
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  await button("读取可关联活动").trigger("click");
  await flushPromises();
  const choices = fetch.mock.calls.find(([url]) =>
    url.includes("/commitment-activities?"),
  )!;
  expect(
    new URL(choices[0], "http://localhost").searchParams.get("run_id"),
  ).toBe("trusted-source-run");
  await wrapper
    .get('[aria-label="可关联日程活动"]')
    .setValue("real-reading-activity");
  await button("关联选中活动").trigger("click");
  await flushPromises();
  const bound = fetch.mock.calls.find(([url]) => url.includes("/activity?"))!;
  expect(JSON.parse(String(bound[1]?.body))).toEqual({
    activity_id: "real-reading-activity",
    activity_revision: 3,
    occurrence_id: null,
  });
  expect(bound[1]?.headers).toMatchObject({
    "If-Match": '"1"',
    "Idempotency-Key": expect.any(String),
  });
  expect(wrapper.text()).toContain("已关联真实日程");
  expect(wrapper.text()).toContain("未提供独立投递证明");
  expect(wrapper.text()).not.toContain("提醒已设置");
  expect(
    fetch.mock.calls.some(([url]) => url.includes("/schedule/activities")),
  ).toBe(false);
});

it("keeps personal promises and reminders from turning into executable or outbound actions", async () => {
  const item = candidate();
  item.confirmation_state = "confirmed";
  item.subject_kind = "speaker";
  setup(item);
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  expect(wrapper.text()).toContain("不会被转换为她的执行任务");
  expect(
    wrapper.findAll("button").some((item) => item.text() === "读取可关联活动"),
  ).toBe(false);
  wrapper.unmount();
  wrapper = undefined;
  item.subject_kind = "assistant";
  item.kind = "reminder";
  setup(item);
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  expect(wrapper.text()).toContain("真实提醒与主动联系尚未开放");
  expect(
    wrapper.findAll("button").some((item) => item.text() === "读取可关联活动"),
  ).toBe(false);
});

it("retains unknown execution evidence without offering cancellation or replay", async () => {
  const item = candidate();
  item.confirmation_state = "confirmed";
  item.execution.state = "unknown";
  const fetch = setup(item);
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  expect(wrapper.text()).toContain("执行结果未知");
  expect(wrapper.text()).toContain("不自动重放");
  expect(
    wrapper.findAll("button").some((item) => item.text() === "取消约定"),
  ).toBe(false);
  expect(
    fetch.mock.calls.every(
      ([, options]) => !options?.method || options.method === "GET",
    ),
  ).toBe(true);
});

it("preserves participant edits on conflict and blocks another mutation until an explicit reload", async () => {
  const fetch = setup(candidate(), (operation) =>
    operation === "participants"
      ? response(
          { error: { code: "commitment_conflict", message: "changed" } },
          409,
        )
      : undefined,
  );
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  await wrapper.get('[aria-label="履行主体"]').setValue("assistant");
  await wrapper.get(".participant-review").trigger("submit");
  await flushPromises();
  expect(wrapper.get('[role="alert"]').text()).toContain("你的填写仍保留");
  expect(
    (wrapper.get('[aria-label="履行主体"]').element as HTMLSelectElement).value,
  ).toBe("assistant");
  expect(button("保存主体与对象").attributes("disabled")).toBeDefined();
  expect(
    fetch.mock.calls.filter(([, options]) => options?.method === "POST"),
  ).toHaveLength(1);
});

it("creates only a candidate from text and renders source text safely", async () => {
  const fetch = setup();
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  await button("登记约定候选").trigger("click");
  await wrapper
    .get('[aria-label="约定事项"]')
    .setValue("<script>notExecutable()</script> 下次阅读");
  await wrapper.get('[aria-label="约定时间表达"]').setValue("明天下午");
  await wrapper.get(".commitment-create form").trigger("submit");
  await flushPromises();
  const create = fetch.mock.calls.find(
    ([url, options]) =>
      url.includes("/commitments?") && options?.method === "POST",
  )!;
  const body = JSON.parse(String(create[1]?.body));
  expect(body).toMatchObject({
    kind: "reading",
    time_text: "明天下午",
    sources: [],
  });
  expect(body.owner_id).toBeUndefined();
  expect(body.run_id).toBeUndefined();
  expect(wrapper.find("script").exists()).toBe(false);
  expect(wrapper.text()).toContain("尚未确认，也未创建日程");
});

it("supports escape and restores focus when the modal closes", async () => {
  setup();
  const previous = document.createElement("button");
  document.body.append(previous);
  previous.focus();
  wrapper = mount(ConversationCommitments, { attachTo: document.body });
  await flushPromises();
  expect(document.activeElement).toBe(wrapper.get('[role="dialog"]').element);
  await wrapper.get('[role="dialog"]').trigger("keydown", { key: "Escape" });
  expect(wrapper.emitted("close")).toHaveLength(1);
  wrapper.unmount();
  wrapper = undefined;
  expect(document.activeElement).toBe(previous);
});

it("keeps keyboard focus in the modal when confirmation removes the focused button", async () => {
  const item = candidate();
  item.subject_kind = "speaker";
  item.counterparty_kind = "assistant";
  setup(item);
  const confirm = vi.fn();
  vi.stubGlobal("confirm", confirm);
  wrapper = mount(ConversationCommitments, { attachTo: document.body });
  await flushPromises();
  await wrapper.get('.commitment-check input[type="checkbox"]').setValue(true);
  const control = button("确认约定");
  (control.element as HTMLButtonElement).focus();
  expect(document.activeElement).toBe(control.element);
  await control.trigger("click");
  await flushPromises();
  const modal = wrapper.get('[role="dialog"]').element;
  expect(control.element.isConnected).toBe(false);
  expect(modal.contains(document.activeElement)).toBe(true);
  document.activeElement!.dispatchEvent(
    new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
  );
  expect(wrapper.emitted("close")).toHaveLength(1);
  expect(confirm).not.toHaveBeenCalled();
});

it("still requires confirmation to discard unsaved participant edits on Escape", async () => {
  setup();
  const confirm = vi.fn().mockReturnValue(false);
  vi.stubGlobal("confirm", confirm);
  wrapper = mount(ConversationCommitments, { attachTo: document.body });
  await flushPromises();
  const control = wrapper.get('[aria-label="履行主体"]');
  await control.setValue("speaker");
  (control.element as HTMLSelectElement).focus();
  document.activeElement!.dispatchEvent(
    new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
  );
  expect(confirm).toHaveBeenCalledWith("约定中有尚未保存的填写，确定关闭？");
  expect(wrapper.emitted("close")).toBeUndefined();
  expect(control.element).toHaveProperty("value", "speaker");
  confirm.mockReturnValue(true);
  document.activeElement!.dispatchEvent(
    new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
  );
  expect(wrapper.emitted("close")).toHaveLength(1);
});

it("reports source authorization expiry without making any write request", async () => {
  const fetch = vi.fn(async () =>
    response({ error: { code: "unauthorized", message: "请重新登录" } }, 401),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(ConversationCommitments);
  await flushPromises();
  expect(wrapper.emitted("unauthorized")).toHaveLength(1);
  expect(fetch).toHaveBeenCalledTimes(1);
});
