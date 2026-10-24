import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError, clearCsrfToken, setCsrfToken } from "../src/api";

afterEach(() => {
  clearCsrfToken();
  vi.unstubAllGlobals();
});

describe("authenticated API requests", () => {
  it("binds a mutation to the in-memory CSRF token and expected configuration revision", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(new Response("{}", { status: 202 }));
    vi.stubGlobal("fetch", fetch);
    setCsrfToken("test-csrf");
    await api("/config/drafts/test/apply", { method: "POST", revision: 3 });
    expect(fetch).toHaveBeenCalledWith(
      "/api/admin/v1/config/drafts/test/apply",
      expect.objectContaining({
        credentials: "same-origin",
        cache: "no-store",
        headers: expect.objectContaining({
          "X-CSRF-Token": "test-csrf",
          "If-Match": '"3"',
        }),
      }),
    );
  });
  it("does not reuse a CSRF token after logout", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response("{}"));
    vi.stubGlobal("fetch", fetch);
    setCsrfToken("old-token");
    clearCsrfToken();
    await api("/auth/login", {
      method: "POST",
      body: { username: "admin", password: "not-a-real-password" },
    });
    expect(fetch.mock.calls[0]![1].headers).not.toHaveProperty("X-CSRF-Token");
  });
  it("explains a version conflict instead of claiming a save succeeded", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(
          new Response(
            JSON.stringify({ error: { code: "conflict", message: "stale" } }),
            { status: 409 },
          ),
        ),
    );
    await expect(
      api("/config/drafts/a/apply", { method: "POST", revision: 1 }),
    ).rejects.toMatchObject({
      status: 409,
      message: expect.stringContaining("重新载入"),
    });
  });
  it("treats an unauthenticated API response as failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("{}", { status: 401 })),
    );
    await expect(api("/config")).rejects.toBeInstanceOf(ApiError);
  });
});
