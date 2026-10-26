// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import RelationshipsPanel from "../src/components/RelationshipsPanel.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";
import { selectValue } from "./select-helpers";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});
const person = {
  id: "person-a",
  display_name: "小张",
  revision: 4,
  visibility: "private",
  source_owner_id: "owner-a",
  source_conversation_id: "chat-a",
  merged_into: null,
};
const other = {
  ...person,
  id: "person-b",
  display_name: "微信小张",
  source_owner_id: "owner-b",
  source_conversation_id: "chat-b",
};
const assertion = {
  id: "assertion-a",
  person_id: person.id,
  field: "hobby",
  value: "围棋",
  revision: 1,
  status: "pending",
  statement_kind: "self_report",
  source_type: "diary_event",
  source_id: "event-a",
  source_revision: 2,
  reason: "本人在当前来源中明确陈述",
  valid_until: null,
};
function detail() {
  return {
    ...person,
    assertions: [assertion],
    identities: [],
    affinity: {
      score: 50,
      daily_decay: 1,
      grace_seconds: 259200,
      last_direct_at: null,
    },
    contact_permission: {
      status: "not_granted",
      revision: 0,
      delivery_available: false,
      blocked_reason: "须接收方许可及真实渠道验收",
    },
    identity_group: null,
  };
}
function response(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
function setup(
  handler?: (
    path: string,
    options?: RequestInit,
  ) => Response | undefined | Promise<Response | undefined>,
) {
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1", "");
    const custom = await handler?.(path, options);
    if (custom) return custom;
    if (path.startsWith("/relationships/people?") && options?.method === "GET")
      return response({ items: [person, other], total: 2 });
    if (path === "/relationships/edges" && options?.method === "GET")
      return response({ items: [] });
    if (path === "/conversations")
      return response({
        items: [
          { id: "chat-a", title: "QQ私聊" },
          { id: "chat-b", title: "微信私聊" },
        ],
      });
    if (path === "/relationships/people/person-a" && options?.method === "GET")
      return response(detail());
    throw new Error(`Unexpected request ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("relationships-csrf");
  wrapper = mount(RelationshipsPanel, { attachTo: document.body });
  return fetch;
}
function button(label: string) {
  const b = wrapper!.findAll("button").find((b) => b.text() === label);
  if (!b) throw new Error(`Missing ${label}`);
  return b;
}
function label(text: string) {
  const l = wrapper!.findAll("label").find((l) => l.text().startsWith(text));
  if (!l) throw new Error(`Missing label ${text}`);
  return l;
}
async function choose() {
  await flushPromises();
  await wrapper!.find(".relationship-list > button").trigger("click");
  await flushPromises();
}

describe("人物关系管理", () => {
  it("selects the same profile from graph clicks and keyboard activation", async () => {
    setup((path, options) =>
      path === "/relationships/people/person-b" && options?.method === "GET"
        ? response({ ...detail(), ...other })
        : undefined,
    );
    await flushPromises();
    const node = (name: string) =>
      wrapper!.get(`.relationship-graph-node[aria-label="查看${name}的资料"]`);
    await node(person.display_name).trigger("click");
    await flushPromises();
    expect(wrapper!.get("article.relationship-detail h3").text()).toBe(
      person.display_name,
    );
    expect(node(person.display_name).attributes("aria-pressed")).toBe("true");
    expect(wrapper!.get(".relationship-list > button.active").text()).toContain(
      person.display_name,
    );
    await node(other.display_name).trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(wrapper!.get("article.relationship-detail h3").text()).toBe(
      other.display_name,
    );
    expect(node(person.display_name).attributes("aria-pressed")).toBe("false");
    await node(person.display_name).trigger("keydown", { key: " " });
    await flushPromises();
    expect(wrapper!.get("article.relationship-detail h3").text()).toBe(
      person.display_name,
    );
  });

  it("draws only approved relationships with two visible unmerged endpoints", async () => {
    const merged = { ...person, id: "merged", merged_into: person.id };
    const edge = {
      id: "approved",
      from_person_id: person.id,
      to_person_id: other.id,
      relationship_type: "friend",
      revision: 1,
      status: "approved",
      statement_kind: "confirmed",
      reason: "人工核验",
    };
    setup((path) => {
      if (path.startsWith("/relationships/people?"))
        return response({ items: [person, other, merged], total: 3 });
      if (path === "/relationships/edges")
        return response({
          items: [
            edge,
            { ...edge, id: "pending", status: "pending" },
            { ...edge, id: "rejected", status: "rejected" },
            { ...edge, id: "outside", to_person_id: "outside" },
            { ...edge, id: "merged-edge", to_person_id: merged.id },
          ],
        });
    });
    await flushPromises();
    expect(wrapper!.findAll(".relationship-graph-node")).toHaveLength(2);
    expect(wrapper!.findAll(".relationship-graph-edge")).toHaveLength(1);
    await wrapper!.get(".relationship-graph-node").trigger("click");
    await flushPromises();
    expect(wrapper!.get(".relationship-graph-edge").classes()).toContain(
      "highlighted",
    );
    expect(wrapper!.get(".relationship-graph-edge").text()).toContain("朋友");
  });

  it("bounds zoom controls and restores the default view", async () => {
    setup();
    await flushPromises();
    const zoomIn = wrapper!.get('button[aria-label="放大关系图"]');
    const zoomOut = wrapper!.get('button[aria-label="缩小关系图"]');
    const scale = () =>
      wrapper!.get('output[aria-label="关系图缩放比例"]').text();
    for (let i = 0; i < 12; i++) await zoomIn.trigger("click");
    expect(scale()).toBe("240%");
    expect(zoomIn.attributes()).toHaveProperty("disabled");
    for (let i = 0; i < 15; i++) await zoomOut.trigger("click");
    expect(scale()).toBe("50%");
    expect(zoomOut.attributes()).toHaveProperty("disabled");
    await wrapper!.get('button[aria-label="重置关系图"]').trigger("click");
    expect(scale()).toBe("100%");
    expect(zoomIn.attributes()).not.toHaveProperty("disabled");
    expect(zoomOut.attributes()).not.toHaveProperty("disabled");
  });

  it("clears selection after search removes a person and ignores their late detail response", async () => {
    let finishDetail!: (response: Response) => void;
    setup((path, options) => {
      if (
        path === "/relationships/people/person-a" &&
        options?.method === "GET"
      )
        return new Promise<Response>((resolve) => {
          finishDetail = resolve;
        });
      if (
        path.startsWith("/relationships/people?") &&
        new URLSearchParams(path.split("?")[1]).get("q") === "微信"
      )
        return response({ items: [other], total: 1 });
    });
    await flushPromises();
    await wrapper!.get(".relationship-graph-node").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("正在载入人物");
    await wrapper!.get('input[placeholder="按称呼查找"]').setValue("微信");
    await wrapper!.get("form.relationship-search").trigger("submit");
    await flushPromises();
    finishDetail(response(detail()));
    await flushPromises();
    expect(wrapper!.find("article.relationship-detail").exists()).toBe(false);
    expect(wrapper!.text()).toContain("从一个人开始了解");
    expect(wrapper!.find(".relationship-graph-node.selected").exists()).toBe(
      false,
    );
    expect(wrapper!.find(".relationship-list > button.active").exists()).toBe(
      false,
    );
    expect(wrapper!.find('[role="alert"]').exists()).toBe(false);
  });

  it("shows stable person IDs, source scopes and a directed graph without inventing edges", async () => {
    setup();
    await flushPromises();
    expect(wrapper!.text()).toContain("同名不会自动合并");
    expect(wrapper!.findAll(".relationship-list > button")).toHaveLength(2);
    expect(wrapper!.findAll(".relationship-graph-edge")).toHaveLength(0);
    expect(
      wrapper!.find('svg[aria-label="已审核的有向人物关系图"]').exists(),
    ).toBe(true);
  });
  it("creates a private person with provenance and CSRF without trusting a supplied owner", async () => {
    let saved: Record<string, unknown> | undefined;
    const fetch = setup((path, options) => {
      if (path === "/relationships/people" && options?.method === "POST") {
        saved = JSON.parse(String(options.body));
        return response(person, 201);
      }
    });
    await flushPromises();
    await button("建立人物").trigger("click");
    await label("称呼").find("input").setValue("小张");
    await label("建立依据").find("textarea").setValue("本人确认建立");
    await wrapper!.find("form.relationship-form").trigger("submit");
    await flushPromises();
    expect(saved).toMatchObject({
      display_name: "小张",
      visibility: "private",
      source_conversation_id: null,
      publish: false,
      reason: "本人确认建立",
    });
    expect(saved).not.toHaveProperty("source_owner_id");
    const call = fetch.mock.calls.find(([, o]) => o?.method === "POST")!;
    expect(call[1]?.headers).toMatchObject({
      "X-CSRF-Token": "relationships-csrf",
    });
  });
  it("does not approve a profile candidate until a reason is supplied and uses its exact revision", async () => {
    const fetch = setup((path, options) =>
      path.endsWith("/assertions/assertion-a/review") &&
      options?.method === "POST"
        ? response({ ...assertion, status: "approved", revision: 2 })
        : undefined,
    );
    await choose();
    await button("批准资料").trigger("click");
    await flushPromises();
    expect(wrapper!.find('[role="alert"]').text()).toContain("审核理由");
    expect(
      fetch.mock.calls.filter(([, o]) => o?.method === "POST"),
    ).toHaveLength(0);
    await label("审核理由").find("textarea").setValue("核对本人原话");
    await button("批准资料").trigger("click");
    await flushPromises();
    const call = fetch.mock.calls.find(
      ([p, o]) =>
        p.endsWith("/assertions/assertion-a/review") && o?.method === "POST",
    )!;
    expect(call[1]?.headers).toMatchObject({ "If-Match": '"1"' });
    expect(JSON.parse(String(call[1]?.body))).toEqual({
      decision: "approve",
      supersedes: [],
      reason: "核对本人原话",
    });
  });
  it("keeps candidate input visible after a version conflict", async () => {
    setup((path, options) =>
      path.endsWith("/person-a/assertions") && options?.method === "POST"
        ? response(
            { error: { code: "revision_conflict", message: "版本已变化" } },
            409,
          )
        : undefined,
    );
    await choose();
    await label("字段内容").find("textarea").setValue("待保存偏好");
    await label("来源与依据").find("textarea").setValue("本人陈述");
    const form = wrapper!
      .findAll("form")
      .find((f) => f.text().includes("提交候选"))!;
    await form.trigger("submit");
    await flushPromises();
    expect(wrapper!.find('[role="alert"]').text()).toContain("重新载入");
    expect(
      (label("字段内容").find("textarea").element as HTMLTextAreaElement).value,
    ).toBe("待保存偏好");
  });
  it("requires explicit confirmation before linking two software identities", async () => {
    let request: unknown;
    const fetch = setup((path, options) =>
      path.endsWith("/identity-group") && options?.method === "POST"
        ? ((request = JSON.parse(String(options.body))),
          response({ id: "group", members: [person, other] }))
        : undefined,
    );
    await choose();
    await selectValue(
      label("另一个已核验的账号档案").find('[role="combobox"]'),
      other.id,
    );
    await label("两侧核验").find("textarea").setValue("两个账号分别核验");
    await button("确认同人关联").trigger("submit");
    await flushPromises();
    expect(
      fetch.mock.calls.filter(([, o]) => o?.method === "POST"),
    ).toHaveLength(0);
    await label("已核对真实身份").find("input").setValue(true);
    const form = wrapper!
      .findAll("form")
      .find((f) => f.text().includes("确认同人关联"))!;
    await form.trigger("submit");
    await flushPromises();
    expect(request).toMatchObject({
      other_person_id: "person-b",
      other_revision: 4,
      confirmed: true,
      reason: "两个账号分别核验",
    });
  });
  it("requires a separate confirmation for forgetting and submits the selected person revision", async () => {
    const fetch = setup((path, options) =>
      path === "/relationships/people/person-a" && options?.method === "DELETE"
        ? response({ deleted: true })
        : undefined,
    );
    await choose();
    await button("遗忘人物").trigger("click");
    await flushPromises();
    expect(wrapper!.find('[role="alertdialog"]').exists()).toBe(true);
    expect(
      fetch.mock.calls.filter(([, o]) => o?.method === "DELETE"),
    ).toHaveLength(0);
    await button("确认遗忘").trigger("click");
    await flushPromises();
    const call = fetch.mock.calls.find(([, o]) => o?.method === "DELETE")!;
    expect(call[1]?.headers).toMatchObject({ "If-Match": '"4"' });
    expect(wrapper!.text()).toContain("离线备份");
  });
  it("labels contact as blocked and offers revocation without a send action", async () => {
    setup();
    await choose();
    expect(wrapper!.text()).toContain("主动投递：阻断");
    expect(button("撤销主动联系许可").exists()).toBe(true);
    expect(
      wrapper!
        .findAll("button")
        .some((b) => /发送消息|立即联系|批准发送/.test(b.text())),
    ).toBe(false);
  });
});
