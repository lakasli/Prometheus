import { test, expect, type Page } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const dataDir = path.resolve(
  process.env.DSH_CHARACTER_E2E_DATA_DIR || "../../.local/skills-e2e/data",
);
const evidenceDir = path.resolve("../../.local/skills-e2e/screenshots");
const prefix = "/api/admin/v1";
async function login(page: Page) {
  fs.mkdirSync(dataDir, { recursive: true });
  const accountFile = path.join(dataDir, "e2e-account.json");
  const account = fs.existsSync(accountFile)
    ? JSON.parse(fs.readFileSync(accountFile, "utf8"))
    : {
        username: `skills_${crypto.randomBytes(4).toString("hex")}`,
        password: crypto.randomBytes(24).toString("base64url"),
      };
  if (!fs.existsSync(accountFile))
    fs.writeFileSync(accountFile, JSON.stringify(account), {
      mode: 0o600,
      flag: "wx",
    });
  const status = await (await page.request.get(prefix + "/auth/status")).json();
  await page.goto("/admin/");
  await page.getByLabel("管理员用户名").fill(account.username);
  if (status.initialized) {
    await page.getByLabel("管理密码", { exact: true }).fill(account.password);
    await page.getByRole("button", { name: "登录控制台", exact: true }).click();
  } else {
    await page
      .getByLabel("一次性配对凭证")
      .fill(
        JSON.parse(
          fs.readFileSync(path.join(dataDir, "bootstrap-token.json"), "utf8"),
        ).token,
      );
    await page
      .getByLabel("设置管理密码", { exact: true })
      .fill(account.password);
    await page.getByLabel("再次输入密码").fill(account.password);
    await page.getByRole("button", { name: "创建账户并进入控制台" }).click();
  }
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
}
async function shot(page: Page, file: string) {
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.evaluate(() => document.fonts.ready);
  fs.mkdirSync(evidenceDir, { recursive: true });
  await page.screenshot({
    path: path.join(evidenceDir, file),
    fullPage: true,
    animations: "disabled",
  });
}

test("Skill management, generated draft review, HTML canvas and native PNG use actual APIs", async ({
  page,
  baseURL,
}) => {
  test.setTimeout(120_000);
  expect(new URL(baseURL!).port).toBe("18421");
  const errors: string[] = [];
  const external: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("request", (request) => {
    if (
      !request.url().startsWith(baseURL!) &&
      !request.url().startsWith("data:")
    )
      external.push(request.url());
  });
  await login(page);
  const auth = await (await page.request.get(prefix + "/auth/me")).json();
  const headers = { "X-CSRF-Token": auth.csrf_token, Origin: baseURL! };
  const fixture = JSON.parse(
    fs.readFileSync(path.resolve(dataDir, "../fixture-info.json"), "utf8"),
  );
  async function configure(native: boolean) {
    const current = await (await page.request.get(prefix + "/config")).json();
    const secret =
      current.config.model.credential_ref ||
      (
        await (
          await page.request.post(prefix + "/secrets", {
            headers,
            data: {
              kind: "model_api_key",
              value: "synthetic-browser-model-key",
            },
          })
        ).json()
      ).id;
    const config = {
      ...current.config,
      model: {
        base_url: fixture.model_url,
        model: "synthetic-drawing-model",
        credential_ref: secret,
        image_generation: native,
      },
    };
    const draft = await (
      await page.request.post(prefix + "/config/drafts", {
        headers,
        data: { base_revision: current.revision, config },
      })
    ).json();
    const valid = await (
      await page.request.post(`${prefix}/config/drafts/${draft.id}/validate`, {
        headers,
        data: {},
      })
    ).json();
    expect(valid.valid).toBe(true);
    const applied = await page.request.post(
      `${prefix}/config/drafts/${draft.id}/apply`,
      {
        headers: { ...headers, "If-Match": `"${current.revision}"` },
        data: {},
      },
    );
    expect(applied.status()).toBe(202);
  }
  await configure(false);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Skill 管理", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Skill 库", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".skill-item")).toHaveCount(2);
  await page.getByRole("button", { name: "新建 Skill", exact: true }).click();
  const name = "浏览器技能_" + crypto.randomBytes(3).toString("hex");
  await page.getByLabel("Skill 名称", { exact: true }).fill(name);
  await page
    .getByLabel("Skill 介绍", { exact: true })
    .fill("编辑后应立即显示新的介绍。");
  await page
    .getByLabel("Skill 指令", { exact: true })
    .fill("简洁回答并检查结果。");
  await page.getByRole("button", { name: "保存 Skill", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Skill 已保存");
  await expect(
    page.locator(".skill-item").filter({ hasText: name }),
  ).toContainText("编辑后应立即显示");
  await shot(page, "skills-desktop.png");
  await page.getByRole("button", { name: "在对话中使用", exact: true }).click();
  await expect(
    page.getByRole("combobox", { name: "本次 Skill", exact: true }),
  ).not.toHaveValue("");
  await page
    .getByRole("combobox", { name: "本次 Skill", exact: true })
    .selectOption("drawing");
  await page
    .getByLabel("对话消息", { exact: true })
    .fill("画一只蓝色鲸鱼，使用自包含 SVG。");
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await expect(page.getByTitle("HTML 绘画预览")).toBeVisible();
  await expect(
    page.frameLocator('iframe[title="HTML 绘画预览"]').locator("svg"),
  ).toBeVisible();
  expect(await page.getByTitle("HTML 绘画预览").getAttribute("sandbox")).toBe(
    "",
  );
  await shot(page, "drawing-html-desktop.png");
  await page
    .getByRole("combobox", { name: "本次 Skill", exact: true })
    .selectOption("skill-creator");
  await page
    .getByLabel("对话消息", { exact: true })
    .fill("制作一个检查文章的 skill。");
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await page
    .getByRole("button", { name: "审核并保存为 Skill", exact: true })
    .click();
  await expect(page.getByLabel("Skill 名称", { exact: true })).not.toHaveValue(
    "",
  );
  await expect(
    page.getByLabel("启用此 Skill", { exact: true }),
  ).not.toBeChecked();
  const before = (await (await page.request.get(prefix + "/skills")).json())
    .items.length;
  await page.getByRole("button", { name: "保存 Skill", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Skill 已保存");
  expect(
    (await (await page.request.get(prefix + "/skills")).json()).items.length,
  ).toBe(before + 1);
  await configure(true);
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Skill 管理", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "对话工作台", exact: true }).click();
  await page
    .getByRole("combobox", { name: "本次 Skill", exact: true })
    .selectOption("drawing");
  await page.getByLabel("对话消息", { exact: true }).fill("画一个蓝色方块。");
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await expect(page.getByAltText("助手生成的绘画")).toBeVisible();
  expect(
    await page
      .getByAltText("助手生成的绘画")
      .evaluate((image: HTMLImageElement) => image.naturalWidth),
  ).toBeGreaterThan(0);
  await shot(page, "drawing-native-desktop.png");
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await expect
      .poll(() =>
        page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      )
      .toBe(true);
    await shot(page, `drawing-${width}.png`);
  }
  expect(errors).toEqual([]);
  expect(external).toEqual([]);
});
