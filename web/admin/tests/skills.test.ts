// @vitest-environment happy-dom
import { afterEach, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import SkillsPanel from "../src/components/SkillsPanel.vue";
import SkillOutput from "../src/components/SkillOutput.vue";
import { drawingHtml, drawingImage, skillCandidate } from "../src/skill-output";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  clearCsrfToken();
});
const skill = {
  id: "drawing",
  name: "绘画",
  description: "画图",
  instructions: "绘制自包含图画",
  kind: "drawing",
  enabled: true,
  revision: 3,
  builtin: true,
  updated_at: 1,
};
const response = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
it("uses the saved Skill revision and CSRF on update; conflicts preserve local changes", async () => {
  setCsrfToken("test-csrf");
  const fetch = vi.fn(async (_url: string, opts?: RequestInit) =>
    opts?.method === "PATCH"
      ? response(
          { error: { code: "revision_conflict", message: "reload" } },
          409,
        )
      : response({ items: [skill], drawing: { native_available: false } }),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(SkillsPanel);
  await flushPromises();
  await wrapper.get(".skill-item").trigger("click");
  await wrapper.get("input[maxlength='80']").setValue("新的绘画方式");
  await wrapper.get("form").trigger("submit");
  await flushPromises();
  const update = fetch.mock.calls.find((call) => call[1]?.method === "PATCH")!;
  expect(JSON.parse(update[1]!.body as string)).toMatchObject({
    expected_revision: 3,
    name: "新的绘画方式",
  });
  expect(update[1]?.headers).toMatchObject({ "X-CSRF-Token": "test-csrf" });
  expect(wrapper.get("input[maxlength='80']").element).toHaveProperty(
    "value",
    "新的绘画方式",
  );
  expect(wrapper.text()).toContain("重新载入后合并");
});
it("imports model drafts disabled and requires an explicit save", async () => {
  const fetch = vi.fn(async () =>
    response({ items: [skill], drawing: { native_available: false } }),
  );
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(SkillsPanel, {
    props: {
      initialDraft: {
        name: "检查文章",
        description: "文章检查",
        instructions: "先理解，再检查。",
        kind: "instruction",
        enabled: true,
      },
    },
  });
  await flushPromises();
  expect(wrapper.get("input[type='checkbox']").element).toHaveProperty(
    "checked",
    false,
  );
  expect(wrapper.text()).toContain("请核对内容后保存");
  expect(
    fetch.mock.calls.every(
      (call) =>
        !(call as unknown as [string, RequestInit])[1]?.method ||
        (call as unknown as [string, RequestInit])[1]?.method === "GET",
    ),
  ).toBe(true);
  expect(wrapper.emitted("consumed")).toHaveLength(1);
});
it("removes active HTML and navigation, preserves inline SVG in a sandbox", () => {
  const content =
    '```html\n<meta http-equiv="refresh" content="0;url=https://bad.test"><script>parent.alert(1)</script><svg onload="alert(1)" viewBox="0 0 40 40"><circle cx="20" cy="20" r="10" fill="blue"/></svg><iframe src="https://bad.test"></iframe><a href="https://bad.test">leave</a><img src="https://bad.test/tracker">\n```';
  const html = drawingHtml(content)!;
  expect(html).toContain("<circle");
  expect(html).toContain("script-src 'none'");
  expect(html).not.toMatch(
    /<script|<iframe|onload=|http-equiv="refresh"|bad\.test/,
  );
  wrapper = mount(SkillOutput, { props: { content } });
  expect(wrapper.get("iframe").attributes("sandbox")).toBe("");
  expect(wrapper.find("script").exists()).toBe(false);
});
it("accepts only bounded PNG data URLs and validated Skill JSON", () => {
  expect(drawingImage("![image](https://bad.test/p.png)")).toBeNull();
  expect(
    drawingImage("![image](data:image/svg+xml;base64,PHN2Zz4=)"),
  ).toBeNull();
  expect(
    drawingImage("![绘画](data:image/png;base64,iVBORw0KGgoAAA=)"),
  ).toMatch(/^data:image\/png/);
  expect(
    skillCandidate(
      '```json\n{"name":"test","description":"when","instructions":"steps","kind":"instruction","enabled":true}\n```',
    ),
  ).toMatchObject({ enabled: false, kind: "instruction" });
  expect(
    skillCandidate(
      '```json\n{"name":"test","description":"when","instructions":"steps","kind":"shell"}\n```',
    ),
  ).toBeNull();
});
