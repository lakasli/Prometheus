import { test, expect, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const dataDir = path.resolve(
  process.env.DSH_CHARACTER_E2E_DATA_DIR || "../../.local/trigger-e2e/data",
);
const evidenceDir = path.resolve("../../.local/trigger-e2e/screenshots");
const prefix = "/api/admin/v1";
async function login(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const file = path.join(dataDir, "e2e-account.json");
  const account = fs.existsSync(file)
    ? JSON.parse(fs.readFileSync(file, "utf8"))
    : {
        username: `triggers_${crypto.randomBytes(4).toString("hex")}`,
        password: crypto.randomBytes(24).toString("base64url"),
      };
  if (!fs.existsSync(file))
    fs.writeFileSync(file, JSON.stringify(account), {
      mode: 0o600,
      flag: "wx",
    });
  const status = await (await page.request.get(prefix + "/auth/status")).json();
  await page.goto("/admin/");
  await page.getByLabel("管理员用户名").fill(account.username);
  if (status.initialized) {
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  } else {
    await page
      .getByLabel("一次性配对凭证")
      .fill(
        JSON.parse(
          fs.readFileSync(path.join(dataDir, "bootstrap-token.json"), "utf8"),
        ).token,
      );
    await page
      .getByLabel("设置管理密码", { exact: true })
      .fill(account.password);
    await page.getByLabel("再次输入密码").fill(account.password);
    await page.getByRole("button", { name: "创建账户并进入控制台" }).click();
  }
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
}
async function shot(page: Page, name: string) {
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.evaluate(() => document.fonts.ready);
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, name),
    fullPage: true,
    animations: "disabled",
  });
}

