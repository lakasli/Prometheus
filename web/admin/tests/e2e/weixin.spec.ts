import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

// Browser-only QR/state fixtures exercise the local UI and real QR encoder.
// They do not verify a WeChat account, a real scan, or backend login/send APIs.
const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/weixin-e2e/data",
);
const accountFile = path.join(dataDir, "e2e-account.json");
const evidenceDir = path.resolve("../../.local/weixin-e2e/screenshots");

async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `weixin_${crypto.randomBytes(4).toString("hex")}`,
        password: crypto.randomBytes(24).toString("base64url"),
      };
  if (!fs.existsSync(accountFile))
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      mode: 0o600,
      flag: "wx",
    });
  const authStatus = await (
    await page.request.get("/api/admin/v1/auth/status")
  ).json();
  await page.goto("/admin/");
  if (!authStatus.initialized) {
    const pairing = JSON.parse(
      fs.readFileSync(path.join(dataDir, "bootstrap-token.json"), "utf8"),
    );
    await page.getByLabel("一次性配对凭证").fill(pairing.token);
    await page.getByLabel("管理员用户名").fill(account.username);
    await page
      .getByLabel("设置管理密码", { exact: true })
      .fill(account.password);
    await page.getByLabel("再次输入密码").fill(account.password);
    await page.getByRole("button", { name: "创建账户并进入控制台" }).click();
  } else {
    await page.getByLabel("管理员用户名").fill(account.username);
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  }
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
}

