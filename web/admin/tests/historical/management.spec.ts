import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/browser-qa",
);
const evidenceDir = path.resolve("../../.local/browser-e2e/screenshots");
const accountFile = path.join(dataDir, "e2e-account.json");

async function waitForVisualAssets(page: Page) {
  await page.evaluate(() => document.fonts.ready);
  await expect
    .poll(() =>
      page
        .locator("img")
        .evaluateAll((images) =>
          images.every(
            (image) =>
              (image as HTMLImageElement).complete &&
              (image as HTMLImageElement).naturalWidth > 0,
          ),
        ),
    )
    .toBe(true);
}

async function screenshot(page: Page, name: string) {
  await waitForVisualAssets(page);
  await page.screenshot({
    path: path.join(evidenceDir, name),
    fullPage: true,
    animations: "disabled",
  });
}

async function expectNoHorizontalOverflow(page: Page) {
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
}
async function chooseTransport(page: Page, value: string) {
  await page.getByRole("combobox", { name: "接入方式", exact: true }).click();
  await page.getByRole("listbox").locator(`[data-value="${value}"]`).click();
}

async function expectLoginPortraitBesideCopy(page: Page) {
  await waitForVisualAssets(page);
  const portrait = await page.locator(".story-portrait").boundingBox();
  expect(portrait).not.toBeNull();
  for (const selector of [".story-content > h1", ".story-content > p"]) {
    const copy = await page.locator(selector).boundingBox();
    expect(copy).not.toBeNull();
    expect(
      copy!.x + copy!.width,
      `${selector} must stay beside the portrait`,
    ).toBeLessThanOrEqual(portrait!.x + 2);
  }
}

