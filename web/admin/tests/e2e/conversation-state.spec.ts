import { test, expect, type Locator, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { selectValue } from "./select-helpers";
import type {
  ContextPolicyView,
  ConversationCommitment,
  CommitmentScope,
} from "../../src/context-management";

const prefix = "/api/admin/v1";
interface SelectionEvidence {
  available: boolean;
  selection?: {
    accepted: boolean;
    blocks: {
      required: boolean;
      selected: boolean;
      serialized_bytes: number;
      source_refs: { revision: number }[];
    }[];
  } | null;
}
const evidenceDir = path.resolve(
  process.env.DSH_E2E_EVIDENCE_DIR ||
    "../../.local/conversation-state-e2e/evidence",
);

async function json<T>(page: Page, route: string): Promise<T> {
  const response = await page.request.get(prefix + route);
  expect(response.ok(), `authenticated GET ${route}`).toBe(true);
  return response.json() as Promise<T>;
}

async function navigate(page: Page, name: string) {
  const open = page.getByRole("button", { name: "打开导航", exact: true });
  if (await open.isVisible()) await open.click();
  await page
    .getByRole("navigation", { name: "主导航" })
    .getByRole("button", { name, exact: true })
    .click();
}

function drawer(page: Page, name: string) {
  return page
    .locator("details.config-drawer")
    .filter({ has: page.getByText(name, { exact: true }) });
}

async function openDrawer(control: Locator) {
  if ((await control.getAttribute("open")) === null)
    await control.locator("summary").first().click();
  await expect(control).toHaveAttribute("open");
}

async function choose(page: Page, control: Locator, value: string) {
  if (await control.evaluate((element) => element.tagName === "SELECT"))
    await control.selectOption(value);
  else await selectValue(page, control, value);
}

async function screenshot(page: Page, name: string) {
  await page.evaluate(() => document.fonts.ready);
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, name),
    fullPage: true,
    animations: "disabled",
  });
}

async function authenticate(page: Page) {
  const configuredData = process.env.DSH_E2E_DATA_DIR;
  if (!configuredData)
    throw new Error(
      "DSH_E2E_DATA_DIR must identify a fresh isolated browser fixture",
    );
  const dataDir = path.resolve(configuredData);
  const accountFile = path.resolve(
    process.env.DSH_E2E_ACCOUNT_FILE ||
      path.join(dataDir, "conversation-account.json"),
  );
  const status = await json<{ initialized: boolean }>(page, "/auth/status");
  if (status.initialized && !fs.existsSync(accountFile))
    throw new Error(
      "Initialized fixture requires its existing DSH_E2E_ACCOUNT_FILE; do not reuse another service",
    );
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `conversation_${crypto.randomBytes(4).toString("hex")}`,
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
  const me = await json<{ csrf_token: string }>(page, "/auth/me");
  return { csrf: me.csrf_token, password: account.password };
}

