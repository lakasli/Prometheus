import { test, expect, type Locator, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const dataDir = path.resolve(
  process.env.DSH_CONFIGURATION_E2E_DATA_DIR ||
    "../../.local/configuration-layout-e2e/data",
);
const accountFile = path.resolve(
  process.env.DSH_CONFIGURATION_E2E_ACCOUNT_FILE ||
    path.join(dataDir, "configuration-account.json"),
);
const evidenceDir = path.resolve(
  process.env.DSH_CONFIGURATION_E2E_EVIDENCE_DIR ||
    "../../.local/configuration-layout-e2e/evidence",
);

function localModelUrl() {
  const fixtureFile = process.env.DSH_CONFIGURATION_E2E_FIXTURE_INFO;
  const fixture = fixtureFile
    ? JSON.parse(fs.readFileSync(fixtureFile, "utf8"))
    : undefined;
  if (fixture) expect(fixture.production_data_accessed).toBe(false);
  const value =
    process.env.DSH_CONFIGURATION_E2E_MODEL_URL || fixture?.model_url;
  if (!value)
    throw new Error(
      "Set DSH_CONFIGURATION_E2E_MODEL_URL to an isolated loopback model fixture, or DSH_CONFIGURATION_E2E_FIXTURE_INFO to its JSON metadata.",
    );
  const url = new URL(value);
  expect(["localhost", "127.0.0.1", "[::1]"]).toContain(url.hostname);
  expect(url.port).not.toBe("18419");
  return url.href.replace(/\/$/, "");
}

async function authenticate(page: Page) {
  const status = await (
    await page.request.get("/api/admin/v1/auth/status")
  ).json();
  if (status.initialized && !fs.existsSync(accountFile))
    throw new Error(
      "The isolated service is initialized. Set DSH_CONFIGURATION_E2E_ACCOUNT_FILE to its existing test-account file.",
    );
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? JSON.parse(fs.readFileSync(accountFile, "utf8"))
    : {
        username: `configuration_${crypto.randomBytes(4).toString("hex")}`,
        password: crypto.randomBytes(24).toString("base64url"),
      };
  if (!fs.existsSync(accountFile)) {
    fs.mkdirSync(path.dirname(accountFile), { recursive: true });
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      mode: 0o600,
      flag: "wx",
    });
  }
  await page.goto("/admin/");
  await page.getByLabel("管理员用户名").fill(account.username);
  if (!status.initialized) {
    const pairing = JSON.parse(
      fs.readFileSync(path.join(dataDir, "bootstrap-token.json"), "utf8"),
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

function drawer(page: Page, title: string) {
  return page.locator("details.config-drawer").filter({
    has: page.getByText(title, { exact: true }),
  });
}

async function setDrawer(panel: Locator, open: boolean) {
  if (((await panel.getAttribute("open")) !== null) === open) return;
  await panel.locator("summary").first().click();
  await expect
    .poll(async () => (await panel.getAttribute("open")) !== null)
    .toBe(open);
}

async function navigate(page: Page, title: string) {
  const toggle = page.getByRole("button", { name: "打开导航", exact: true });
  if (await toggle.isVisible()) await toggle.click();
  await page
    .getByRole("navigation", { name: "主导航" })
    .getByRole("button", { name: title, exact: true })
    .click();
}

async function screenshot(page: Page, name: string) {
  await expect(page.locator(".activity-bar")).toHaveCount(0);
  await page.evaluate(() => document.fonts.ready);
  await page.evaluate(() => window.scrollTo(0, 0));
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, name),
    fullPage: true,
    animations: "disabled",
  });
}

