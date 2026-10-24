import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

// Authentication and empty-state/CSRF checks use the real isolated backend.
// Extraction, candidate decisions and job status below are browser fixtures:
// they never call a real model or claim real extraction/memory persistence.
const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/memory-candidates-e2e/data",
);
const accountFile = path.join(dataDir, "e2e-account.json");
const evidenceDir = path.resolve(
  "../../.local/memory-candidates-e2e/screenshots",
);

async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `candidate_${crypto.randomBytes(4).toString("hex")}`,
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

test("memory candidate browser fixtures require explicit sources and private review with conflict-safe terminal clearing", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(120000);
  const origin = new URL(baseURL!).origin;
  expect(
    new URL(baseURL!).port,
    "candidate E2E must not target the production tunnel",
  ).not.toBe("18419");
  let authenticated = false;
  let allowExpectedConflict = false;
  const consoleErrors: string[] = [];
  const pageFailures: string[] = [];
  const externalRequests: string[] = [];
  const fixtureViolations: string[] = [];
  let ordinaryMessagesSent = 0;
  page.on("pageerror", () => pageFailures.push("uncaught-browser-error"));
  page.on("console", (message) => {
    if (message.type() !== "error") return;
    if (
      !authenticated &&
      message.location().url === `${origin}/api/admin/v1/auth/me` &&
      /Failed to load resource.*401/.test(message.text())
    )
      return;
    if (
      allowExpectedConflict &&
      /\/memory-candidates\/fixture-candidate-1\/accept$/.test(
        message.location().url,
      ) &&
      /Failed to load resource.*409/.test(message.text())
    )
      return;
    consoleErrors.push(message.text());
  });
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      /\/conversations\/[^/]+\/messages$/.test(request.url())
    )
      ordinaryMessagesSent++;
  });
  await page.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (["http:", "https:"].includes(url.protocol) && url.origin !== origin) {
      externalRequests.push(url.hostname);
      await route.abort();
    } else await route.continue();
  });
  await authenticate(page);
  authenticated = true;
  const auth = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const headers = { Origin: origin, "X-CSRF-Token": auth.csrf_token };
  const suffix = crypto.randomBytes(4).toString("hex");
  const firstResponse = await page.request.post("/api/admin/v1/conversations", {
    headers,
    data: { title: `候选来源甲_${suffix}` },
  });
  const secondResponse = await page.request.post(
    "/api/admin/v1/conversations",
    { headers, data: { title: `候选来源乙_${suffix}` } },
  );
  expect(firstResponse.status()).toBe(200);
  expect(secondResponse.status()).toBe(200);
  const first = await firstResponse.json();
  const second = await secondResponse.json();
  const firstPath = `/api/admin/v1/conversations/${encodeURIComponent(first.id)}/memory-candidates`;
  const secondPath = `/api/admin/v1/conversations/${encodeURIComponent(second.id)}/memory-candidates`;
  const unauthenticated = await browser.newContext();
  expect(
    (await unauthenticated.request.get(`${origin}${firstPath}`)).status(),
  ).toBe(401);
  await unauthenticated.close();
  const backendBefore = await page.request.get(firstPath);
  expect(backendBefore.status()).toBe(200);
  const initialBackend = await backendBefore.json();
  expect(initialBackend.items).toEqual([]);
  expect(initialBackend.sources).toEqual([]);
  expect(typeof initialBackend.source_revision).toBe("string");
  // Missing CSRF is checked before any extraction. No valid backend extract is sent.
  expect(
    (
      await page.request.post(`${firstPath}/extract`, {
        headers: { Origin: origin },
        data: { source_message_ids: [] },
      })
    ).status(),
  ).toBe(403);

  type FixtureJob = {
    id: string;
    conversation_id: string;
    status: string;
    revision: number;
    created_at: number;
    expires_at: number;
    source_count: number;
    redacted_source_count: number;
  };
  type FixtureCandidate = {
    id: string;
    job_id: string;
    status: string;
    revision: number;
    content?: string;
    source_message_ids: string[];
    expires_at: number;
    memory_id?: string;
  };
  const now = () => Math.floor(Date.now() / 1000);
  const sourceText =
    "合成来源甲：我喜欢清晨散步。" + "long-source-fragment_".repeat(12);
  const candidateText = "合成候选正文：偏好清晨散步，需要本人核对";
  const rejectedText = "另一条待拒绝的合成候选";
  const expiredText = "这段合成候选将在到期后清除";
  const sources = [
    {
      id: "fixture-source-a",
      content: sourceText,
      created_at: now() - 20,
      byte_length: Buffer.byteLength(sourceText),
    },
  ];
  const fixture = {
    source_revision: "fixture.epoch.floor.12",
    job: null as FixtureJob | null,
    items: [] as FixtureCandidate[],
    sources,
    policy: {
      max_sources: 12,
      max_source_bytes: 12000,
      max_candidates: 3,
      max_content_bytes: 1000,
    },
  };
  const other = {
    ...fixture,
    source_revision: "fixture.b.floor.0",
    sources: [
      {
        id: "fixture-source-b",
        content: "合成来源乙：只属于另一个会话的可选消息",
        created_at: now() - 20,
        byte_length: 66,
      },
    ],
  };
  let extractionCount = 0;
  let acceptedCount = 0;
  let rejectedCount = 0;
  let cancelledCount = 0;
  let acceptanceAttempts = 0;
  let conflictNextAccept = true;
  let cancelFlow = false;
  const mutations: {
    path: string;
    method: string;
    match: string | undefined;
    body: unknown;
  }[] = [];
  await page.route("**/api/admin/v1/**", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    const method = request.method();
    const candidateRoute =
      pathname === firstPath ||
      pathname === secondPath ||
      pathname === `${firstPath}/extract` ||
      pathname.startsWith("/api/admin/v1/memory-candidate-jobs/") ||
      pathname.startsWith("/api/admin/v1/memory-candidates/");
    if (!candidateRoute) {
      await route.continue();
      return;
    }
    if (method !== "GET")
      mutations.push({
        path: pathname,
        method,
        match: request.headers()["if-match"],
        body: request.postData() ? request.postDataJSON() : undefined,
      });
    if (pathname === firstPath && method === "GET") {
      await route.fulfill({ json: fixture });
      return;
    }
    if (pathname === secondPath && method === "GET") {
      await route.fulfill({ json: other });
      return;
    }
    if (pathname === `${firstPath}/extract` && method === "POST") {
      extractionCount++;
      if (
        JSON.stringify(request.postDataJSON()) !==
        JSON.stringify({ source_message_ids: ["fixture-source-a"] })
      )
        fixtureViolations.push("unexpected-source-selection");
      if (request.headers()["if-match"] !== `"${fixture.source_revision}"`)
        fixtureViolations.push("wrong-source-revision");
      fixture.job = {
        id: `fixture-job-${extractionCount}`,
        conversation_id: first.id,
        status: "queued",
        revision: 1,
        created_at: now(),
        expires_at: now() + 120,
        source_count: 1,
        redacted_source_count: 0,
      };
      await route.fulfill({ status: 202, json: fixture.job });
      return;
    }
    if (
      pathname.startsWith("/api/admin/v1/memory-candidate-jobs/") &&
      fixture.job
    ) {
      if (method === "DELETE") {
        if (request.headers()["if-match"] !== `"${fixture.job.revision}"`)
          fixtureViolations.push("wrong-cancel-revision");
        cancelledCount++;
        fixture.job = {
          ...fixture.job,
          status: "cancelled",
          revision: fixture.job.revision + 1,
        };
        await route.fulfill({ json: fixture.job });
        return;
      }
      if (method === "GET") {
        if (!cancelFlow) {
          fixture.job = { ...fixture.job, status: "completed", revision: 2 };
          fixture.items = [
            {
              id: "fixture-candidate-1",
              job_id: fixture.job.id,
              status: "pending",
              revision: 1,
              content: candidateText,
              source_message_ids: ["fixture-source-a"],
              expires_at: now() + 120,
            },
            {
              id: "fixture-candidate-2",
              job_id: fixture.job.id,
              status: "pending",
              revision: 1,
              content: rejectedText,
              source_message_ids: ["fixture-source-a"],
              expires_at: now() + 120,
            },
          ];
        }
        await route.fulfill({ json: fixture.job });
        return;
      }
    }
    if (
      pathname ===
        "/api/admin/v1/memory-candidates/fixture-candidate-1/accept" &&
      method === "POST"
    ) {
      acceptanceAttempts++;
      if (conflictNextAccept) {
        conflictNextAccept = false;
        fixture.items[0]!.revision++;
        await route.fulfill({
          status: 409,
          json: {
            error: {
              code: "memory_candidate_revision_conflict",
              message: "候选已更新",
            },
          },
        });
        return;
      }
      if (request.headers()["if-match"] !== `"${fixture.items[0]!.revision}"`)
        fixtureViolations.push("wrong-accept-revision");
      acceptedCount++;
      fixture.items[0] = {
        ...fixture.items[0]!,
        status: "accepted",
        revision: fixture.items[0]!.revision + 1,
        content: undefined,
        memory_id: "fixture-memory-private",
      };
      await route.fulfill({ json: fixture.items[0] });
      return;
    }
    if (
      pathname ===
        "/api/admin/v1/memory-candidates/fixture-candidate-2/reject" &&
      method === "POST"
    ) {
      rejectedCount++;
      fixture.items[1] = {
        ...fixture.items[1]!,
        status: "rejected",
        revision: fixture.items[1]!.revision + 1,
        content: undefined,
      };
      await route.fulfill({ json: fixture.items[1] });
      return;
    }
    fixtureViolations.push("unexpected-candidate-route");
    await route.fulfill({
      status: 400,
      json: { error: "unexpected_fixture_request" },
    });
  });

  await page.getByRole("button", { name: "助手记忆", exact: true }).click();
  const panel = page.getByRole("region", { name: "记忆候选审核", exact: true });
  const selector = panel.getByLabel("提取来源会话", { exact: true });
  await expect(selector.locator(`option[value="${first.id}"]`)).toHaveCount(1);
  await selector.selectOption(first.id);
  await expect(panel).toContainText(sourceText);
  await expect(
    panel.getByRole("button", { name: "仅提取所选消息", exact: true }),
  ).toBeDisabled();
  expect(extractionCount).toBe(0);
  await panel.getByLabel("选择来源 1", { exact: true }).check();
  await selector.selectOption(second.id);
  await expect(panel).toContainText("合成来源乙：只属于另一个会话");
  await expect(panel).not.toContainText(sourceText);
  await selector.selectOption(first.id);
  await expect(
    panel.getByLabel("选择来源 1", { exact: true }),
  ).not.toBeChecked();
  await panel.getByLabel("选择来源 1", { exact: true }).check();
  await panel
    .getByRole("button", { name: "仅提取所选消息", exact: true })
    .click();
  await expect(panel).toContainText("模型候选尚未写入记忆");
  await expect(
    panel.getByRole("article", { name: "记忆候选 1", exact: true }),
  ).toContainText(candidateText);
  const firstCandidate = panel.getByRole("article", {
    name: "记忆候选 1",
    exact: true,
  });
  expect(acceptedCount).toBe(0);
  await firstCandidate
    .getByRole("button", { name: "审核并接受此候选", exact: true })
    .click();
  let confirmation = panel.getByRole("region", {
    name: "确认接受记忆候选",
    exact: true,
  });
  await expect(confirmation).toContainText(candidateText);
  await expect(confirmation).toContainText(sourceText);
  await expect(confirmation).toContainText("不发布到共享范围，不覆盖已有事实");
  expect(acceptanceAttempts).toBe(0);
  allowExpectedConflict = true;
  await confirmation
    .getByRole("button", { name: "确认正文与来源并保存私有记忆", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText(
    "当前选择和展示已保留，请刷新候选",
  );
  await expect(firstCandidate).toContainText(candidateText);
  await expect(
    firstCandidate.getByRole("button", {
      name: "审核并接受此候选",
      exact: true,
    }),
  ).toBeDisabled();
  expect(acceptedCount).toBe(0);
  await panel.getByRole("button", { name: "刷新候选", exact: true }).click();
  await firstCandidate
    .getByRole("button", { name: "审核并接受此候选", exact: true })
    .click();
  confirmation = panel.getByRole("region", {
    name: "确认接受记忆候选",
    exact: true,
  });
  fs.mkdirSync(evidenceDir, { recursive: true });
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await panel.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await panel.screenshot({
      path: path.join(evidenceDir, `memory-candidates-fixture-${width}.png`),
      animations: "disabled",
    });
  }
  await confirmation
    .getByRole("button", { name: "确认正文与来源并保存私有记忆", exact: true })
    .click();
  await expect(firstCandidate).toContainText("已接受为来源账号域私有记忆");
  await expect(panel).not.toContainText(candidateText);
  expect(acceptedCount).toBe(1);
  expect(acceptanceAttempts).toBe(2);
  await panel
    .getByRole("article", { name: "记忆候选 2", exact: true })
    .getByRole("button", { name: "拒绝此候选", exact: true })
    .click();
  await expect(panel).not.toContainText(rejectedText);
  expect(rejectedCount).toBe(1);
  cancelFlow = true;
  await panel.getByLabel("选择来源 1", { exact: true }).check();
  await panel
    .getByRole("button", { name: "仅提取所选消息", exact: true })
    .click();
  await panel
    .getByRole("button", { name: "取消提取任务", exact: true })
    .click();
  const cancel = panel.getByRole("region", {
    name: "确认取消提取",
    exact: true,
  });
  await expect(cancel).toContainText(`候选来源甲_${suffix}`);
  await cancel
    .getByRole("button", { name: "确认取消本次提取", exact: true })
    .click();
  await expect(panel).toContainText("提取已取消");
  expect(cancelledCount).toBe(1);
  fixture.items.push({
    id: "fixture-expiring",
    job_id: "fixture-old-job",
    status: "pending",
    revision: 1,
    content: expiredText,
    source_message_ids: ["fixture-source-a"],
    expires_at: now() + 2,
  });
  await panel.getByRole("button", { name: "刷新候选", exact: true }).click();
  await expect(panel).toContainText(expiredText);
  await expect(panel).not.toContainText(expiredText, { timeout: 5000 });
  expect(extractionCount).toBe(2);
  expect(
    mutations
      .filter((item) => item.path.endsWith("/accept"))
      .map((item) => item.body),
  ).toEqual([{}, {}]);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  // APIRequestContext bypasses page.route: browser fixtures changed no backend candidate data.
  const backendAfter = await (await page.request.get(firstPath)).json();
  expect(backendAfter).toEqual(initialBackend);
  expect(ordinaryMessagesSent).toBe(0);
  expect(fixtureViolations).toEqual([]);
  expect(externalRequests).toEqual([]);
  expect(pageFailures).toEqual([]);
  expect(consoleErrors).toEqual([]);
});
