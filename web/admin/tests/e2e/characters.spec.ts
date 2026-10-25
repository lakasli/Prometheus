import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const dataDir = path.resolve(
  process.env.DSH_CHARACTER_E2E_DATA_DIR || "../../.local/character-e2e",
);
const evidenceDir = path.resolve("../../.local/character-e2e/screenshots");
const accountFile = path.join(dataDir, "e2e-account.json");

async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `character_${crypto.randomBytes(4).toString("hex")}`,
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

test("character cards use real local APIs for versioning, import and isolated conversations", async ({
  page,
  baseURL,
}) => {
  test.setTimeout(120_000);
  expect(
    new URL(baseURL!).port,
    "character E2E must use its isolated service on port 18421",
  ).toBe("18421");
  const failures: string[] = [];
  const externalRequests: string[] = [];
  let sentMessages = 0;
  page.on("pageerror", () => failures.push("uncaught-browser-error"));
  page.on("request", (request) => {
    if (!request.url().startsWith(baseURL!))
      externalRequests.push(new URL(request.url()).hostname);
    if (
      request.method() === "POST" &&
      /\/conversations\/[^/]+\/messages$/.test(request.url())
    )
      sentMessages++;
  });
  await authenticate(page);
  await page.getByRole("button", { name: "角色卡", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "角色卡库", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".character-editor")).toHaveCount(0);
  const initialFooter = await page
    .locator(".page-footer")
    .evaluate((footer) => ({
      bottom: footer.getBoundingClientRect().bottom,
      viewportHeight: window.innerHeight,
    }));
  expect(initialFooter.bottom).toBeGreaterThanOrEqual(
    initialFooter.viewportHeight - 32,
  );
  expect(initialFooter.bottom).toBeLessThanOrEqual(
    initialFooter.viewportHeight,
  );
  await page.getByRole("button", { name: "新建卡片", exact: true }).click();
  const name = `浏览器角色_${crypto.randomBytes(4).toString("hex")}`;
  await page.getByLabel("角色名称", { exact: true }).fill(name);
  await page
    .getByLabel("身份与背景", { exact: true })
    .fill("浏览器隔离验收使用的原创文本角色。");
  await page
    .getByLabel("稳定性格", { exact: true })
    .fill("平静、简洁，愿意说明不确定性。");
  await page.getByLabel("人格指令").fill("你是隔离测试角色。用中文简洁回复。");
  await page
    .getByLabel("首条开场白")
    .fill("这是角色卡开场白，不是模型生成的回复。");
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  await expect(page.getByText("草稿已保存", { exact: true })).toBeVisible();
  await expect(
    page
      .locator(".character-gallery .character-grid .character-card-button")
      .filter({ has: page.getByText(name, { exact: true }) }),
  ).toBeVisible();
  const galleryBounds = await page.locator(".character-gallery").boundingBox();
  const libraryBounds = await page.locator(".character-library").boundingBox();
  const editorBounds = await page.locator(".character-editor").boundingBox();
  expect(galleryBounds!.x).toBe(libraryBounds!.x);
  expect(editorBounds!.x).toBeGreaterThanOrEqual(
    galleryBounds!.x + galleryBounds!.width,
  );
  expect(editorBounds!.y).toBe(libraryBounds!.y);
  await expect(
    page.locator(".character-card-button.active .character-card-description"),
  ).toContainText("浏览器隔离验收使用的原创文本角色。");
  const editorFooter = await page
    .locator(".character-editor")
    .evaluate((editor) => ({
      editorBottom: editor.getBoundingClientRect().bottom + window.scrollY,
      footerTop:
        document.querySelector(".page-footer")!.getBoundingClientRect().top +
        window.scrollY,
      viewportHeight: window.innerHeight,
    }));
  expect(editorFooter.footerTop).toBeGreaterThanOrEqual(
    editorFooter.editorBottom,
  );
  expect(editorFooter.footerTop).toBeGreaterThan(editorFooter.viewportHeight);

  await page.getByLabel("昵称", { exact: true }).fill("未保存昵称");
  await page.getByRole("button", { name: "刷新", exact: true }).click();
  await expect(page.getByLabel("昵称", { exact: true })).toHaveValue(
    "未保存昵称",
  );
  await expect(page.getByText("有未保存修改", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "编译预览", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: /运行人格预览/ }),
  ).toBeVisible();
  await expect(page.locator(".character-preview pre")).toContainText(
    "隔离测试角色",
  );
  await expect(page.getByText(/未参与编译：/)).toContainText(
    "post_history_instructions",
  );
  await page.getByRole("button", { name: "校验卡片", exact: true }).click();
  await expect(
    page.getByText("卡片格式校验通过", { exact: true }),
  ).toBeVisible();
  const publication = page.waitForResponse(
    (response) =>
      response.url().endsWith("/publish") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "发布版本", exact: true }).click();
  const published = (await (await publication).json()) as {
    card_id: string;
    revision: number;
  };
  expect(published.revision).toBe(1);
  await expect(page.locator(".character-alert")).toContainText("已发布 v1");
  const draftName = `${name}_草稿修改`;
  await page.getByLabel("角色名称", { exact: true }).fill(draftName);
  await page
    .getByLabel("身份与背景", { exact: true })
    .fill("修改后的角色介绍立即展示在左侧卡片中。");
  await expect(page.locator(".character-card-button.active strong")).toHaveText(
    draftName,
  );
  await expect(
    page.locator(".character-card-button.active .character-card-description"),
  ).toHaveText("修改后的角色介绍立即展示在左侧卡片中。");
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  await expect(page.getByText("草稿已保存", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "返回全部角色", exact: true }).click();
  await expect(page.locator(".character-editor")).toHaveCount(0);
  await page
    .locator(".character-card-button")
    .filter({ has: page.getByText(draftName, { exact: true }) })
    .click();
  await expect(page.getByLabel("角色名称", { exact: true })).toHaveValue(
    draftName,
  );
  let library = await (
    await page.request.get("/api/admin/v1/character-cards")
  ).json();
  expect(library.default?.card_id).not.toBe(published.card_id);
  expect(
    library.items.find((item: { id: string }) => item.id === published.card_id)
      .qq_selectable,
  ).toBe(false);

  await page.getByLabel("允许QQ私聊用户选择此角色", { exact: true }).check();
  await expect(page.locator(".character-alert")).toContainText(
    "已允许QQ私聊用户选择",
  );
  await page
    .getByRole("button", { name: "设为新会话默认", exact: true })
    .click();
  await expect(page.locator(".character-alert")).toContainText(
    "新会话默认已设为",
  );
  library = await (
    await page.request.get("/api/admin/v1/character-cards")
  ).json();
  expect(library.default).toMatchObject({
    card_id: published.card_id,
    revision: 1,
    name,
  });
  expect(
    library.items.find((item: { id: string }) => item.id === published.card_id)
      .qq_selectable,
  ).toBe(true);
  await screenshot(page, "01-character-desktop.png");

  const downloadPromise = page.waitForEvent("download");
  await page.getByRole("button", { name: "导出 JSON", exact: true }).click();
  const download = await downloadPromise;
  const exportPath = path.join(dataDir, "exported-character.json");
  await download.saveAs(exportPath);
  const exported = JSON.parse(fs.readFileSync(exportPath, "utf8"));
  expect(exported.spec).toBe("chara_card_v3");
  expect(exported.data.name).toBe(name);
  expect(exported.data.extensions.dsh.modules.chat.mode).toBe("inherit");
  expect(Object.keys(exported).sort()).toEqual([
    "data",
    "spec",
    "spec_version",
  ]);
  const importedName = `导入_${name}`;
  exported.data.name = importedName;
  exported.data.assets = [
    {
      type: "icon",
      uri: "https://example.invalid/never-fetch.png",
      name: "icon",
      ext: "png",
    },
  ];
  const beforeImport = library.items.length;
  await page.locator('input[type="file"]').setInputFiles({
    name: "character.json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify(exported)),
  });
  await expect(
    page.getByRole("heading", { name: "确认转换内容", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".character-import")).toContainText("assets");
  library = await (
    await page.request.get("/api/admin/v1/character-cards")
  ).json();
  expect(library.items).toHaveLength(beforeImport);
  await page
    .getByRole("button", { name: "确认内容，创建草稿", exact: true })
    .click();
  await expect(page.getByLabel("角色名称", { exact: true })).toHaveValue(
    importedName,
  );
  library = await (
    await page.request.get("/api/admin/v1/character-cards")
  ).json();
  expect(library.items).toHaveLength(beforeImport + 1);
  expect(library.default.card_id).toBe(published.card_id);
  expect(
    library.items.find((item: { name: string }) => item.name === importedName)
      .latest_revision,
  ).toBe(0);

  await page.getByRole("button", { name: "对话工作台", exact: true }).click();
  await page.getByRole("combobox", { name: "新对话角色", exact: true }).click();
  await page
    .getByRole("listbox")
    .locator(`[data-value="${published.card_id}"]`)
    .click();
  const conversationResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/conversations") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "新建对话", exact: true }).click();
  const conversation = await (await conversationResponse).json();
  expect(conversation.character).toMatchObject({
    card_id: published.card_id,
    revision: 1,
    name,
  });
  await expect(page.locator(".chat-heading")).toContainText(name);
  await expect(page.locator(".chat-heading")).toContainText("角色 v1");
  await expect(page.locator(".character-greeting")).toContainText(
    "这是角色卡开场白",
  );
  await screenshot(page, "04-character-conversation.png");

  await page.getByRole("button", { name: "角色卡", exact: true }).click();
  await page
    .locator(".character-card-button")
    .filter({ has: page.getByText(importedName, { exact: true }) })
    .click();
  const testResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/test-conversations") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "独立测试对话", exact: true }).click();
  const testConversation = await (await testResponse).json();
  expect(
    testConversation.character.is_test ||
      testConversation.is_test ||
      testConversation.test,
  ).toBe(true);
  await expect(page.locator(".chat-heading")).toContainText("草稿测试会话");
  expect(
    sentMessages,
    "creating a test conversation must not call a model or send QQ messages",
  ).toBe(0);

  await page.getByRole("button", { name: "角色卡", exact: true }).click();
  await page
    .locator(".character-card-button")
    .filter({ has: page.getByText(importedName, { exact: true }) })
    .click();
  await page.getByRole("button", { name: "校验卡片", exact: true }).click();
  await expect(
    page.getByText("卡片格式校验通过", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "发布版本", exact: true }).click();
  await expect(page.locator(".character-alert")).toContainText("已发布 v1");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "归档卡片", exact: true }).click();
  await expect(page.locator(".character-alert")).toContainText("卡片已归档");
  library = await (
    await page.request.get("/api/admin/v1/character-cards")
  ).json();
  expect(
    library.items.find((item: { name: string }) => item.name === importedName)
      .archived_at,
  ).toBeGreaterThan(0);
  expect(library.default.card_id).toBe(published.card_id);

  await page.getByRole("button", { name: "对话工作台", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("button", { name: "打开导航", exact: true }).click();
  await page.getByRole("button", { name: "角色卡", exact: true }).click();
  await page
    .locator(".character-card-button")
    .filter({ has: page.getByText(draftName, { exact: true }) })
    .click();
  await expect(page.getByLabel("角色名称", { exact: true })).toHaveValue(
    draftName,
  );
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await screenshot(page, "02-character-mobile.png");
  await page.setViewportSize({ width: 320, height: 740 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await screenshot(page, "03-character-320.png");
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(externalRequests).toEqual([]);
  expect(failures).toEqual([]);
});
