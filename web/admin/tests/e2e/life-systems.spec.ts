import { test, expect, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const data = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/life-e2e/data",
);
const evidence = path.resolve(
  process.env.DSH_E2E_EVIDENCE_DIR || "../../.local/life-e2e/evidence",
);
async function authenticate(page: Page) {
  const accountFile = path.join(data, "life-account.json");
  const account = fs.existsSync(accountFile)
    ? JSON.parse(fs.readFileSync(accountFile, "utf8"))
    : {
        username: "life_" + crypto.randomBytes(4).toString("hex"),
        password: crypto.randomBytes(24).toString("base64url"),
      };
  if (!fs.existsSync(accountFile))
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      flag: "wx",
      mode: 0o600,
    });
  const status = await (
    await page.request.get("/api/admin/v1/auth/status")
  ).json();
  await page.goto("/admin/");
  await page.getByLabel("管理员用户名").fill(account.username);
  if (!status.initialized) {
    const pairing = JSON.parse(
      fs.readFileSync(path.join(data, "bootstrap-token.json"), "utf8"),
    );
    await page.getByLabel("一次性配对凭证").fill(pairing.token);
    await page
      .getByLabel("设置管理密码", { exact: true })
      .fill(account.password);
    await page.getByLabel("再次输入密码").fill(account.password);
    await page
      .getByRole("button", { name: "创建账户并进入控制台", exact: true })
      .click();
  } else {
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  }
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  return (await (await page.request.get("/api/admin/v1/auth/me")).json())
    .csrf_token as string;
}
async function screenshot(page: Page, filename: string) {
  fs.mkdirSync(evidence, { recursive: true });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({
    path: path.join(evidence, filename),
    fullPage: true,
    animations: "disabled",
  });
}

