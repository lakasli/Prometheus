import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { deflateSync } from "node:zlib";

// Real isolated backend: authentication, empty owner list, 401 and CSRF.
// All valid image submissions/job transitions below are browser API fixtures.
// The PNG is generated here, decoded by real Chromium, and never sent to a
// real model/provider. This does not claim actual model analysis passed.
const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/image-e2e/data",
);
const accountFile = path.join(dataDir, "e2e-account.json");
const evidenceDir = path.resolve("../../.local/image-e2e/screenshots");

function syntheticPng() {
  const width = 240;
  const height = 160;
  const raw = Buffer.alloc((width * 3 + 1) * height);
  for (let y = 0; y < height; y++) {
    const row = y * (width * 3 + 1);
    for (let x = 0; x < width; x++) {
      const color =
        x >= 30 && x < 95 && y >= 30 && y < 130
          ? [235, 35, 35]
          : x >= 145 && x < 215 && y >= 50 && y < 140
            ? [20, 170, 65]
            : [255, 255, 255];
      raw.set(color, row + 1 + x * 3);
    }
  }
  function chunk(kind: string, payload: Buffer) {
    const bytes = Buffer.concat([Buffer.from(kind, "ascii"), payload]);
    let checksum = 0xffffffff;
    for (const value of bytes) {
      checksum ^= value;
      for (let bit = 0; bit < 8; bit++)
        checksum = (checksum >>> 1) ^ (checksum & 1 ? 0xedb88320 : 0);
    }
    const length = Buffer.alloc(4);
    const crc = Buffer.alloc(4);
    length.writeUInt32BE(payload.length);
    crc.writeUInt32BE((checksum ^ 0xffffffff) >>> 0);
    return Buffer.concat([length, bytes, crc]);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 2;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `image_${crypto.randomBytes(4).toString("hex")}`,
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

test("synthetic PNG browser fixtures require explicit submission and preserve privacy through unknown, conflict and terminal states", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(120000);
  const origin = new URL(baseURL!).origin;
  expect(
    new URL(baseURL!).port,
    "image E2E must not target the production tunnel",
  ).not.toBe("18419");
  let authenticated = false;
  let expectedUnknown = false;
  let expectedConflict = false;
  let expectedExpiredSession = false;
  const pageFailures: string[] = [];
  const consoleErrors: string[] = [];
  const externalRequests: string[] = [];
  const fixtureViolations: string[] = [];
  let conversationOrMemoryWrites = 0;
  page.on("pageerror", () => pageFailures.push("uncaught-browser-error"));
  page.on("console", (message) => {
    if (message.type() !== "error") return;
    const url = message.location().url;
    if (
      !authenticated &&
      url === `${origin}/api/admin/v1/auth/me` &&
      /Failed to load resource.*401/.test(message.text())
    )
      return;
    if (
      expectedUnknown &&
      url === `${origin}/api/admin/v1/image-analysis/jobs` &&
      /Failed to load resource.*net::ERR_FAILED/.test(message.text())
    )
      return;
    if (
      expectedConflict &&
      url === `${origin}/api/admin/v1/image-analysis/jobs/fixture-image-1` &&
      /Failed to load resource.*409/.test(message.text())
    )
      return;
    if (
      expectedExpiredSession &&
      url === `${origin}/api/admin/v1/image-analysis` &&
      /Failed to load resource.*401/.test(message.text())
    )
      return;
    consoleErrors.push(message.text());
  });
  page.on("request", (request) => {
    if (
      request.method() !== "GET" &&
      (/\/conversations\/[^/]+\/messages$/.test(request.url()) ||
        /\/memory(?:\?|$|\/)/.test(request.url()))
    )
      conversationOrMemoryWrites++;
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
  const anonymous = await browser.newContext();
  expect(
    (
      await anonymous.request.get(`${origin}/api/admin/v1/image-analysis`)
    ).status(),
  ).toBe(401);
  expect(
    (
      await anonymous.request.get(
        `${origin}/api/admin/v1/image-analysis/jobs/not-owned`,
      )
    ).status(),
  ).toBe(401);
  await anonymous.close();
  const backendResponse = await page.request.get(
    "/api/admin/v1/image-analysis",
  );
  expect(backendResponse.status()).toBe(200);
  const initialBackend = await backendResponse.json();
  expect(initialBackend.items).toEqual([]);
  expect(initialBackend.credential_configured).toBe(false);
  // Even a CSRF regression cannot dispatch this: no image/question is supplied.
  const forbidden = await page.request.post(
    "/api/admin/v1/image-analysis/jobs",
    {
      headers: { Origin: origin },
      data: {
        client_request_id: crypto.randomUUID(),
        image_base64: "",
        question: "",
      },
    },
  );
  expect(forbidden.status()).toBe(403);
  expect(
    (
      await page.request.get("/api/admin/v1/image-analysis/jobs/not-owned")
    ).status(),
  ).toBe(404);

  const png = syntheticPng();
  const pngBase64 = png.toString("base64");
  // Independently validate the generated PNG with Chromium's native decoder.
  // This unattached synthetic-only Image does not change the product's blob
  // preview path, mock its decoder, or place a data URL into the DOM.
  expect(
    await page.evaluate(
      (encoded) =>
        new Promise((resolve, reject) => {
          const image = new Image();
          image.onload = () =>
            resolve({ width: image.naturalWidth, height: image.naturalHeight });
          image.onerror = () =>
            reject(new Error("synthetic PNG did not decode"));
          image.src = `data:image/png;base64,${encoded}`;
        }),
      pngBase64,
    ),
  ).toEqual({ width: 240, height: 160 });
  type Job = {
    id: string;
    status: string;
    revision: number;
    created_at: number;
    expires_at: number;
    model: string;
    question?: string;
    result?: string;
    width: number;
    height: number;
    image_sha256: string;
  };
  const now = () => Math.floor(Date.now() / 1000);
  const question =
    "只分析这张合成图片中的颜色与形状。" + "long-question-fragment_".repeat(8);
  const result =
    '合成结果文本：两个彩色矩形。<img src="https://example.invalid/never-load"> <script>throw new Error("never-run")</script>';
  const fixture = {
    available: true,
    provider: "DeepSeek",
    model: "deepseek-flash",
    credential_configured: true,
    items: [] as Job[],
    policy: {
      max_image_bytes: 1048576,
      max_width: 2048,
      max_height: 2048,
      max_pixels: 2097152,
      max_question_bytes: 2000,
      result_ttl_seconds: 86400,
    },
  };
  let attempts = 0;
  let deletes = 0;
  let jobGets = 0;
  let failedOnce = false;
  let conflictOnce = false;
  let expireSession = false;
  let releasePoll!: () => void;
  let pollSeen!: () => void;
  const pollGate = new Promise<void>((resolve) => {
    releasePoll = resolve;
  });
  const firstPollSeen = new Promise<void>((resolve) => {
    pollSeen = resolve;
  });
  const submissions: { id: string; question: string }[] = [];
  const deleteMatches: string[] = [];
  await page.evaluate(() => {
    const trace = { created: [] as string[], revoked: [] as string[] };
    const browser = window as unknown as { imagePreviewTrace: typeof trace };
    browser.imagePreviewTrace = trace;
    const create = URL.createObjectURL.bind(URL);
    const revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = (blob: Blob) => {
      const url = create(blob);
      trace.created.push(url);
      return url;
    };
    URL.revokeObjectURL = (url: string) => {
      trace.revoked.push(url);
      revoke(url);
    };
  });
  await page.route("**/api/admin/v1/image-analysis{,/**}", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    const method = request.method();
    if (pathname === "/api/admin/v1/image-analysis" && method === "GET") {
      if (expireSession) {
        await route.fulfill({ status: 401, json: { error: "unauthorized" } });
        return;
      }
      await route.fulfill({ json: fixture });
      return;
    }
    if (pathname === "/api/admin/v1/image-analysis/jobs" && method === "POST") {
      attempts++;
      const body = request.postDataJSON();
      if (
        Object.keys(body).sort().join(",") !==
        "client_request_id,image_base64,question"
      )
        fixtureViolations.push("unexpected-post-fields");
      if (
        body.image_base64 !== pngBase64 ||
        String(body.image_base64).startsWith("data:")
      )
        fixtureViolations.push("image-wire-mismatch");
      if (!request.headers()["x-csrf-token"])
        fixtureViolations.push("missing-browser-csrf");
      submissions.push({ id: body.client_request_id, question: body.question });
      if (!failedOnce) {
        failedOnce = true;
        await route.abort("failed");
        return;
      }
      const id = attempts === 2 ? "fixture-image-1" : "fixture-image-2";
      const item: Job = {
        id,
        status: "queued",
        revision: 1,
        created_at: now(),
        expires_at: now() + 86400,
        model: "deepseek-flash",
        question: body.question,
        width: 240,
        height: 160,
        image_sha256: crypto.createHash("sha256").update(png).digest("hex"),
      };
      fixture.items.unshift(item);
      await route.fulfill({ status: 202, json: item });
      return;
    }
    const item = fixture.items.find((entry) =>
      pathname.endsWith(`/${entry.id}`),
    );
    if (item && method === "GET") {
      jobGets++;
      if (item.id === "fixture-image-1") {
        pollSeen();
        await pollGate;
        item.status = "completed";
        item.revision = 2;
        item.result = result;
      }
      await route.fulfill({ json: item });
      return;
    }
    if (item && method === "DELETE") {
      deletes++;
      deleteMatches.push(request.headers()["if-match"] ?? "");
      if (item.id === "fixture-image-1" && !conflictOnce) {
        conflictOnce = true;
        item.revision++;
        await route.fulfill({
          status: 409,
          json: {
            error: {
              code: "image_analysis_revision_conflict",
              message: "任务已更新",
            },
          },
        });
        return;
      }
      item.status =
        item.status === "queued" || item.status === "running"
          ? "cancelled"
          : "deleted";
      item.revision++;
      item.question = undefined;
      item.result = undefined;
      await route.fulfill({ json: item });
      return;
    }
    fixtureViolations.push("unexpected-image-route");
    await route.fulfill({
      status: 400,
      json: { error: "unexpected_fixture_request" },
    });
  });
  // Legacy workbench survives only behind the explicit development region.
  await page.evaluate(() => {
    location.hash = "images";
  });
  const panel = page.getByRole("region", { name: "独立图片分析", exact: true });
  await expect(page).toHaveURL(/#diagnostics$/);
  await expect(panel).toContainText("DeepSeek · deepseek-flash · 凭证已配置");
  await expect(panel).toContainText("不自动接入普通聊天或助手记忆");
  await expect(panel).toContainText("结果最多保留 24 小时");
  async function chooseSyntheticPng() {
    const chooser = page.waitForEvent("filechooser");
    await panel.getByLabel("选择本地 PNG 图片", { exact: true }).click();
    await (
      await chooser
    ).setFiles({
      name: "synthetic-two-rectangles.png",
      mimeType: "image/png",
      buffer: png,
    });
    const preview = panel.getByRole("img", {
      name: "待发送 PNG 的本地预览",
      exact: true,
    });
    await expect(preview).toBeVisible();
    await expect
      .poll(() =>
        preview.evaluate((image) => ({
          complete: (image as HTMLImageElement).complete,
          width: (image as HTMLImageElement).naturalWidth,
          height: (image as HTMLImageElement).naturalHeight,
        })),
      )
      .toEqual({ complete: true, width: 240, height: 160 });
    expect(
      await panel
        .getByLabel("选择本地 PNG 图片", { exact: true })
        .evaluate((input) => (input as HTMLInputElement).files?.[0]?.name),
    ).toBe("synthetic-two-rectangles.png");
    expect(await preview.getAttribute("src")).toMatch(/^blob:/);
  }
  const postRequest = (request: { method(): string; url(): string }) =>
    request.method() === "POST" &&
    request.url() === `${origin}/api/admin/v1/image-analysis/jobs`;
  await chooseSyntheticPng();
  await panel.getByLabel("要分析的问题", { exact: true }).fill(question);
  await expect(
    panel.getByRole("button", { name: "发送至 DeepSeek 并分析", exact: true }),
  ).toBeEnabled();
  // Observe longer than a poll interval: choosing a file must not upload it.
  await expect(
    page.waitForRequest(postRequest, { timeout: 2200 }),
  ).rejects.toThrow(/Timeout/);
  expect(attempts).toBe(0);
  const firstPreview = await panel
    .getByRole("img", { name: "待发送 PNG 的本地预览", exact: true })
    .getAttribute("src");
  await page.getByRole("button", { name: "运行总览", exact: true }).click();
  expect(
    await page.evaluate(
      (url) =>
        (
          window as unknown as {
            imagePreviewTrace: { revoked: string[] };
          }
        ).imagePreviewTrace.revoked.includes(url!),
      firstPreview,
    ),
  ).toBe(true);
  await page.evaluate(() => {
    location.hash = "images";
  });
  await chooseSyntheticPng();
  await panel.getByLabel("要分析的问题", { exact: true }).fill(question);
  expectedUnknown = true;
  await panel
    .getByRole("button", { name: "发送至 DeepSeek 并分析", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText("不会自动重发图片");
  await expect(
    panel.getByLabel("要分析的问题", { exact: true }),
  ).toBeDisabled();
  await expect(
    panel.getByRole("img", { name: "待发送 PNG 的本地预览", exact: true }),
  ).toBeVisible();
  await expect(
    page.waitForRequest(postRequest, { timeout: 2200 }),
  ).rejects.toThrow(/Timeout/);
  expect(attempts).toBe(1);
  await panel
    .getByRole("button", { name: "刷新图片任务", exact: true })
    .click();
  await expect(panel.getByLabel("要分析的问题", { exact: true })).toBeEnabled();
  await panel
    .getByRole("button", { name: "发送至 DeepSeek 并分析", exact: true })
    .click();
  await expect(
    panel.getByRole("img", { name: "待发送 PNG 的本地预览", exact: true }),
  ).toHaveCount(0);
  expect(submissions[0]!.id).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,
  );
  expect(submissions[1]!.id).toBe(submissions[0]!.id);
  expect(submissions[0]!.question).toBe(question);
  await firstPollSeen;
  await expect(
    page.waitForRequest(
      (request) =>
        request.method() === "GET" &&
        request.url().endsWith("/image-analysis/jobs/fixture-image-1"),
      { timeout: 2200 },
    ),
  ).rejects.toThrow(/Timeout/);
  expect(jobGets).toBe(1);
  releasePoll();
  await expect(panel.getByLabel("图片分析结果", { exact: true })).toHaveText(
    result,
  );
  expect(await panel.locator("script").count()).toBe(0);
  expect(await panel.locator("img").count()).toBe(0);
  expect(await panel.locator("a").count()).toBe(0);
  await chooseSyntheticPng();
  await panel
    .getByLabel("要分析的问题", { exact: true })
    .fill("第二张合成 PNG，仅预览，随后验证取消。");
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
      path: path.join(evidenceDir, `image-analysis-fixture-${width}.png`),
      animations: "disabled",
    });
  }
  const completed = panel.getByRole("article", {
    name: "图片任务 1",
    exact: true,
  });
  await completed
    .getByRole("button", { name: "清除此任务", exact: true })
    .click();
  let confirmation = panel.getByRole("region", {
    name: "确认清除图片任务",
    exact: true,
  });
  await expect(confirmation).toContainText(question);
  await expect(confirmation).toContainText(
    "原数据库/WAL及离线备份按独立策略保留",
  );
  expectedConflict = true;
  await confirmation
    .getByRole("button", { name: "确认取消或清除本任务", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText(
    "当前展示已保留，请刷新图片任务",
  );
  await expect(panel.getByLabel("图片分析结果", { exact: true })).toHaveText(
    result,
  );
  await expect(
    completed.getByRole("button", { name: "清除此任务", exact: true }),
  ).toBeDisabled();
  await panel
    .getByRole("button", { name: "刷新图片任务", exact: true })
    .click();
  await completed
    .getByRole("button", { name: "清除此任务", exact: true })
    .click();
  confirmation = panel.getByRole("region", {
    name: "确认清除图片任务",
    exact: true,
  });
  await confirmation
    .getByRole("button", { name: "确认取消或清除本任务", exact: true })
    .click();
  await expect(panel.getByLabel("图片分析结果", { exact: true })).toHaveCount(
    0,
  );
  await expect(panel).not.toContainText(question);
  expect(deleteMatches.slice(0, 2)).toEqual(['"2"', '"3"']);
  await panel
    .getByRole("button", { name: "发送至 DeepSeek 并分析", exact: true })
    .click();
  await expect(
    panel.getByRole("img", { name: "待发送 PNG 的本地预览", exact: true }),
  ).toHaveCount(0);
  const active = panel.getByRole("article", {
    name: "图片任务 1",
    exact: true,
  });
  await active
    .getByRole("button", { name: "取消并清除此任务", exact: true })
    .click();
  confirmation = panel.getByRole("region", {
    name: "确认清除图片任务",
    exact: true,
  });
  await expect(confirmation).toContainText("第二张合成 PNG");
  await confirmation
    .getByRole("button", { name: "确认取消或清除本任务", exact: true })
    .click();
  await expect(active).toContainText("已取消");
  expect(deleteMatches[2]).toBe('"1"');
  expect(submissions[2]!.id).not.toBe(submissions[0]!.id);
  const ttlResult = "合成的即将到期结果，必须清除正文";
  fixture.items.unshift({
    id: "fixture-expiring",
    status: "completed",
    revision: 1,
    created_at: now(),
    expires_at: now() + 2,
    question: "到期问题正文",
    result: ttlResult,
    model: "deepseek-flash",
    width: 240,
    height: 160,
    image_sha256: "synthetic-expiring-hash",
  });
  await panel
    .getByRole("button", { name: "刷新图片任务", exact: true })
    .click();
  await expect(panel).toContainText(ttlResult);
  await expect(panel).not.toContainText(ttlResult, { timeout: 5000 });
  await expect(panel).not.toContainText("到期问题正文");
  await chooseSyntheticPng();
  await panel
    .getByLabel("要分析的问题", { exact: true })
    .fill("登录过期时必须清除的本地草稿");
  expireSession = true;
  expectedExpiredSession = true;
  await panel
    .getByRole("button", { name: "刷新图片任务", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText(
    "登录已过期，本地图片预览、问题和分析结果已清除",
  );
  await expect(
    panel.getByRole("img", { name: "待发送 PNG 的本地预览", exact: true }),
  ).toHaveCount(0);
  await expect(panel.getByLabel("要分析的问题", { exact: true })).toHaveValue(
    "",
  );
  const trace = await page.evaluate(
    () =>
      (
        window as unknown as {
          imagePreviewTrace: { created: string[]; revoked: string[] };
        }
      ).imagePreviewTrace,
  );
  expect(trace.created.length).toBe(4);
  expect(trace.created.every((url) => trace.revoked.includes(url))).toBe(true);
  expect(attempts).toBe(3);
  expect(deletes).toBe(3);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(await panel.textContent()).not.toContain(pngBase64);
  // APIRequestContext bypasses page.route: fixtures created no real backend job.
  expect(
    await (await page.request.get("/api/admin/v1/image-analysis")).json(),
  ).toEqual(initialBackend);
  expect(conversationOrMemoryWrites).toBe(0);
  expect(fixtureViolations).toEqual([]);
  expect(externalRequests).toEqual([]);
  expect(pageFailures).toEqual([]);
  expect(consoleErrors).toEqual([]);
});
