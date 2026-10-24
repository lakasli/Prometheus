// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import WeixinChannel from "../src/components/WeixinChannel.vue";
import { clearCsrfToken, setCsrfToken } from "../src/api";

const { encodeQr } = vi.hoisted(() => ({ encodeQr: vi.fn() }));
vi.mock("qrcode", () => ({ default: { toDataURL: encodeQr } }));
let wrapper: VueWrapper | undefined;
beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-10-24T10:00:00Z"));
  encodeQr.mockReset().mockResolvedValue("data:image/png;base64,QUJD");
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
function job(status = "waiting", revision = 1) {
  return {
    id: "job-1",
    status,
    revision,
    expires_at: Math.floor(Date.now() / 1000) + 120,
    error_code: null,
    qr_content: "synthetic-local-qr-content" as string | null | undefined,
  };
}
function channel(account: Record<string, unknown> | null = null) {
  return {
    revision: account ? 3 : 0,
    account,
    pending_login: null as ReturnType<typeof job> | null,
    check: null as Record<string, unknown> | null,
    capabilities: {
      text_private: true,
      linked_owner_only: true,
      groups: false,
      media: false,
      max_accounts: 1,
    },
  };
}
function account(status = "connecting") {
  return {
    account_id: "synthetic-account",
    enabled: true,
    credential_configured: true,
    connection_status: status,
    last_error_code: null,
    last_event_at: null,
    bot_token: "never-render-this-token",
  };
}
function model(snapshot = channel()) {
  return { snapshot, login: job() };
}
function setup(
  server = model(),
  handler?: (
    path: string,
    options?: RequestInit,
  ) => Response | Promise<Response> | undefined,
) {
  const fetch = vi.fn(async (input: string, options?: RequestInit) => {
    const path = input.replace("/api/admin/v1", "");
    const custom = await handler?.(path, options);
    if (custom) return custom;
    if (path === "/weixin") return response(server.snapshot);
    if (path === "/weixin/login" && options?.method === "POST") {
      server.snapshot.pending_login = server.login;
      return response(server.login, 202);
    }
    if (path === "/weixin/login/job-1" && options?.method === "DELETE") {
      server.login = {
        ...server.login,
        status: "cancelled",
        revision: server.login.revision + 1,
        qr_content: null,
      };
      server.snapshot.pending_login = null;
      return response(server.login);
    }
    if (path === "/weixin/login/job-1") return response(server.login);
    if (
      path === "/weixin/login/job-1/verification" &&
      options?.method === "POST"
    ) {
      server.login = {
        ...server.login,
        status: "verifying",
        revision: server.login.revision + 1,
        qr_content: null,
      };
      server.snapshot.pending_login = server.login;
      return response(server.login, 202);
    }
    if (path === "/weixin/enabled" && options?.method === "POST") {
      server.snapshot.revision++;
      server.snapshot.account = {
        ...server.snapshot.account,
        enabled: JSON.parse(String(options.body)).enabled,
      };
      return response({
        revision: server.snapshot.revision,
        account: server.snapshot.account,
      });
    }
    if (path === "/weixin/check" && options?.method === "POST")
      return response({ id: "check-new", status: "pending" }, 202);
    throw new Error(`Unexpected Weixin request: ${options?.method} ${path}`);
  });
  vi.stubGlobal("fetch", fetch);
  setCsrfToken("weixin-test-csrf");
  wrapper = mount(WeixinChannel);
  return fetch;
}
function button(text: string) {
  const result = wrapper!
    .findAll("button")
    .find((entry) => entry.text() === text);
  if (!result) throw new Error(`Missing button ${text}`);
  return result;
}
async function start() {
  await button("开始微信扫码").trigger("click");
  await flushPromises();
}

