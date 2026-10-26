import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { selectValue } from "./select-helpers";

const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/memory-e2e",
);
const accountFile = path.join(dataDir, "e2e-account.json");
const evidenceDir = path.resolve("../../.local/summary-e2e/screenshots");

async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `summary_${crypto.randomBytes(4).toString("hex")}`,
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

test("conversation summary management rebuilds derived data with an opaque version and retains source boundaries", async ({
  page,
  baseURL,
}) => {
  test.setTimeout(120_000);
  const origin = new URL(baseURL!).origin;
  expect(
    new URL(baseURL!).port,
    "summary E2E must not target the production SSH tunnel",
  ).not.toBe("18419");
  let authenticated = false;
  let allowExpectedConflict = false;
  let sentMessages = 0;
  const pageFailures: string[] = [];
  const consoleErrors: string[] = [];
  const externalRequests: string[] = [];
  const summaryRequests: string[] = [];
  page.on("pageerror", () => pageFailures.push("uncaught-browser-error"));
  page.on("console", (message) => {
    if (message.type() !== "error") return;
    if (
      !authenticated &&
      message.location().url === `${origin}/api/admin/v1/auth/me` &&
      /Failed to load resource.*401/.test(message.text())
    )
      return;
    if (
      allowExpectedConflict &&
      /\/summary\/rebuild$/.test(message.location().url) &&
      /Failed to load resource.*409/.test(message.text())
    )
      return;
    consoleErrors.push(message.text());
  });
  page.on("request", (request) => {
    if (
      /^https?:/.test(request.url()) &&
      !request.url().startsWith(origin + "/")
    )
      externalRequests.push(new URL(request.url()).hostname);
    if (
      request.method() === "POST" &&
      /\/conversations\/[^/]+\/messages$/.test(request.url())
    )
      sentMessages++;
    if (/\/conversations\/[^/]+\/summary$/.test(request.url()))
      summaryRequests.push(request.url());
  });
  await authenticate(page);
  authenticated = true;
  const auth = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const mutationHeaders = { Origin: origin, "X-CSRF-Token": auth.csrf_token };
  const suffix = crypto.randomBytes(4).toString("hex");
  const firstResponse = await page.request.post("/api/admin/v1/conversations", {
    headers: mutationHeaders,
    data: { title: `摘要会话甲_${suffix}` },
  });
  expect(firstResponse.ok()).toBe(true);
  const first = await firstResponse.json();
  const secondResponse = await page.request.post(
    "/api/admin/v1/conversations",
    { headers: mutationHeaders, data: { title: `摘要会话乙_${suffix}` } },
  );
  expect(secondResponse.ok()).toBe(true);
  const second = await secondResponse.json();
  const firstPath = `/api/admin/v1/conversations/${encodeURIComponent(first.id)}/summary`;
  const secondPath = `/api/admin/v1/conversations/${encodeURIComponent(second.id)}/summary`;
  // Only the draft list entry is a UI fixture; all summary bodies and mutations
  // come from the real isolated backend. This fixture is never queried.
  const draftId = `draft-summary-fixture-${suffix}`;
  await page.route("**/api/admin/v1/conversations", async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    const actual = await route.fetch();
    const body = await actual.json();
    await route.fulfill({
      response: actual,
      json: {
        ...body,
        items: [
          ...body.items,
          {
            id: draftId,
            title: "仅用于筛选断言的角色草稿",
            is_test: true,
            created_at: 1792830000,
            character: { is_test: true },
          },
        ],
      },
    });
  });
  await page.getByRole("button", { name: "记忆", exact: true }).click();
  const panel = page.getByRole("region", { name: "会话摘要", exact: true });
  await expect(
    panel.getByRole("heading", { name: "长会话摘要", exact: true }),
  ).toBeVisible();
  const selector = panel.getByLabel("查看摘要的会话", { exact: true });
  await selector.click();
  const options = page.getByRole("listbox");
  await expect(options.locator(`[data-value="${first.id}"]`)).toHaveCount(1);
  await expect(options.locator(`[data-value="${second.id}"]`)).toHaveCount(1);
  await expect(options.locator(`[data-value="${draftId}"]`)).toHaveCount(0);
  const firstGet = page.waitForResponse(
    (response) =>
      response.url().endsWith(firstPath) &&
      response.request().method() === "GET",
  );
  await selectValue(page, selector, first.id);
  const initial = await (await firstGet).json();
  expect(initial).toMatchObject({ conversation_id: first.id, summary: null });
  expect(typeof initial.revision).toBe("string");
  await expect(panel.locator(".summary-current-heading")).toContainText(
    `摘要会话甲_${suffix}`,
  );
  await expect(panel.locator(".summary-empty")).toContainText(
    "此会话尚无派生摘要",
  );
  const messagesPath = `/api/admin/v1/conversations/${encodeURIComponent(first.id)}/messages`;
  const messagesBefore = await (await page.request.get(messagesPath)).json();
  await panel.getByRole("button", { name: "重建摘要", exact: true }).click();
  const confirmation = panel.getByRole("region", {
    name: "确认重建摘要",
    exact: true,
  });
  await expect(confirmation).toContainText(`摘要会话甲_${suffix}`);
  await expect(confirmation).toContainText(
    "清除当前派生摘要，下次普通消息时重建，原始消息保留",
  );
  const acceptedResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith(`${firstPath}/rebuild`) &&
      response.request().method() === "POST",
  );
  await confirmation
    .getByRole("button", { name: "确认清除派生摘要并安排重建", exact: true })
    .click();
  const accepted = await acceptedResponse;
  expect(accepted.status()).toBe(202);
  expect(accepted.request().headers()["if-match"]).toBe(
    `"${initial.revision}"`,
  );
  expect(accepted.request().postDataJSON()).toEqual({});
  const pending = await accepted.json();
  expect(pending.status).toBe("pending_next_run");
  expect(typeof pending.revision).toBe("string");
  expect(pending.revision).not.toBe(initial.revision);
  await expect(panel.locator(".alert.success")).toContainText(
    "下次普通消息时重建；原始消息保留",
  );
  await expect(
    panel.getByRole("button", { name: "重建摘要", exact: true }),
  ).toBeEnabled();
  const afterFirst = await (await page.request.get(firstPath)).json();
  expect(afterFirst.revision).toBe(pending.revision);
  const concurrent = await page.request.post(`${firstPath}/rebuild`, {
    headers: { ...mutationHeaders, "If-Match": `"${afterFirst.revision}"` },
    data: {},
  });
  expect(concurrent.status()).toBe(202);
  const concurrentBody = await concurrent.json();
  expect(concurrentBody.revision).not.toBe(afterFirst.revision);
  allowExpectedConflict = true;
  await panel.getByRole("button", { name: "重建摘要", exact: true }).click();
  const conflictResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith(`${firstPath}/rebuild`) &&
      response.request().method() === "POST",
  );
  await panel
    .getByRole("button", { name: "确认清除派生摘要并安排重建", exact: true })
    .click();
  expect((await conflictResponse).status()).toBe(409);
  await expect(panel.getByRole("alert")).toContainText(
    "当前展示已保留，请刷新摘要",
  );
  await expect(panel.locator(".summary-current-heading")).toContainText(
    `摘要会话甲_${suffix}`,
  );
  await expect(
    panel.getByRole("button", { name: "重建摘要", exact: true }),
  ).toBeDisabled();
  const refreshResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith(firstPath) &&
      response.request().method() === "GET",
  );
  await panel.getByRole("button", { name: "刷新摘要", exact: true }).click();
  expect((await (await refreshResponse).json()).revision).toBe(
    concurrentBody.revision,
  );
  await expect(
    panel.getByRole("button", { name: "重建摘要", exact: true }),
  ).toBeEnabled();
  const secondGet = page.waitForResponse(
    (response) =>
      response.url().endsWith(secondPath) &&
      response.request().method() === "GET",
  );
  await selectValue(page, selector, second.id);
  const secondView = await (await secondGet).json();
  expect(secondView).toMatchObject({
    conversation_id: second.id,
    summary: null,
  });
  expect(secondView.revision).not.toBe(concurrentBody.revision);
  await expect(panel.locator(".summary-current-heading")).toContainText(
    `摘要会话乙_${suffix}`,
  );
  await expect(panel.locator(".summary-current-heading")).not.toContainText(
    `摘要会话甲_${suffix}`,
  );
  expect(summaryRequests.some((url) => url.includes(draftId))).toBe(false);
  expect(await (await page.request.get(messagesPath)).json()).toEqual(
    messagesBefore,
  );
  fs.mkdirSync(evidenceDir, { recursive: true });
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await panel.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await panel.screenshot({
      path: path.join(evidenceDir, `summary-${width}.png`),
      animations: "disabled",
    });
  }
  expect(sentMessages).toBe(0);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(pageFailures).toEqual([]);
  expect(consoleErrors).toEqual([]);
  expect(externalRequests).toEqual([]);
});