test("M1 browser initialization, authenticated configuration and navigation", async ({
  page,
  browser,
}) => {
  const failures: string[] = [];
  page.on("pageerror", () => failures.push("uncaught-browser-error"));
  const authStatus = await (
    await page.request.get("/api/admin/v1/auth/status")
  ).json();
  let account: { username: string; password: string };
  if (fs.existsSync(accountFile)) {
    account = JSON.parse(fs.readFileSync(accountFile, "utf8"));
  } else {
    account = {
      username: `e2e_${crypto.randomBytes(5).toString("hex")}`,
      password: crypto.randomBytes(24).toString("base64url"),
    };
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      mode: 0o600,
      flag: "wx",
    });
  }
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.goto("/admin/");
  for (const [asset, contentType] of [
    ["brand-mark.svg", "image/svg+xml"],
    ["favicon.svg", "image/svg+xml"],
    ["mascot.png", "image/png"],
  ]) {
    const response = await page.request.get(`/admin/${asset}`);
    expect(response.status(), asset).toBe(200);
    expect(response.headers()["content-type"], asset).toContain(contentType);
  }

  if (!authStatus.initialized) {
    await expect(
      page.getByRole("heading", { name: "建立你的管理账户" }),
    ).toBeVisible();
    await screenshot(page, "00-setup-desktop.png");
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
    await expect(page.getByRole("heading", { name: "欢迎回来" })).toBeVisible();
    await screenshot(page, "00-login-desktop.png");
    await page.getByLabel("管理员用户名").fill(account.username);
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  }
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("服务已连接")).toBeVisible();
  await expect(page.locator(".mascot-image")).toBeVisible();
  await screenshot(page, "01-overview-desktop.png");

  const isolated = await browser.newContext();
  const unauthorized = await isolated.request.get(
    new URL("/api/admin/v1/config", page.url()).toString(),
  );
  expect(unauthorized.status()).toBe(401);
  await isolated.close();

  await page.getByRole("button", { name: "QQ 连接", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "QQ 官方机器人" }),
  ).toBeVisible();
  await chooseTransport(page, "websocket");
  await expect(page.getByText("设备主动建立连接")).toBeVisible();
  await chooseTransport(page, "webhook");
  await expect(page.getByText("Webhook 回调路径")).toBeVisible();
  await chooseTransport(page, "websocket");
  await screenshot(page, "05-qq-desktop.png");

  await page.getByRole("button", { name: "助手与模型", exact: true }).click();
  await page.getByLabel("助手名称", { exact: true }).fill("");
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  await expect(page.getByText("请为助手填写一个名称。")).toBeVisible();
  const name = `浏览器验证助手 ${new Date().toISOString().slice(11, 19)}`;
  await page.getByLabel("助手名称", { exact: true }).fill(name);
  await expect(
    page.getByRole("button", { name: "应用配置", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "校验配置", exact: true }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "校验配置", exact: true }).click();
  await expect(
    page.getByText("配置校验通过，可以进行连接测试或应用。"),
  ).toBeVisible();
  await page.getByRole("button", { name: "应用配置", exact: true }).click();
  await expect(
    page.getByText("配置已生效。新的对话将使用这一版本。"),
  ).toBeVisible();
  const saved = await (await page.request.get("/api/admin/v1/config")).json();
  expect(saved.config.assistant.name).toBe(name);
  expect(saved.revision).toBe(saved.applied_revision);
  await screenshot(page, "02-configuration-desktop.png");

  await page.getByRole("button", { name: "配置历史", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "配置版本", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(`配置 v${saved.revision}`, { exact: true }),
  ).toBeVisible();
  await screenshot(page, "06-history-desktop.png");
  await page.getByRole("button", { name: "对话工作台", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "开始一段新的对话" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "发送", exact: true }),
  ).toBeDisabled();
  await expect(page.locator(".mascot-image")).toBeVisible();
  await screenshot(page, "03-conversation-desktop.png");

  await page.setViewportSize({ width: 390, height: 844 });
  const mobilePages = [
    ["运行总览", "10-overview-mobile.png"],
    ["助手与模型", "11-configuration-mobile.png"],
    ["QQ 连接", "12-qq-mobile.png"],
    ["对话工作台", "13-conversation-mobile.png"],
    ["配置历史", "14-history-mobile.png"],
  ];
  for (const [title, filename] of mobilePages) {
    await page.getByRole("button", { name: "打开导航" }).click();
    await page.getByRole("button", { name: title, exact: true }).click();
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    await expect(page.locator(".sidebar")).not.toHaveClass(/open/);
    await expect
      .poll(async () => {
        const sidebar = await page.locator(".sidebar").boundingBox();
        return sidebar ? sidebar.x + sidebar.width : 0;
      })
      .toBeLessThanOrEqual(1);
    await expectNoHorizontalOverflow(page);
    await screenshot(page, filename);
  }

  await page.setViewportSize({ width: 320, height: 740 });
  for (const title of ["运行总览", "助手与模型", "对话工作台"]) {
    await page.getByRole("button", { name: "打开导航" }).click();
    await page.getByRole("button", { name: title, exact: true }).click();
    await expect(
      page.getByRole("heading", { name: title, exact: true }),
    ).toBeVisible();
    await expectNoHorizontalOverflow(page);
  }

  await page.getByRole("button", { name: "打开导航" }).click();
  await page.getByRole("button", { name: "运行总览", exact: true }).click();
  await page.getByRole("button", { name: "打开导航" }).click();
  await page.getByRole("button", { name: "退出登录" }).click();
  await expect(page.getByRole("heading", { name: "欢迎回来" })).toBeVisible();
  await expectNoHorizontalOverflow(page);
  await expectLoginPortraitBesideCopy(page);
  await screenshot(page, "15-login-320.png");
  await page.setViewportSize({ width: 390, height: 844 });
  await expectLoginPortraitBesideCopy(page);
  await screenshot(page, "16-login-mobile.png");
  await page.setViewportSize({ width: 1440, height: 1000 });
  await screenshot(page, "00-login-desktop.png");
  await page.getByLabel("管理员用户名").fill(account.username);
  await page.getByLabel("管理密码", { exact: true }).fill(account.password);
  await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(failures).toEqual([]);
});