test("Weixin browser fixtures render only local QR images and keep login, receiver and probe states distinct", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(120_000);
  const origin = new URL(baseURL!).origin;
  expect(
    new URL(baseURL!).port,
    "fixture E2E must not use the production tunnel",
  ).not.toBe("18419");
  const failures: string[] = [];
  const consoleErrors: string[] = [];
  const externalRequests: string[] = [];
  const fixtureErrors: string[] = [];
  let authenticated = false;
  page.on("pageerror", () => failures.push("uncaught-browser-error"));
  page.on("console", (message) => {
    if (message.type() !== "error") return;
    if (
      !authenticated &&
      message.location().url === `${origin}/api/admin/v1/auth/me` &&
      /Failed to load resource.*401/.test(message.text())
    )
      return;
    consoleErrors.push(message.text());
  });
  await page.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (["http:", "https:"].includes(url.protocol) && url.origin !== origin) {
      externalRequests.push(url.hostname);
      await route.abort();
    } else await route.continue();
  });
  await authenticate(page);
  authenticated = true;
  const isolated = await browser.newContext();
  const unauthorized = await isolated.request.get(
    `${origin}/api/admin/v1/weixin`,
  );
  expect(unauthorized.status()).toBe(401);
  await isolated.close();
  const emptyResponse = await page.request.get("/api/admin/v1/weixin");
  expect(emptyResponse.status()).toBe(200);
  const originalBackend = await emptyResponse.json();
  expect(originalBackend.account).toBeNull();
  expect(originalBackend.pending_login).toBeNull();
  // Deliberately use disabling, never real login, for the missing-CSRF check.
  const forbidden = await page.request.post("/api/admin/v1/weixin/enabled", {
    headers: { Origin: origin },
    data: { enabled: false },
  });
  expect(forbidden.status()).toBe(403);

  type FixtureLogin = {
    id: string;
    status: string;
    revision: number;
    expires_at: number;
    qr_content?: string | null;
    error_code?: string | null;
  };
  const qrContent =
    "https://example.invalid/synthetic-weixin-qr-browser-fixture";
  const hiddenToken = "synthetic-weixin-token-never-render";
  const fixture = {
    revision: 0,
    account: null as null | {
      account_id: string;
      enabled: boolean;
      credential_configured: boolean;
      connection_status: string;
      last_error_code: null;
      last_event_at: null;
      bot_token: string;
    },
    login: null as FixtureLogin | null,
    check: null as null | { id: string; status: string; requested_at: number },
    creates: 0,
    deletes: 0,
    enables: 0,
    verificationBody: null as unknown,
  };
  let releaseVerification: (() => void) | null = null;
  const previousLogins = new Map<string, FixtureLogin>();
  const active = new Set([
    "creating",
    "waiting",
    "scanned",
    "need_verification",
    "verifying",
    "redirecting",
  ]);
  await page.route(/\/api\/admin\/v1\/weixin(?:[/?]|$)/, async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) {
      externalRequests.push(url.hostname);
      await route.abort();
      return;
    }
    const method = route.request().method();
    const endpoint = url.pathname.replace("/api/admin/v1", "");
    const json = (body: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    if (endpoint === "/weixin" && method === "GET") {
      return json({
        revision: fixture.revision,
        account: fixture.account,
        pending_login:
          fixture.login && active.has(fixture.login.status)
            ? {
                id: fixture.login.id,
                status: fixture.login.status,
                revision: fixture.login.revision,
                expires_at: fixture.login.expires_at,
              }
            : null,
        check: fixture.check,
        capabilities: {
          text_private: true,
          linked_owner_only: true,
          groups: false,
          media: false,
          max_accounts: 1,
        },
      });
    }
    if (endpoint === "/weixin/login" && method === "POST") {
      fixture.creates++;
      expect(route.request().postDataJSON()).toEqual({});
      expect(route.request().headers()["if-match"]).toBe(
        `"${fixture.revision}"`,
      );
      if (fixture.login)
        previousLogins.set(fixture.login.id, { ...fixture.login });
      fixture.login = {
        id: `fixture-job-${fixture.creates}`,
        status: "waiting",
        revision: 1,
        expires_at: Math.floor(Date.now() / 1000) + 300,
        qr_content: qrContent,
      };
      return json(fixture.login, 202);
    }
    if (
      fixture.login &&
      endpoint === `/weixin/login/${fixture.login.id}` &&
      method === "GET"
    )
      return json(fixture.login);
    if (method === "GET" && endpoint.startsWith("/weixin/login/")) {
      const previous = previousLogins.get(
        endpoint.slice("/weixin/login/".length),
      );
      if (previous) return json(previous);
    }
    if (
      fixture.login &&
      endpoint === `/weixin/login/${fixture.login.id}` &&
      method === "DELETE"
    ) {
      fixture.deletes++;
      expect(route.request().headers()["if-match"]).toBe(
        `"${fixture.login.revision}"`,
      );
      fixture.login = {
        ...fixture.login,
        status: "cancelled",
        revision: fixture.login.revision + 1,
        qr_content: null,
      };
      return json(fixture.login);
    }
    if (
      fixture.login &&
      endpoint === `/weixin/login/${fixture.login.id}/verification` &&
      method === "POST"
    ) {
      expect(route.request().headers()["if-match"]).toBe(
        `"${fixture.login.revision}"`,
      );
      fixture.verificationBody = route.request().postDataJSON();
      await new Promise<void>((resolve) => {
        releaseVerification = resolve;
      });
      fixture.login = {
        ...fixture.login,
        status: "verifying",
        revision: fixture.login.revision + 1,
        qr_content: null,
      };
      return json(fixture.login, 202);
    }
    if (endpoint === "/weixin/check" && method === "POST")
      return json({ id: "fixture-new-check", status: "pending" }, 202);
    if (
      endpoint === "/weixin/enabled" &&
      method === "POST" &&
      fixture.account
    ) {
      fixture.enables++;
      expect(route.request().headers()["if-match"]).toBe(
        `"${fixture.revision}"`,
      );
      fixture.revision++;
      fixture.account = {
        ...fixture.account,
        enabled: route.request().postDataJSON().enabled,
        connection_status: "disabled",
      };
      return json({ revision: fixture.revision, account: fixture.account });
    }
    fixtureErrors.push(`${method} ${endpoint}`);
    // Never forward an unhandled Weixin request to real backend login APIs.
    return json(
      { error: { code: "unhandled_fixture", message: "fixture only" } },
      501,
    );
  });
  await page.getByRole("button", { name: "微信连接", exact: true }).click();
  const workspace = page.locator(".weixin-workspace");
  await expect(
    workspace.getByRole("heading", { name: "微信连接", exact: true }),
  ).toBeVisible();
  await expect(workspace).toContainText("群聊与媒体尚未支持");
  await workspace
    .getByRole("button", { name: "开始微信扫码", exact: true })
    .click();
  const qr = workspace.getByRole("img", {
    name: "微信扫码授权二维码",
    exact: true,
  });
  await expect(qr).toBeVisible();
  expect(await qr.getAttribute("src")).toMatch(/^data:image\/png;base64,/);
  expect(
    await qr.evaluate(
      (image) =>
        (image as HTMLImageElement).complete &&
        (image as HTMLImageElement).naturalWidth > 0,
    ),
  ).toBe(true);
  expect(await page.content()).not.toContain(qrContent);
  expect(await page.content()).not.toContain(hiddenToken);
  expect(await workspace.locator("a").count()).toBe(0);
  fs.mkdirSync(evidenceDir, { recursive: true });
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await workspace.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await workspace.screenshot({
      path: path.join(evidenceDir, `weixin-fixture-${width}.png`),
      animations: "disabled",
    });
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  fixture.login = {
    ...fixture.login!,
    status: "need_verification",
    revision: 2,
    qr_content: null,
  };
  await workspace
    .getByRole("button", { name: "刷新微信状态", exact: true })
    .click();
  const code = workspace.getByLabel("微信配对数字", { exact: true });
  await expect(code).toBeVisible();
  await expect(code).toHaveAttribute("type", "password");
  await expect(code).toHaveAttribute("inputmode", "numeric");
  await expect(code).toHaveAttribute("autocomplete", "off");
  await code.fill("654321");
  await workspace
    .getByRole("button", { name: "提交配对数字", exact: true })
    .click();
  await expect(code).toHaveValue("");
  expect(fixture.verificationBody).toEqual({ code: "654321" });
  expect(releaseVerification).not.toBeNull();
  (releaseVerification as unknown as () => void)();
  await expect(workspace.getByTestId("weixin-login-status")).toContainText(
    "正在核对配对数字",
  );
  await expect(qr).toHaveCount(0);
  fixture.login = {
    ...fixture.login!,
    status: "confirmed",
    revision: 4,
    qr_content: null,
  };
  fixture.account = {
    account_id: "fixture-linked-account",
    enabled: true,
    credential_configured: true,
    connection_status: "connecting",
    last_error_code: null,
    last_event_at: null,
    bot_token: hiddenToken,
  };
  fixture.revision = 1;
  await expect(workspace.getByTestId("weixin-login-status")).toContainText(
    "扫码凭据已保存",
  );
  await expect(workspace.getByTestId("weixin-connection-status")).toHaveText(
    "正在连接私聊接收",
  );
  await workspace
    .getByRole("button", { name: "重新扫码授权", exact: true })
    .click();
  await expect(qr).toBeVisible();
  fixture.login = {
    ...fixture.login!,
    status: "existing_credentials",
    revision: 2,
    qr_content: null,
  };
  await expect(workspace.getByTestId("weixin-login-status")).toContainText(
    "已有凭据",
  );
  await expect(workspace.getByTestId("weixin-connection-status")).toHaveText(
    "正在连接私聊接收",
  );
  fixture.account.connection_status = "connected";
  fixture.check = {
    id: "fixture-old-check",
    status: "passed",
    requested_at: Math.floor(Date.now() / 1000),
  };
  await workspace
    .getByRole("button", { name: "刷新微信状态", exact: true })
    .click();
  await expect(workspace.getByTestId("weixin-connection-status")).toHaveText(
    "私聊接收已连接",
  );
  await workspace
    .getByRole("button", { name: "检查微信连接", exact: true })
    .click();
  const check = workspace.locator('[aria-label="微信连接检查"]');
  await expect(check).toContainText("本次检查正在进行");
  await expect(check).not.toContainText("本次连接检查通过");
  fixture.check = {
    id: "fixture-new-check",
    status: "passed",
    requested_at: Math.floor(Date.now() / 1000),
  };
  await workspace
    .getByRole("button", { name: "刷新微信状态", exact: true })
    .click();
  await expect(check).toContainText("本次连接检查通过");
  await workspace
    .getByRole("button", { name: "重新扫码授权", exact: true })
    .click();
  await expect(qr).toBeVisible();
  fixture.login = {
    ...fixture.login!,
    revision: 2,
    expires_at: Math.floor(Date.now() / 1000) + 1,
  };
  await workspace
    .getByRole("button", { name: "刷新微信状态", exact: true })
    .click();
  await expect(workspace.getByTestId("weixin-login-status")).toContainText(
    "登录任务已过期",
  );
  await expect(qr).toHaveCount(0);
  const creationsAtExpiry = fixture.creates;
  await page.waitForTimeout(2200);
  expect(fixture.creates).toBe(creationsAtExpiry);
  await workspace
    .getByRole("button", { name: "重新扫码授权", exact: true })
    .click();
  await expect(qr).toBeVisible();
  await workspace
    .getByRole("button", { name: "取消当前登录", exact: true })
    .click();
  await expect(qr).toHaveCount(0);
  await expect(workspace.getByTestId("weixin-login-status")).toContainText(
    "登录任务已取消",
  );
  expect(fixture.deletes).toBe(1);
  await workspace
    .getByRole("button", { name: "停用微信", exact: true })
    .click();
  const confirmation = workspace.locator('[aria-label="确认停用微信"]');
  await expect(confirmation).toContainText("正在发送的结果可能未知");
  await expect(confirmation).toContainText("已有消息和数据不会删除");
  expect(fixture.enables).toBe(0);
  await confirmation
    .getByRole("button", { name: "确认停用微信", exact: true })
    .click();
  await expect(workspace.getByTestId("weixin-connection-status")).toHaveText(
    "已停用",
  );
  expect(fixture.enables).toBe(1);
  expect(await page.content()).not.toContain(qrContent);
  expect(await page.content()).not.toContain(hiddenToken);
  expect(await page.content()).not.toContain("654321");
  // APIRequestContext bypasses page.route; the actual server still has no
  // WeChat account or login task after every fixture-only browser operation.
  const untouched = await (
    await page.request.get("/api/admin/v1/weixin")
  ).json();
  expect(untouched.account).toBeNull();
  expect(untouched.pending_login).toBeNull();
  expect(untouched.revision).toBe(originalBackend.revision);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(fixtureErrors).toEqual([]);
  expect(externalRequests).toEqual([]);
  expect(failures).toEqual([]);
  expect(consoleErrors).toEqual([]);
});
