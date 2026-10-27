/** Real HTTP/static browser acceptance, prepared only; no route interception.
 * Run against a NEW conversation-state-browser-fixture.py instance using a
 * fixed full SHA binary/static bundle and fresh data. Required: DSH_E2E_BASE_URL,
 * DSH_E2E_DATA_DIR, DSH_E2E_FIXTURE_INFO, DSH_E2E_EXPECTED_COMMIT.
 * Optional DSH_E2E_ACK_HOST_CLOCK=1 uses the existing authenticated clock API
 * with the server's own freshly cross-checked sample, never Date.now/client time.
 */
import { test, expect, type Page, type Request } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import type {
  RestGrant,
  RestPolicyView,
  RestStatus,
} from "../../src/rest-management";
import type { CommitmentScope } from "../../src/context-management";

const prefix = "/api/admin/v1";
const evidenceDir = path.resolve(
  process.env.DSH_E2E_EVIDENCE_DIR || "../../.local/rest-state-e2e/evidence",
);
async function get<T>(page: Page, route: string): Promise<T> {
  const response = await page.request.get(prefix + route);
  expect(response.ok(), `authenticated GET ${route}`).toBe(true);
  return response.json() as Promise<T>;
}
async function navigate(page: Page, name: string) {
  const toggle = page.getByRole("button", { name: "打开导航", exact: true });
  if (await toggle.isVisible()) await toggle.click();
  await page
    .getByRole("navigation", { name: "主导航" })
    .getByRole("button", { name, exact: true })
    .click();
}
function workRhythm(page: Page) {
  return page
    .locator("details.config-drawer")
    .filter({ has: page.getByText("工作节奏", { exact: true }) });
}
async function openRhythm(page: Page) {
  const drawer = workRhythm(page);
  if ((await drawer.getAttribute("open")) === null)
    await drawer.locator("summary").click();
  const panel = drawer.getByRole("region", { name: "作息与整理", exact: true });
  await expect(panel).toBeVisible();
  await expect(panel).toHaveAttribute("aria-busy", "false");
  return panel;
}
async function confirmNative(
  page: Page,
  action: () => Promise<unknown>,
  accept: boolean,
) {
  const handled = page.waitForEvent("dialog").then(async (dialog) => {
    expect(dialog.type()).toBe("confirm");
    if (accept) await dialog.accept();
    else await dialog.dismiss();
  });
  await action();
  await handled;
}
async function picture(page: Page, name: string) {
  await page.evaluate(() => document.fonts.ready);
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, name),
    fullPage: true,
    animations: "disabled",
  });
}
function localExpiry(timestamp: number, zone: string) {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: zone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).formatToParts(new Date(timestamp * 1000));
  const part = (type: string) =>
    parts.find((item) => item.type === type)!.value;
  return `${part("year")}-${part("month")}-${part("day")}T${part("hour")}:${part("minute")}`;
}
async function bootstrap(page: Page) {
  const configured = process.env.DSH_E2E_DATA_DIR;
  if (!configured)
    throw new Error(
      "DSH_E2E_DATA_DIR must identify a new isolated Rest fixture",
    );
  const data = path.resolve(configured);
  expect(
    (await get<{ initialized: boolean }>(page, "/auth/status")).initialized,
    "a modified or production fixture must never be reused",
  ).toBe(false);
  const account = {
    username: `rest_${crypto.randomBytes(4).toString("hex")}`,
    password: crypto.randomBytes(24).toString("base64url"),
  };
  const accountFile = path.resolve(
    process.env.DSH_E2E_ACCOUNT_FILE || path.join(data, "rest-account.json"),
  );
  fs.mkdirSync(path.dirname(accountFile), { recursive: true });
  fs.writeFileSync(accountFile, JSON.stringify(account), {
    mode: 0o600,
    flag: "wx",
  });
  const pairing = JSON.parse(
    fs.readFileSync(path.join(data, "bootstrap-token.json"), "utf8"),
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
  return (await get<{ csrf_token: string }>(page, "/auth/me")).csrf_token;
}

test("Rest uses independent policy and scope grants, versioned host controls and accessible work-rhythm forms", async ({
  page,
  browser,
  baseURL,
}, testInfo) => {
  test.setTimeout(180000);
  page.setDefaultTimeout(15000);
  const origin = new URL(baseURL!).origin;
  expect(["127.0.0.1", "localhost", "[::1]"]).toContain(
    new URL(origin).hostname,
  );
  expect(["", "8419", "18419"]).not.toContain(new URL(origin).port);
  const infoFile = process.env.DSH_E2E_FIXTURE_INFO,
    expectedCommit = process.env.DSH_E2E_EXPECTED_COMMIT;
  if (!infoFile || !expectedCommit || !/^[a-f0-9]{40}$/.test(expectedCommit))
    throw new Error(
      "Use fresh fixture metadata and a full fixed DSH_E2E_EXPECTED_COMMIT",
    );
  const fixture = JSON.parse(fs.readFileSync(infoFile, "utf8"));
  expect(fixture.production_data_accessed).toBe(false);
  expect(fixture.real_external_model_tested).toBe(false);
  expect(new URL(fixture.origin).origin).toBe(origin);
  expect(fixture.commit).toBe(expectedCommit);
  expect(["127.0.0.1", "localhost", "[::1]"]).toContain(
    new URL(fixture.model_url).hostname,
  );
  const health = await (await page.request.get("/health/ready")).json();
  expect(health.commit).toBe(expectedCommit);
  const report: Record<string, unknown> = {
    kind: "Rest isolated browser acceptance",
    commit: expectedCommit,
    binary_sha256: fixture.binary_sha256,
    production_data_accessed: false,
    real_external_model_tested: false,
    real_channel_tested: false,
    host_clock_modified: false,
    client_clock_submitted: false,
    status: "running",
    controlled_sleep: { status: "not_evaluated" },
    cleanup: { status: "not_started" },
  };
  const faults: string[] = [],
    writes: { path: string; method: string }[] = [];
  const relevant = (request: Request) =>
    new URL(request.url()).pathname.startsWith(prefix + "/rest/") &&
    !["GET", "HEAD", "OPTIONS"].includes(request.method());
  page.on("pageerror", (error) => faults.push(error.message));
  page.on("request", (request) => {
    if (relevant(request))
      writes.push({
        path: new URL(request.url()).pathname,
        method: request.method(),
      });
  });
  let csrf = "",
    sourceRun: string | null = null,
    grantExpiry: number | null = null;
  let headers: Record<string, string> = {};
  try {
    const anonymous = await browser.newContext();
    for (const route of [
      "/rest",
      "/rest/policy",
      "/rest/cycles/current",
      "/rest/cycles?limit=20",
    ])
      expect(
        (await anonymous.request.get(origin + prefix + route)).status(),
      ).toBe(401);
    await anonymous.close();
    csrf = await bootstrap(page);
    headers = { Origin: origin, "X-CSRF-Token": csrf };
    const firstPolicy = await get<RestPolicyView>(page, "/rest/policy");
    expect(firstPolicy.policy.natural_enabled).toBe(false);
    expect(firstPolicy.policy.schedule_enabled).toBe(false);
    expect(firstPolicy.policy.default_sleep_seconds).toBeNull();
    expect((await get<RestStatus>(page, "/rest")).state.phase).toBe("awake");
    await navigate(page, "配置");
    await expect(page.locator("details.config-drawer > summary")).toHaveText([
      "模型配置",
      "工作节奏",
      "绘画能力",
      "好感规则配置",
      "配置历史",
    ]);
    await expect(
      page
        .getByRole("navigation", { name: "主导航" })
        .getByRole("button", { name: /作息|睡眠|整理/ }),
    ).toHaveCount(0);
    let panel = await openRhythm(page);
    await expect(
      panel.getByRole("checkbox", { name: "允许对话控制作息", exact: true }),
    ).not.toBeChecked();
    await expect(
      panel.getByRole("checkbox", { name: "启用定时作息", exact: true }),
    ).not.toBeChecked();
    await expect(
      workRhythm(page).getByRole("region", { name: "上下文预算", exact: true }),
    ).toBeVisible();
    const runtimeBefore = await get<{ revision: number }>(page, "/config");
    await panel.getByLabel("默认睡眠时长", { exact: true }).fill("45");
    await panel.getByLabel("最长睡眠", { exact: true }).fill("60");
    await panel.getByLabel("排空窗口", { exact: true }).fill("1");
    await panel.getByLabel("整理窗口", { exact: true }).fill("1");
    const policySave = page.waitForRequest(
      (request) =>
        request.method() === "PUT" &&
        new URL(request.url()).pathname === prefix + "/rest/policy",
    );
    await panel
      .getByRole("button", { name: "保存作息策略", exact: true })
      .click();
    const request = await policySave;
    expect(request.headers()["if-match"]).toBe(`"${firstPolicy.revision}"`);
    expect(request.headers()["idempotency-key"]).toBeTruthy();
    await expect(panel.getByRole("status")).toContainText("仅用于后续新周期");
    expect((await get<{ revision: number }>(page, "/config")).revision).toBe(
      runtimeBefore.revision,
    );
    const savedPolicy = await get<RestPolicyView>(page, "/rest/policy");
    expect(savedPolicy.revision).toBe(firstPolicy.revision + 1);
    expect(savedPolicy.policy.default_sleep_seconds).toBe(45);
    expect(savedPolicy.policy.natural_enabled).toBe(false);
    expect(savedPolicy.policy.schedule_enabled).toBe(false);
    await expect
      .poll(() =>
        panel.evaluate((element) => element.contains(document.activeElement)),
      )
      .toBe(true);
    await picture(page, "rest-00-independent-policy-desktop.png");

    // A real concurrent API save makes the visible form's exact revision stale.
    await panel.getByLabel("排空窗口", { exact: true }).fill("2");
    const concurrent = await page.request.put(prefix + "/rest/policy", {
      headers: {
        ...headers,
        "If-Match": `"${savedPolicy.revision}"`,
        "Idempotency-Key": crypto.randomUUID(),
      },
      data: { ...savedPolicy.policy, drain_seconds: 3 },
    });
    expect(concurrent.ok()).toBe(true);
    const conflict = page.waitForResponse(
      (response) =>
        response.request().method() === "PUT" &&
        new URL(response.url()).pathname === prefix + "/rest/policy" &&
        response.status() === 409,
    );
    const beforeConflict = writes.length;
    await panel
      .getByRole("button", { name: "保存作息策略", exact: true })
      .click();
    await conflict;
    await expect(panel.getByRole("alert")).toContainText("你的填写仍保留");
    await expect(panel.getByLabel("排空窗口", { exact: true })).toHaveValue(
      "2",
    );
    await expect(
      panel.getByRole("button", { name: "保存作息策略", exact: true }),
    ).toBeDisabled();
    expect(writes.length).toBe(beforeConflict + 1);
    await confirmNative(
      page,
      () =>
        panel
          .getByRole("button", { name: "重新载入作息策略", exact: true })
          .click(),
      false,
    );
    await expect(panel.getByLabel("排空窗口", { exact: true })).toHaveValue(
      "2",
    );
    await confirmNative(
      page,
      () =>
        panel
          .getByRole("button", { name: "重新载入作息策略", exact: true })
          .click(),
      true,
    );
    await expect(panel.getByLabel("排空窗口", { exact: true })).toHaveValue(
      "3",
    );
    await panel.getByLabel("排空窗口", { exact: true }).fill("4");
    await panel.getByLabel("排空窗口", { exact: true }).focus();
    await page.keyboard.press("Escape");
    await expect(panel.getByLabel("排空窗口", { exact: true })).toHaveValue(
      "4",
    );
    await confirmNative(page, () => navigate(page, "日志记录"), false);
    await expect(panel).toBeVisible();
    await confirmNative(
      page,
      () => page.getByRole("button", { name: "退出登录", exact: true }).click(),
      false,
    );
    await expect(panel).toBeVisible();
    await confirmNative(
      page,
      () =>
        panel
          .getByRole("button", { name: "重新载入作息策略", exact: true })
          .click(),
      true,
    );
    await expect(panel.getByLabel("排空窗口", { exact: true })).toHaveValue(
      "3",
    );

    // Configure only this disposable service to its fixture's loopback provider.
    const secret = await page.request.post(prefix + "/secrets", {
      headers,
      data: {
        kind: "model_api_key",
        value: `synthetic-rest-${crypto.randomUUID()}`,
      },
    });
    expect(secret.ok()).toBe(true);
    const credential = (await secret.json()).id;
    const configuration = await get<{
      revision: number;
      config: Record<string, any>;
    }>(page, "/config");
    configuration.config.model = {
      ...configuration.config.model,
      base_url: fixture.model_url,
      model: "isolated-rest-browser",
      credential_ref: credential,
    };
    configuration.config.qq.enabled = false;
    configuration.config.assistant.max_concurrent_runs = 1;
    const draftResponse = await page.request.post(prefix + "/config/drafts", {
      headers,
      data: {
        base_revision: configuration.revision,
        config: configuration.config,
      },
    });
    expect(draftResponse.ok()).toBe(true);
    const draft = await draftResponse.json();
    const valid = await page.request.post(
      prefix + `/config/drafts/${draft.id}/validate`,
      { headers, data: {} },
    );
    expect((await valid.json()).valid).toBe(true);
    const applied = await page.request.post(
      prefix + `/config/drafts/${draft.id}/apply`,
      {
        headers: { ...headers, "If-Match": `"${configuration.revision}"` },
        data: {},
      },
    );
    expect(applied.status()).toBe(202);
    await expect
      .poll(
        async () =>
          (await get<{ config: Record<string, any> }>(page, "/config")).config
            .model.model,
      )
      .toBe("isolated-rest-browser");
    const conversationResponse = await page.request.post(
      prefix + "/conversations",
      { headers, data: { title: "合成作息管理来源" } },
    );
    expect(conversationResponse.ok()).toBe(true);
    const conversation = (await conversationResponse.json()).id;
    const inputResponse = await page.request.post(
      prefix + `/conversations/${conversation}/messages`,
      {
        headers,
        data: {
          content: `合成作息控制来源 ${crypto.randomUUID()}`,
          client_message_id: crypto.randomUUID(),
        },
      },
    );
    expect(inputResponse.status()).toBe(202);
    sourceRun = (await inputResponse.json()).run_id;
    await expect
      .poll(
        async () =>
          (await get<{ status: string }>(page, `/runs/${sourceRun}`)).status,
      )
      .toBe("completed");
    const scopes = await get<{ items: CommitmentScope[] }>(
      page,
      "/commitments/scopes?limit=50",
    );
    const scope = scopes.items.find(
      (item) =>
        item.run_id === sourceRun && item.conversation_id === conversation,
    );
    expect(scope).toBeTruthy();
    await panel
      .getByRole("button", { name: "读取可管理来源", exact: true })
      .click();
    await panel
      .getByRole("combobox", { name: "作息控制来源", exact: true })
      .selectOption(scope!.scope_ref);
    await expect(
      panel.getByRole("form", { name: "来源控制许可", exact: true }),
    ).toBeVisible();
    await expect(
      panel.getByRole("checkbox", { name: "本来源可请求入睡", exact: true }),
    ).not.toBeChecked();
    await panel
      .getByRole("checkbox", { name: "本来源可请求唤醒", exact: true })
      .check();
    const sampled = await get<RestStatus>(page, "/rest");
    expect(Number.isSafeInteger(sampled.clock.sampled_wall_at)).toBe(true);
    grantExpiry = Math.floor((sampled.clock.sampled_wall_at! + 3600) / 60) * 60;
    const grantPolicy = await get<RestPolicyView>(page, "/rest/policy");
    await panel
      .getByLabel("控制许可截止", { exact: true })
      .fill(localExpiry(grantExpiry, grantPolicy.policy.time_zone));
    const grantSave = page.waitForRequest(
      (request) =>
        request.method() === "PUT" &&
        new URL(request.url()).pathname ===
          prefix + `/rest/control-grants/${sourceRun}`,
    );
    await panel
      .getByRole("button", { name: "保存本来源控制许可", exact: true })
      .click();
    const grantRequest = await grantSave;
    expect(grantRequest.headers()["if-match"]).toBe('"0"');
    expect(grantRequest.headers()["idempotency-key"]).toBeTruthy();
    expect(grantRequest.postDataJSON()).toEqual({
      can_sleep: false,
      can_wake: true,
      valid_until: grantExpiry,
    });
    await expect(panel.getByRole("status")).toContainText(
      "没有增加资料、模型出域或外发许可",
    );
    const grant = await get<RestGrant>(
      page,
      `/rest/control-grants/${sourceRun}`,
    );
    expect(grant.permissions).toEqual({
      can_sleep: false,
      can_wake: true,
      valid_until: grantExpiry,
    });
    expect(grant.recipient_or_model_permission).toBe(false);
    expect(grant.natural_control_enabled).toBe(false);
    await expect(
      panel.locator(
        'input[name="owner"],input[name="receiver"],input[name="platform_id"]',
      ),
    ).toHaveCount(0);
    expect(
      (
        await page.request.get(
          prefix + `/rest/control-grants/${crypto.randomUUID()}`,
        )
      ).status(),
    ).toBe(404);
    expect(
      (await get<RestGrant>(page, `/rest/control-grants/${sourceRun}`))
        .revision,
    ).toBe(grant.revision);
    await expect
      .poll(() =>
        panel.evaluate((element) => element.contains(document.activeElement)),
      )
      .toBe(true);
    await picture(page, "rest-01-real-source-grant-desktop.png");

    let state = await get<RestStatus>(page, "/rest");
    report.clock_before_control = state.clock;
    const beforeControlConfig = (
      await get<{ revision: number }>(page, "/config")
    ).revision;
    if (
      state.clock.trusted_for_new_sleep !== true &&
      process.env.DSH_E2E_ACK_HOST_CLOCK === "1"
    ) {
      const capabilities = await get<{
        now: number;
        clock: { trusted: boolean };
      }>(page, "/task-triggers/capabilities");
      expect(Number.isSafeInteger(capabilities.now)).toBe(true);
      const fresh = await get<RestStatus>(page, "/rest");
      expect(fresh.clock.host_sample_available).toBe(true);
      expect(
        Math.abs(capabilities.now - fresh.clock.sampled_wall_at!),
      ).toBeLessThanOrEqual(5);
      const acknowledgement = await page.request.post(
        prefix + "/task-triggers/clock/acknowledge",
        { headers, data: { observed_now: capabilities.now, confirmed: true } },
      );
      report.clock_acknowledgement_status = acknowledgement.status();
      report.clock_acknowledgement_observed_server_at = capabilities.now;
      if (acknowledgement.ok())
        await expect
          .poll(
            async () =>
              (await get<RestStatus>(page, "/rest")).clock
                .trusted_for_new_sleep,
          )
          .toBe(true);
      state = await get<RestStatus>(page, "/rest");
    }
    report.clock_used_for_control = state.clock;
    await panel
      .getByRole("button", { name: "刷新作息状态", exact: true })
      .click();
    await expect(panel).toHaveAttribute("aria-busy", "false");
    if (state.clock.trusted_for_new_sleep === true) {
      const generation = state.state.generation;
      const sleepResponse = page.waitForResponse(
        (response) =>
          response.request().method() === "POST" &&
          new URL(response.url()).pathname === prefix + "/rest/sleep",
      );
      await confirmNative(
        page,
        () =>
          panel
            .getByRole("button", { name: "按已保存策略入睡", exact: true })
            .click(),
        true,
      );
      const sleeping = await sleepResponse;
      expect(sleeping.ok()).toBe(true);
      expect(sleeping.request().headers()["if-match"]).toBe(`"${generation}"`);
      expect(sleeping.request().postDataJSON()).toEqual({ wake_at: null });
      const accepted = await sleeping.json();
      expect(accepted.state.generation).toBe(generation + 1);
      expect(accepted.physical_leases_released).toBe(false);
      const wakeAt = accepted.state.wake_at as number;
      expect(Number.isSafeInteger(wakeAt)).toBe(true);
      await expect(
        panel.getByRole("button", { name: "按已保存策略入睡", exact: true }),
      ).toBeDisabled();
      state = await get<RestStatus>(page, "/rest");
      expect(state.state.generation).toBe(generation + 1);
      expect(["sleep_pending", "consolidating", "sleeping"]).toContain(
        state.state.phase,
      );
      const cycle = await get<{
        wake_at: number;
        metadata_only: boolean;
        steps: unknown[];
        jobs: unknown[];
      }>(page, `/rest/cycles/${state.state.cycle_id}`);
      expect(cycle.metadata_only).toBe(true);
      expect(cycle.wake_at).toBe(wakeAt);
      await panel.getByLabel("排空窗口", { exact: true }).fill("4");
      await panel
        .getByRole("button", { name: "保存作息策略", exact: true })
        .click();
      await expect(panel.getByRole("status")).toContainText(
        "当前周期沿用原策略与原唤醒期限",
      );
      expect((await get<RestStatus>(page, "/rest")).state.wake_at).toBe(wakeAt);
      const wakeResponse = page.waitForResponse(
        (response) =>
          response.request().method() === "POST" &&
          new URL(response.url()).pathname === prefix + "/rest/wake",
      );
      await confirmNative(
        page,
        () =>
          panel.getByRole("button", { name: "请求唤醒", exact: true }).click(),
        true,
      );
      const awake = await wakeResponse;
      expect(awake.ok()).toBe(true);
      expect(awake.request().postDataJSON()).toEqual({});
      expect(awake.request().headers()["if-match"]).toBe(`"${generation + 1}"`);
      await expect
        .poll(async () => (await get<RestStatus>(page, "/rest")).state.phase)
        .toBe("awake");
      report.controlled_sleep = {
        status: "exercised",
        generation: generation + 1,
        original_wake_at: wakeAt,
        read_only_steps: cycle.steps.length,
        read_only_jobs: cycle.jobs.length,
      };
    } else {
      await expect(panel).toContainText("主机时钟尚未获确认");
      await expect(
        panel.getByRole("button", { name: "按已保存策略入睡", exact: true }),
      ).toBeDisabled();
      await expect(
        panel.getByRole("button", { name: "请求唤醒", exact: true }),
      ).toBeEnabled();
      const blocked = await page.request.post(prefix + "/rest/sleep", {
        headers: {
          ...headers,
          "If-Match": `"${state.state.generation}"`,
          "Idempotency-Key": crypto.randomUUID(),
        },
        data: { wake_at: null },
      });
      expect(blocked.status()).toBe(409);
      expect((await blocked.json()).error.code).toBe("rest_clock_untrusted");
      await confirmNative(
        page,
        () =>
          panel.getByRole("button", { name: "请求唤醒", exact: true }).click(),
        true,
      );
      await expect
        .poll(async () => (await get<RestStatus>(page, "/rest")).state.phase)
        .toBe("awake");
      report.controlled_sleep = {
        status: "blocked_clock",
        sleep_execution_tested: false,
        wake_available: true,
      };
    }
    expect((await get<{ revision: number }>(page, "/config")).revision).toBe(
      beforeControlConfig,
    );
    await panel
      .getByRole("button", { name: "读取最近周期", exact: true })
      .click();
    await expect(panel).toHaveAttribute("aria-busy", "false");
    await expect(panel).toContainText(
      "不能完成步骤、重试作业、执行命令或释放请求占用",
    );
    await expect(
      panel.getByRole("button", {
        name: /完成步骤|重试作业|执行命令|释放请求/,
      }),
    ).toHaveCount(0);
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({
        width,
        height: width === 1440 ? 1000 : 844,
      });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
        `no horizontal overflow at ${width}`,
      ).toBe(true);
      await panel.getByLabel("排空窗口", { exact: true }).focus();
      await page.keyboard.press("Escape");
      await expect(panel.getByLabel("排空窗口", { exact: true })).toBeFocused();
      await picture(page, `rest-02-work-rhythm-${width}.png`);
    }
    expect(faults).toEqual([]);
    report.page_errors = 0;
    report.viewports = [1440, 390, 320];
    report.status = "passed";
  } catch (error) {
    report.status = "failed";
    report.error_type = error instanceof Error ? error.name : "unknown";
    throw error;
  } finally {
    const assertionsCompleted = report.status === "passed";
    const cleanup: {
      awake: boolean;
      grant_revoked: boolean;
      error_type?: string;
    } = { awake: false, grant_revoked: sourceRun === null };
    if (csrf) {
      try {
        let current = await get<RestStatus>(page, "/rest");
        if (current.state.phase !== "awake") {
          const response = await page.request.post(prefix + "/rest/wake", {
            headers: {
              ...headers,
              "If-Match": `"${current.state.generation}"`,
              "Idempotency-Key": crypto.randomUUID(),
            },
            data: {},
          });
          expect(response.ok(), "cleanup explicit wake").toBe(true);
          await expect
            .poll(
              async () => (await get<RestStatus>(page, "/rest")).state.phase,
            )
            .toBe("awake");
        }
        current = await get<RestStatus>(page, "/rest");
        cleanup.awake = current.state.phase === "awake";
        if (sourceRun) {
          const grant = await get<RestGrant>(
            page,
            `/rest/control-grants/${sourceRun}`,
          );
          const serverNow = current.clock.sampled_wall_at;
          if (serverNow == null)
            throw new Error("cleanup_host_sample_unavailable");
          const until = Math.max(grantExpiry || 0, serverNow + 3600);
          const response = await page.request.put(
            prefix + `/rest/control-grants/${sourceRun}`,
            {
              headers: {
                ...headers,
                "If-Match": `"${grant.revision}"`,
                "Idempotency-Key": crypto.randomUUID(),
              },
              data: { can_sleep: false, can_wake: false, valid_until: until },
            },
          );
          expect(response.ok(), "cleanup fixture-only source grant").toBe(true);
          const revoked = await get<RestGrant>(
            page,
            `/rest/control-grants/${sourceRun}`,
          );
          cleanup.grant_revoked =
            !revoked.permissions.can_sleep && !revoked.permissions.can_wake;
        }
      } catch (error) {
        cleanup.error_type = error instanceof Error ? error.name : "unknown";
      }
    }
    if (assertionsCompleted && (!cleanup.awake || !cleanup.grant_revoked)) {
      report.status = "failed";
      report.cleanup_failed = true;
    }
    report.cleanup = cleanup;
    fs.mkdirSync(evidenceDir, { recursive: true });
    const filename = path.join(evidenceDir, "rest-browser-metadata.json");
    fs.writeFileSync(filename, JSON.stringify(report, null, 2) + "\n");
    await testInfo.attach("rest-browser-metadata", {
      path: filename,
      contentType: "application/json",
    });
    if (assertionsCompleted) {
      expect(cleanup.awake, "fixture must finish awake").toBe(true);
      expect(cleanup.grant_revoked, "fixture grant must finish revoked").toBe(
        true,
      );
    }
  }
});