test("diary provenance, reviewed people and calendar work through the authenticated browser", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(180000);
  page.setDefaultTimeout(15000);
  expect(new URL(baseURL!).port).toBe("18421");
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const anonymous = await browser.newContext();
  for (const endpoint of [
    "diary/entries",
    "relationships/people",
    "schedule/calendar?start=1&end=2",
  ])
    expect(
      (
        await anonymous.request.get(`${baseURL}/api/admin/v1/${endpoint}`)
      ).status(),
    ).toBe(401);
  await anonymous.close();
  const csrf = await authenticate(page);
  const headers = { Origin: new URL(baseURL!).origin, "X-CSRF-Token": csrf };
  await page.request.post("/api/admin/v1/conversations", {
    headers,
    data: { title: "浏览器合成来源" },
  });
  await page.request.post("/api/admin/v1/task-triggers/clock/acknowledge", {
    headers,
    data: { observed_now: Math.floor(Date.now() / 1000), confirmed: true },
  });
  const nav = page.getByRole("navigation", { name: "主导航" });
  await nav.getByRole("button", { name: "日记与资料", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "把经历慢慢记下来" }),
  ).toBeVisible();
  await page.getByRole("tab", { name: "待整理事件", exact: true }).click();
  const marker = `浏览器合成经历-${crypto.randomBytes(3).toString("hex")}`;
  const personName = `小禾（验收${crypto.randomBytes(3).toString("hex")}）`;
  await page.getByLabel("内容", { exact: true }).fill(marker);
  await page.getByRole("button", { name: "保存事件", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("已记下");
  await page.getByRole("tab", { name: "来源与整理", exact: true }).click();
  await page.getByRole("button", { name: "整理并保存", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("整理结果已保存");
  await page.getByRole("tab", { name: "日记与资料", exact: true }).click();
  await page.getByLabel("搜索", { exact: true }).fill(marker);
  await page.getByRole("button", { name: "查询", exact: true }).click();
  await page.locator(".diary-card").first().click();
  await expect(page.getByRole("article", { name: "日记详情" })).toContainText(
    marker,
  );
  await screenshot(page, "00-diary-with-sources-desktop.png");
  await page.getByText(/来源与版本 ·/).click();
  await page
    .locator(".life-evidence")
    .filter({ hasText: marker })
    .getByRole("button", { name: "更正", exact: true })
    .click();
  await page.getByLabel("正确内容", { exact: true }).fill(`${marker}-已更正`);
  await page.getByRole("button", { name: "保存更正", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(
    "旧日记与派生资料已失效",
  );
  await screenshot(page, "01-diary-desktop.png");

  await nav.getByRole("button", { name: "人际关系", exact: true }).click();
  await page.getByRole("button", { name: "建立人物", exact: true }).click();
  await page.getByLabel("称呼", { exact: true }).fill(personName);
  await page
    .getByLabel("建立依据", { exact: true })
    .fill("隔离的合成人物，用于浏览器验收。");
  await page.getByRole("button", { name: "保存人物", exact: true }).click();
  await expect(
    page.getByRole("status").filter({ hasText: "人物已建立" }),
  ).toContainText("人物已建立");
  await page
    .getByRole("complementary", { name: "人物列表" })
    .getByRole("button")
    .filter({ hasText: personName })
    .click();
  await page.getByText("提交资料候选", { exact: true }).click();
  await page
    .getByLabel("字段内容", { exact: true })
    .fill("喜欢观察星空（合成事实）");
  await page
    .getByLabel("来源与依据", { exact: true })
    .fill("合成人物明确陈述。");
  await page.getByRole("button", { name: "提交候选", exact: true }).click();
  await expect(
    page.getByText("喜欢观察星空（合成事实）", { exact: false }),
  ).toBeVisible();
  await page
    .getByLabel("审核理由", { exact: true })
    .fill("仅批准合成验收资料。");
  await page.getByRole("button", { name: "批准资料", exact: true }).click();
  await expect(page.getByText("已审核", { exact: true })).toBeVisible();
  await screenshot(page, "02-relationships-desktop.png");

  await nav.getByRole("button", { name: "日程", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "安排今天，也看见进展" }),
  ).toBeVisible();
  await page.getByRole("tab", { name: "授权书架", exact: true }).click();
  await page
    .getByLabel("资料名称", { exact: true })
    .fill("星空入门（合成资料）");
  await page
    .getByLabel("正文", { exact: true })
    .fill("这是一份合成验收资料。观察记录应包含日期、地点和实际观测范围。");
  await page.getByLabel("我有权提供这些资料", { exact: false }).check();
  await page.getByRole("button", { name: "保存到书架", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("资料已保存");
  await page.getByRole("tab", { name: "日历", exact: true }).click();
  await page.getByRole("button", { name: "新建活动", exact: true }).click();
  await page.getByLabel("活动名称", { exact: true }).fill("合成阅读日程");
  await page
    .getByLabel("目的", { exact: true })
    .fill("验证资料来源、时间和真实进度。");
  const now = Date.now() + 3600000;
  const local = (at: number) =>
    new Date(at + 8 * 3600000).toISOString().slice(0, 16);
  await page.getByLabel("计划开始", { exact: true }).fill(local(now));
  await page
    .getByLabel("计划结束", { exact: true })
    .fill(local(now + 20 * 60000));
  const resource = (
    await (await page.request.get("/api/admin/v1/schedule/resources")).json()
  ).items[0];
  await page
    .getByRole("combobox", { name: "资料", exact: true })
    .selectOption(resource.id);
  await page
    .getByRole("button", { name: "预演冲突与机会", exact: true })
    .click();
  await expect(
    page.getByText("1 次机会 · 0 项冲突", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "保存日程", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("日程已保存");
  await expect(
    page.locator(".calendar-event").filter({ hasText: "合成阅读日程" }),
  ).toBeVisible();
  await screenshot(page, "03-schedule-week-desktop.png");
  const pending = await page.request.post("/api/admin/v1/schedule/proposals", {
    headers: { ...headers, "Idempotency-Key": crypto.randomUUID() },
    data: {
      title: "合成待批系列",
      type: "reading",
      purpose: "仅用于验证明确人工批准。",
      start_at: Math.floor(now / 1000) + 3600,
      end_at: Math.floor(now / 1000) + 4800,
      time_zone: "Asia/Shanghai",
      resource_id: resource.id,
      target_ref: null,
      grant_id: null,
      repeat: null,
      movable: true,
    },
  });
  expect(pending.ok()).toBe(true);
  const proposal = await pending.json();
  expect(proposal.state).toBe("pending_approval");
  await page.getByRole("button", { name: "刷新", exact: true }).click();
  await page
    .locator(".calendar-event")
    .filter({ hasText: "合成待批系列" })
    .click();
  const approval = page.getByRole("region", {
    name: "人工批准日程",
    exact: true,
  });
  await expect(
    approval.getByRole("button", { name: "批准已保存系列", exact: true }),
  ).toBeDisabled();
  await approval
    .getByLabel("我已核对定义与来源，明确批准整个已保存系列")
    .check();
  await approval
    .getByRole("button", { name: "批准已保存系列", exact: true })
    .click();
  await expect(page.getByRole("status")).toContainText("已明确批准");
  const approved = await (
    await page.request.get(`/api/admin/v1/schedule/activities/${proposal.id}`)
  ).json();
  expect(approved.state).toBe("enabled");
  expect(approved.creator).toBe("autonomous");
  await page
    .getByRole("combobox", { name: "视图", exact: true })
    .selectOption("month");
  await expect(page.locator(".calendar-day-label")).toHaveCount(42);
  await screenshot(page, "04-schedule-month-desktop.png");
  await page.setViewportSize({ width: 390, height: 844 });
  await page
    .getByRole("combobox", { name: "视图", exact: true })
    .selectOption("agenda");
  await expect(
    page.locator(".agenda-item").filter({ hasText: "合成阅读日程" }),
  ).toBeVisible();
  await screenshot(page, "05-schedule-mobile-agenda.png");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  expect(errors).toEqual([]);
});
