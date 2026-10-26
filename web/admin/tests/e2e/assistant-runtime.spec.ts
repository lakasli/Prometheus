import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { selectValue } from "./select-helpers";

const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/assistant-runtime-e2e/data",
);
const accountFile =
  process.env.DSH_E2E_ACCOUNT_FILE || path.join(dataDir, "e2e-account.json");
const evidenceDir = path.resolve(
  process.env.DSH_E2E_EVIDENCE_DIR ||
    "../../.local/assistant-runtime-e2e/screenshots",
);
async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `runtime_${crypto.randomBytes(4).toString("hex")}`,
        password: crypto.randomBytes(24).toString("base64url"),
      };
  if (!fs.existsSync(accountFile))
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      mode: 0o600,
      flag: "wx",
    });
  const status = await (
    await page.request.get("/api/admin/v1/auth/status")
  ).json();
  await page.goto("/admin/");
  if (!status.initialized) {
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
    await page.getByLabel("管理员用户名").fill(account.username);
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  }
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
}
async function screenshot(page: Page, name: string) {
  if (await page.locator(".runtime-log-list").count())
    await expect(page.locator(".runtime-log-list")).toHaveAttribute(
      "aria-busy",
      "false",
    );
  if (await page.locator(".personality-panel").count())
    await expect(
      page
        .getByRole("region", { name: "她的性格", exact: true })
        .getByLabel("姓名", { exact: true }),
    ).toBeVisible();
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.evaluate(() => document.fonts.ready);
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, name),
    fullPage: true,
    animations: "disabled",
  });
}
test("single assistant personality and authenticated runtime journal replace retired workspaces", async ({
  page,
  baseURL,
  browser,
}) => {
  test.setTimeout(120_000);
  expect(new URL(baseURL!).port, "use the isolated service on 18421").toBe(
    "18421",
  );
  const faults: string[] = [];
  page.on("pageerror", (error) => faults.push(error.message));
  const anonymous = await browser.newContext();
  for (const endpoint of ["assistant/personality", "runtime-logs"])
    expect(
      (
        await anonymous.request.get(`${baseURL}/api/admin/v1/${endpoint}`)
      ).status(),
    ).toBe(401);
  await anonymous.close();
  await authenticate(page);
  const navigation = page.getByRole("navigation", { name: "主导航" });
  for (const retired of ["角色卡", "对话工作台", "任务与诊断"])
    await expect(
      navigation.getByRole("button", { name: retired, exact: true }),
    ).toHaveCount(0);
  await navigation
    .getByRole("button", { name: "性格与模型", exact: true })
    .click();
  const panel = page.getByRole("region", { name: "她的性格", exact: true });
  await expect(panel.getByLabel("姓名", { exact: true })).toBeVisible();
  const original = await (
    await page.request.get("/api/admin/v1/assistant/personality")
  ).json();
  expect(original.personality.name).toBe("刘嘉卉");
  await expect(panel.getByLabel("姓名", { exact: true })).toHaveValue("刘嘉卉");
  const displayedPersonality = await panel
    .locator("input, textarea")
    .evaluateAll((fields) =>
      fields
        .map((field) => (field as HTMLInputElement | HTMLTextAreaElement).value)
        .join("\n"),
    );
  expect(
    `${await page.locator("body").innerText()}\n${displayedPersonality}`,
  ).not.toMatch(/助手|\bAI\b|工具|\bassistant\b/i);
  await screenshot(page, "00-liu-jiahui-initial-desktop.png");
  const csrf = (await (await page.request.get("/api/admin/v1/auth/me")).json())
    .csrf_token as string;
  const mutationHeaders = {
    Origin: new URL(baseURL!).origin,
    "X-CSRF-Token": csrf,
  };
  const assistantName = `验收性格-${crypto.randomBytes(3).toString("hex")}`;
  await panel.getByLabel("姓名", { exact: true }).fill(assistantName);
  await panel
    .getByLabel("性情与脾气", { exact: true })
    .fill("温和、坦诚，统一服务所有会话。");
  await panel
    .getByRole("button", { name: "保存并应用性格", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText("已保存并生效");
  const saved = await (
    await page.request.get("/api/admin/v1/assistant/personality")
  ).json();
  expect(saved.personality.name).toBe(assistantName);
  expect(saved.etag).not.toBe(original.etag);
  expect(saved.applied).toBe(true);
  const conflict = await page.request.put(
    "/api/admin/v1/assistant/personality",
    {
      headers: { ...mutationHeaders, "If-Match": `"${original.etag}"` },
      data: { personality: original.personality },
    },
  );
  expect(conflict.status()).toBe(409);
  const concurrent = await page.request.put(
    "/api/admin/v1/assistant/personality",
    {
      headers: { ...mutationHeaders, "If-Match": `"${saved.etag}"` },
      data: {
        personality: {
          ...saved.personality,
          description: "另一个配置窗口保存",
        },
      },
    },
  );
  expect(concurrent.ok()).toBe(true);
  await panel.getByLabel("相处情境", { exact: true }).fill("本地未保存内容");
  await panel
    .getByRole("button", { name: "保存并应用性格", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText("你的修改仍保留");
  await expect(panel.getByLabel("相处情境", { exact: true })).toHaveValue(
    "本地未保存内容",
  );
  page.once("dialog", (dialog) => dialog.accept());
  await panel
    .getByRole("button", { name: "重新载入性格", exact: true })
    .click();
  await expect(panel.getByLabel("个人简介", { exact: true })).toHaveValue(
    "另一个配置窗口保存",
  );
  await panel.getByText("性格版本与恢复", { exact: true }).click();
  const oldVersions = await (
    await page.request.get("/api/admin/v1/assistant/personality/history")
  ).json();
  expect(oldVersions.items.length).toBeGreaterThan(0);
  await selectValue(
    page,
    panel.getByRole("combobox", { name: "历史性格版本", exact: true }),
    String(oldVersions.items[0].revision),
  );
  const beforeRestore = await (
    await page.request.get("/api/admin/v1/assistant/personality")
  ).json();
  await panel.getByRole("button", { name: "恢复此版本", exact: true }).click();
  await expect(panel.getByRole("status")).toContainText("已保存并生效");
  const restored = await (
    await page.request.get("/api/admin/v1/assistant/personality")
  ).json();
  expect(restored.revision).toBeGreaterThan(beforeRestore.revision);
  expect(restored.personality).toEqual(oldVersions.items[0].personality);
  const fixtureInfo = JSON.parse(
    fs.readFileSync(
      process.env.DSH_E2E_FIXTURE_INFO ||
        path.resolve(
          "../../.local/assistant-logs-e2e/output/fixture-info.json",
        ),
      "utf8",
    ),
  );
  expect(new URL(fixtureInfo.model_url).hostname).toBe("127.0.0.1");
  expect(fixtureInfo.production_data_accessed).toBe(false);
  const secretResponse = await page.request.post("/api/admin/v1/secrets", {
    headers: mutationHeaders,
    data: { kind: "model_api_key", value: `synthetic-${crypto.randomUUID()}` },
  });
  expect(secretResponse.ok()).toBe(true);
  const secret = await secretResponse.json();
  const config = await (await page.request.get("/api/admin/v1/config")).json();
  config.config.model = {
    ...config.config.model,
    base_url: fixtureInfo.model_url,
    model: "synthetic-browser-model",
    credential_ref: secret.id,
  };
  config.config.qq.enabled = false;
  const draftResponse = await page.request.post("/api/admin/v1/config/drafts", {
    headers: mutationHeaders,
    data: { base_revision: config.revision, config: config.config },
  });
  expect(draftResponse.ok()).toBe(true);
  const draft = await draftResponse.json();
  const validation = await (
    await page.request.post(
      `/api/admin/v1/config/drafts/${draft.id}/validate`,
      { headers: mutationHeaders },
    )
  ).json();
  expect(validation.valid).toBe(true);
  const application = await page.request.post(
    `/api/admin/v1/config/drafts/${draft.id}/apply`,
    { headers: { ...mutationHeaders, "If-Match": `"${config.revision}"` } },
  );
  expect(application.ok()).toBe(true);
  await expect
    .poll(
      async () =>
        (await (await page.request.get("/api/admin/v1/config")).json()).config
          .model.model,
    )
    .toBe("synthetic-browser-model");
  const conversationResponse = await page.request.post(
    "/api/admin/v1/conversations",
    { headers: mutationHeaders, data: { title: "运行日志合成验收" } },
  );
  expect(conversationResponse.ok()).toBe(true);
  const conversation = await conversationResponse.json();
  const syntheticInput = `runtime-browser-${crypto.randomUUID()}`;
  const submissionResponse = await page.request.post(
    `/api/admin/v1/conversations/${conversation.id}/messages`,
    {
      headers: mutationHeaders,
      data: { content: syntheticInput, client_message_id: crypto.randomUUID() },
    },
  );
  expect(submissionResponse.status()).toBe(202);
  const submission = await submissionResponse.json();
  await expect
    .poll(
      async () =>
        (
          await (
            await page.request.get(`/api/admin/v1/runs/${submission.run_id}`)
          ).json()
        ).status,
    )
    .toBe("completed");
  await screenshot(page, "01-personality-desktop.png");
  await navigation
    .getByRole("button", { name: "日志记录", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "日志记录", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "新建对话", exact: true }),
  ).toHaveCount(0);
  await expect(page.locator(".conversation-composer")).toHaveCount(0);
  await expect(
    page.getByText("协作分工尚未接入。", { exact: false }),
  ).toBeVisible();
  await selectValue(
    page,
    page.getByRole("combobox", { name: "记录类型", exact: true }),
    "input",
  );
  await page.getByLabel("Run ID", { exact: true }).fill(submission.run_id);
  await page.getByRole("button", { name: "筛选日志", exact: true }).click();
  await expect(page.locator(".runtime-log-list")).toHaveAttribute(
    "aria-busy",
    "false",
  );
  await expect(page.locator(".runtime-log-entry").first()).toBeVisible();
  await page.locator(".runtime-log-summary").first().click();
  await expect(page.locator(".runtime-log-detail").first()).toContainText(
    syntheticInput,
  );
  await screenshot(page, "02-runtime-logs-desktop.png");
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `03-runtime-logs-${width}.png`);
    await page.getByRole("button", { name: "打开导航", exact: true }).click();
    await navigation
      .getByRole("button", { name: "性格与模型", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "她的性格", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `04-personality-${width}.png`);
    await page.getByRole("button", { name: "打开导航", exact: true }).click();
    await navigation
      .getByRole("button", { name: "日志记录", exact: true })
      .click();
  }
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(faults).toEqual([]);
});
