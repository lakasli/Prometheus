import { test, expect, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

// This suite creates only synthetic records in its own disposable instance.
// An initialized instance must already have this suite's private account file.
const data = path.resolve(
  process.env.DSH_E2E_DATA_DIR || "../../.local/relationship-network-e2e/data",
);
const evidence = path.resolve(
  process.env.DSH_E2E_EVIDENCE_DIR ||
    "../../.local/relationship-network-e2e/evidence",
);
const prefix = "/api/admin/v1";

interface Person {
  id: string;
  display_name: string;
  revision: number;
}

async function authenticate(page: Page) {
  const accountFile = path.join(data, "relationship-network-account.json");
  const statusResponse = await page.request.get(`${prefix}/auth/status`);
  expect(statusResponse.ok()).toBe(true);
  const status = await statusResponse.json();
  if (status.initialized && !fs.existsSync(accountFile))
    throw new Error(
      "Refusing to seed an initialized instance without this suite's isolated account file.",
    );
  const account = fs.existsSync(accountFile)
    ? JSON.parse(fs.readFileSync(accountFile, "utf8"))
    : {
        username: "network_" + crypto.randomBytes(4).toString("hex"),
        password: crypto.randomBytes(24).toString("base64url"),
      };
  if (!fs.existsSync(accountFile)) {
    fs.mkdirSync(data, { recursive: true });
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      flag: "wx",
      mode: 0o600,
    });
  }
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
  const session = await (await page.request.get(`${prefix}/auth/me`)).json();
  return session.csrf_token as string;
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

