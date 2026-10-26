import { test, expect, type Locator, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

// Run against a separate localhost instance and its disposable data directory.
// This suite never uses the production tunnel, production accounts or model calls.
const evidenceDir = path.resolve("../../.local/ui-design-evidence");
const pages = [
  ["overview", "运行总览"],
  ["assistant", "助手与模型"],
  ["characters", "角色卡"],
  ["qq", "QQ 连接"],
  ["weixin", "微信连接"],
  ["chat", "对话工作台"],
  ["diagnostics", "任务与诊断"],
  ["memory", "助手记忆"],
  ["history", "配置历史"],
] as const;

async function authenticate(page: Page) {
  if (!process.env.DSH_E2E_DATA_DIR)
    throw new Error(
      "UI design tests require DSH_E2E_DATA_DIR for their isolated instance.",
    );
  const base = new URL(
    process.env.DSH_E2E_BASE_URL || "http://127.0.0.1:18420",
  );
  if (
    !["localhost", "127.0.0.1", "[::1]"].includes(base.hostname) ||
    base.port === "18419"
  )
    throw new Error(
      "UI design tests must not target the production management tunnel.",
    );
  const dataDir = path.resolve(process.env.DSH_E2E_DATA_DIR);
  const accountFile = path.join(dataDir, "e2e-account.json");
  fs.mkdirSync(dataDir, { recursive: true });
  fs.mkdirSync(evidenceDir, { recursive: true });
  const statusResponse = await page.request.get("/api/admin/v1/auth/status");
  expect(statusResponse.ok()).toBe(true);
  const status = await statusResponse.json();
  let account: { username: string; password: string };
  if (fs.existsSync(accountFile))
    account = JSON.parse(fs.readFileSync(accountFile, "utf8"));
  else {
    if (status.initialized)
      throw new Error(
        "The isolated instance is initialized but has no matching test account file.",
      );
    account = {
      username: `ui_qa_${crypto.randomBytes(5).toString("hex")}`,
      password: crypto.randomBytes(24).toString("base64url"),
    };
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      mode: 0o600,
      flag: "wx",
    });
  }
  await page.goto("/admin/#overview");
  if (!status.initialized) {
    await expect(
      page.getByRole("heading", { name: "建立你的管理账户" }),
    ).toBeVisible();
    const pairing = JSON.parse(
      fs.readFileSync(path.join(dataDir, "bootstrap-token.json"), "utf8"),
    );
    await page.getByLabel("一次性配对凭证").fill(pairing.token);
    await page.getByLabel("管理员用户名").fill(account.username);
    await page
      .getByLabel("设置管理密码", { exact: true })
      .fill(account.password);
    await page.getByLabel("再次输入密码").fill(account.password);
    await page
      .getByRole("button", { name: "创建账户并进入控制台", exact: true })
      .click();
  } else {
    await expect(page.getByRole("heading", { name: "欢迎回来" })).toBeVisible();
    await page.getByLabel("管理员用户名").fill(account.username);
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  }
  await expect(
    page.getByRole("heading", { level: 1, name: "运行总览", exact: true }),
  ).toBeVisible();
}

async function screenshot(page: Page, name: string) {
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
  await page.screenshot({
    path: path.join(evidenceDir, name),
    animations: "disabled",
  });
}
async function noHorizontalOverflow(page: Page) {
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
}
async function anchoredSections(page: Page) {
  const viewport = page.viewportSize()!;
  const sidebar = page.getByRole("complementary", {
    name: "工作空间导航",
    exact: true,
  });
  const header = sidebar.locator(".sidebar-header");
  const footer = sidebar.locator(".sidebar-footer");
  await expect(header).toBeInViewport({ ratio: 1 });
  await expect(footer).toBeInViewport({ ratio: 1 });
  const headerBox = (await header.boundingBox())!;
  const footerBox = (await footer.boundingBox())!;
  expect(headerBox.y).toBeGreaterThanOrEqual(0);
  expect(footerBox.y + footerBox.height).toBeLessThanOrEqual(
    viewport.height + 1,
  );
  expect(headerBox.y + headerBox.height).toBeLessThan(footerBox.y);
  const state = await sidebar.evaluate((element) => ({
    scrollTop: element.scrollTop,
    scrollHeight: element.scrollHeight,
    clientHeight: element.clientHeight,
    overflow: getComputedStyle(element).overflowY,
  }));
  expect(state.scrollTop).toBe(0);
  expect(state.scrollHeight).toBeLessThanOrEqual(state.clientHeight + 1);
  expect(["auto", "scroll"]).not.toContain(state.overflow);
  return { headerY: headerBox.y, footerY: footerBox.y };
}
async function expectSameAnchors(
  page: Page,
  previous: { headerY: number; footerY: number },
) {
  const current = await anchoredSections(page);
  expect(Math.abs(current.headerY - previous.headerY)).toBeLessThanOrEqual(1);
  expect(Math.abs(current.footerY - previous.footerY)).toBeLessThanOrEqual(1);
}
async function menuWithinViewport(page: Page, control: Locator) {
  await expect(control).toHaveAttribute("aria-expanded", "true");
  const menu = page.getByRole("listbox");
  await expect(menu).toBeVisible();
  const box = (await menu.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(viewport.width + 1);
  expect(box.y + box.height).toBeLessThanOrEqual(viewport.height + 1);
  return menu;
}

test("desktop sidebar anchors and all nine management pages", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1920, height: 1080 });
  await authenticate(page);
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.name));
  for (const [id, title] of pages) {
    await page
      .getByRole("navigation", { name: "主导航", exact: true })
      .getByRole("button", { name: title, exact: true })
      .click();
    await expect(
      page.getByRole("heading", { level: 1, name: title, exact: true }),
    ).toBeVisible();
    await noHorizontalOverflow(page);
    await anchoredSections(page);
    await screenshot(page, `desktop-1920-${id}.png`);
  }
  await page.getByRole("button", { name: "助手记忆", exact: true }).click();
  for (const [width, height] of [
    [1920, 1080],
    [1366, 768],
    [1024, 600],
  ]) {
    await page.setViewportSize({ width: width!, height: height! });
    await page.evaluate(() => scrollTo(0, 0));
    const before = await anchoredSections(page);
    await page.evaluate(() =>
      scrollTo(0, document.documentElement.scrollHeight),
    );
    await expect.poll(() => page.evaluate(() => scrollY)).toBeGreaterThan(0);
    await expectSameAnchors(page, before);
    const last = page
      .getByRole("navigation", { name: "主导航", exact: true })
      .getByRole("button", { name: "配置历史", exact: true });
    await last.scrollIntoViewIfNeeded();
    await expectSameAnchors(page, before);
    await noHorizontalOverflow(page);
    await screenshot(page, `sidebar-desktop-${width}x${height}.png`);
  }
  expect(errors).toEqual([]);
});