test("task trigger UI persists reviewed drafts, executes actual reminders and imports model proposals without auto-enabling", async ({
  page,
  baseURL,
}) => {
  test.setTimeout(120_000);
  expect(new URL(baseURL!).port).toBe("18421");
  const failures: string[] = [];
  page.on("pageerror", (error) => failures.push(error.message));
  await login(page);
  const auth = await (await page.request.get(prefix + "/auth/me")).json();
  const headers = { "X-CSRF-Token": auth.csrf_token, Origin: baseURL! };
  await page.getByRole("button", { name: "任务触发", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "触发任务", exact: true }),
  ).toBeVisible();
  const caps = await (
    await page.request.get(prefix + "/task-triggers/capabilities")
  ).json();
  if (!caps.clock.trusted) {
    page.once("dialog", (dialog) => dialog.accept());
    await page
      .getByRole("button", { name: "核对并确认服务器时间", exact: true })
      .click();
    await expect(page.getByRole("status")).toContainText(
      "服务器时钟已人工确认",
    );
  }
  await page.getByRole("button", { name: "新建触发任务", exact: true }).click();
  const title = "浏览器提醒_" + crypto.randomBytes(3).toString("hex");
  await page.getByLabel("任务名称", { exact: true }).fill(title);
  await page
    .getByLabel("固定提醒内容", { exact: true })
    .fill("这是一条合成浏览器验收提醒。");
  await page.getByLabel("首次启用后等待（秒）", { exact: true }).fill("2");
  const savedResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/task-triggers/jobs") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "保存停用草稿", exact: true }).click();
  const saved = await (await savedResponse).json();
  expect(saved.lifecycle).toBe("draft");
  expect(saved.active_revision).toBeNull();
  expect(
    (
      await (
        await page.request.get(
          `${prefix}/task-triggers/jobs/${saved.id}/occurrences`,
        )
      ).json()
    ).items,
  ).toHaveLength(0);
  await expect(
    page.getByRole("button", { name: "应用并启用", exact: true }),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "校验并预演规则", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "规则校验通过", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("以下按现在启用估算", { exact: false }),
  ).toBeVisible();
  await page.getByRole("button", { name: "应用并启用", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("任务版本已应用并启用");
  await expect
    .poll(
      async () => {
        const records = await (
          await page.request.get(
            `${prefix}/task-triggers/jobs/${saved.id}/occurrences`,
          )
        ).json();
        return records.items.find(
          (item: { status: string }) => item.status === "completed",
        )?.status;
      },
      { timeout: 15000 },
    )
    .toBe("completed");
  await page.getByRole("button", { name: "刷新执行记录", exact: true }).click();
  await page.locator(".trigger-occurrence").first().click();
  await expect(page.getByLabel("触发任务结果")).toContainText(
    "这是一条合成浏览器验收提醒",
  );
  await expect(page.getByLabel("触发任务结果")).toContainText(
    "已保存到结果中心",
  );
  await expect(page.locator(".trigger-job.active")).toContainText("已完成");
  await shot(page, "reminder-result-desktop.png");

  const fixture = JSON.parse(
    fs.readFileSync(path.resolve(dataDir, "../fixture-info.json"), "utf8"),
  );
  const current = await (await page.request.get(prefix + "/config")).json();
  const secret =
    current.config.model.credential_ref ||
    (
      await (
        await page.request.post(prefix + "/secrets", {
          headers,
          data: {
            kind: "model_api_key",
            value: "synthetic-trigger-browser-key",
          },
        })
      ).json()
    ).id;
  const config = {
    ...current.config,
    model: {
      base_url: fixture.model_url,
      model: "synthetic-trigger-model",
      credential_ref: secret,
      image_generation: false,
    },
  };
  const draft = await (
    await page.request.post(prefix + "/config/drafts", {
      headers,
      data: { base_revision: current.revision, config },
    })
  ).json();
  expect(
    (
      await (
        await page.request.post(
          `${prefix}/config/drafts/${draft.id}/validate`,
          { headers, data: {} },
        )
      ).json()
    ).valid,
  ).toBe(true);
  expect(
    (
      await page.request.post(`${prefix}/config/drafts/${draft.id}/apply`, {
        headers: { ...headers, "If-Match": `"${current.revision}"` },
        data: {},
      })
    ).status(),
  ).toBe(202);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "任务触发", exact: true }),
  ).toBeVisible();
  const before = (
    await (await page.request.get(prefix + "/task-triggers/jobs")).json()
  ).total;
  await page.getByRole("button", { name: "对话工作台", exact: true }).click();
  await page
    .getByRole("combobox", { name: "本次 Skill", exact: true })
    .selectOption("task-trigger-setup");
  await page
    .getByLabel("对话消息", { exact: true })
    .fill(
      "task-trigger-proposal-probe 请整理一个首次启用后2秒的提醒草稿，先不启用。",
    );
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "审核任务草稿", exact: true }),
  ).toBeVisible();
  expect(
    (await (await page.request.get(prefix + "/task-triggers/jobs")).json())
      .total,
  ).toBe(before);
  await page.getByRole("button", { name: "审核任务草稿", exact: true }).click();
  await expect(page.getByLabel("任务名称", { exact: true })).not.toHaveValue(
    "",
  );
  await expect(page.getByRole("status")).toContainText(
    "这是模型生成的任务草稿",
  );
  const importedSave = page.waitForResponse(
    (response) =>
      response.url().endsWith("/task-triggers/jobs") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "保存停用草稿", exact: true }).click();
  const imported = await (await importedSave).json();
  expect(imported.lifecycle).toBe("draft");
  expect(imported.active_revision).toBeNull();
  await expect(page.getByRole("status")).toContainText("任务已保存为停用草稿");
  await page.getByText("管理中国年度日历", { exact: true }).click();
  const calendars = await (
    await page.request.get(prefix + "/task-triggers/calendars")
  ).json();
  const official = calendars.items.find(
    (item: { active: boolean; document: { year: number } }) =>
      item.active && item.document.year === 2026,
  );
  expect(official).toBeTruthy();
  await page
    .getByLabel("官方年度日历 JSON", { exact: true })
    .fill(JSON.stringify(official.document));
  const calendarSave = page.waitForResponse(
    (response) =>
      response.url().endsWith("/task-triggers/calendars/drafts") &&
      response.request().method() === "POST",
  );
  await page
    .getByRole("button", { name: "保存日历导入草稿", exact: true })
    .click();
  expect((await (await calendarSave).json()).active).toBe(false);
  await expect(
    page.getByRole("button", { name: "核验并应用日历版本", exact: true }),
  ).toBeDisabled();
  await page
    .getByLabel("我已核对官方来源、放假及补班日期", { exact: true })
    .check();
  page.once("dialog", (dialog) => dialog.accept());
  await page
    .getByRole("button", { name: "核验并应用日历版本", exact: true })
    .click();
  await expect(page.getByRole("status")).toContainText("已核验日历版本已应用");
  await page
    .getByRole("combobox", { name: "触发方式", exact: true })
    .selectOption("workday");
  await page
    .getByRole("button", { name: "校验并预演规则", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "规则校验通过", exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("触发规则预演")).toContainText("Asia/Shanghai");
  await shot(page, "workday-preview-desktop.png");
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await expect
      .poll(() =>
        page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      )
      .toBe(true);
    await shot(page, `task-trigger-${width}.png`);
  }
  expect(failures).toEqual([]);
});
