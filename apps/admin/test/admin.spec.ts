import { expect, test } from "@playwright/test";
import { readFileSync, mkdirSync, writeFileSync, existsSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { resolve } from "node:path";
test.describe.configure({ mode: "serial" });
const password = "Synthetic-browser-password-2026!";
test("first-run setup consumes an actual one-time credential", async ({
  page,
}) => {
  await page.goto("/admin/");
  await expect(
    page.getByRole("heading", { name: "建立你的日常空间" }),
  ).toBeVisible();
  const state = JSON.parse(
    readFileSync(resolve("../../.local/prometheus/e2e-state.json"), "utf8"),
  );
  const record = JSON.parse(readFileSync(state.tokenFile, "utf8"));
  let token = record.token;
  await expect(page.getByTestId("bootstrap-file")).toHaveText(state.tokenFile);
  await page.getByText("生成或更新此实例的凭证", { exact: true }).click();
  await expect(page.getByTestId("bootstrap-command")).toContainText(
    `--data-dir '${state.dataDir}'`,
  );
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.setViewportSize({ width: 1280, height: 720 });
  await page.getByLabel("用户名", { exact: true }).fill("browser");
  await page.getByLabel("密码", { exact: true }).fill(password);
  await page.getByLabel("确认密码", { exact: true }).fill(password);
  await page.getByLabel("一次性初始化凭证").fill(JSON.stringify(record));
  await page.getByRole("button", { name: "创建空间" }).click();
  await expect(page.getByRole("alert")).toContainText(
    "不要填写文件路径或整段JSON",
  );
  await page
    .getByLabel("一次性初始化凭证")
    .fill(token[0] === "0" ? "1".repeat(64) : "0".repeat(64));
  await page.getByRole("button", { name: "创建空间" }).click();
  await expect(page.getByRole("alert")).toContainText("当前运行实例不匹配");
  writeFileSync(state.tokenFile, JSON.stringify({ ...record, expires_at: 1 }));
  await page.getByRole("button", { name: "刷新凭证状态" }).click();
  await expect(
    page.getByText("凭证已过期，需要重新生成", { exact: false }),
  ).toBeVisible();
  await page.getByLabel("一次性初始化凭证").fill(token);
  await page.getByRole("button", { name: "创建空间" }).click();
  await expect(page.getByRole("alert")).toContainText("初始化凭证已过期");
  token = randomBytes(32).toString("hex");
  writeFileSync(
    state.tokenFile,
    JSON.stringify({ token, expires_at: Math.floor(Date.now() / 1000) + 900 }),
  );
  await page.getByRole("button", { name: "刷新凭证状态" }).click();
  await expect(
    page.getByText("文件中的凭证当前有效", { exact: false }),
  ).toBeVisible();
  await page.getByLabel("一次性初始化凭证").fill(` ${token} `);
  await page.getByRole("button", { name: "创建空间" }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".local-badge")).toHaveText("Windows 本机");
  expect(existsSync(state.tokenFile)).toBe(false);
  await page.getByRole("button", { name: "退出登录" }).click();
  await expect(
    page.getByRole("button", { name: "进入日常空间" }),
  ).toBeVisible();
  await expect(page.getByLabel("一次性初始化凭证")).toHaveCount(0);
  await page.getByLabel("用户名", { exact: true }).fill("browser");
  await page.getByLabel("密码", { exact: true }).fill(password);
  await page.getByRole("button", { name: "进入日常空间" }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
});
test.beforeEach(async ({ page }, info) => {
  if (info.title.startsWith("first-run")) return;
  await page.goto("/admin/");
  await expect(
    page.getByRole("button", { name: "进入日常空间" }),
  ).toBeVisible();
  await page.getByLabel("用户名", { exact: true }).fill("browser");
  await page.getByLabel("密码", { exact: true }).fill(password);
  await page.getByRole("button", { name: "进入日常空间" }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
});
test("overview renders without console, CSP or failed asset errors at desktop and mobile widths", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  await page.setViewportSize({ width: 1440, height: 1000 });
  const folder = resolve("../../.local/prometheus/screenshots");
  mkdirSync(folder, { recursive: true });
  await page.screenshot({
    path: resolve(folder, "overview-desktop.png"),
    fullPage: true,
  });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole("navigation")).not.toBeInViewport();
  await page.screenshot({
    path: resolve(folder, "overview-mobile.png"),
    fullPage: true,
  });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.getByRole("button", { name: "展开导航" }).click();
  await expect(page.getByRole("navigation")).toBeVisible();
  expect(errors).toEqual([]);
});
test("private memories can be created, edited with a revision and forgotten", async ({
  page,
}) => {
  await page.getByRole("button", { name: "记忆与资料", exact: false }).click();
  await page.getByRole("button", { name: "＋ 新增记忆与资料" }).click();
  await page
    .getByLabel("内容", { exact: true })
    .fill("浏览器真实保存：喜欢雨后的薄荷。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("喜欢雨后的薄荷");
  await page.getByRole("button", { name: "编辑", exact: true }).click();
  await page
    .getByLabel("内容", { exact: true })
    .fill("浏览器真实更正：喜欢清晨的薄荷。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("v2");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "遗忘", exact: true }).click();
  await expect(page.locator(".record-card")).toHaveCount(0);
});
test("personality changes persist, configuration conflicts are shown, and gated features remain disabled", async ({
  page,
}) => {
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "性格", exact: true })
    .click();
  await page
    .getByLabel("个人简介", { exact: true })
    .fill("浏览器验收的合成性格资料。");
  await page.getByRole("button", { name: "保存并生效", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("性格已保存并生效");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "模型与运行", exact: true })
    .click();
  await expect(page.getByLabel("模型名称", { exact: true })).toHaveValue(
    "deepseek-chat",
  );
  const me = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const current = await (await page.request.get("/api/admin/v1/config")).json();
  await page.request.put("/api/admin/v1/config", {
    data: { config: current.config },
    headers: {
      origin: "http://127.0.0.1:18449",
      "x-csrf-token": me.csrf_token,
      "if-match": `"${current.revision}"`,
    },
  });
  await page.getByRole("button", { name: "保存并生效", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("版本已变化");
  await page.getByRole("button", { name: "↻ 刷新", exact: true }).click();
  await page.getByRole("button", { name: "保存并生效", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("配置已保存并生效");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "渠道与媒体", exact: true })
    .click();
  await expect(
    page.locator(".capability-card").filter({ hasText: "QQ 连接" }),
  ).toContainText("真实平台待验收");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "插件", exact: true })
    .click();
  await expect(
    page.locator(".capability-card").filter({ hasText: "生成插件" }),
  ).toContainText("待迁移 · 禁用");
});
test("schedules, diaries, people, relationships and skills save through their real APIs", async ({
  page,
}) => {
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日程", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增日程" }).click();
  await page.getByLabel("标题", { exact: true }).fill("合成阅读计划");
  await page.getByLabel("内容", { exact: true }).fill("手动阅读与进度记录");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("合成阅读计划");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日记", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增日记" }).click();
  await page.getByLabel("标题", { exact: true }).fill("合成日记");
  await page
    .getByLabel("事实与经历", { exact: true })
    .fill("这里是浏览器测试合成的资料。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("合成日记");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "人物", exact: true })
    .click();
  for (const name of ["甲", "乙"]) {
    await page.getByRole("button", { name: "＋ 新增人物" }).click();
    await page.getByLabel("名称", { exact: true }).fill(name);
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(
      page.locator(".record-card").filter({ hasText: name }),
    ).toBeVisible();
  }
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "人际关系", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增人际关系" }).click();
  await page.getByLabel("起点人物").selectOption({ label: "甲" });
  await page.getByLabel("终点人物").selectOption({ label: "乙" });
  await page.getByLabel("说明", { exact: true }).fill("合成同事关系");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(
    page.locator(".record-card").filter({ hasText: "甲 → 乙" }),
  ).toBeVisible();
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "Skill 管理", exact: true })
    .click();
  await page.getByRole("button", { name: "＋ 新增Skill 管理" }).click();
  await page.getByLabel("名称", { exact: true }).fill("合成条理说明");
  await page
    .getByLabel("方法与步骤", { exact: true })
    .fill("先说结论，再补充必要细节。");
  await page.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.locator(".record-card")).toContainText("合成条理说明");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日志记录", exact: true })
    .click();
  await expect(page.locator(".log-row").first()).toContainText("资料已保存");
});
test("source review, trigger preview and context budgets persist through the new management pages", async ({
  page,
}) => {
  const session = await (
      await page.request.get("/api/admin/v1/auth/me")
    ).json(),
    headers = {
      origin: "http://127.0.0.1:18449",
      "x-csrf-token": session.csrf_token,
    };
  const fixture = JSON.parse(
    readFileSync(resolve("../../.local/prometheus/e2e-state.json"), "utf8"),
  );
  const key = await (
    await page.request.post("/api/admin/v1/secrets", {
      headers,
      data: { kind: "model_api_key", value: "synthetic-browser-provider-key" },
    })
  ).json();
  const current = await (await page.request.get("/api/admin/v1/config")).json();
  current.config.model = {
    base_url: fixture.modelUrl,
    model: "synthetic",
    credential_ref: key.id,
  };
  expect(
    (
      await page.request.put("/api/admin/v1/config", {
        headers: { ...headers, "if-match": `"${current.revision}"` },
        data: { config: current.config },
      })
    ).status(),
  ).toBe(200);
  const conversation = await (
    await page.request.post("/api/admin/v1/conversations", {
      headers,
      data: { title: "浏览器明确来源" },
    })
  ).json();
  const run = await (
    await page.request.post(
      `/api/admin/v1/conversations/${conversation.id}/messages`,
      {
        headers,
        data: {
          content: "这里是一条明确的合成事实。",
          client_message_id: "browser-source",
        },
      },
    )
  ).json();
  await expect
    .poll(
      async () =>
        (
          await (
            await page.request.get(`/api/admin/v1/runs/${run.run_id}`)
          ).json()
        ).status,
    )
    .toBe("completed");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日记", exact: true })
    .click();
  await page
    .getByLabel("会话来源")
    .selectOption({ label: `浏览器明确来源 · ${session.user.id}` });
  await expect(page.locator(".source-message")).toHaveCount(2);
  await page.locator(".source-message input").first().check();
  await page.getByRole("button", { name: "整理选中的 1 条来源" }).click();
  await expect
    .poll(async () => {
      await page.getByRole("button", { name: "刷新整理记录" }).click();
      return await page
        .locator(".job-card")
        .filter({ hasText: "记忆候选" })
        .innerText();
    })
    .toContain("待审核");
  await page.getByRole("button", { name: "审核后保存私有资料" }).click();
  await expect(page.getByRole("status")).toContainText(
    "结果已审核保存为私有资料",
  );
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日程", exact: true })
    .click();
  await page.getByText("新增触发草稿", { exact: true }).click();
  await page.getByLabel("触发任务名称").fill("浏览器合成提醒");
  await page.getByLabel("提醒或任务内容").fill("本地待审核的合成文字。");
  await page.getByRole("button", { name: "保存停用草稿" }).click();
  await expect(
    page.locator(".job-card").filter({ hasText: "浏览器合成提醒" }),
  ).toContainText("已停用");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "预演并启用" }).click();
  await expect(
    page.locator(".job-card").filter({ hasText: "浏览器合成提醒" }),
  ).toContainText("已启用");
  await page.getByLabel("日历视图").selectOption("week");
  await expect(page.locator(".calendar-day")).toHaveCount(7);
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "模型与运行", exact: true })
    .click();
  await page.getByLabel("请求字节上限").fill("90000");
  await page.getByRole("button", { name: "保存上下文预算" }).click();
  await expect(page.getByRole("status")).toContainText("上下文预算已保存");
});
test("reviewed relationship events, disabled model proposals and isolated artwork work through the browser", async ({
  page,
}) => {
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "人际关系", exact: true })
    .click();
  await page.getByLabel("相处人物").selectOption({ label: "甲" });
  await page.getByLabel("相处依据").fill("浏览器明确确认的合成相处事件");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "确认相处事件" }).click();
  await expect(page.getByRole("status")).toContainText(
    "已保存人工确认的相处事件",
  );
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日程", exact: true })
    .click();
  await page.getByLabel("规划要求").fill("请生成待审核提醒");
  await page.getByRole("button", { name: "生成待审核建议" }).click();
  await expect
    .poll(async () => {
      await page.getByRole("button", { name: "刷新建议" }).click();
      return await page.getByRole("button", { name: "审核并保存建议" }).count();
    })
    .toBe(1);
  await expect(
    page.getByRole("button", { name: "审核并保存建议" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "审核并保存建议" }).click();
  await page.getByRole("button", { name: "刷新触发任务" }).click();
  await expect(
    page.locator(".job-card").filter({ hasText: "合成规划提醒" }),
  ).toContainText("已停用");
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "Skill 管理", exact: true })
    .click();
  await page.getByLabel("画面要求").fill("合成的隔离插画");
  await page.getByRole("button", { name: "生成隔离插画" }).click();
  await expect
    .poll(async () => {
      await page.getByRole("button", { name: "刷新插画" }).click();
      return await page.locator('iframe[title="隔离插画预览"]').count();
    })
    .toBe(1);
  await expect(page.locator("iframe")).toHaveAttribute("sandbox", "");
  await expect(
    page.frameLocator("iframe").getByRole("heading", { name: "合成插画" }),
  ).toBeVisible();
  expect(await page.locator("body").getAttribute("data-unsafe")).toBeNull();
  await page.screenshot({
    path: resolve(
      "../../.local/prometheus/screenshots/skills-isolated-preview.png",
    ),
    fullPage: true,
  });
});
