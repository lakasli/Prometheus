// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import WasmWorkbench from "../src/components/WasmWorkbench.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

let wrapper: VueWrapper | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-24T13:00:00Z"));
  vi.spyOn(crypto, "randomUUID").mockReturnValue(
    "47edff88-d861-4b38-a02d-ec8f70942f02",
  );
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
function expires(seconds = 300) {
  return Math.floor(Date.now() / 1000) + seconds;
}
function artifact(id = "a", status = "ready", revision = 1) {
  return {
    id,
    name: id === "a" ? "整数加法" : "整数比较",
    status,
    revision,
    wasm_sha256: (id === "a" ? "a" : "b").repeat(64),
    tests_sha256: "c".repeat(64),
    engine_version: "wasmi-2.0.0",
    policy_version: "pure-i32-v1",
    created_at: expires(-5),
    wat: `(module ;; 独属于${id}的源码 <img src="https://example.invalid/never">\n(func (export "calc") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))`,
    tests: [
      { a: 0, b: 0, expected: 0 },
      { a: 2147483647, b: 1, expected: -2147483648 },
    ],
    test_results: [
      { actual: 0, passed: true },
      { actual: -2147483648, passed: true },
    ],
  };
}
function job(kind = "generate", status = "queued", revision = 3) {
  return {
    id: "job-a",
    kind,
    status,
    revision,
    created_at: expires(-2),
    expires_at: expires(),
    name: "明确生成的私有任务",
    artifact_id: "a",
    result: undefined as number | undefined,
    error_code: undefined as string | undefined,
  };
}
function state() {
  const a = artifact();
  const b = artifact("b");
  return {
    view: {
      available: true,
      generation_available: true,
      generation_model: "synthetic-generation-model",
      engine_version: "wasmi-2.0.0",
      policy_version: "pure-i32-v1",
      availability_code: null as string | null,
      jobs: [] as ReturnType<typeof job>[],
      artifacts: [a, b],
      policy: {
        min_cases: 2,
        max_cases: 8,
        max_spec_bytes: 4000,
        max_wat_bytes: 65536,
        max_wasm_bytes: 65536,
        fuel_per_call: 100000,
      },
    },
    details: { a, b },
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function setup(
  server = state(),
  handler?: (
    path: string,
    options?: RequestInit,
  ) => Response | Promise<Response> | undefined,
) {
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1/wasm-workbench", "");
    const custom = await handler?.(path, options);
    if (custom) return custom;
    if (!path) return response(server.view);
    if (path === "/artifacts/a") return response(server.details.a);
    if (path === "/artifacts/b") return response(server.details.b);
    if (path === "/generate") {
      server.view.jobs = [job()];
      return response(server.view.jobs[0], 202);
    }
    if (path === "/jobs/job-a" && options?.method !== "DELETE")
      return response(server.view.jobs[0]);
    if (path === "/artifacts/a/approve") {
      server.details.a.status = "approved";
      server.details.a.revision++;
      return response(server.details.a);
    }
    throw new Error(`Unexpected workbench request ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("workbench-csrf");
  wrapper = mount(WasmWorkbench);
  return fetch;
}
function button(text: string) {
  const found = wrapper!.findAll("button").find((item) => item.text() === text);
  if (!found) throw new Error(`Missing button ${text}`);
  return found;
}
function field(label: string) {
  return wrapper!.get(`[aria-label="${label}"]`);
}
async function setCase(index: number, a: string, b: string, expected: string) {
  await field(`测试 ${index} 输入 a`).setValue(a);
  await field(`测试 ${index} 输入 b`).setValue(b);
  await field(`测试 ${index} 预期输出`).setValue(expected);
}
async function fill() {
  await field("制品名称").setValue("加法制品");
  await field("计算需求").setValue("返回两个整数之和，遵循32位WASM回绕。");
  await setCase(1, "0", "0", "0");
  await setCase(2, "2147483647", "1", "-2147483648");
}
async function choose(id = "a") {
  await field(`查看制品 ${id === "a" ? "整数加法" : "整数比较"}`).trigger(
    "click",
  );
  await flushPromises();
}
async function generate() {
  await field("生成纯整数制品").trigger("submit");
  await flushPromises();
}
async function run() {
  await field("手动执行制品").trigger("submit");
  await flushPromises();
}
function writes(fetch: ReturnType<typeof setup>) {
  return fetch.mock.calls.filter(
    ([, options]) => options?.method && options.method !== "GET",
  );
}

describe("explicit pure-i32 WASM workbench", () => {
  it("keeps unavailable workers closed and never starts generation, execution or persistent storage from editing or viewing", async () => {
    const server = state();
    server.view.available = false;
    server.view.generation_available = false;
    server.view.availability_code = "/private/path/model-reference";
    const store = vi.spyOn(Storage.prototype, "setItem");
    const fetch = setup(server);
    await flushPromises();
    await fill();
    await choose();
    expect(wrapper!.text()).toContain("资源门禁尚不可用");
    expect(wrapper!.text()).toContain("审批前不得手动执行");
    expect(wrapper!.text()).not.toContain("/private/path/model-reference");
    expect(button("发送需求并生成").attributes("disabled")).toBeDefined();
    await field("手动执行输入 a").setValue("0");
    await field("手动执行输入 b").setValue("0");
    expect(button("手动执行已批准制品").attributes("disabled")).toBeDefined();
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toEqual([]);
    expect(store).not.toHaveBeenCalled();
  });

  it("validates UTF-8 limits, distinct tests and exact signed integer inputs without scientific or fractional coercion", async () => {
    const fetch = setup();
    await flushPromises();
    await fill();
    expect(button("发送需求并生成").attributes("disabled")).toBeUndefined();
    for (const invalid of ["2147483648", "-2147483649", "1.1", "1e2", ""]) {
      await field("测试 1 输入 a").setValue(invalid);
      expect(button("发送需求并生成").attributes("disabled")).toBeDefined();
    }
    await setCase(1, "0", "0", "0");
    await setCase(2, "0", "0", "1");
    expect(wrapper!.text()).toContain("两组不同的 a/b");
    await setCase(2, "1", "0", "1");
    await field("制品名称").setValue("界".repeat(27));
    expect(wrapper!.text()).toContain("1–80 UTF-8 字节");
    await field("制品名称").setValue("合格名称");
    await field("计算需求").setValue("界".repeat(1334));
    expect(wrapper!.text()).toContain("1–4,000 UTF-8 字节");
    await field("计算需求").setValue("sk-" + "a".repeat(24));
    expect(wrapper!.text()).toContain("疑似凭证");
    await fill();
    for (let count = 0; count < 6; count++)
      await button("添加测试").trigger("click");
    expect(wrapper!.findAll(".wasm-case")).toHaveLength(8);
    expect(button("添加测试").attributes("disabled")).toBeDefined();
    expect(writes(fetch)).toEqual([]);
  });

  it("sends only the explicit name/spec/test body once, preserving zero and overflow test expectations", async () => {
    const pending = deferred<Response>();
    const fetch = setup(undefined, (path) =>
      path === "/generate" ? pending.promise : undefined,
    );
    await flushPromises();
    await fill();
    await generate();
    await generate();
    expect(writes(fetch)).toHaveLength(1);
    const sent = JSON.parse(String(writes(fetch)[0]![1]!.body));
    expect(Object.keys(sent).sort()).toEqual([
      "client_request_id",
      "name",
      "spec",
      "tests",
    ]);
    expect(sent.tests).toEqual([
      { a: 0, b: 0, expected: 0 },
      { a: 2147483647, b: 1, expected: -2147483648 },
    ]);
    expect(writes(fetch)[0]![1]!.headers).toMatchObject({
      "X-CSRF-Token": "workbench-csrf",
    });
    pending.resolve(response(job(), 202));
    await flushPromises();
    expect(wrapper!.text()).toContain("未自动执行");
    expect(writes(fetch)).toHaveLength(1);
  });

  it("freezes uncertain generation bodies, reuses their UUID on original retry and uses a new UUID only for a later changed explicit submission", async () => {
    vi.mocked(crypto.randomUUID)
      .mockReturnValueOnce("47edff88-d861-4b38-a02d-ec8f70942f02")
      .mockReturnValueOnce("4893d124-c8d4-40f0-bfa1-b8b1ee71a48a");
    const fetch = setup(undefined, (path) => {
      if (path === "/generate")
        throw new Error("do-not-print-this-private-path");
      return undefined;
    });
    await flushPromises();
    await fill();
    await generate();
    expect(field("制品名称").element.closest("fieldset")!.disabled).toBe(true);
    expect(wrapper!.text()).not.toContain("do-not-print-this-private-path");
    await vi.advanceTimersByTimeAsync(10000);
    expect(writes(fetch)).toHaveLength(1);
    await button("刷新工坊").trigger("click");
    await flushPromises();
    await generate();
    const original = writes(fetch).map(([, options]) =>
      JSON.parse(String(options!.body)),
    );
    expect(original[1]).toEqual(original[0]);
    await button("刷新工坊").trigger("click");
    await flushPromises();
    await field("计算需求").setValue("用户明确修改后的新纯整数需求");
    expect(crypto.randomUUID).toHaveBeenCalledTimes(1);
    await generate();
    expect(JSON.parse(String(writes(fetch)[2]![1]!.body))).toMatchObject({
      client_request_id: "4893d124-c8d4-40f0-bfa1-b8b1ee71a48a",
      spec: "用户明确修改后的新纯整数需求",
    });
  });

  it("renders WAT as read-only escaped text and requires actual values to match expectations before approval", async () => {
    const server = state();
    server.details.a.test_results[0] = { actual: 1, passed: true };
    const fetch = setup(server);
    await flushPromises();
    await choose();
    expect(field("WAT 源码").text()).toContain(
      '<img src="https://example.invalid/never">',
    );
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(wrapper!.text()).toContain("测试实际值尚未逐项匹配预期");
    expect(wrapper!.get(".wasm-test-results li").text()).toContain(
      "未通过或尚未验证",
    );
    expect(button("核对并批准此制品").attributes("disabled")).toBeDefined();
    expect(writes(fetch)).toEqual([]);
  });

  it("requires a specific source/tests/hash confirmation and sends only the two hashes with the current artifact revision", async () => {
    const server = state();
    const fetch = setup(server);
    await flushPromises();
    await choose();
    expect(field("所选制品详情").text()).toContain("实际：0");
    await button("核对并批准此制品").trigger("click");
    const confirmation = field("确认制品授权操作");
    expect(confirmation.text()).toContain(server.details.a.wat);
    expect(confirmation.text()).toContain("wasmi-2.0.0");
    expect(confirmation.text()).toContain("pure-i32-v1");
    expect(confirmation.text()).toContain("a".repeat(64));
    expect(confirmation.text()).toContain("c".repeat(64));
    expect(confirmation.text()).toContain("预期=0；实际=0");
    expect(button("确认按两个哈希批准").attributes("disabled")).toBeDefined();
    expect(writes(fetch)).toEqual([]);
    await field("已核对源码测试及两个制品哈希").setValue(true);
    await button("确认按两个哈希批准").trigger("click");
    await flushPromises();
    expect(writes(fetch)).toHaveLength(1);
    expect(JSON.parse(String(writes(fetch)[0]![1]!.body))).toEqual({
      wasm_sha256: "a".repeat(64),
      tests_sha256: "c".repeat(64),
    });
    expect(writes(fetch)[0]![1]!.headers).toMatchObject({ "If-Match": '"1"' });
    expect(wrapper!.text()).toContain("未自动执行");
    expect(writes(fetch).some(([path]) => path.endsWith("/execute"))).toBe(
      false,
    );
  });

  it("does not resurrect execution permission when approval succeeds but reading its new detail fails", async () => {
    const server = state();
    let approved = false;
    const fetch = setup(server, (path) => {
      if (path.endsWith("/approve")) {
        approved = true;
        server.details.a.status = "approved";
        return response({});
      }
      if (approved && path === "/artifacts/a")
        return response(
          {
            error: { code: "worker_error", message: "/private/secret/command" },
          },
          503,
        );
      return undefined;
    });
    await flushPromises();
    await choose();
    await button("核对并批准此制品").trigger("click");
    await field("已核对源码测试及两个制品哈希").setValue(true);
    await button("确认按两个哈希批准").trigger("click");
    await flushPromises();
    expect(wrapper!.find('[aria-label="手动执行制品"]').exists()).toBe(false);
    expect(wrapper!.text()).not.toContain("/private/secret/command");
    expect(writes(fetch)).toHaveLength(1);
  });

  it("executes only an approved version on explicit valid inputs and displays the integer result zero", async () => {
    const server = state();
    server.details.a.status = "approved";
    server.details.a.revision = 4;
    const fetch = setup(server, (path) => {
      if (path.endsWith("/execute")) {
        server.view.jobs = [{ ...job("execute", "completed", 1), result: 0 }];
        return response(server.view.jobs[0], 202);
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    expect(writes(fetch)).toEqual([]);
    await field("手动执行输入 a").setValue("0");
    await field("手动执行输入 b").setValue("1e2");
    expect(button("手动执行已批准制品").attributes("disabled")).toBeDefined();
    await field("手动执行输入 b").setValue("0");
    await run();
    expect(JSON.parse(String(writes(fetch)[0]![1]!.body))).toEqual({
      client_request_id: "47edff88-d861-4b38-a02d-ec8f70942f02",
      a: 0,
      b: 0,
      wasm_sha256: "a".repeat(64),
    });
    expect(writes(fetch)[0]![1]!.headers).toMatchObject({ "If-Match": '"4"' });
    expect(field("整数计算结果").text()).toBe("结果：0");
  });

  it("keeps uncertain execution idempotent and generates a new request only after refresh and a changed explicit input", async () => {
    vi.mocked(crypto.randomUUID)
      .mockReturnValueOnce("47edff88-d861-4b38-a02d-ec8f70942f02")
      .mockReturnValueOnce("4893d124-c8d4-40f0-bfa1-b8b1ee71a48a");
    const server = state();
    server.details.a.status = "approved";
    const fetch = setup(server, (path) => {
      if (path.endsWith("/execute"))
        throw new Error("synthetic unknown result");
      return undefined;
    });
    await flushPromises();
    await choose();
    await field("手动执行输入 a").setValue("0");
    await field("手动执行输入 b").setValue("0");
    await run();
    expect(field("手动执行输入 a").element.closest("fieldset")!.disabled).toBe(
      true,
    );
    await button("刷新工坊").trigger("click");
    await flushPromises();
    await run();
    const bodies = writes(fetch).map(([, options]) =>
      JSON.parse(String(options!.body)),
    );
    expect(bodies[0]).toEqual(bodies[1]);
    await button("刷新工坊").trigger("click");
    await flushPromises();
    await field("手动执行输入 a").setValue("1");
    expect(crypto.randomUUID).toHaveBeenCalledTimes(1);
    await run();
    expect(JSON.parse(String(writes(fetch)[2]![1]!.body))).toMatchObject({
      client_request_id: "4893d124-c8d4-40f0-bfa1-b8b1ee71a48a",
      a: 1,
    });
  });

  it("allows explicit revocation even for an unsupported old engine and closes authority before a failed refresh", async () => {
    const server = state();
    server.details.a.status = "approved";
    server.details.a.engine_version = "unsupported-old-engine";
    server.view.jobs = [{ ...job("execute", "running"), result: 55 }];
    let revoked = false;
    const fetch = setup(server, (path) => {
      if (path.endsWith("/revoke")) {
        revoked = true;
        return response({});
      }
      if (!path && revoked) return response({ error: "unavailable" }, 503);
      return undefined;
    });
    await flushPromises();
    await choose();
    await button("撤销此制品授权").trigger("click");
    expect(field("确认制品授权操作").text()).toContain(
      "中断该制品排队或运行中的计算结果",
    );
    expect(field("确认制品授权操作").text()).toContain("不等于删除代码");
    await button("确认撤销并中断相关计算").trigger("click");
    await flushPromises();
    expect(writes(fetch)[0]![1]!.headers).toMatchObject({ "If-Match": '"1"' });
    expect(JSON.parse(String(writes(fetch)[0]![1]!.body))).toEqual({});
    expect(wrapper!.find('[aria-label="手动执行制品"]').exists()).toBe(false);
    expect(wrapper!.text()).toContain("授权已撤销");
    expect(wrapper!.text()).not.toContain("结果：55");
  });

  it("preserves the reviewed source on a 409 and refreshes before offering approval again without echoing server details", async () => {
    const server = state();
    const fetch = setup(server, (path) =>
      path.endsWith("/approve")
        ? response(
            {
              error: {
                code: "wasm_revision_conflict",
                message: "/private/system-command model-ref",
              },
            },
            409,
          )
        : undefined,
    );
    await flushPromises();
    await choose();
    await button("核对并批准此制品").trigger("click");
    await field("已核对源码测试及两个制品哈希").setValue(true);
    await button("确认按两个哈希批准").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("当前展示已保留");
    expect(field("WAT 源码").text()).toContain("独属于a的源码");
    expect(wrapper!.text()).not.toContain("/private/system-command");
    expect(button("核对并批准此制品").attributes("disabled")).toBeDefined();
    await vi.advanceTimersByTimeAsync(6000);
    expect(writes(fetch)).toHaveLength(1);
    server.details.a.revision = 5;
    await button("刷新工坊").trigger("click");
    await flushPromises();
    expect(button("核对并批准此制品").attributes("disabled")).toBeUndefined();
  });

  it("ignores an aborted detail response when another artifact is selected", async () => {
    const pending = deferred<Response>();
    let signal: AbortSignal | undefined;
    setup(undefined, (path, options) => {
      if (path === "/artifacts/a") {
        signal = options?.signal ?? undefined;
        return pending.promise;
      }
      return undefined;
    });
    await flushPromises();
    await choose();
    await choose("b");
    expect(signal?.aborted).toBe(true);
    pending.resolve(response(artifact()));
    await flushPromises();
    expect(field("WAT 源码").text()).toContain("独属于b的源码");
    expect(field("WAT 源码").text()).not.toContain("独属于a的源码");
  });

  it("clears private drafts, WAT and result bodies on 401 and stops polling", async () => {
    const server = state();
    server.view.jobs = [job("execute", "running")];
    const fetch = setup(server, (path) =>
      path.startsWith("/jobs/")
        ? response({ error: "unauthorized" }, 401)
        : undefined,
    );
    await flushPromises();
    await fill();
    await choose();
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    expect(wrapper!.text()).toContain("需求、测试、代码详情和结果已清除");
    expect(wrapper!.text()).not.toContain("独属于a的源码");
    expect(wrapper!.text()).not.toContain("明确生成的私有任务");
    const count = fetch.mock.calls.length;
    await vi.advanceTimersByTimeAsync(10000);
    expect(fetch).toHaveBeenCalledTimes(count);
  });

  it("bounds serial polling and aborts an outstanding request on unmount", async () => {
    const server = state();
    server.view.jobs = [{ ...job(), expires_at: expires(600) }];
    const fetch = setup(server);
    await flushPromises();
    for (let count = 0; count < 62; count++) {
      await vi.advanceTimersByTimeAsync(2000);
      await flushPromises();
    }
    expect(
      fetch.mock.calls.filter(([path]) => path.endsWith("/jobs/job-a")),
    ).toHaveLength(60);
    expect(wrapper!.text()).toContain("轮询已暂停");
    wrapper!.unmount();
    const pending = deferred<Response>();
    let signal: AbortSignal | undefined;
    const second = setup(server, (path, options) => {
      if (path.startsWith("/jobs/")) {
        signal = options?.signal ?? undefined;
        return pending.promise;
      }
      return undefined;
    });
    await flushPromises();
    await vi.advanceTimersByTimeAsync(6000);
    expect(
      second.mock.calls.filter(([path]) => path.endsWith("/jobs/job-a")),
    ).toHaveLength(1);
    wrapper!.unmount();
    expect(signal?.aborted).toBe(true);
    pending.resolve(response(job()));
    await flushPromises();
  });

  it("cancels a concrete task using its revision and clears zero results at expiry without retrying", async () => {
    const server = state();
    server.view.jobs = [job("execute", "running", 7)];
    const fetch = setup(server, (path, options) => {
      if (path === "/jobs/job-a" && options?.method === "DELETE") {
        server.view.jobs[0] = job("execute", "deleted", 8);
        return response(server.view.jobs[0]);
      }
      return undefined;
    });
    await flushPromises();
    await button("取消并清除此任务").trigger("click");
    expect(field("确认清除工坊任务").text()).toContain("明确生成的私有任务");
    await button("确认取消或清除本任务").trigger("click");
    await flushPromises();
    expect(writes(fetch)[0]![1]).toMatchObject({
      method: "DELETE",
      headers: expect.objectContaining({ "If-Match": '"7"' }),
    });
    server.view.jobs = [
      { ...job("execute", "completed"), result: 0, expires_at: expires(2) },
    ];
    await button("刷新工坊").trigger("click");
    await flushPromises();
    expect(field("整数计算结果").text()).toBe("结果：0");
    await vi.advanceTimersByTimeAsync(2001);
    expect(wrapper!.find('[aria-label="整数计算结果"]').exists()).toBe(false);
    expect(writes(fetch)).toHaveLength(1);
  });
});