test("mobile navigation keeps its brand and account fixed while its middle section scrolls", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await authenticate(page);
  const sidebar = page.getByRole("complementary", {
    name: "工作空间导航",
    exact: true,
  });
  for (const height of [844, 480]) {
    await page.setViewportSize({ width: 390, height });
    await page.evaluate(() => scrollTo(0, 0));
    const open = page.getByRole("button", { name: "打开导航", exact: true });
    await open.click();
    await expect(open).toHaveAttribute("aria-expanded", "true");
    const before = await anchoredSections(page);
    const last = sidebar.getByRole("button", { name: "配置历史", exact: true });
    await last.scrollIntoViewIfNeeded();
    await expect(last).toBeInViewport({ ratio: 1 });
    await expectSameAnchors(page, before);
    const middle = sidebar.locator(".sidebar-main");
    const scroll = await middle.evaluate((element) => ({
      top: element.scrollTop,
      max: element.scrollHeight - element.clientHeight,
    }));
    if (scroll.max > 1) expect(scroll.top).toBeGreaterThan(0);
    await expect(
      sidebar.getByRole("button", { name: "关闭导航", exact: true }),
    ).toBeInViewport({ ratio: 1 });
    await screenshot(page, `sidebar-mobile-390x${height}.png`);
    await last.click();
    await expect(
      page.getByRole("heading", { level: 1, name: "配置历史", exact: true }),
    ).toBeVisible();
    await expect(sidebar).not.toBeInViewport();
    await open.click();
    await sidebar
      .getByRole("button", { name: "关闭导航", exact: true })
      .click();
    await expect(open).toHaveAttribute("aria-expanded", "false");
    await expect(sidebar).not.toBeInViewport();
    await noHorizontalOverflow(page);
  }
});

test("QQ dropdown has the shared theme, stays inside the viewport and supports keyboard dismissal", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1366, height: 768 });
  await authenticate(page);
  const writes: string[] = [];
  page.on("request", (request) => {
    if (["POST", "PUT", "PATCH", "DELETE"].includes(request.method()))
      writes.push(new URL(request.url()).pathname);
  });
  await page.getByRole("button", { name: "QQ 连接", exact: true }).click();
  const control = page.getByRole("combobox", { name: "接入方式", exact: true });
  await control.press("Home");
  await control.press("Enter");
  await expect(
    page.getByText("设备主动建立连接", { exact: true }),
  ).toBeVisible();
  await control.press("ArrowDown");
  await control.press("ArrowDown");
  await control.press("Enter");
  await expect(
    page.getByText("Webhook 回调路径", { exact: true }),
  ).toBeVisible();
  await control.press("Home");
  await control.press("Enter");
  await expect(
    page.getByText("设备主动建立连接", { exact: true }),
  ).toBeVisible();
  await control.click();
  const menu = await menuWithinViewport(page, control);
  await expect(menu.getByRole("option")).toHaveCount(2);
  const theme = await menu.evaluate((element) => {
    const style = getComputedStyle(element);
    return {
      borderRadius: parseFloat(style.borderRadius),
      background: style.backgroundColor,
      shadow: style.boxShadow,
      position: style.position,
    };
  });
  expect(theme.borderRadius).toBeGreaterThanOrEqual(10);
  expect(theme.background).toBe("rgb(255, 255, 255)");
  expect(theme.shadow).not.toBe("none");
  expect(theme.position).toBe("fixed");
  await screenshot(page, "dropdown-qq-desktop.png");
  await control.press("Escape");
  await expect(menu).toHaveCount(0);
  await control.click();
  await page
    .getByRole("heading", { level: 1, name: "QQ 连接", exact: true })
    .click();
  await expect(page.getByRole("listbox")).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 480 });
  await control.scrollIntoViewIfNeeded();
  await control.click();
  await menuWithinViewport(page, control);
  await screenshot(page, "dropdown-qq-mobile.png");
  await control.press("Escape");
  await expect(page.getByRole("listbox")).toHaveCount(0);
  expect(writes).toEqual([]);
});