describe("Weixin private text channel", () => {
  it("shows expired credentials distinctly from a user stop and preserves delivery warnings", async () => {
    const snapshot = {
      ...channel({ ...account("needs_login"), enabled: false }),
      delivery: { queued: 2, sending: 0, accepted: 3, failed: 1, unknown: 4 },
    };
    setup(model(snapshot));
    await flushPromises();
    expect(wrapper!.get('[data-testid="weixin-connection-status"]').text()).toBe("凭据失效，需要重新扫码");
    expect(wrapper!.text()).toContain("待发 2");
    expect(wrapper!.text()).toContain("平台接受 3");
    expect(wrapper!.text()).toContain("结果未知 4");
    expect(wrapper!.text()).toContain("不会自动重试");
    expect(button("启用微信").attributes("disabled")).toBeDefined();
  });

  it("treats blocked verification as terminal and permits a later manual login", async () => {
    const server = model();
    setup(server);
    await flushPromises();
    await start();
    server.login = { ...job("verification_blocked", 2), qr_content: null };
    server.snapshot.pending_login = null;
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    expect(wrapper!.text()).toContain("请稍后手动重新扫码");
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(wrapper!.findAll("button").some((entry) => entry.text() === "取消当前登录")).toBe(false);
    expect(button("开始微信扫码").attributes("disabled")).toBeUndefined();
  });

  it("encodes a synthetic QR locally without links, remote requests, storage or token rendering", async () => {
    const server = model();
    const fetch = setup(server);
    const storage = vi.spyOn(Storage.prototype, "setItem");
    await flushPromises();
    await start();
    expect(encodeQr).toHaveBeenCalledWith(
      "synthetic-local-qr-content",
      expect.objectContaining({ width: 240 }),
    );
    expect(
      wrapper!.get('img[alt="微信扫码授权二维码"]').attributes("src"),
    ).toBe("data:image/png;base64,QUJD");
    expect(wrapper!.html()).not.toContain("synthetic-local-qr-content");
    expect(wrapper!.find("a").exists()).toBe(false);
    expect(
      fetch.mock.calls.every(([url]) => url.startsWith("/api/admin/v1/weixin")),
    ).toBe(true);
    expect(storage).not.toHaveBeenCalled();
    expect(wrapper!.text()).toContain("群聊与媒体尚未支持");
    const creation = fetch.mock.calls.find(
      ([, options]) => options?.method === "POST",
    )!;
    expect(creation[1]?.headers).toMatchObject({
      "If-Match": '"0"',
      "X-CSRF-Token": "weixin-test-csrf",
    });
    expect(JSON.parse(String(creation[1]?.body))).toEqual({});
  });

  it("clears the QR at expiry and never automatically creates a replacement login", async () => {
    const server = model();
    server.login.expires_at = Math.floor(Date.now() / 1000) + 1;
    const fetch = setup(server);
    await flushPromises();
    await start();
    expect(wrapper!.find("img").exists()).toBe(true);
    await vi.advanceTimersByTimeAsync(1100);
    await flushPromises();
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(wrapper!.text()).toContain("登录任务已过期，请手动重新扫码");
    await vi.advanceTimersByTimeAsync(15_000);
    await flushPromises();
    expect(
      fetch.mock.calls.filter(
        ([url, options]) =>
          url.endsWith("/weixin/login") && options?.method === "POST",
      ),
    ).toHaveLength(1);
  });

  it("keeps verification input private, preserves typing during polling and clears it on submission", async () => {
    const server = model();
    server.login = job("need_verification", 7);
    server.snapshot.pending_login = server.login;
    let release: (value: Response) => void = () => {};
    const submitted = new Promise<Response>((resolve) => {
      release = resolve;
    });
    const fetch = setup(server, (path) =>
      path.endsWith("/verification") ? submitted : undefined,
    );
    await flushPromises();
    const input = wrapper!.get('input[aria-label="微信配对数字"]');
    expect(input.attributes()).toMatchObject({
      type: "password",
      inputmode: "numeric",
      autocomplete: "off",
      maxlength: "16",
    });
    expect(
      wrapper!.get(`[id="${input.attributes("aria-describedby")}"]`).text(),
    ).toContain("离开页面后立即清空");
    await input.setValue("12x");
    await wrapper!.get("form").trigger("submit");
    expect(wrapper!.text()).toContain("1 到 16 位数字");
    expect((input.element as HTMLInputElement).value).toBe("");
    await input.setValue("123456");
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    expect((input.element as HTMLInputElement).value).toBe("123456");
    await wrapper!.get("form").trigger("submit");
    await flushPromises();
    expect((input.element as HTMLInputElement).value).toBe("");
    const mutation = fetch.mock.calls.find(([path]) =>
      path.endsWith("/verification"),
    )!;
    expect(mutation[1]?.headers).toMatchObject({ "If-Match": '"7"' });
    expect(JSON.parse(String(mutation[1]?.body))).toEqual({ code: "123456" });
    server.login = {
      ...server.login,
      status: "verifying",
      revision: 8,
      qr_content: null,
    };
    server.snapshot.pending_login = server.login;
    release(response(server.login, 202));
    await flushPromises();
    expect(wrapper!.html()).not.toContain("123456");
    expect(wrapper!.find("img").exists()).toBe(false);
  });

  it("does not restore a late QR encoding after cancellation", async () => {
    let release: (value: string) => void = () => {};
    encodeQr.mockImplementation(
      () =>
        new Promise<string>((resolve) => {
          release = resolve;
        }),
    );
    const server = model();
    const fetch = setup(server);
    await flushPromises();
    await start();
    await button("取消当前登录").trigger("click");
    await flushPromises();
    release("data:image/png;base64,TEFURQ==");
    await flushPromises();
    expect(wrapper!.find("img").exists()).toBe(false);
    expect(wrapper!.text()).toContain("登录任务已取消");
    const deletion = fetch.mock.calls.find(
      ([, options]) => options?.method === "DELETE",
    )!;
    expect(deletion[1]?.headers).toMatchObject({ "If-Match": '"1"' });
  });

  it("aborts requests and discards late QR results after leaving the page", async () => {
    const server = model();
    server.snapshot.pending_login = server.login;
    let release: (value: Response) => void = () => {};
    const late = new Promise<Response>((resolve) => {
      release = resolve;
    });
    const fetch = setup(server, (path) =>
      path === "/weixin/login/job-1" ? late : undefined,
    );
    await flushPromises();
    const request = fetch.mock.calls.find(([url]) => url.endsWith("/job-1"))!;
    wrapper!.unmount();
    wrapper = undefined;
    expect(request[1]?.signal?.aborted).toBe(true);
    release(response(server.login));
    await flushPromises();
    await vi.advanceTimersByTimeAsync(15_000);
    expect(encodeQr).not.toHaveBeenCalled();
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("distinguishes confirmed or existing credentials from a connected receiver", async () => {
    for (const loginStatus of ["confirmed", "existing_credentials"]) {
      const server = model(channel(account()));
      server.login = job(loginStatus);
      server.snapshot.pending_login = server.login;
      setup(server);
      await flushPromises();
      expect(
        wrapper!.get('[data-testid="weixin-connection-status"]').text(),
      ).toBe("正在连接私聊接收");
      expect(wrapper!.text()).toContain(
        loginStatus === "confirmed" ? "扫码凭据已保存" : "已有凭据",
      );
      expect(wrapper!.html()).not.toContain("never-render-this-token");
      expect(wrapper!.find("img").exists()).toBe(false);
      server.snapshot.account!.connection_status = "connected";
      await vi.advanceTimersByTimeAsync(5000);
      await flushPromises();
      expect(
        wrapper!.get('[data-testid="weixin-connection-status"]').text(),
      ).toBe("私聊接收已连接");
      wrapper!.unmount();
      wrapper = undefined;
    }
  });

  it("waits for the newly requested check ID even while the account remains connected", async () => {
    const server = model(channel(account("connected")));
    server.snapshot.check = {
      id: "check-old",
      status: "passed",
      requested_at: 1792830000,
    };
    setup(server);
    await flushPromises();
    await button("检查微信连接").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("本次检查正在进行");
    expect(wrapper!.text()).not.toContain("本次连接检查通过");
    server.snapshot.check = {
      id: "check-new",
      status: "passed",
      requested_at: 1792830000,
      completed_at: 1792830060,
    };
    await vi.advanceTimersByTimeAsync(5000);
    await flushPromises();
    expect(wrapper!.text()).toContain("本次连接检查通过");
  });

  it("requires a refresh after a version conflict and preserves the actual account state", async () => {
    const server = model(channel(account("connected")));
    setup(server, (path, options) =>
      path === "/weixin/login" && options?.method === "POST"
        ? response(
            { error: { code: "weixin_revision_conflict", message: "stale" } },
            409,
          )
        : undefined,
    );
    await flushPromises();
    await button("重新扫码授权").trigger("click");
    await flushPromises();
    expect(wrapper!.text()).toContain("当前状态已保留，请先刷新微信状态");
    expect(wrapper!.text()).toContain("synthetic-account");
    expect(button("重新扫码授权").attributes()).toHaveProperty("disabled");
    await button("刷新微信状态").trigger("click");
    await flushPromises();
    expect(button("重新扫码授权").attributes()).not.toHaveProperty("disabled");
  });

  it("never overlaps a slow account poll and a disable action explains uncertain sends", async () => {
    const server = model(channel(account("connected")));
    let calls = 0;
    let release: (value: Response) => void = () => {};
    const slow = new Promise<Response>((resolve) => {
      release = resolve;
    });
    const fetch = setup(server, (path) =>
      path === "/weixin" && ++calls === 2 ? slow : undefined,
    );
    await flushPromises();
    await vi.advanceTimersByTimeAsync(5000);
    await flushPromises();
    await vi.advanceTimersByTimeAsync(4000);
    await flushPromises();
    expect(calls).toBe(2);
    release(response(server.snapshot));
    await flushPromises();
    await button("停用微信").trigger("click");
    expect(wrapper!.get('[aria-label="确认停用微信"]').text()).toContain(
      "正在发送的结果可能未知",
    );
    expect(
      fetch.mock.calls.filter(([path]) => path.endsWith("/enabled")),
    ).toHaveLength(0);
    await button("确认停用微信").trigger("click");
    await flushPromises();
    expect(
      wrapper!.get('[data-testid="weixin-connection-status"]').text(),
    ).toBe("已停用");
  });

  it("never overlaps slow QR polls and drops the QR when access is revoked", async () => {
    const server = model();
    server.snapshot.pending_login = server.login;
    let qrCalls = 0;
    let release: (value: Response) => void = () => {};
    const slow = new Promise<Response>((resolve) => {
      release = resolve;
    });
    setup(server, (path) => {
      if (path !== "/weixin/login/job-1") return;
      qrCalls++;
      if (qrCalls === 2) return slow;
      if (qrCalls === 3)
        return response(
          { error: { code: "unauthorized", message: "请登录" } },
          401,
        );
    });
    await flushPromises();
    expect(wrapper!.find("img").exists()).toBe(true);
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    await vi.advanceTimersByTimeAsync(4000);
    await flushPromises();
    expect(qrCalls).toBe(2);
    release(response(server.login));
    await flushPromises();
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    expect(qrCalls).toBe(3);
    expect(wrapper!.find("img").exists()).toBe(false);
    await vi.advanceTimersByTimeAsync(4000);
    await flushPromises();
    expect(qrCalls).toBe(3);
  });
});