test("conversation evidence uses independent budget revisions, reviewed commitments and read-only runtime logs", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(180_000);
  page.setDefaultTimeout(15_000);
  const origin = new URL(baseURL!).origin;
  expect(["127.0.0.1", "localhost", "[::1]"]).toContain(
    new URL(origin).hostname,
  );
  expect(["", "8419", "18419"]).not.toContain(new URL(origin).port);
  const fixtureFile = process.env.DSH_E2E_FIXTURE_INFO;
  if (!fixtureFile)
    throw new Error(
      "DSH_E2E_FIXTURE_INFO must identify the isolated conversation-state browser fixture",
    );
  const fixture = JSON.parse(fs.readFileSync(fixtureFile, "utf8"));
  expect(fixture.production_data_accessed).toBe(false);
  expect(fixture.real_external_model_tested).toBe(false);
  expect(new URL(fixture.origin).origin).toBe(origin);
  expect(["127.0.0.1", "localhost", "[::1]"]).toContain(
    new URL(fixture.model_url).hostname,
  );
  if (process.env.DSH_E2E_EXPECTED_COMMIT)
    expect(fixture.commit).toBe(process.env.DSH_E2E_EXPECTED_COMMIT);
  const faults: string[] = [];
  const mutations: string[] = [];
  let budgetReads = 0;
  let monitorReadOnly = false;
  page.on("pageerror", (error) => faults.push(error.message));
  page.on("request", (request) => {
    const route = new URL(request.url()).pathname;
    if (request.method() === "GET" && route === prefix + "/context-policy")
      budgetReads++;
    if (
      monitorReadOnly &&
      !["GET", "HEAD", "OPTIONS"].includes(request.method())
    )
      mutations.push(route);
  });
  const anonymous = await browser.newContext();
  for (const route of [
    "/context-policy",
    "/commitments/scopes",
    "/runtime-logs",
  ])
    expect(
      (await anonymous.request.get(origin + prefix + route)).status(),
    ).toBe(401);
  await anonymous.close();
  const account = await authenticate(page);
  const headers = { Origin: origin, "X-CSRF-Token": account.csrf };
  const syntheticKey = `synthetic-${crypto.randomUUID()}`;
  const secretResponse = await page.request.post(prefix + "/secrets", {
    headers,
    data: { kind: "model_api_key", value: syntheticKey },
  });
  expect(secretResponse.ok()).toBe(true);
  const secret = await secretResponse.json();
  const initialConfig = await json<{
    revision: number;
    config: Record<string, any>;
  }>(page, "/config");
  initialConfig.config.model = {
    ...initialConfig.config.model,
    base_url: fixture.model_url,
    model: "isolated-conversation-browser",
    credential_ref: secret.id,
  };
  initialConfig.config.qq.enabled = false;
  initialConfig.config.assistant.max_concurrent_runs = 1;
  const draftResponse = await page.request.post(prefix + "/config/drafts", {
    headers,
    data: {
      base_revision: initialConfig.revision,
      config: initialConfig.config,
    },
  });
  expect(draftResponse.ok()).toBe(true);
  const draft = await draftResponse.json();
  const validation = await page.request.post(
    prefix + `/config/drafts/${draft.id}/validate`,
    { headers, data: {} },
  );
  expect((await validation.json()).valid).toBe(true);
  const application = await page.request.post(
    prefix + `/config/drafts/${draft.id}/apply`,
    {
      headers: { ...headers, "If-Match": `"${initialConfig.revision}"` },
      data: {},
    },
  );
  expect(application.status()).toBe(202);
  await expect
    .poll(
      async () =>
        (await json<{ config: Record<string, any> }>(page, "/config")).config
          .model.model,
    )
    .toBe("isolated-conversation-browser");
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await navigate(page, "性格");
  expect(budgetReads).toBe(0);
  await expect(page.getByLabel("API 基础地址", { exact: true })).toHaveCount(0);
  await navigate(page, "配置");
  await expect(page.locator("details.config-drawer > summary")).toHaveText([
    "模型配置",
    "工作节奏",
    "绘画能力",
    "好感规则配置",
    "配置历史",
  ]);
  expect(budgetReads).toBe(0);
  await openDrawer(drawer(page, "模型配置"));
  expect(budgetReads).toBe(0);
  await openDrawer(drawer(page, "工作节奏"));
  const budget = page.getByRole("region", { name: "上下文预算", exact: true });
  await expect(
    budget.getByLabel("请求字节上限", { exact: true }),
  ).toBeVisible();
  expect(budgetReads).toBe(1);
  await expect(budget).toContainText("不是精确 tokenizer 或 token 计费");
  await expect(budget).toContainText("未知，使用本地估算限制");
  const configBeforeBudget = await json<{ revision: number }>(page, "/config");
  await budget.getByLabel("请求字节上限", { exact: true }).fill("95000");
  await budget
    .getByRole("button", { name: "保存上下文预算", exact: true })
    .click();
  await expect(budget.getByRole("status")).toContainText("将用于新的请求");
  expect((await json<{ revision: number }>(page, "/config")).revision).toBe(
    configBeforeBudget.revision,
  );
  const saved = await json<ContextPolicyView>(page, "/context-policy");
  const deniedPolicyWrite = await page.request.put(prefix + "/context-policy", {
    headers: { Origin: origin, "If-Match": saved.etag },
    data: saved.policy,
  });
  expect(deniedPolicyWrite.status()).toBe(403);
  await budget.getByLabel("请求字节上限", { exact: true }).fill("94000");
  const concurrent = await page.request.put(prefix + "/context-policy", {
    headers: { ...headers, "If-Match": saved.etag },
    data: { ...saved.policy, max_input_bytes: 93000 },
  });
  expect(concurrent.ok()).toBe(true);
  await budget
    .getByRole("button", { name: "保存上下文预算", exact: true })
    .click();
  await expect(budget.getByRole("alert")).toContainText("你的修改仍保留");
  await expect(budget.getByLabel("请求字节上限", { exact: true })).toHaveValue(
    "94000",
  );
  await expect(
    budget.getByRole("button", { name: "保存上下文预算", exact: true }),
  ).toBeDisabled();
  page.once("dialog", (dialog) => dialog.accept());
  await budget
    .getByRole("button", { name: "重新载入预算", exact: true })
    .click();
  await expect(budget.getByLabel("请求字节上限", { exact: true })).toHaveValue(
    "93000",
  );
  expect((await json<{ revision: number }>(page, "/config")).revision).toBe(
    configBeforeBudget.revision,
  );

  async function createConversation(title: string) {
    const response = await page.request.post(prefix + "/conversations", {
      headers,
      data: { title },
    });
    expect(response.ok()).toBe(true);
    return (await response.json()).id as string;
  }
  async function turn(conversation: string, content: string) {
    const response = await page.request.post(
      prefix + `/conversations/${conversation}/messages`,
      {
        headers,
        data: { content, client_message_id: crypto.randomUUID() },
      },
    );
    expect(response.status()).toBe(202);
    const run = (await response.json()).run_id as string;
    await expect
      .poll(
        async () =>
          (await json<{ status: string }>(page, `/runs/${run}`)).status,
      )
      .toBe("completed");
    return run;
  }
  const baselineChat = await createConversation("合成预算基线");
  const baseline = await turn(
    baselineChat,
    `合成预算基线 ${crypto.randomUUID()}`,
  );
  const baselineSelection = await json<SelectionEvidence>(
    page,
    `/runs/${baseline}/context-selection`,
  );
  expect(baselineSelection.available).toBe(true);
  const requiredBytes = baselineSelection
    .selection!.blocks.filter((block) => block.required)
    .reduce((sum, block) => sum + block.serialized_bytes, 0);
  const chat = await createConversation("合成自然约定来源");
  await turn(chat, "合成先前条件：" + "旧资料".repeat(700));
  const limit = Math.min(90000, requiredBytes + 2200);
  await budget.getByLabel("请求字节上限", { exact: true }).fill(String(limit));
  await budget
    .getByRole("button", { name: "保存上下文预算", exact: true })
    .click();
  await expect(budget.getByRole("status")).toContainText("将用于新的请求");
  const input = `我答应你明天阅读合成资料 ${crypto.randomUUID()}`;
  const source = await turn(chat, input);
  const privateReader = await browser.newContext();
  expect(
    (
      await privateReader.request.get(
        origin + prefix + `/runs/${source}/context-selection`,
      )
    ).status(),
  ).toBe(401);
  await privateReader.close();
  const selection = await json<SelectionEvidence>(
    page,
    `/runs/${source}/context-selection`,
  );
  expect(selection.selection!.accepted).toBe(true);
  expect(selection.selection!.blocks.some((block) => block.selected)).toBe(
    true,
  );
  expect(selection.selection!.blocks.some((block) => !block.selected)).toBe(
    true,
  );
  expect(
    selection.selection!.blocks.some(
      (block) => block.selected && block.source_refs.length > 0,
    ),
  ).toBe(true);
  await screenshot(page, "00-context-budget-desktop.png");

  await navigate(page, "日程");
  await expect(
    page.getByRole("tab", { name: "日历", exact: true }),
  ).toBeVisible();
  const beforeActivities = await json<{ items: unknown[] }>(
    page,
    "/schedule/activities",
  );
  const beforeGrants = await json<{ items: unknown[] }>(
    page,
    "/schedule/autonomy-grants",
  );
  const scopes = await json<{ items: CommitmentScope[] }>(
    page,
    "/commitments/scopes?limit=50",
  );
  const scope = scopes.items.find(
    (item) => item.conversation_id === chat && item.run_id === source,
  );
  expect(scope).toBeTruthy();
  const candidates = await json<{ items: ConversationCommitment[] }>(
    page,
    `/conversations/${chat}/commitments?run_id=${source}`,
  );
  const candidate = candidates.items.find(
    (item) => item.source_run_id === source && item.intent === input,
  )!;
  expect(candidate.confirmation_state).toBe("proposed");
  expect(candidate.subject_kind).toBe("speaker");
  expect(candidate.counterparty_kind).toBe("assistant");
  const toolbar = page.getByRole("button", { name: "会话约定", exact: true });
  await expect(
    page
      .getByRole("navigation", { name: "主导航" })
      .getByRole("button", { name: "会话约定", exact: true }),
  ).toHaveCount(0);
  monitorReadOnly = true;
  await toolbar.click();
  const dialog = page.getByRole("dialog", { name: "会话约定", exact: true });
  await expect(dialog).toBeVisible();
  await choose(
    page,
    dialog.getByRole("combobox", { name: "约定来源", exact: true }),
    scope!.scope_ref,
  );
  await expect(dialog.getByText(input, { exact: true }).first()).toBeVisible();
  await expect(
    dialog.getByRole("combobox", { name: "履行主体", exact: true }),
  ).toHaveValue("speaker");
  await expect(
    dialog.getByRole("combobox", { name: "约定对象", exact: true }),
  ).toHaveValue("assistant");
  await expect(
    dialog.getByRole("button", { name: "确认约定", exact: true }),
  ).toBeDisabled();
  await dialog.locator('.commitment-check input[type="checkbox"]').check();
  await dialog.getByRole("button", { name: "确认约定", exact: true }).click();
  await expect(dialog).toContainText("资料已审核确认");
  await expect(dialog).toContainText("当前管理员对约定资料的审核");
  await expect(dialog).toContainText("未核验来源发言者或接收者同意");
  await expect(dialog).toContainText("尚未调度");
  await expect(dialog).toContainText("资料确认不代表已建立日程或获得发送许可");
  const reviewed = await json<ConversationCommitment>(
    page,
    `/conversations/${chat}/commitments/${candidate.id}?run_id=${source}`,
  );
  expect(reviewed.confirmation_state).toBe("confirmed");
  expect(reviewed.confirmation_evidence?.basis).toBe(
    "authenticated_management_review",
  );
  expect(reviewed.confirmation_evidence?.source_assent_verified).toBe(false);
  expect(reviewed.execution.state).toBe("unscheduled");
  expect(reviewed.recipient_permission).toBe(false);
  expect(
    (await json<{ items: unknown[] }>(page, "/schedule/activities")).items,
  ).toEqual(beforeActivities.items);
  expect(
    (await json<{ items: unknown[] }>(page, "/schedule/autonomy-grants")).items,
  ).toEqual(beforeGrants.items);
  expect(mutations.every((route) => route.endsWith("/confirm"))).toBe(true);
  await expect
    .poll(() =>
      dialog.evaluate((element) => element.contains(document.activeElement)),
    )
    .toBe(true);
  await screenshot(page, "01-reviewed-commitment-desktop.png");
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(toolbar).toBeFocused();

  mutations.length = 0;
  await navigate(page, "日志记录");
  await choose(
    page,
    page.getByRole("combobox", { name: "记录类型", exact: true }),
    "input",
  );
  await page.getByLabel("Run ID", { exact: true }).fill(source);
  await page.getByRole("button", { name: "筛选日志", exact: true }).click();
  await expect(page.locator(".runtime-log-list")).toHaveAttribute(
    "aria-busy",
    "false",
  );
  await page.locator(".runtime-log-summary").first().click();
  const context = page
    .getByRole("region", { name: "上下文选择依据", exact: true })
    .first();
  await context
    .getByRole("button", { name: "读取上下文选择记录", exact: true })
    .click();
  await expect(context).toContainText("预算校验通过");
  await expect(context).toContainText("已选入");
  await expect(context).toContainText("已省略");
  await expect(context).toContainText("来源");
  await expect(context).toContainText("不是精确 token 计费");
  expect(mutations).toEqual([]);
  await expect(
    page.getByRole("button", { name: /发送|执行|重试|新建对话/ }),
  ).toHaveCount(0);
  await screenshot(page, "02-context-selection-desktop.png");
  monitorReadOnly = false;
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `03-runtime-evidence-${width}.png`);
    await navigate(page, "日程");
    await toolbar.click();
    await expect(dialog).toBeVisible();
    expect(
      await dialog.evaluate(
        (element) => element.scrollWidth <= element.clientWidth + 1,
      ),
    ).toBe(true);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `04-commitments-${width}.png`);
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(toolbar).toBeFocused();
    await navigate(page, "配置");
    await openDrawer(drawer(page, "工作节奏"));
    await expect(
      budget.getByLabel("请求字节上限", { exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await screenshot(page, `05-context-budget-${width}.png`);
    await navigate(page, "日志记录");
  }
  const visibleText = await page.locator("body").innerText();
  expect(visibleText).not.toContain(syntheticKey);
  expect(visibleText).not.toContain(account.password);
  expect(
    await page.evaluate(
      ({ key, password }) =>
        [...Object.values(localStorage), ...Object.values(sessionStorage)].some(
          (value) => value.includes(key) || value.includes(password),
        ),
      { key: syntheticKey, password: account.password },
    ),
  ).toBe(false);
  expect(faults).toEqual([]);
});
