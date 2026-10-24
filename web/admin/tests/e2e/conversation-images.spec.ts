import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { deflateSync } from "node:zlib";

// Real isolated backend: authentication, image capabilities, CSRF and empty
// owned conversation history. Valid image/text responses below are fixtures.
// PNG bytes are generated here and decoded natively; no user image, real model
// request or claim of provider vision quality is involved in this browser test.
const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/conversation-image-e2e/data",
);
const accountFile = path.join(dataDir, "e2e-account.json");
const evidenceDir = path.resolve(
  "../../.local/conversation-image-e2e/screenshots",
);
function png() {
  const width = 240;
  const height = 160;
  const raw = Buffer.alloc((width * 3 + 1) * height);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++)
      raw.set(
        x > 30 && x < 95 && y > 30 && y < 130
          ? [235, 35, 35]
          : x > 145 && x < 215 && y > 50 && y < 140
            ? [20, 170, 65]
            : [255, 255, 255],
        y * (width * 3 + 1) + 1 + x * 3,
      );
  const chunk = (kind: string, payload: Buffer) => {
    const body = Buffer.concat([Buffer.from(kind), payload]);
    let value = 0xffffffff;
    for (const byte of body) {
      value ^= byte;
      for (let bit = 0; bit < 8; bit++)
        value = (value >>> 1) ^ (value & 1 ? 0xedb88320 : 0);
    }
    const length = Buffer.alloc(4);
    const crc = Buffer.alloc(4);
    length.writeUInt32BE(payload.length);
    crc.writeUInt32BE((value ^ 0xffffffff) >>> 0);
    return Buffer.concat([length, body, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width);
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
        username: `conversation_${crypto.randomBytes(4).toString("hex")}`,
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

test("normal conversation PNG messages preserve frozen persona, visual grounding, idempotency and diagnostic boundaries", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(120000);
  const origin = new URL(baseURL!).origin;
  expect(new URL(baseURL!).port, "never target the production tunnel").not.toBe(
    "18419",
  );
  let authenticated = false;
  let expectedUnknown = false;
  const consoleErrors: string[] = [];
  const pageErrors: string[] = [];
  const externalRequests: string[] = [];
  const violations: string[] = [];
  page.on("pageerror", () => pageErrors.push("uncaught-browser-error"));
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
      /\/image-messages$/.test(url) &&
      /Failed to load resource.*net::ERR_FAILED/.test(message.text())
    )
      return;
    consoleErrors.push(message.text());
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
      await anonymous.request.get(
        `${origin}/api/admin/v1/conversation-images/capabilities`,
      )
    ).status(),
  ).toBe(401);
  await anonymous.close();
  const capabilities = await page.request.get(
    "/api/admin/v1/conversation-images/capabilities",
  );
  expect(capabilities.status()).toBe(200);
  const actualCaps = await capabilities.json();
  expect(actualCaps).toMatchObject({
    provider: "deepseek",
    model: "deepseek-flash",
    basis: "vision_description",
    max_image_bytes: 1048576,
    max_images: 1,
  });
  expect(
    actualCaps.available,
    "isolated QA has no actual provider credential",
  ).toBe(false);
  const me = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const headers = { Origin: origin, "X-CSRF-Token": me.csrf_token };
  const suffix = crypto.randomBytes(4).toString("hex");
  const createdA = await page.request.post("/api/admin/v1/conversations", {
    headers,
    data: { title: `正常图片会话甲_${suffix}` },
  });
  const createdB = await page.request.post("/api/admin/v1/conversations", {
    headers,
    data: { title: `独立会话乙_${suffix}` },
  });
  expect(createdA.status()).toBe(200);
  expect(createdB.status()).toBe(200);
  const a = await createdA.json();
  const b = await createdB.json();
  const baseA = `/api/admin/v1/conversations/${a.id}`;
  const baseB = `/api/admin/v1/conversations/${b.id}`;
  expect(
    (
      await page.request.post(`${baseA}/image-messages`, {
        headers: { Origin: origin },
        data: {
          content: "",
          client_message_id: crypto.randomUUID(),
          image_base64: "",
        },
      })
    ).status(),
  ).toBe(403);
  const actualHistory = await (
    await page.request.get(`${baseA}/messages`)
  ).json();
  expect(actualHistory.items).toEqual([]);
  const picture = png();
  const picture64 = picture.toString("base64");
  const pictureHash = crypto.createHash("sha256").update(picture).digest("hex");
  const question = "当前角色请帮我分析图片，并结合我们此前的对话回答。";
  const answer =
    '这是当前文本助手基于视觉描述给出的回答：红绿两个矩形。<img src="https://example.invalid/never">';
  const privateDescription =
    "SYNTHETIC_INTERNAL_VISUAL_DESCRIPTION_NEVER_PUBLIC";
  type Message = {
    id: string;
    role: string;
    content: string;
    created_at: number;
    image?: {
      width: number;
      height: number;
      sha256: string;
      status: string;
      original_retained: false;
    };
    basis?: string;
  };
  const histories: Record<string, Message[]> = {
    [a.id]: [],
    [b.id]: [],
    "draft-fixture": [],
  };
  let imageAttempts = 0;
  let acceptedImageTasks = 0;
  let imageGets = 0;
  let ordinaryPosts = 0;
  let memoryPosts = 0;
  let standaloneWrites = 0;
  let diagnosticGets = 0;
  const imageBodies: unknown[] = [];
  const runKinds = new Map<string, "image" | "text" | "memory">();
  await page.addInitScript(() => {
    const trace = { created: [] as string[], revoked: [] as string[] };
    (
      window as unknown as { conversationPreviewTrace: typeof trace }
    ).conversationPreviewTrace = trace;
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
  await page.route("**/api/admin/v1/**", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    const method = request.method();
    if (pathname === "/api/admin/v1/config" && method === "GET") {
      const actual = await route.fetch();
      const body = await actual.json();
      body.config.model.credential_ref = "fixture-configured-reference";
      body.config.model.model = "fixture-current-text-assistant";
      await route.fulfill({ response: actual, json: body });
      return;
    }
    if (pathname === "/api/admin/v1/status" && method === "GET") {
      const actual = await route.fetch();
      const body = await actual.json();
      body.model = {
        configured: true,
        model: "fixture-current-text-assistant",
      };
      await route.fulfill({ response: actual, json: body });
      return;
    }
    if (pathname === "/api/admin/v1/conversation-images/capabilities") {
      await route.fulfill({ json: { ...actualCaps, available: true } });
      return;
    }
    if (pathname === "/api/admin/v1/conversations" && method === "GET") {
      const actual = await route.fetch();
      const body = await actual.json();
      body.items = body.items.map((item: { id: string }) =>
        item.id === a.id
          ? {
              ...item,
              character: {
                card_id: "fixture-character",
                name: "冻结的发布角色",
                revision: 1,
                is_test: false,
              },
              test: false,
            }
          : item,
      );
      body.items.push({
        id: "draft-fixture",
        title: "角色草稿测试（合成）",
        created_at: Math.floor(Date.now() / 1000),
        test: true,
        character: { name: "草稿", revision: 1, is_test: true },
      });
      await route.fulfill({ response: actual, json: body });
      return;
    }
    const conversation = pathname.startsWith(baseA + "/")
      ? a.id
      : pathname.startsWith(baseB + "/")
        ? b.id
        : pathname.startsWith("/api/admin/v1/conversations/draft-fixture/")
          ? "draft-fixture"
          : null;
    if (conversation && pathname.endsWith("/messages") && method === "GET") {
      await route.fulfill({ json: { items: histories[conversation] } });
      return;
    }
    if (
      conversation &&
      pathname.endsWith("/image-messages") &&
      method === "POST"
    ) {
      imageAttempts++;
      const body = request.postDataJSON();
      imageBodies.push(body);
      if (
        conversation !== a.id ||
        body.image_base64 !== picture64 ||
        body.content !== question ||
        Object.keys(body).sort().join(",") !==
          "client_message_id,content,image_base64"
      )
        violations.push("image-message-wire-mismatch");
      if (imageAttempts === 1) {
        acceptedImageTasks++;
        histories[a.id]!.push({
          id: "fixture-image-user",
          role: "user",
          content: question,
          created_at: Math.floor(Date.now() / 1000),
          image: {
            width: 240,
            height: 160,
            sha256: pictureHash,
            status: "queued",
            original_retained: false,
          },
        });
        runKinds.set("fixture-image-run", "image");
        await route.abort("failed");
        return;
      }
      await route.fulfill({
        status: 202,
        json: { run_id: "fixture-image-run", conversation_id: a.id },
      });
      return;
    }
    if (conversation && pathname.endsWith("/messages") && method === "POST") {
      const body = request.postDataJSON();
      if (body.image_base64 !== undefined)
        violations.push("image-sent-through-text-route");
      const memory = String(body.content).startsWith("/memory");
      if (memory) memoryPosts++;
      else ordinaryPosts++;
      const run = `fixture-${memory ? "memory" : "text"}-run`;
      runKinds.set(run, memory ? "memory" : "text");
      histories[conversation]!.push({
        id: `fixture-${run}-user`,
        role: "user",
        content: body.content,
        created_at: Math.floor(Date.now() / 1000),
      });
      histories[conversation]!.push({
        id: `fixture-${run}-reply`,
        role: "assistant",
        content: memory ? "记忆命令结果（合成）" : "普通文本回答（合成）",
        created_at: Math.floor(Date.now() / 1000),
      });
      await route.fulfill({
        status: 202,
        json: { run_id: run, conversation_id: conversation },
      });
      return;
    }
    if (pathname.startsWith("/api/admin/v1/runs/") && method === "GET") {
      const run = pathname.split("/").at(-1)!;
      const kind = runKinds.get(run);
      if (kind === "image") {
        imageGets++;
        if (imageGets === 1) {
          await route.fulfill({ json: { id: run, status: "running" } });
          return;
        }
        histories[a.id]![0]!.image!.status = "described";
        if (
          !histories[a.id]!.some((item) => item.id === "fixture-image-answer")
        )
          histories[a.id]!.push({
            id: "fixture-image-answer",
            role: "assistant",
            content: answer,
            basis: "vision_description",
            created_at: Math.floor(Date.now() / 1000),
          });
      }
      await route.fulfill({ json: { id: run, status: "completed" } });
      return;
    }
    if (pathname === "/api/admin/v1/wasm-workbench") {
      diagnosticGets++;
      await route.fulfill({
        json: {
          available: false,
          generation_available: false,
          generation_model: "",
          engine_version: "wasmi-2.0.0",
          policy_version: "pure-i32-v1",
          jobs: [],
          artifacts: [],
          policy: {
            min_cases: 2,
            max_cases: 8,
            max_spec_bytes: 4000,
            max_wat_bytes: 65536,
            max_wasm_bytes: 65536,
            fuel_per_call: 100000,
          },
        },
      });
      return;
    }
    if (pathname === "/api/admin/v1/image-analysis") {
      diagnosticGets++;
      await route.fulfill({
        json: {
          available: false,
          provider: "DeepSeek",
          model: "deepseek-flash",
          credential_configured: false,
          items: [],
          policy: {
            max_image_bytes: 1048576,
            max_width: 2048,
            max_height: 2048,
            max_pixels: 2097152,
            max_question_bytes: 2000,
            result_ttl_seconds: 86400,
          },
        },
      });
      return;
    }
    if (
      (pathname.startsWith("/api/admin/v1/wasm-workbench/") ||
        pathname.startsWith("/api/admin/v1/image-analysis/")) &&
      method !== "GET"
    ) {
      standaloneWrites++;
      await route.fulfill({
        status: 400,
        json: { error: "unexpected-experiment-write" },
      });
      return;
    }
    await route.continue();
  });
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  const nav = page.getByRole("navigation", { name: "主导航", exact: true });
  await expect(
    nav.getByRole("button", { name: "图片分析", exact: true }),
  ).toHaveCount(0);
  await expect(
    nav.getByRole("button", { name: "计算工坊", exact: true }),
  ).toHaveCount(0);
  await expect(
    nav.getByRole("button", { name: "任务与诊断", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".device-card")).toContainText("Ubuntu 24.04 LTS");
  await nav.getByRole("button", { name: "对话工作台", exact: true }).click();
  const workspace = page.locator(".chat-workspace");
  await workspace.getByRole("button").filter({ hasText: a.title }).click();
  await expect(workspace.locator(".chat-heading")).toContainText(
    "冻结的发布角色",
  );
  async function chooseImage() {
    const chooser = page.waitForEvent("filechooser");
    await workspace
      .getByLabel("附加一张对话 PNG 图片", { exact: true })
      .click();
    await (
      await chooser
    ).setFiles({
      name: "synthetic-conversation.png",
      mimeType: "image/png",
      buffer: picture,
    });
    const preview = workspace.getByRole("img", {
      name: "本次对话图片的本地预览",
      exact: true,
    });
    await expect(preview).toBeVisible();
    await expect
      .poll(() =>
        preview.evaluate((image) => ({
          width: (image as HTMLImageElement).naturalWidth,
          height: (image as HTMLImageElement).naturalHeight,
        })),
      )
      .toEqual({ width: 240, height: 160 });
  }
  await chooseImage();
  await workspace
    .getByLabel("对话消息", { exact: true })
    .fill("/memory save 不可把图片吞成命令");
  await expect(
    workspace.getByRole("button", { name: "发送", exact: true }),
  ).toBeDisabled();
  await expect(workspace.getByRole("alert")).toContainText(
    "不能发送任何 / 开头的控制命令",
  );
  await workspace.getByLabel("对话消息", { exact: true }).fill(question);
  expect(imageAttempts).toBe(0);
  expectedUnknown = true;
  await workspace.getByRole("button", { name: "发送", exact: true }).click();
  await expect(workspace.getByRole("alert")).toContainText("请求编号已冻结");
  await expect(
    workspace.getByLabel("对话消息", { exact: true }),
  ).toBeDisabled();
  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toContain("提交结果未确认");
    await dialog.dismiss();
  });
  await nav.getByRole("button", { name: "任务与诊断", exact: true }).click();
  await expect(page).toHaveURL(/#chat$/);
  expect(imageAttempts).toBe(1);
  await workspace
    .getByRole("button", { name: "查看会话历史", exact: true })
    .click();
  const metadata = workspace.getByLabel("历史图片信息", { exact: true });
  await expect(metadata).toContainText("图片处理中");
  await expect(metadata).not.toContainText("处理图片已清理");
  await expect(
    page.waitForRequest(
      (request) =>
        request.method() === "POST" &&
        request.url().endsWith("/image-messages"),
      { timeout: 2200 },
    ),
  ).rejects.toThrow(/Timeout/);
  await workspace
    .getByRole("button", { name: "原样重试此消息", exact: true })
    .click();
  await expect(
    workspace.getByRole("img", { name: "本次对话图片的本地预览", exact: true }),
  ).toHaveCount(0);
  await expect(workspace.getByText(answer, { exact: true })).toBeVisible();
  await expect(metadata).toContainText("视觉描述已生成，处理图片已清理");
  await expect(
    workspace.getByText("基于图片视觉描述的回答", { exact: true }),
  ).toBeVisible();
  expect(imageAttempts).toBe(2);
  expect(acceptedImageTasks).toBe(1);
  expect(imageBodies[1]).toEqual(imageBodies[0]);
  expect(
    await workspace
      .locator(".message-content img,.message-content script")
      .count(),
  ).toBe(0);
  await workspace.getByRole("button").filter({ hasText: b.title }).click();
  await expect(workspace).not.toContainText(question);
  await workspace.getByRole("button").filter({ hasText: a.title }).click();
  await expect(workspace).toContainText(answer);
  await chooseImage();
  await workspace
    .getByLabel("对话消息", { exact: true })
    .fill("另一张本地预览，不发送。");
  fs.mkdirSync(evidenceDir, { recursive: true });
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 950 });
    await workspace.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    const rolePicker = await workspace
      .getByLabel("新对话角色", { exact: true })
      .boundingBox();
    expect(rolePicker!.width).toBeGreaterThanOrEqual(150);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await workspace.screenshot({
      path: path.join(evidenceDir, `conversation-images-${width}.png`),
      animations: "disabled",
    });
  }
  await page.setViewportSize({ width: 1440, height: 950 });
  await nav.getByRole("button", { name: "任务与诊断", exact: true }).click();
  const development = page.locator(".development-diagnostics");
  await expect(development).toHaveJSProperty("open", false);
  expect(diagnosticGets).toBe(0);
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 950 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await page.locator(".diagnostics-page").screenshot({
      path: path.join(evidenceDir, `diagnostics-default-${width}.png`),
      animations: "disabled",
    });
  }
  await page.setViewportSize({ width: 1440, height: 950 });
  await page.evaluate(() => {
    location.hash = "wasm";
  });
  await expect(page).toHaveURL(/#diagnostics$/);
  await expect(development).toHaveJSProperty("open", true);
  await expect(
    page.getByRole("region", { name: "纯整数计算工坊", exact: true }),
  ).toBeVisible();
  await page.evaluate(() => {
    location.hash = "images";
  });
  await expect(
    page.getByRole("region", { name: "独立图片分析", exact: true }),
  ).toBeVisible();
  expect(standaloneWrites).toBe(0);
  await nav.getByRole("button", { name: "对话工作台", exact: true }).click();
  await workspace
    .getByRole("button")
    .filter({ hasText: "角色草稿测试（合成）" })
    .click();
  await expect(
    workspace.getByLabel("附加一张对话 PNG 图片", { exact: true }),
  ).toBeDisabled();
  await expect(workspace).toContainText("草稿测试会话暂不支持图片");
  await workspace.getByRole("button").filter({ hasText: a.title }).click();
  await workspace.getByLabel("对话消息", { exact: true }).fill("普通文本回归");
  await workspace.getByRole("button", { name: "发送", exact: true }).click();
  await expect(
    workspace.getByText("普通文本回答（合成）", { exact: true }),
  ).toBeVisible();
  await workspace.getByLabel("对话消息", { exact: true }).fill("/memory list");
  await workspace.getByRole("button", { name: "发送", exact: true }).click();
  await expect(
    workspace.getByText("记忆命令结果（合成）", { exact: true }),
  ).toBeVisible();
  const trace = await page.evaluate(
    () =>
      (
        window as unknown as {
          conversationPreviewTrace: { created: string[]; revoked: string[] };
        }
      ).conversationPreviewTrace,
  );
  expect(trace.created.length).toBe(2);
  expect(trace.created.every((url) => trace.revoked.includes(url))).toBe(true);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  await expect(page.locator("body")).not.toContainText(picture64);
  await expect(page.locator("body")).not.toContainText(privateDescription);
  expect(await (await page.request.get(`${baseA}/messages`)).json()).toEqual(
    actualHistory,
  );
  expect(
    await (
      await page.request.get("/api/admin/v1/conversation-images/capabilities")
    ).json(),
  ).toEqual(actualCaps);
  expect(ordinaryPosts).toBe(1);
  expect(memoryPosts).toBe(1);
  expect(standaloneWrites).toBe(0);
  expect(violations).toEqual([]);
  expect(externalRequests).toEqual([]);
  expect(pageErrors).toEqual([]);
  expect(consoleErrors).toEqual([]);
});
