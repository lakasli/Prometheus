import { test, expect, type Page } from "@playwright/test";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

// Real isolated backend: owner authentication, empty GET, 401 and CSRF only.
// All generation, worker values, hashes, approval and execution below are UI
// fixtures. No model or real WASM worker is invoked; this is not execution QA.
const dataDir = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/wasm-e2e/data",
);
const accountFile = path.join(dataDir, "e2e-account.json");
const evidenceDir = path.resolve("../../.local/wasm-e2e/screenshots");

async function authenticate(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const account = fs.existsSync(accountFile)
    ? (JSON.parse(fs.readFileSync(accountFile, "utf8")) as {
        username: string;
        password: string;
      })
    : {
        username: `wasm_${crypto.randomBytes(4).toString("hex")}`,
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

test("pure-i32 workbench browser fixtures separate explicit generation, hash approval and manual execution", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(120000);
  const origin = new URL(baseURL!).origin;
  expect(
    new URL(baseURL!).port,
    "workbench E2E must not use the production tunnel",
  ).not.toBe("18419");
  const apiRoot = "/api/admin/v1/wasm-workbench";
  let authenticated = false;
  let expectedGenerationFailure = false;
  let expectedExecutionFailure = false;
  let expectedConflict = false;
  let expectedDetailFailure = false;
  let expectedSessionFailure = false;
  const consoleErrors: string[] = [];
  const pageErrors: string[] = [];
  const foreignRequests: string[] = [];
  const fixtureViolations: string[] = [];
  let conversationOrMemoryWrites = 0;
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
      expectedGenerationFailure &&
      url === `${origin}${apiRoot}/generate` &&
      /Failed to load resource.*net::ERR_FAILED/.test(message.text())
    )
      return;
    if (
      expectedExecutionFailure &&
      url === `${origin}${apiRoot}/artifacts/fixture-a/execute` &&
      /Failed to load resource.*net::ERR_FAILED/.test(message.text())
    )
      return;
    if (
      expectedConflict &&
      url === `${origin}${apiRoot}/artifacts/fixture-a/approve` &&
      /Failed to load resource.*409/.test(message.text())
    )
      return;
    if (
      expectedDetailFailure &&
      url === `${origin}${apiRoot}/artifacts/fixture-a` &&
      /Failed to load resource.*503/.test(message.text())
    )
      return;
    if (
      expectedSessionFailure &&
      url === `${origin}${apiRoot}` &&
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
      foreignRequests.push(url.hostname);
      await route.abort();
    } else await route.continue();
  });
  await authenticate(page);
  authenticated = true;
  const anonymous = await browser.newContext();
  expect((await anonymous.request.get(`${origin}${apiRoot}`)).status()).toBe(
    401,
  );
  expect(
    (
      await anonymous.request.get(`${origin}${apiRoot}/artifacts/not-owned`)
    ).status(),
  ).toBe(401);
  await anonymous.close();
  const actualResponse = await page.request.get(apiRoot);
  expect(actualResponse.status()).toBe(200);
  const actualBefore = await actualResponse.json();
  expect(actualBefore.jobs).toEqual([]);
  expect(actualBefore.artifacts).toEqual([]);
  expect(
    actualBefore.available,
    "Windows UI QA must leave the real worker gate closed",
  ).toBe(false);
  expect(actualBefore.generation_available).toBe(false);
  // Deliberately invalid input: even a CSRF regression cannot dispatch a model.
  expect(
    (
      await page.request.post(`${apiRoot}/generate`, {
        headers: { Origin: origin },
        data: {
          client_request_id: crypto.randomUUID(),
          name: "",
          spec: "",
          tests: [],
        },
      })
    ).status(),
  ).toBe(403);
  expect(
    (await page.request.get(`${apiRoot}/artifacts/not-owned`)).status(),
  ).toBe(404);

  type Case = { a: number; b: number; expected: number };
  type Result = { actual: number; passed: boolean };
  type Artifact = {
    id: string;
    name: string;
    revision: number;
    status: string;
    wasm_sha256: string;
    tests_sha256: string;
    engine_version: string;
    policy_version: string;
    created_at: number;
    wat: string;
    tests: Case[];
    test_results: Result[];
  };
  type Job = {
    id: string;
    kind: string;
    status: string;
    revision: number;
    created_at: number;
    expires_at: number;
    name?: string;
    artifact_id?: string;
    result?: number;
    error_code?: string;
  };
  const now = () => Math.floor(Date.now() / 1000);
  const tests: Case[] = [
    { a: 0, b: 0, expected: 0 },
    { a: 2147483647, b: 1, expected: -2147483648 },
  ];
  const artifactA: Artifact = {
    id: "fixture-a",
    name: "合成的纯整数加法",
    revision: 1,
    status: "ready",
    wasm_sha256: crypto
      .createHash("sha256")
      .update("synthetic-artifact-hash-not-a-real-wasm")
      .digest("hex"),
    tests_sha256: crypto
      .createHash("sha256")
      .update(JSON.stringify(tests))
      .digest("hex"),
    engine_version: "wasmi-2.0.0",
    policy_version: "pure-i32-v1",
    created_at: now(),
    wat: '(module ;; A的合成源码 <img src="https://example.invalid/never">\n  (func (export "calc") (param i32 i32) (result i32)\n    local.get 0 local.get 1 i32.add))',
    tests,
    test_results: [
      { actual: 0, passed: true },
      { actual: -2147483648, passed: true },
    ],
  };
  const artifactB: Artifact = {
    ...artifactA,
    id: "fixture-b",
    name: "合成的另一份私有制品",
    wasm_sha256: "b".repeat(64),
    wat: '(module ;; B独立源码\n (func (export "calc") (param i32 i32) (result i32) i32.const 0))',
  };
  const fixture = {
    available: true,
    generation_available: true,
    generation_model: "synthetic-generation-model",
    engine_version: "wasmi-2.0.0",
    policy_version: "pure-i32-v1",
    jobs: [] as Job[],
    artifacts: [] as Artifact[],
    policy: {
      min_cases: 2,
      max_cases: 8,
      max_spec_bytes: 4000,
      max_wat_bytes: 65536,
      max_wasm_bytes: 65536,
      fuel_per_call: 100000,
    },
  };
  let generationAttempts = 0;
  let executionAttempts = 0;
  let approvals = 0;
  let rejections = 0;
  let revocations = 0;
  let cancellations = 0;
  let conflictOnce = false;
  let failedApprovedDetail = false;
  let expireSession = false;
  let holdFirstA = true;
  let heldSeen!: () => void;
  let releaseHeld!: () => void;
  let heldDone!: () => void;
  const heldRequestSeen = new Promise<void>((resolve) => {
    heldSeen = resolve;
  });
  const heldGate = new Promise<void>((resolve) => {
    releaseHeld = resolve;
  });
  const heldHandled = new Promise<void>((resolve) => {
    heldDone = resolve;
  });
  const generations: {
    client_request_id: string;
    name: string;
    spec: string;
    tests: Case[];
  }[] = [];
  const executions: {
    client_request_id: string;
    a: number;
    b: number;
    wasm_sha256: string;
  }[] = [];
  const matches: { action: string; value: string }[] = [];
  await page.evaluate(() => {
    const trace = { calls: 0 };
    (window as unknown as { nativeWasmTrace: typeof trace }).nativeWasmTrace =
      trace;
    const blocked = () => {
      trace.calls++;
      return Promise.reject(
        new Error("browser-wasm-execution-forbidden-in-ui-fixture"),
      );
    };
    WebAssembly.compile = blocked as typeof WebAssembly.compile;
    WebAssembly.compileStreaming =
      blocked as typeof WebAssembly.compileStreaming;
    WebAssembly.instantiate = blocked as typeof WebAssembly.instantiate;
    WebAssembly.instantiateStreaming =
      blocked as typeof WebAssembly.instantiateStreaming;
  });
  await page.route("**/api/admin/v1/wasm-workbench{,/**}", async (route) => {
    const request = route.request();
    const pathname = new URL(request.url()).pathname;
    const method = request.method();
    const rest = pathname.slice(apiRoot.length);
    if (!rest && method === "GET") {
      if (expireSession) {
        await route.fulfill({ status: 401, json: { error: "unauthorized" } });
        return;
      }
      await route.fulfill({ json: fixture });
      return;
    }
    if (rest === "/generate" && method === "POST") {
      generationAttempts++;
      const body = request.postDataJSON();
      generations.push(body);
      if (
        Object.keys(body).sort().join(",") !==
          "client_request_id,name,spec,tests" ||
        JSON.stringify(body.tests) !== JSON.stringify(tests)
      )
        fixtureViolations.push("invalid-generation-body");
      if (generationAttempts === 1) {
        await route.abort("failed");
        return;
      }
      const item: Job = {
        id: "fixture-generation",
        kind: "generate",
        status: "queued",
        revision: 1,
        created_at: now(),
        expires_at: now() + 300,
        name: body.name,
      };
      fixture.jobs.unshift(item);
      await route.fulfill({ status: 202, json: item });
      return;
    }
    if (rest.startsWith("/jobs/")) {
      const item = fixture.jobs.find((entry) => rest.endsWith(`/${entry.id}`));
      if (item && method === "GET") {
        if (item.kind === "generate") {
          item.status = "completed";
          item.revision++;
          item.artifact_id = artifactA.id;
          fixture.artifacts = [artifactA, artifactB];
        } else if (item.id === "fixture-execution-zero") {
          item.status = "completed";
          item.revision++;
          item.result = 0;
        }
        await route.fulfill({ json: item });
        return;
      }
      if (item && method === "DELETE") {
        cancellations++;
        matches.push({
          action: "cancel",
          value: request.headers()["if-match"] ?? "",
        });
        item.status = "deleted";
        item.revision++;
        item.name = undefined;
        item.result = undefined;
        await route.fulfill({ json: item });
        return;
      }
    }
    const artifact = rest.startsWith("/artifacts/fixture-a")
      ? artifactA
      : rest.startsWith("/artifacts/fixture-b")
        ? artifactB
        : undefined;
    if (artifact && rest === `/artifacts/${artifact.id}` && method === "GET") {
      if (artifact.id === artifactA.id && holdFirstA) {
        holdFirstA = false;
        heldSeen();
        await heldGate;
        try {
          await route.fulfill({ json: artifact });
        } catch {
          if (!request.failure()?.errorText.includes("ERR_ABORTED"))
            fixtureViolations.push("unexpected-held-detail-failure");
        } finally {
          heldDone();
        }
        return;
      }
      if (artifact.status === "approved" && !failedApprovedDetail) {
        failedApprovedDetail = true;
        await route.fulfill({
          status: 503,
          json: {
            error: {
              code: "unavailable",
              message:
                "/private/system-command model-reference-must-not-render",
            },
          },
        });
        return;
      }
      await route.fulfill({ json: artifact });
      return;
    }
    if (artifact && rest.endsWith("/approve") && method === "POST") {
      matches.push({
        action: "approve",
        value: request.headers()["if-match"] ?? "",
      });
      const body = request.postDataJSON();
      if (
        JSON.stringify(body) !==
        JSON.stringify({
          wasm_sha256: artifact.wasm_sha256,
          tests_sha256: artifact.tests_sha256,
        })
      )
        fixtureViolations.push("approval-hash-mismatch");
      if (!conflictOnce) {
        conflictOnce = true;
        artifact.revision++;
        await route.fulfill({
          status: 409,
          json: {
            error: {
              code: "wasm_revision_conflict",
              message: "/private/path-do-not-render",
            },
          },
        });
        return;
      }
      approvals++;
      artifact.status = "approved";
      artifact.revision++;
      await route.fulfill({ json: artifact });
      return;
    }
    if (artifact && rest.endsWith("/execute") && method === "POST") {
      executionAttempts++;
      executions.push(request.postDataJSON());
      matches.push({
        action: "execute",
        value: request.headers()["if-match"] ?? "",
      });
      if (
        artifact.status !== "approved" ||
        request.postDataJSON().wasm_sha256 !== artifact.wasm_sha256
      )
        fixtureViolations.push("unapproved-execution-attempt");
      if (executionAttempts === 1) {
        await route.abort("failed");
        return;
      }
      const item: Job = {
        id:
          executionAttempts === 2
            ? "fixture-execution-zero"
            : "fixture-execution-cancel",
        kind: "execute",
        status: "queued",
        revision: 1,
        created_at: now(),
        expires_at: now() + 300,
        name: artifact.name,
        artifact_id: artifact.id,
      };
      fixture.jobs.unshift(item);
      await route.fulfill({ status: 202, json: item });
      return;
    }
    if (artifact && rest.endsWith("/reject") && method === "POST") {
      rejections++;
      artifact.status = "rejected";
      artifact.revision++;
      if (JSON.stringify(request.postDataJSON()) !== "{}")
        fixtureViolations.push("unexpected-reject-body");
      await route.fulfill({ json: artifact });
      return;
    }
    if (artifact && rest.endsWith("/revoke") && method === "POST") {
      revocations++;
      matches.push({
        action: "revoke",
        value: request.headers()["if-match"] ?? "",
      });
      if (JSON.stringify(request.postDataJSON()) !== "{}")
        fixtureViolations.push("unexpected-revoke-body");
      artifact.status = "revoked";
      artifact.revision++;
      for (const item of fixture.jobs)
        if (
          item.artifact_id === artifact.id &&
          (item.status === "queued" || item.status === "running")
        ) {
          item.status = "interrupted";
          item.result = undefined;
        }
      await route.fulfill({ json: artifact });
      return;
    }
    fixtureViolations.push("unexpected-workbench-route");
    await route.fulfill({
      status: 400,
      json: { error: "unexpected_fixture_request" },
    });
  });

  await page.getByRole("button", { name: "计算工坊", exact: true }).click();
  await expect(page).toHaveURL(/#wasm$/);
  const panel = page.getByRole("region", {
    name: "纯整数计算工坊",
    exact: true,
  });
  await expect(panel).toContainText("不注册聊天工具，不授予宿主权限");
  await panel.getByLabel("制品名称", { exact: true }).fill("合成的纯整数加法");
  await panel
    .getByLabel("计算需求", { exact: true })
    .fill("返回a和b之和，按i32回绕。模型代码需要实际worker测试和人工批准。");
  for (const [index, item] of tests.entries()) {
    await panel
      .getByLabel(`测试 ${index + 1} 输入 a`, { exact: true })
      .fill(String(item.a));
    await panel
      .getByLabel(`测试 ${index + 1} 输入 b`, { exact: true })
      .fill(String(item.b));
    await panel
      .getByLabel(`测试 ${index + 1} 预期输出`, { exact: true })
      .fill(String(item.expected));
  }
  const generateRequest = (request: { method(): string; url(): string }) =>
    request.method() === "POST" &&
    request.url() === `${origin}${apiRoot}/generate`;
  await expect(
    page.waitForRequest(generateRequest, { timeout: 2200 }),
  ).rejects.toThrow(/Timeout/);
  expect(generationAttempts).toBe(0);
  expectedGenerationFailure = true;
  await panel
    .getByRole("button", { name: "发送需求并生成", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText("未自动重发");
  await expect(panel.getByLabel("计算需求", { exact: true })).toBeDisabled();
  await expect(
    page.waitForRequest(generateRequest, { timeout: 2200 }),
  ).rejects.toThrow(/Timeout/);
  await panel.getByRole("button", { name: "刷新工坊", exact: true }).click();
  await panel
    .getByRole("button", { name: "发送需求并生成", exact: true })
    .click();
  expect(generations[1]).toEqual(generations[0]);
  expect(generations[0]!.client_request_id).toMatch(/^[0-9a-f-]{36}$/i);
  await expect(
    panel.getByRole("button", {
      name: `查看制品 ${artifactA.name}`,
      exact: true,
    }),
  ).toBeVisible();
  expect(approvals).toBe(0);
  expect(executionAttempts).toBe(0);
  await panel
    .getByRole("button", { name: `查看制品 ${artifactA.name}`, exact: true })
    .click();
  await heldRequestSeen;
  await panel
    .getByRole("button", { name: `查看制品 ${artifactB.name}`, exact: true })
    .click();
  await expect(panel.getByLabel("WAT 源码", { exact: true })).toContainText(
    "B独立源码",
  );
  releaseHeld();
  await heldHandled;
  await expect(panel.getByLabel("WAT 源码", { exact: true })).not.toContainText(
    "A的合成源码",
  );
  await panel
    .getByRole("button", { name: `查看制品 ${artifactA.name}`, exact: true })
    .click();
  await expect(panel.getByLabel("WAT 源码", { exact: true })).toHaveText(
    artifactA.wat,
  );
  expect(await panel.locator("img,script").count()).toBe(0);
  await panel.getByLabel("手动执行输入 a", { exact: true }).fill("0");
  await panel.getByLabel("手动执行输入 b", { exact: true }).fill("0");
  await expect(
    panel.getByRole("button", { name: "手动执行已批准制品", exact: true }),
  ).toBeDisabled();
  await panel
    .getByRole("button", { name: "核对并批准此制品", exact: true })
    .click();
  let confirmation = panel.getByRole("region", {
    name: "确认制品授权操作",
    exact: true,
  });
  await expect(confirmation).toContainText(artifactA.name);
  await expect(confirmation).toContainText(artifactA.wasm_sha256);
  await expect(confirmation).toContainText(artifactA.tests_sha256);
  await expect(confirmation).toContainText("wasmi-2.0.0");
  await expect(confirmation).toContainText("pure-i32-v1");
  await expect(confirmation).toContainText("预期=0；实际=0");
  await expect(
    confirmation.getByRole("button", {
      name: "确认按两个哈希批准",
      exact: true,
    }),
  ).toBeDisabled();
  fs.mkdirSync(evidenceDir, { recursive: true });
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await confirmation.scrollIntoViewIfNeeded();
    await page.evaluate(() => document.fonts.ready);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await panel.screenshot({
      path: path.join(evidenceDir, `wasm-fixture-${width}.png`),
      animations: "disabled",
    });
    await confirmation.screenshot({
      path: path.join(evidenceDir, `wasm-approval-fixture-${width}.png`),
      animations: "disabled",
    });
  }
  await confirmation
    .getByLabel("已核对源码测试及两个制品哈希", { exact: true })
    .check();
  expectedConflict = true;
  await confirmation
    .getByRole("button", { name: "确认按两个哈希批准", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText("当前展示已保留");
  await expect(panel.getByLabel("WAT 源码", { exact: true })).toHaveText(
    artifactA.wat,
  );
  await expect(panel).not.toContainText("/private/path-do-not-render");
  await expect(
    panel.getByRole("button", { name: "核对并批准此制品", exact: true }),
  ).toBeDisabled();
  await panel.getByRole("button", { name: "刷新工坊", exact: true }).click();
  await panel
    .getByRole("button", { name: "核对并批准此制品", exact: true })
    .click();
  confirmation = panel.getByRole("region", {
    name: "确认制品授权操作",
    exact: true,
  });
  await confirmation
    .getByLabel("已核对源码测试及两个制品哈希", { exact: true })
    .check();
  expectedDetailFailure = true;
  await confirmation
    .getByRole("button", { name: "确认按两个哈希批准", exact: true })
    .click();
  await expect(
    panel.getByRole("form", { name: "手动执行制品", exact: true }),
  ).toHaveCount(0);
  await expect(panel).not.toContainText("model-reference-must-not-render");
  expect(executionAttempts).toBe(0);
  expect(approvals).toBe(1);
  await panel.getByRole("button", { name: "刷新工坊", exact: true }).click();
  await panel.getByLabel("手动执行输入 a", { exact: true }).fill("0");
  await panel.getByLabel("手动执行输入 b", { exact: true }).fill("0");
  expectedExecutionFailure = true;
  await panel
    .getByRole("button", { name: "手动执行已批准制品", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText("未自动重发");
  await expect(
    panel.getByLabel("手动执行输入 a", { exact: true }),
  ).toBeDisabled();
  await panel.getByRole("button", { name: "刷新工坊", exact: true }).click();
  await panel
    .getByRole("button", { name: "手动执行已批准制品", exact: true })
    .click();
  expect(executions[1]).toEqual(executions[0]);
  await expect(panel.getByLabel("整数计算结果", { exact: true })).toHaveText(
    "结果：0",
  );
  await panel.getByLabel("手动执行输入 a", { exact: true }).fill("1");
  await panel.getByLabel("手动执行输入 b", { exact: true }).fill("2");
  await panel
    .getByRole("button", { name: "手动执行已批准制品", exact: true })
    .click();
  const job = panel.getByRole("article", { name: "工坊任务 1", exact: true });
  await job
    .getByRole("button", { name: "取消并清除此任务", exact: true })
    .click();
  await panel
    .getByRole("region", { name: "确认清除工坊任务", exact: true })
    .getByRole("button", { name: "确认取消或清除本任务", exact: true })
    .click();
  await expect(job).toContainText("任务已清除");
  expect(cancellations).toBe(1);
  expect(executions[2]!.client_request_id).not.toBe(
    executions[0]!.client_request_id,
  );
  fixture.jobs.unshift({
    id: "fixture-running-at-revoke",
    kind: "execute",
    status: "running",
    revision: 1,
    created_at: now(),
    expires_at: now() + 300,
    name: artifactA.name,
    artifact_id: artifactA.id,
  });
  await panel.getByRole("button", { name: "刷新工坊", exact: true }).click();
  await panel
    .getByRole("button", { name: "撤销此制品授权", exact: true })
    .click();
  confirmation = panel.getByRole("region", {
    name: "确认制品授权操作",
    exact: true,
  });
  await expect(confirmation).toContainText("中断该制品排队或运行中的计算结果");
  await expect(confirmation).toContainText("不等于删除代码");
  await confirmation
    .getByRole("button", { name: "确认撤销并中断相关计算", exact: true })
    .click();
  await expect(
    panel.getByRole("article", { name: "所选制品详情", exact: true }),
  ).toContainText("授权已撤销");
  await expect(
    panel.getByRole("button", { name: "手动执行已批准制品", exact: true }),
  ).toBeDisabled();
  expect(revocations).toBe(1);
  await panel
    .getByRole("button", { name: `查看制品 ${artifactB.name}`, exact: true })
    .click();
  await panel.getByRole("button", { name: "拒绝此制品", exact: true }).click();
  await panel
    .getByRole("button", { name: "确认拒绝此制品", exact: true })
    .click();
  await expect(panel.getByLabel("WAT 源码", { exact: true })).toContainText(
    "B独立源码",
  );
  expect(rejections).toBe(1);
  fixture.jobs.unshift({
    id: "fixture-expiring",
    kind: "execute",
    status: "completed",
    revision: 1,
    created_at: now(),
    expires_at: now() + 2,
    result: 0,
    error_code: "/private/error-path-model-ref",
  });
  await panel.getByRole("button", { name: "刷新工坊", exact: true }).click();
  await expect(
    panel.getByRole("article", { name: "工坊任务 1", exact: true }),
  ).toContainText("结果：0");
  await expect(panel).not.toContainText("/private/error-path-model-ref");
  await expect(
    panel.getByRole("article", { name: "工坊任务 1", exact: true }),
  ).not.toContainText("结果：0", { timeout: 5000 });
  expireSession = true;
  expectedSessionFailure = true;
  await panel.getByRole("button", { name: "刷新工坊", exact: true }).click();
  await expect(panel.getByRole("alert")).toContainText(
    "登录已过期，需求、测试、代码详情和结果已清除",
  );
  await expect(panel.getByLabel("WAT 源码", { exact: true })).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { nativeWasmTrace: { calls: number } })
          .nativeWasmTrace.calls,
    ),
  ).toBe(0);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
  expect(generationAttempts).toBe(2);
  expect(executionAttempts).toBe(3);
  expect(
    matches
      .filter((entry) => entry.action === "approve")
      .map((entry) => entry.value),
  ).toEqual(['"1"', '"2"']);
  expect(
    matches
      .filter((entry) => entry.action === "execute")
      .map((entry) => entry.value),
  ).toEqual(['"3"', '"3"', '"3"']);
  expect(matches.find((entry) => entry.action === "cancel")?.value).toBe('"1"');
  expect(matches.find((entry) => entry.action === "revoke")?.value).toBe('"3"');
  // APIRequestContext bypasses page.route: no real job, artifact or grant exists.
  expect(await (await page.request.get(apiRoot)).json()).toEqual(actualBefore);
  expect(conversationOrMemoryWrites).toBe(0);
  expect(fixtureViolations).toEqual([]);
  expect(foreignRequests).toEqual([]);
  expect(pageErrors).toEqual([]);
  expect(consoleErrors).toEqual([]);
});
