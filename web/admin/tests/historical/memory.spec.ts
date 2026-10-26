import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { selectValue } from "../e2e/select-helpers";

const dataDir = path.resolve(
  process.env.DSH_MEMORY_E2E_DATA_DIR ||
    process.env.DSH_E2E_DATA_DIR ||
    "../../.local/memory-e2e",
);
const evidenceDir = path.resolve("../../.local/memory-e2e/screenshots");
const accountFile = path.join(dataDir, "e2e-account.json");

async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `memory_${crypto.randomBytes(4).toString("hex")}`,
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

async function screenshot(page: Page, name: string) {
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.evaluate(() => document.fonts.ready);
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, name),
    fullPage: true,
    animations: "disabled",
  });
}

test("assistant memories retain scope, survive session navigation and support conflict-safe correction", async ({
  page,
  baseURL,
}) => {
  test.setTimeout(120_000);
  const origin = new URL(baseURL!).origin;
  expect(
    new URL(baseURL!).port,
    "memory E2E must not target the production SSH tunnel",
  ).not.toBe("18419");
  const failures: string[] = [];
  const consoleErrors: string[] = [];
  const externalRequests: string[] = [];
  let deletes = 0;
  let allowExpectedConflict = false;
  let authenticated = false;
  page.on("pageerror", () => failures.push("uncaught-browser-error"));
  page.on("console", (message) => {
    if (message.type() !== "error") return;
    // The login screen probes /auth/me before a session exists.
    if (
      !authenticated &&
      message.location().url === `${origin}/api/admin/v1/auth/me` &&
      /Failed to load resource.*401/.test(message.text())
    )
      return;
    if (
      allowExpectedConflict &&
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
    if (request.method() === "DELETE" && /\/memory\/[^/]+$/.test(request.url()))
      deletes++;
  });
  await authenticate(page);
  authenticated = true;
  const auth = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const mutationHeaders = { Origin: origin, "X-CSRF-Token": auth.csrf_token };
  const suffix = crypto.randomBytes(5).toString("hex");
  const sharedContent = `浏览器共享_${suffix}：共同确认的项目事实。`;
  const privateContent = `浏览器私有_${suffix}：仅在来源账号域中使用。`;
  const firstConversation = await page.request.post(
    "/api/admin/v1/conversations",
    {
      headers: mutationHeaders,
      data: { title: `记忆来源甲_${suffix}` },
    },
  );
  expect(firstConversation.ok()).toBe(true);
  const first = await firstConversation.json();
  const secondConversation = await page.request.post(
    "/api/admin/v1/conversations",
    {
      headers: mutationHeaders,
      data: { title: `记忆来源乙_${suffix}` },
    },
  );
  expect(secondConversation.ok()).toBe(true);
  const second = await secondConversation.json();

  await page.getByRole("button", { name: "助手记忆", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "助手记忆库", exact: true }),
  ).toBeVisible();
  const identity = page.locator(".memory-identity code");
  await expect(identity).toBeVisible();
  const assistantId = await identity.getAttribute("title");
  expect(assistantId).toBeTruthy();
  const create = page.locator('section[aria-label="新增记忆"]');
  await create.getByLabel("记忆内容").fill(sharedContent);
  await selectValue(
    page,
    create.getByLabel("使用范围", { exact: true }),
    "shared",
  );
  await create.getByRole("button", { name: "保存记忆", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("请明确确认");
  const beforePublish = await (
    await page.request.get("/api/admin/v1/memory")
  ).json();
  expect(
    beforePublish.items.some(
      (item: { content: string }) => item.content === sharedContent,
    ),
  ).toBe(false);
  await create
    .getByLabel("允许助手在所有渠道使用这条记忆，包括群聊。", { exact: true })
    .check();
  const publishResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/memory") &&
      response.request().method() === "POST",
  );
  await create.getByRole("button", { name: "保存记忆", exact: true }).click();
  const shared = await (await publishResponse).json();
  expect(shared).toMatchObject({
    content: sharedContent,
    visibility: "shared",
    assistant_id: assistantId,
    revision: 1,
  });
  expect(shared.source_conversation_id).toBeNull();
  const sharedArticle = page.locator(`[data-memory-id="${shared.id}"]`);
  await expect(sharedArticle).toContainText(sharedContent);

  await create.getByLabel("记忆内容").fill(privateContent);
  await selectValue(
    page,
    create.getByLabel("使用范围", { exact: true }),
    "private",
  );
  await selectValue(
    page,
    create.getByRole("combobox", { name: "来源会话", exact: true }),
    first.id,
  );
  const privateResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/memory") &&
      response.request().method() === "POST",
  );
  await create.getByRole("button", { name: "保存记忆", exact: true }).click();
  const privateMemory = await (await privateResponse).json();
  expect(privateMemory).toMatchObject({
    content: privateContent,
    visibility: "private",
    source_conversation_id: first.id,
    assistant_id: assistantId,
  });
  await expect(
    page.locator(`[data-memory-id="${privateMemory.id}"]`),
  ).toContainText("来源账号域私有");

  for (const title of [`记忆来源甲_${suffix}`, `记忆来源乙_${suffix}`]) {
    await page.getByRole("button", { name: "对话工作台", exact: true }).click();
    await page
      .locator(".conversation-button")
      .filter({ hasText: title })
      .click();
    await expect(page.locator(".chat-heading")).toBeVisible();
    await page.getByRole("button", { name: "助手记忆", exact: true }).click();
    await expect(identity).toHaveAttribute("title", assistantId!);
    await expect(sharedArticle).toContainText(sharedContent);
    await expect(
      page.locator(`[data-memory-id="${privateMemory.id}"]`),
    ).toContainText(privateContent);
  }
  expect(first.id).not.toBe(second.id);
  const filters = page.getByRole("form", { name: "筛选记忆", exact: true });
  await filters
    .getByLabel("查询内容", { exact: true })
    .fill(`浏览器共享_${suffix}`);
  await selectValue(
    page,
    filters.getByLabel("可见范围", { exact: true }),
    "shared",
  );
  await filters.getByRole("button", { name: "查询", exact: true }).click();
  await expect(sharedArticle).toBeVisible();
  await expect(
    page.locator(`[data-memory-id="${privateMemory.id}"]`),
  ).toHaveCount(0);
  await sharedArticle
    .getByRole("button", { name: "编辑纠正", exact: true })
    .click();
  const edit = page.locator('section[aria-label="编辑记忆"]');
  const corrected = `浏览器共享_${suffix}：这是已对照合并的纠正内容。`;
  await edit.getByLabel("纠正后的内容").fill(corrected);
  await edit.getByLabel("纠正后的可见范围").click();
  await expect(
    page.getByRole("option", { name: "来源账号域私有", exact: true }),
  ).toHaveAttribute("aria-disabled", "true");
  await edit.getByLabel("纠正后的可见范围").press("Escape");
  const concurrent = await page.request.patch(
    `/api/admin/v1/memory/${encodeURIComponent(shared.id)}`,
    {
      headers: { ...mutationHeaders, "If-Match": '"1"' },
      data: { content: `浏览器共享_${suffix}：另一处刚保存的内容。` },
    },
  );
  expect(concurrent.status()).toBe(200);
  allowExpectedConflict = true;
  await edit.getByRole("button", { name: "保存纠正", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("你的编辑草稿仍保留");
  await expect(edit.getByLabel("纠正后的内容")).toHaveValue(corrected);
  await expect(
    edit.getByRole("button", { name: "保存纠正", exact: true }),
  ).toBeDisabled();
  await edit.getByRole("button", { name: "载入最新版本", exact: true }).click();
  await expect(edit.locator(".memory-conflict")).toContainText(
    "另一处刚保存的内容",
  );
  await edit
    .getByRole("button", {
      name: "已对照，采用最新版本号并保留草稿",
      exact: true,
    })
    .click();
  await expect(edit.getByLabel("纠正后的内容")).toHaveValue(corrected);
  await edit.getByRole("button", { name: "保存纠正", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("纠正已保存，版本 3");
  const saved = await (
    await page.request.get(
      `/api/admin/v1/memory/${encodeURIComponent(shared.id)}`,
    )
  ).json();
  expect(saved).toMatchObject({
    content: corrected,
    revision: 3,
    visibility: "shared",
    assistant_id: assistantId,
  });

  const downloadPromise = page.waitForEvent("download");
  await page.getByRole("button", { name: "导出记忆", exact: true }).click();
  const download = await downloadPromise;
  const exportPath = path.join(dataDir, `exported-memory-${suffix}.json`);
  await download.saveAs(exportPath);
  const exported = JSON.parse(fs.readFileSync(exportPath, "utf8"));
  expect(exported.schema).toBe("dsh.assistant-memory.v1");
  expect(
    exported.items.find((item: { id: string }) => item.id === shared.id)
      .content,
  ).toBe(corrected);
  expect(
    exported.items.find((item: { id: string }) => item.id === privateMemory.id)
      .visibility,
  ).toBe("private");
  await screenshot(page, "01-memory-desktop.png");
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `02-memory-${width}.png`);
  }
  await page.setViewportSize({ width: 1440, height: 1000 });
  await sharedArticle
    .getByRole("button", { name: "遗忘此条", exact: true })
    .click();
  const confirmation = page.getByRole("region", {
    name: "确认遗忘",
    exact: true,
  });
  await expect(confirmation).toContainText(corrected);
  await expect(confirmation).toContainText("版本 3");
  await expect(confirmation).toContainText("原始消息与离线备份仍独立保留");
  expect(deletes).toBe(0);
  await confirmation
    .getByRole("button", { name: "确认遗忘此条", exact: true })
    .click();
  await expect(page.getByRole("status")).toContainText(
    "该记忆库条目及其旧版本已清除",
  );
  await expect(sharedArticle).toHaveCount(0);
  expect(deletes).toBe(1);
  const afterForget = await (
    await page.request.get("/api/admin/v1/memory/export")
  ).json();
  expect(
    afterForget.items.some((item: { id: string }) => item.id === shared.id),
  ).toBe(false);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(externalRequests).toEqual([]);
  expect(failures).toEqual([]);
  expect(consoleErrors).toEqual([]);
});