test("configuration drawers retain authenticated configuration flows after the page reorganization", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(180_000);
  page.setDefaultTimeout(15_000);
  const target = new URL(baseURL!);
  expect(["localhost", "127.0.0.1", "[::1]"]).toContain(target.hostname);
  expect(
    target.port,
    "never write configuration through the production tunnel",
  ).not.toBe("18419");
  expect(target.port, "an explicit isolated service port is required").not.toBe(
    "",
  );
  const modelUrl = localModelUrl();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  let affinityReads = 0;
  page.on("request", (request) => {
    if (
      request.method() === "GET" &&
      new URL(request.url()).pathname ===
        "/api/admin/v1/relationships/affinity-policy"
    )
      affinityReads++;
  });
  const anonymous = await browser.newContext();
  for (const endpoint of [
    "config",
    "config/revisions",
    "secrets",
    "relationships/affinity-policy",
  ])
    expect(
      (
        await anonymous.request.get(`${target.origin}/api/admin/v1/${endpoint}`)
      ).status(),
    ).toBe(401);
  await anonymous.close();
  const csrf = await authenticate(page);
  const headers = { Origin: target.origin, "X-CSRF-Token": csrf };
  const nav = page.getByRole("navigation", { name: "主导航" });
  for (const retired of ["性格与模型", "配置历史"])
    await expect(
      nav.getByRole("button", { name: retired, exact: true }),
    ).toHaveCount(0);
  await navigate(page, "性格");
  await expect(
    page.getByRole("region", { name: "她的性格", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".settings-summary")).toHaveCount(0);
  await expect(page.getByLabel("API 基础地址", { exact: true })).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "工作节奏", exact: true }),
  ).toHaveCount(0);
  await screenshot(page, "00-personality-desktop.png");
  await navigate(page, "日程");
  for (const retired of ["授权书架", "兼容触发设置"])
    await expect(
      page.getByRole("tab", { name: retired, exact: true }),
    ).toHaveCount(0);
  await expect(
    page.getByRole("tab", { name: "日历", exact: true }),
  ).toBeVisible();
  await navigate(page, "QQ 连接");
  await expect(
    page.getByRole("heading", { name: "实际运行状态", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "测试连接", exact: true }),
  ).toBeVisible();
  for (const retired of ["QQ 能力与接口", "高级接口管理", "运行只读连接探测"])
    await expect(page.getByText(retired, { exact: true })).toHaveCount(0);
  await expect(page.getByText(/QQ 不确定投递核对/)).toHaveCount(0);
  await screenshot(page, "01-qq-connection-desktop.png");
  await navigate(page, "人际关系");
  await expect(
    page.getByText("好感规则与停机预演", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "载入好感规则", exact: true }),
  ).toHaveCount(0);

  await navigate(page, "配置");
  await expect(page).toHaveURL(/#config$/);
  await expect(
    page.getByRole("heading", { level: 1, name: "配置", exact: true }),
  ).toBeVisible();
  const titles = [
    "模型配置",
    "工作节奏",
    "绘画能力",
    "好感规则配置",
    "配置历史",
  ];
  await expect(page.locator("details.config-drawer > summary")).toHaveText(
    titles,
  );
  for (const title of titles)
    await expect(drawer(page, title)).not.toHaveAttribute("open");
  expect(
    affinityReads,
    "affinity rules load only after their drawer opens",
  ).toBe(0);
  await expect(page.locator(".affinity-settings")).toHaveCount(0);
  await screenshot(page, "02-configuration-collapsed-desktop.png");
  const model = drawer(page, "模型配置");
  const modelSummary = model.locator("summary").first();
  await modelSummary.focus();
  await page.keyboard.press("Enter");
  await expect(model).toHaveAttribute("open");
  await page.keyboard.press("Space");
  await expect(model).not.toHaveAttribute("open");
  await setDrawer(model, true);
  const rhythm = drawer(page, "工作节奏");
  await setDrawer(rhythm, true);

  const before = await (await page.request.get("/api/admin/v1/config")).json();
  const denied = await page.request.post("/api/admin/v1/config/drafts", {
    headers: { Origin: target.origin },
    data: { base_revision: before.revision, config: before.config },
  });
  expect(denied.status(), "authenticated writes still require CSRF").toBe(403);
  const modelName = `configuration-browser-${crypto.randomBytes(4).toString("hex")}`;
  const syntheticKey = `synthetic-configuration-${crypto.randomUUID()}`;
  const key = model.getByPlaceholder("输入新的 API Key");
  await expect(key).toHaveAttribute("type", "password");
  await expect(key).toHaveValue("");
  await model.getByPlaceholder("https://api.deepseek.com").fill(modelUrl);
  await model.getByLabel("模型标识", { exact: true }).fill(modelName);
  await key.fill(syntheticKey);
  const timeout = rhythm.getByLabel("任务超时（秒）", { exact: true });
  const originalTimeout = before.config.assistant.timeout_seconds;
  const nextTimeout = originalTimeout === 59 ? 60 : 59;
  await timeout.fill(String(nextTimeout));
  // Moving between the shared configuration pages must retain pending fields.
  await navigate(page, "QQ 连接");
  await page
    .getByRole("checkbox", { name: "启用 QQ 机器人", exact: true })
    .uncheck();
  await navigate(page, "配置");
  await setDrawer(model, true);
  await setDrawer(rhythm, true);
  await expect(model.getByLabel("模型标识", { exact: true })).toHaveValue(
    modelName,
  );
  await expect(key).toHaveValue(syntheticKey);
  await expect(timeout).toHaveValue(String(nextTimeout));
  const savedResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/api/admin/v1/config/drafts") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "保存草稿", exact: true }).click();
  const saved = await savedResponse;
  expect(saved.ok()).toBe(true);
  const draft = await saved.json();
  expect(draft.config.model.model).toBe(modelName);
  expect(draft.config.assistant.timeout_seconds).toBe(nextTimeout);
  expect(draft.config.qq.enabled).toBe(false);
  await expect(key).toHaveValue("");
  expect(JSON.stringify(draft)).not.toContain(syntheticKey);
  await page.getByRole("button", { name: "校验配置", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "应用配置", exact: true }),
  ).toBeEnabled();
  const checkedResponse = page.waitForResponse((response) =>
    response.url().endsWith(`/config/drafts/${draft.id}/checks`),
  );
  await page.getByRole("button", { name: "测试连接", exact: true }).click();
  const checked = await checkedResponse;
  expect(checked.ok()).toBe(true);
  expect(
    (await checked.json()).ok,
    "the loopback model fixture answers the connection test",
  ).toBe(true);
  await page.getByRole("button", { name: "应用配置", exact: true }).click();
  await expect
    .poll(
      async () =>
        (await (await page.request.get("/api/admin/v1/config")).json()).config
          .model.model,
    )
    .toBe(modelName);
  const applied = await (await page.request.get("/api/admin/v1/config")).json();
  expect(applied.revision).toBeGreaterThan(before.revision);
  expect(applied.applied_revision).toBe(applied.revision);
  expect(applied.config.assistant.timeout_seconds).toBe(nextTimeout);
  const stale = await page.request.post(
    `/api/admin/v1/config/drafts/${draft.id}/apply`,
    {
      headers: { ...headers, "If-Match": `"${before.revision}"` },
    },
  );
  expect(
    stale.status(),
    "a stale configuration cannot overwrite the applied version",
  ).toBe(409);
  for (const endpoint of ["config", "config/revisions", "secrets"])
    expect(
      await (await page.request.get(`/api/admin/v1/${endpoint}`)).text(),
    ).not.toContain(syntheticKey);
  await expect(key).toHaveValue("");
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);

  const affection = drawer(page, "好感规则配置");
  await setDrawer(affection, true);
  await expect(
    affection.getByLabel("新档案初始分", { exact: true }),
  ).toBeVisible();
  expect(affinityReads).toBe(1);
  const policy = await (
    await page.request.get("/api/admin/v1/relationships/affinity-policy")
  ).json();
  const policyFields = {
    initial: "新档案初始分",
    minimum: "下界",
    maximum: "上界",
    daily_decay: "每日空闲衰减",
    grace_seconds: "直接互动后宽限秒数",
    downtime_cap_seconds: "未观测停机区间最多补算秒数",
  };
  for (const [field, label] of Object.entries(policyFields))
    await expect(affection.getByLabel(label, { exact: true })).toHaveValue(
      String(policy[field]),
    );
  await affection
    .getByLabel("调整依据", { exact: true })
    .fill("隔离浏览器验收：保持全部规则参数，仅验证迁移后的保存流程。");
  await affection
    .getByRole("checkbox", {
      name: "明确确认新规则从生效水位向后应用",
      exact: true,
    })
    .check();
  await affection
    .getByRole("button", { name: "保存好感规则", exact: true })
    .click();
  await expect(affection.getByRole("status")).toContainText("新规则已生效");
  const updatedPolicy = await (
    await page.request.get("/api/admin/v1/relationships/affinity-policy")
  ).json();
  expect(updatedPolicy.revision).toBeGreaterThan(policy.revision);
  for (const field of Object.keys(policyFields))
    expect(updatedPolicy[field]).toBe(policy[field]);
  await setDrawer(affection, false);
  await setDrawer(affection, true);
  expect(
    affinityReads,
    "closing and reopening does not remount or discard the loaded form",
  ).toBe(1);
  const history = drawer(page, "配置历史");
  await setDrawer(history, true);
  await history.getByRole("button", { name: "刷新", exact: true }).click();
  await expect(history.locator(".revision-row").first()).toContainText(
    `配置 v${applied.revision}`,
  );
  await expect(history.locator(".revision-row").first()).toContainText(
    modelName,
  );
  await screenshot(page, "03-configuration-expanded-desktop.png");

  // Existing bookmarked history routes should enter the new configuration page.
  await page.goto("/admin/#history");
  await expect(page).toHaveURL(/#config$/);
  await expect(
    page.getByRole("heading", { level: 1, name: "配置", exact: true }),
  ).toBeVisible();
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    for (const title of titles) await setDrawer(drawer(page, title), true);
    await expect(
      drawer(page, "好感规则配置").getByLabel("新档案初始分", { exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `04-configuration-expanded-${width}.png`);
    for (const title of titles) await setDrawer(drawer(page, title), false);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `05-configuration-collapsed-${width}.png`);
    await navigate(page, "性格");
    await expect(
      page.getByRole("region", { name: "她的性格", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `06-personality-${width}.png`);
    await navigate(page, "配置");
  }
  expect(errors).toEqual([]);
});
