import { expect, test } from "@playwright/test";
import { readFileSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
test.describe.configure({ mode: "serial" });
const password = "Synthetic-browser-password-2026!";
test("first-run setup consumes an actual one-time credential", async ({
  page,
}) => {
  await page.goto("/admin/");
  await expect(
    page.getByRole("heading", { name: "建立你的日常空间" }),
  ).toBeVisible();
  const state = JSON.parse(
    readFileSync(resolve("../../.local/prometheus/e2e-state.json"), "utf8"),
  );
  const token = JSON.parse(readFileSync(state.tokenFile, "utf8")).token;
  await page.getByLabel("用户名", { exact: true }).fill("browser");
  await page.getByLabel("密码", { exact: true }).fill(password);
  await page.getByLabel("确认密码", { exact: true }).fill(password);
  await page.getByLabel("一次性初始化凭证").fill(token);
  await page.getByRole("button", { name: "创建空间" }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".local-badge")).toHaveText("Windows 本机");
});
test.beforeEach(async ({ page }, info) => {
  if (info.title.startsWith("first-run")) return;
  await page.goto("/admin/");
  await expect(
    page.getByRole("button", { name: "进入日常空间" }),
  ).toBeVisible();
  await page.getByLabel("用户名", { exact: true }).fill("browser");
  await page.getByLabel("密码", { exact: true }).fill(password);
  await page.getByRole("button", { name: "进入日常空间" }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
});
test("overview renders without console, CSP or failed asset errors at desktop and mobile widths", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  await page.setViewportSize({ width: 1440, height: 1000 });
  const folder = resolve("../../.local/prometheus/screenshots");
  mkdirSync(folder, { recursive: true });
  await page.screenshot({
    path: resolve(folder, "overview-desktop.png"),
    fullPage: true,
  });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole("navigation")).not.toBeInViewport();
  await page.screenshot({
    path: resolve(folder, "overview-mobile.png"),
    fullPage: true,
  });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.getByRole("button", { name: "展开导航" }).click();
  await expect(page.getByRole("navigation")).toBeVisible();
  expect(errors).toEqual([]);
});
test("private memories can be created, edited with a revision and forgotten", async ({
  page,
}) => {
  await page.getByRole("button", { name: "记忆与资料", exact: false }).click();
  await page.getByRole("button", { name: "＋ 新增记忆与资料" }).click();
  await page
    .getByLabel("内容", { exact: true })
    .fill("浏览器真实保存：喜欢雨后的薄荷。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("喜欢雨后的薄荷");
  await page.getByRole("button", { name: "编辑", exact: true }).click();
  await page
    .getByLabel("内容", { exact: true })
    .fill("浏览器真实更正：喜欢清晨的薄荷。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("v2");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "遗忘", exact: true }).click();
  await expect(page.locator(".record-card")).toHaveCount(0);
});
test("personality changes persist, configuration conflicts are shown, and gated features remain disabled", async ({
  page,
}) => {
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "性格", exact: true })
    .click();
  await page
    .getByLabel("个人简介", { exact: true })
    .fill("浏览器验收的合成性格资料。");
  await page.getByRole("button", { name: "保存并生效", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("性格已保存并生效");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "模型与运行", exact: true })
    .click();
  await expect(page.getByLabel("模型名称", { exact: true })).toHaveValue(
    "deepseek-chat",
  );
  const me = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const current = await (await page.request.get("/api/admin/v1/config")).json();
  await page.request.put("/api/admin/v1/config", {
    data: { config: current.config },
    headers: {
      origin: "http://127.0.0.1:18449",
      "x-csrf-token": me.csrf_token,
      "if-match": `"${current.revision}"`,
    },
  });
  await page.getByRole("button", { name: "保存并生效", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("版本已变化");
  await page.getByRole("button", { name: "↻ 刷新", exact: true }).click();
  await page.getByRole("button", { name: "保存并生效", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("配置已保存并生效");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "渠道与媒体", exact: true })
    .click();
  await expect(
    page.locator(".capability-card").filter({ hasText: "QQ 连接" }),
  ).toContainText("待迁移 · 禁用");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "插件", exact: true })
    .click();
  await expect(
    page.locator(".capability-card").filter({ hasText: "生成插件" }),
  ).toContainText("待迁移 · 禁用");
});
test("schedules, diaries, people, relationships and skills save through their real APIs", async ({
  page,
}) => {
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日程", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增日程" }).click();
  await page.getByLabel("标题", { exact: true }).fill("合成阅读计划");
  await page.getByLabel("内容", { exact: true }).fill("手动阅读与进度记录");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("合成阅读计划");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日记", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增日记" }).click();
  await page.getByLabel("标题", { exact: true }).fill("合成日记");
  await page
    .getByLabel("事实与经历", { exact: true })
    .fill("这里是浏览器测试合成的资料。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("合成日记");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "人物", exact: true })
    .click();
  for (const name of ["甲", "乙"]) {
    await page.getByRole("button", { name: "＋ 新增人物" }).click();
    await page.getByLabel("名称", { exact: true }).fill(name);
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(
      page.locator(".record-card").filter({ hasText: name }),
    ).toBeVisible();
  }
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "人际关系", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增人际关系" }).click();
  await page.getByLabel("起点人物").selectOption({ label: "甲" });
  await page.getByLabel("终点人物").selectOption({ label: "乙" });
  await page.getByLabel("说明", { exact: true }).fill("合成同事关系");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("甲 → 乙");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "Skill 管理", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增Skill 管理" }).click();
  await page.getByLabel("名称", { exact: true }).fill("合成条理说明");
  await page
    .getByLabel("方法与步骤", { exact: true })
    .fill("先说结论，再补充必要细节。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("合成条理说明");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日志记录", exact: true })
    .click();
  await expect(page.locator(".log-row").first()).toContainText("资料已保存");
});
