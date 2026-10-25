// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import PluginFactoryPanel from "../src/components/PluginFactoryPanel.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  setCsrfToken("synthetic-csrf");
});
afterEach(() => {
  wrapper?.unmount();
  wrapper = undefined;
  clearCsrfToken();
  vi.clearAllTimers();
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
function fixture(status = "eligible") {
  return {
    id: "version-one",
    plugin_id: "user.sum",
    version: "0.1.0",
    status,
    revision: 7,
    source_sha256: "a".repeat(64),
    component_sha256: "b".repeat(64),
    bundle_sha256: "c".repeat(64),
    test_report: { passed: true, cases: [{ passed: true, actual: 5 }] },
    specification: {
      plugin_id: "user.sum",
      version: "0.1.0",
      description: '合成插件 <img src="https://example.invalid/never">',
      input_schema: { type: "array", items: { type: "integer" } },
      output_schema: { type: "integer" },
      tests: [{ input: [2, 3], expected: 5 }],
    },
    body: '"<script>untrusted source</script>".to_owned()',
  };
}
function setup(status = "eligible", rejectApproval = false, available = true) {
  const item = fixture(status);
  const requests: Array<{ path: string; options?: RequestInit }> = [];
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    const path = url.replace("/api/admin/v1/plugin-factory", "");
    requests.push({ path, options });
    if (!options?.method || options.method === "GET") {
      if (path === "")
        return response({
          available,
          candidate_ready: true,
          probe_required: !available,
          availability_code: null,
          engine: "wasmtime-36.0.19",
          host_api: "dsh:plugin@1.0.0",
          versions: [item],
          jobs: [],
          routes: [],
        });
      if (path === "/versions/version-one") return response(item);
    }
    if (path === "/versions/version-one/approve") {
      if (rejectApproval)
        return response(
          {
            error: {
              code: "plugin_revision_conflict",
              message: "插件版本已变化，请重新加载",
            },
          },
          409,
        );
      item.status = "approved";
      item.revision++;
      return response(item);
    }
    if (path === "/versions/version-one/activate") {
      item.status = "active";
      item.revision++;
      return response(item);
    }
    throw new Error("unexpected fixture request");
  });
  vi.stubGlobal("fetch", fetch);
  wrapper = mount(PluginFactoryPanel);
  return { requests, fetch, item };
}
async function select() {
  await flushPromises();
  await wrapper!.get(".factory-version").trigger("click");
  await flushPromises();
}
function button(text: string) {
  const result = wrapper!
    .findAll("button")
    .find((button) => button.text() === text);
  if (!result) throw new Error("missing button: " + text);
  return result;
}
describe("Rust Component factory review controls", () => {
  it("renders untrusted source as text and requires explicit review before approval and separate activation", async () => {
    const { requests } = setup();
    await select();
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(wrapper!.find("script").exists()).toBe(false);
    expect(button("批准此制品").attributes("disabled")).toBeDefined();
    expect(requests.some((request) => request.options?.method === "POST")).toBe(
      false,
    );
    await wrapper!.get('input[type="checkbox"]').setValue(true);
    await button("批准此制品").trigger("click");
    await flushPromises();
    const approvals = requests.filter((request) =>
      request.path.endsWith("/approve"),
    );
    expect(approvals).toHaveLength(1);
    expect(JSON.parse(String(approvals[0].options?.body))).toEqual({
      reviewed: true,
      source_sha256: "a".repeat(64),
      component_sha256: "b".repeat(64),
      bundle_sha256: "c".repeat(64),
    });
    expect(approvals[0].options?.headers).toMatchObject({
      "If-Match": '"7"',
      "X-CSRF-Token": "synthetic-csrf",
    });
    expect(requests.some((request) => request.path.endsWith("/activate"))).toBe(
      false,
    );
    await button("启用此版本").trigger("click");
    await flushPromises();
    const activation = requests.find((request) =>
      request.path.endsWith("/activate"),
    );
    expect(JSON.parse(String(activation?.options?.body))).toEqual({
      route_revision: 0,
      bundle_sha256: "c".repeat(64),
    });
  });
  it("does not retry or override a stale approval after the server rejects its revision", async () => {
    const { requests } = setup("eligible", true);
    await select();
    await wrapper!.get('input[type="checkbox"]').setValue(true);
    await button("批准此制品").trigger("click");
    await flushPromises();
    expect(wrapper!.get('[role="alert"]').text()).toContain("插件版本已变化");
    expect(
      requests.filter((request) => request.path.endsWith("/approve")),
    ).toHaveLength(1);
    expect(requests.some((request) => request.path.endsWith("/activate"))).toBe(
      false,
    );
  });
  it("keeps builds disabled until an actual isolation probe has verified the environment", async () => {
    const { requests } = setup("generated", false, false);
    await select();
    expect(button("隔离构建").attributes("disabled")).toBeDefined();
    expect(button("检查构建环境").attributes("disabled")).toBeUndefined();
    expect(requests.some((request) => request.path.endsWith("/build"))).toBe(
      false,
    );
  });
});
