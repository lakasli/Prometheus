import { test, expect } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

test("daily routine and personal activities save through the real authenticated calendar", async ({
  page,
  baseURL,
}) => {
  const root = path.resolve(process.env.DSH_E2E_DATA_DIR!);
  const evidence = path.resolve(process.env.DSH_E2E_EVIDENCE_DIR!);
  expect(new URL(baseURL!).hostname).toBe("127.0.0.1");
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  expect(
    (await (await page.request.get("/api/admin/v1/auth/status")).json())
      .initialized,
  ).toBe(false);
  const account = {
    username: "daily_" + crypto.randomBytes(4).toString("hex"),
    password: crypto.randomBytes(24).toString("base64url"),
  };
  fs.writeFileSync(
    path.join(root, "daily-account.json"),
    JSON.stringify(account),
    { mode: 0o600, flag: "wx" },
  );
  const pairing = JSON.parse(
    fs.readFileSync(path.join(root, "bootstrap-token.json"), "utf8"),
  );
  await page.goto("/admin/");
  await page.getByLabel("管理员用户名").fill(account.username);
  await page.getByLabel("一次性配对凭证").fill(pairing.token);
  await page.getByLabel("设置管理密码", { exact: true }).fill(account.password);
  await page.getByLabel("再次输入密码").fill(account.password);
  await page
    .getByRole("button", { name: "创建账户并进入控制台", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  const me = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const caps = await (
    await page.request.get("/api/admin/v1/task-triggers/capabilities")
  ).json();
  const ack = await page.request.post(
    "/api/admin/v1/task-triggers/clock/acknowledge",
    {
      headers: {
        Origin: new URL(baseURL!).origin,
        "X-CSRF-Token": me.csrf_token,
      },
      data: { observed_now: caps.now, confirmed: true },
    },
  );
  expect(ack.ok()).toBe(true);
  await page
    .getByRole("navigation", { name: "主导航" })
    .getByRole("button", { name: "日程", exact: true })
    .click();
  await page.getByText("作息设置", { exact: true }).click();
  const form = page.getByRole("form", { name: "每日固定作息" });
  await form.getByLabel("每天起床", { exact: true }).fill("07:00");
  await form.getByLabel("每天睡觉", { exact: true }).fill("23:00");
  await form.getByLabel("启用每天起床和睡觉").check();
  await form.getByLabel("提前一分钟选择日程").check();
  await form.getByLabel("睡眠时重置上下文，记忆从日记读取").check();
  await form.getByRole("button", { name: "保存每日作息" }).click();
  await expect(page.getByRole("status")).toContainText("每日作息已保存");
  const policy = await (
    await page.request.get("/api/admin/v1/rest/policy")
  ).json();
  expect(policy.policy).toMatchObject({
    sleep_local: "23:00",
    wake_local: "07:00",
    anticipation_enabled: true,
    reset_context_on_sleep: true,
    schedule_enabled: true,
  });
  await expect(page.getByLabel("允许嘉卉自动安排抄诗")).toHaveCount(0);
  await page.reload();
  await expect(page.getByLabel("每天起床", { exact: true })).toHaveValue(
    "07:00",
  );
  await expect(page.getByText("保存小安排授权",{exact:true})).toHaveCount(0);
  await expect(
    page.getByText("每天 07:00 起床，23:00 睡觉。", { exact: false }),
  ).toBeVisible();
  fs.mkdirSync(evidence, { recursive: true });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({
    path: path.join(evidence, "daily-calendar-desktop.png"),
    fullPage: false,
    animations: "disabled",
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await expect(page.locator(".sidebar")).toHaveAttribute("aria-hidden","true");
  await expect(page.getByText("每天 07:00 起床，23:00 睡觉。",{exact:false})).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, 0));
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth + 1,
    ),
  ).toBe(true);
  expect(await page.locator('.schedule-system > .panel').evaluateAll(elements=>elements.every(el=>el.getBoundingClientRect().right<=window.innerWidth))).toBe(true);
  await page.screenshot({
    path: path.join(evidence, "daily-calendar-mobile.png"),
    fullPage: false,
    animations: "disabled",
  });
  expect(errors).toEqual([]);
});
