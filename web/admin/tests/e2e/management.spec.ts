import { test, expect } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/browser-qa",
);
const evidenceDir = path.resolve("../../.local/browser-e2e/screenshots");
const accountFile = path.join(dataDir, "e2e-account.json");

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

  if (!authStatus.initialized) {
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
    await page.getByRole("button", { name: "创建账户并进入控制台" }).click();
  } else {
    await expect(page.getByRole("heading", { name: "欢迎回来" })).toBeVisible();
    await page.getByLabel("管理员用户名").fill(account.username);
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  }
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("服务已连接")).toBeVisible();
  await page.screenshot({
    path: path.join(evidenceDir, "01-overview-desktop.png"),
    fullPage: true,
    animations: "disabled",
  });

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
  await page.getByLabel("接入方式").selectOption("websocket");
  await expect(page.getByText("设备主动建立连接")).toBeVisible();
  await page.getByLabel("接入方式").selectOption("webhook");
  await expect(page.getByText("Webhook 回调路径")).toBeVisible();
  await page.getByLabel("接入方式").selectOption("websocket");

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
  await page.screenshot({
    path: path.join(evidenceDir, "02-configuration-desktop.png"),
    fullPage: true,
    animations: "disabled",
  });

  await page.getByRole("button", { name: "配置历史", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "配置版本", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText(`配置 v${saved.revision}`, { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "对话工作台", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "开始一段新的对话" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "发送", exact: true }),
  ).toBeDisabled();
  await page.screenshot({
    path: path.join(evidenceDir, "03-conversation-desktop.png"),
    fullPage: true,
    animations: "disabled",
  });

  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole("button", { name: "打开导航" }).click();
  await page.getByRole("button", { name: "运行总览", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".sidebar")).not.toHaveClass(/open/);
  await expect(page.locator(".sidebar")).toHaveCSS(
    "transform",
    "matrix(1, 0, 0, 1, -235, 0)",
  );
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: path.join(evidenceDir, "04-overview-mobile.png"),
    fullPage: true,
    animations: "disabled",
  });

  await page.getByRole("button", { name: "打开导航" }).click();
  await page.getByRole("button", { name: "退出登录" }).click();
  await expect(page.getByRole("heading", { name: "欢迎回来" })).toBeVisible();
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