test("reviewed relationship network supports linked selection, navigation and a narrow screen", async ({
  page,
  browser,
  baseURL,
}) => {
  test.setTimeout(120000);
  page.setDefaultTimeout(15000);
  const target = new URL(baseURL!);
  expect(["localhost", "127.0.0.1", "[::1]"]).toContain(target.hostname);
  const expectedSha = process.env.DSH_E2E_EXPECT_SHA;
  if (expectedSha) {
    expect(
      (await (await page.request.get("/health/ready")).json()).commit,
    ).toBe(expectedSha);
    expect(
      (await (await page.request.get("/admin/release.json")).json()).commit,
    ).toBe(expectedSha);
  }
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const anonymous = await browser.newContext();
  for (const endpoint of [
    "relationships/people",
    "relationships/edges",
    "diary/entries",
  ])
    expect(
      (await anonymous.request.get(`${baseURL}${prefix}/${endpoint}`)).status(),
    ).toBe(401);
  await anonymous.close();

  const csrf = await authenticate(page);
  const headers = { Origin: target.origin, "X-CSRF-Token": csrf };
  expect(
    (
      await page.request.post(`${prefix}/relationships/people`, {
        headers: { Origin: target.origin },
        data: { display_name: "缺少 CSRF 的请求不得创建人物" },
      })
    ).status(),
  ).toBe(403);
  const names = [
    "林知夏",
    "陈予安",
    "周屿",
    "林晚晴",
    "许清和",
    "宋书宁",
    "陆川",
    "林星遥",
    "沈南乔",
    "江若初",
  ];
  const people: Person[] = [];
  for (const display_name of names) {
    const response = await page.request.post(`${prefix}/relationships/people`, {
      headers,
      data: {
        display_name,
        visibility: "private",
        source_conversation_id: null,
        publish: false,
        reason: "仅为隔离浏览器关系网络验收建立的合成人物。",
      },
    });
    expect(response.status()).toBe(201);
    people.push(await response.json());
  }
  const links: [number, number, string][] = [
    [0, 1, "friend"],
    [0, 2, "colleague"],
    [0, 3, "family"],
    [1, 4, "friend"],
    [1, 5, "colleague"],
    [2, 5, "friend"],
    [2, 6, "colleague"],
    [3, 7, "family"],
    [4, 8, "friend"],
    [5, 8, "colleague"],
    [6, 8, "friend"],
    [7, 4, "acquaintance"],
  ];
  const makeEdge = async (
    from: number,
    to: number,
    relationship_type: string,
  ) => {
    const response = await page.request.post(`${prefix}/relationships/edges`, {
      headers,
      data: {
        from_person_id: people[from]!.id,
        to_person_id: people[to]!.id,
        relationship_type,
        evidence: {
          source_type: "manual",
          source_id: null,
          source_revision: null,
          statement_kind: "self_report",
          reason: "仅用于隔离合成资料，已核对本条有向关系。",
        },
      },
    });
    expect(response.status()).toBe(201);
    return response.json();
  };
  for (const [from, to, kind] of links) {
    const edge = await makeEdge(from, to, kind);
    expect(edge.status).toBe("pending");
    const response = await page.request.post(
      `${prefix}/relationships/edges/${edge.id}/review`,
      {
        headers: { ...headers, "If-Match": `"${edge.revision}"` },
        data: {
          decision: "approve",
          supersedes: [],
          reason: "仅在临时合成账户中明确审核该关系。",
        },
      },
    );
    expect(response.ok()).toBe(true);
  }
  // A real pending record must not appear as an approved graph connection.
  const pending = await makeEdge(9, 0, "mentioned");
  expect(pending.status).toBe("pending");

  await page.setViewportSize({ width: 1600, height: 1000 });
  const nav = page.getByRole("navigation", { name: "主导航" });
  await nav.getByRole("button", { name: "日记与资料", exact: true }).click();
  await expect(
    page.getByText("历史记忆与会话摘要兼容管理", { exact: true }),
  ).toHaveCount(0);
  await nav.getByRole("button", { name: "人际关系", exact: true }).click();
  const workspace = page
    .locator(".relationship-workspace, .relationship-columns")
    .first();
  const list = page.locator(".relationship-list");
  const graph = page.locator(".relationship-graph");
  const detail = page.locator(".relationship-detail");
  await expect(workspace).toBeVisible();
  await expect(
    graph.locator(".relationship-graph-node circle.node-disc"),
  ).toHaveCount(names.length);
  await expect(graph.locator(".relationship-graph-edge")).toHaveCount(
    links.length,
  );
  await expect(graph.getByText("100%", { exact: true })).toBeVisible();
  const listBox = await list.boundingBox();
  const graphBox = await graph.boundingBox();
  const detailBox = await detail.boundingBox();
  expect(listBox).not.toBeNull();
  expect(graphBox).not.toBeNull();
  expect(detailBox).not.toBeNull();
  expect(listBox!.x + listBox!.width).toBeLessThanOrEqual(graphBox!.x + 2);
  expect(graphBox!.x + graphBox!.width).toBeLessThanOrEqual(detailBox!.x + 2);
  await screenshot(page, "01-relationship-network-desktop.png");

  const isolatedName = names[9]!;
  const isolatedNode = graph.getByRole("button", {
    name: `查看${isolatedName}的资料`,
    exact: true,
  });
  await isolatedNode.click();
  await expect(isolatedNode).toHaveAttribute("aria-pressed", "true");
  await expect(isolatedNode).toHaveClass(/selected/);
  await expect(isolatedNode).not.toHaveClass(/dimmed/);
  await expect(list.locator("button.active")).toContainText(isolatedName);
  await expect(detail).toContainText(isolatedName);

  const selectedName = names[0]!;
  await graph
    .getByRole("button", { name: `查看${selectedName}的资料`, exact: true })
    .click();
  await expect(list.locator("button.active")).toContainText(selectedName);
  await expect(detail).toContainText(selectedName);
  const keyboardName = names[1]!;
  const keyboardNode = graph.getByRole("button", {
    name: `查看${keyboardName}的资料`,
    exact: true,
  });
  await keyboardNode.focus();
  await keyboardNode.press("Enter");
  await expect(list.locator("button.active")).toContainText(keyboardName);
  await expect(detail).toContainText(keyboardName);
  await screenshot(page, "02-relationship-network-selected.png");

  const reset = graph.getByRole("button", { name: "重置关系图", exact: true });
  await graph.getByRole("button", { name: "放大关系图", exact: true }).click();
  await expect(graph.getByText("100%", { exact: true })).toHaveCount(0);
  await graph.getByRole("button", { name: "缩小关系图", exact: true }).click();
  await reset.click();
  await expect(graph.getByText("100%", { exact: true })).toBeVisible();
  const svg = graph.locator(".graph-canvas > svg");
  await svg.hover();
  await page.mouse.wheel(0, -360);
  await expect(graph.getByText("100%", { exact: true })).toHaveCount(0);
  await reset.click();
  const viewport = svg.locator(".relationship-graph-viewport");
  const beforePan = await viewport.getAttribute("transform");
  const svgBox = await svg.boundingBox();
  expect(svgBox).not.toBeNull();
  await page.mouse.move(svgBox!.x + 15, svgBox!.y + 15);
  await page.mouse.down();
  await page.mouse.move(svgBox!.x + 70, svgBox!.y + 55, { steps: 6 });
  await page.mouse.up();
  await expect(viewport).not.toHaveAttribute("transform", beforePan!);
  await reset.click();

  await page.setViewportSize({ width: 390, height: 844 });
  await expect(graph).toBeVisible();
  await expect(
    graph.locator(".relationship-graph-node circle.node-disc"),
  ).toHaveCount(names.length);
  await graph
    .getByRole("button", { name: `查看${names[2]}的资料`, exact: true })
    .click();
  await expect(detail).toContainText(names[2]!);
  await screenshot(page, "03-relationship-network-mobile.png");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  expect(errors).toEqual([]);
});
