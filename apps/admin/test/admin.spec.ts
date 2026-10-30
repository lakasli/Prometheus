import { expect, test } from "@playwright/test";
import { readFileSync, mkdirSync, writeFileSync, existsSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { resolve } from "node:path";
test.describe.configure({ mode: "serial" });
const password = "Synthetic-browser-password-2026!";
let sessionCookies: Awaited<
  ReturnType<import("@playwright/test").BrowserContext["cookies"]>
> = [];

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
test.afterEach(async ({ page }, info) => {
  if (info.title.startsWith("first-run") && info.status === "passed")
    sessionCookies = await page.context().cookies();
});
test.beforeEach(async ({ page }, info) => {
  if (info.title.startsWith("first-run")) return;
  expect(sessionCookies.length).toBeGreaterThan(0);
  await page.context().addCookies(sessionCookies);
  await page.goto("/admin/");
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
});
test("profile source fields, explicit conflict review, identity binding and page graph work on desktop and mobile", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const me = await (await page.request.get("/api/admin/v1/auth/me")).json(),
    headers = {
      origin: "http://127.0.0.1:18449",
      "x-csrf-token": me.csrf_token,
    };
  const c = await (
    await page.request.post("/api/admin/v1/conversations", {
      headers,
      data: { title: "浏览器人物身份专属会话" },
    })
  ).json();
  await page.getByRole("button", { name: "人物", exact: true }).click();
  const panel = page.getByRole("region", { name: "人物资料与身份" });
  await expect(panel).toBeVisible();
  await panel
    .getByRole("button", { name: "建立来源域人物", exact: true })
    .click();
  await panel.getByLabel("名称", { exact: true }).fill("浏览器来源人物");
  await panel
    .getByLabel("人工备注", { exact: true })
    .fill("备注不会自动成为已核实字段");
  await panel.getByLabel("依据", { exact: true }).fill("浏览器人工核对新建");
  await panel.getByRole("button", { name: "核对并保存", exact: true }).click();
  await expect(
    panel.getByRole("heading", { name: "浏览器来源人物", exact: true }),
  ).toBeVisible();
  await panel
    .getByRole("button", { name: "新增字段陈述", exact: true })
    .click();
  await panel
    .getByLabel("字段内容", { exact: true })
    .fill("浏览器明确陈述喜欢古诗");
  await panel.getByLabel("陈述类型", { exact: true }).selectOption("inference");
  await panel.getByLabel("依据", { exact: true }).fill("此处保留推测标记");
  await panel
    .getByRole("button", { name: "保存待审核陈述", exact: true })
    .click();
  const approve = async () => {
    const handler = async (dialog: import("@playwright/test").Dialog) => {
      await dialog.accept(
        dialog.type() === "prompt" ? "逐项人工核对的审核依据" : undefined,
      );
    };
    page.on("dialog", handler);
    try {
      await panel
        .getByRole("button", { name: "审核批准", exact: true })
        .click();
      await expect(
        panel.locator(".profile-statement").filter({ hasText: "已批准" }),
      ).toContainText("推测");
    } finally {
      page.off("dialog", handler);
    }
  };
  await approve();
  await panel
    .getByRole("button", { name: "核对账号身份", exact: true })
    .click();
  await panel.getByLabel("本人私聊会话", { exact: true }).selectOption(c.id);
  await panel.getByLabel("账号别名", { exact: true }).fill("浏览器已核对本人");
  await panel.getByLabel("依据", { exact: true }).fill("独立核对当前账号本人");
  await panel.getByRole("button", { name: "核对并保存", exact: true }).click();
  await expect(panel).toContainText("浏览器已核对本人");
  await panel
    .getByRole("button", { name: "新增字段陈述", exact: true })
    .click();
  await panel
    .getByLabel("字段内容", { exact: true })
    .fill("浏览器纠正后喜欢历史");
  await panel.getByLabel("依据", { exact: true }).fill("本人后续明确纠正");
  await panel
    .getByRole("button", { name: "保存待审核陈述", exact: true })
    .click();
  await approve();
  await expect(
    panel.locator(".profile-statement").filter({ hasText: "已替代" }),
  ).toContainText("浏览器明确陈述喜欢古诗");
  await panel
    .getByRole("button", { name: "版本记录", exact: true })
    .first()
    .click();
  await expect(panel).toContainText("所选陈述的版本记录");
  await panel
    .getByText("本页关系图（最多50个人物，仅显示已批准的有向关系）", {
      exact: true,
    })
    .click();
  await expect(
    panel.getByRole("img", { name: "当前页已批准人物关系图" }),
  ).toBeVisible();
  await panel.getByRole("button", { name: "放大", exact: true }).click();
  await panel.getByRole("button", { name: "右移", exact: true }).click();
  await panel.getByRole("button", { name: "复位", exact: true }).click();
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page.locator("body").evaluate((el) => el.scrollWidth <= innerWidth),
  ).toBe(true);
  await panel.screenshot({
    path: resolve("../../.local/prometheus/screenshots/profiles-mobile.png"),
    animations: "disabled",
  });
  await page.setViewportSize({ width: 1280, height: 900 });
  await panel
    .getByRole("button", { name: "新增字段陈述", exact: true })
    .click();
  await panel.getByLabel("字段内容", { exact: true }).fill("未保存资料");
  page.once("dialog", (d) => d.dismiss());
  await page.getByRole("button", { name: "运行总览", exact: true }).click();
  await expect(panel.getByLabel("字段内容", { exact: true })).toHaveValue(
    "未保存资料",
  );
  await panel.getByRole("button", { name: "取消", exact: true }).click();
  await page.getByRole("button", { name: "运行总览", exact: true }).click();
  expect(errors).toEqual([]);
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
  const beforeRestore = await (
    await page.request.get("/api/admin/v1/assistant/personality")
  ).json();
  await page.getByText("性格版本与恢复", { exact: true }).click();
  await page.getByLabel("历史性格版本").selectOption("1");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "恢复此版本", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(
    "历史性格已恢复为新版本",
  );
  const afterRestore = await (
    await page.request.get("/api/admin/v1/assistant/personality")
  ).json();
  expect(afterRestore.revision).toBe(beforeRestore.revision + 1);
  expect(afterRestore.personality.description).not.toBe(
    "浏览器验收的合成性格资料。",
  );
  const restoreSession = await (
    await page.request.get("/api/admin/v1/auth/me")
  ).json();
  const newer = {
    ...afterRestore.personality,
    description: "另一个窗口保存的性格版本",
  };
  expect(
    (
      await page.request.put("/api/admin/v1/assistant/personality", {
        headers: {
          origin: "http://127.0.0.1:18449",
          "x-csrf-token": restoreSession.csrf_token,
          "if-match": `"${afterRestore.revision}"`,
        },
        data: { personality: newer },
      })
    ).status(),
  ).toBe(200);
  await page.getByLabel("历史性格版本").selectOption("1");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "恢复此版本", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("版本已变化");
  expect(
    (
      await (
        await page.request.get("/api/admin/v1/assistant/personality")
      ).json()
    ).personality.description,
  ).toBe(newer.description);
  await page.getByRole("button", { name: "↻ 刷新", exact: true }).click();
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
  await page.getByLabel("开始时间", { exact: true }).fill("2027-01-01T09:00");
  await page.getByLabel("结束时间", { exact: true }).fill("2027-01-01T10:00");
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
  await page.getByRole("button", { name: "确认人工调整" }).click();
  await expect(page.getByRole("status")).toContainText(
    "已保存人工确认的好感调整",
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

test("reviewed model diaries display their actual provenance", async ({
  page,
}) => {
  const session = await (
    await page.request.get("/api/admin/v1/auth/me")
  ).json();
  const headers = {
    origin: "http://127.0.0.1:18449",
    "x-csrf-token": session.csrf_token,
  };
  const state = JSON.parse(
    readFileSync(resolve("../../.local/prometheus/e2e-state.json"), "utf8"),
  );
  const credential = await (
    await page.request.post("/api/admin/v1/secrets", {
      headers,
      data: { kind: "model_api_key", value: "synthetic-diary-browser-key" },
    })
  ).json();
  const current = await (await page.request.get("/api/admin/v1/config")).json();
  current.config.model = {
    model: "synthetic",
    base_url: state.modelUrl,
    credential_ref: credential.id,
  };
  expect(
    (
      await page.request.put("/api/admin/v1/config", {
        headers: { ...headers, "if-match": `"${current.revision}"` },
        data: { config: current.config },
      })
    ).status(),
  ).toBe(200);
  const memory = await (
    await page.request.post("/api/admin/v1/records/memory", {
      headers,
      data: { data: { content: "日记界面的合成来源" } },
    })
  ).json();
  const created = await (
    await page.request.post("/api/admin/v1/knowledge/jobs", {
      headers,
      data: {
        kind: "diary",
        sources: [{ type: "record", id: memory.id }],
        options: { date: "2026-10-31", period: "day" },
      },
    })
  ).json();
  let job: any;
  await expect
    .poll(async () => {
      job = await (
        await page.request.get(`/api/admin/v1/knowledge/jobs/${created.id}`)
      ).json();
      return job.status;
    })
    .toBe("review_pending");
  expect(
    (
      await page.request.post(`/api/admin/v1/knowledge/jobs/${job.id}/review`, {
        headers: { ...headers, "if-match": `"${job.revision}"` },
        data: { approve: true },
      })
    ).status(),
  ).toBe(200);
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日记", exact: true })
    .click();
  await expect(
    page.locator(".record-card").filter({ hasText: "来源明确的模型日记" }),
  ).toContainText("模型整理 · 已人工审核");
});

test("journal filters show authorized input and actual context with responsive safe previews", async ({
  page,
}) => {
  const me = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const headers = {
    origin: "http://127.0.0.1:18449",
    "x-csrf-token": me.csrf_token,
  };
  const conversation = await (
    await page.request.post("/api/admin/v1/conversations", {
      headers,
      data: { title: "日志浏览器来源" },
    })
  ).json();
  const run = await (
    await page.request.post(
      `/api/admin/v1/conversations/${conversation.id}/messages`,
      {
        headers,
        data: {
          content: "浏览器日志唯一来源 synthetic-diary-browser-key",
          client_message_id: "browser-journal",
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
    .getByRole("button", { name: "日志记录", exact: true })
    .click();
  await page.getByLabel("日志类型", { exact: true }).selectOption("input");
  await page.getByLabel("运行标识", { exact: true }).fill(run.run_id);
  await page.getByLabel("搜索日志", { exact: true }).fill("浏览器日志唯一来源");
  await page.getByRole("button", { name: "筛选日志", exact: true }).click();
  await expect(page.locator(".log-row")).toHaveCount(1);
  await page.locator(".log-row summary").click();
  await expect(page.locator(".log-row pre")).toContainText(
    "浏览器日志唯一来源",
  );
  await expect(page.locator(".log-row pre")).not.toContainText(
    "synthetic-diary-browser-key",
  );
  await page.getByLabel("日志类型", { exact: true }).selectOption("context");
  await page.getByRole("button", { name: "筛选日志", exact: true }).click();
  await expect(page.locator(".log-row")).toHaveCount(1);
  await expect(page.locator(".log-row strong")).toHaveText("模型请求上下文");
  await page.locator(".log-row summary").click();
  await expect(page.locator(".log-row pre")).toContainText('"role":"system"');
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.screenshot({
    path: resolve(
      "../../.local/prometheus/screenshots/logs-filtered-mobile.png",
    ),
    fullPage: true,
    animations: "disabled",
  });
});

test("configuration drafts survive reload and require an explicit rebase before applying a receipt", async ({
  page,
}) => {
  const me = await (await page.request.get("/api/admin/v1/auth/me")).json();
  const headers = {
    origin: "http://127.0.0.1:18449",
    "x-csrf-token": me.csrf_token,
  };
  const original = await (
    await page.request.get("/api/admin/v1/config")
  ).json();
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "模型与运行", exact: true })
    .click();
  await page
    .getByLabel("模型名称", { exact: true })
    .fill("draft-browser-model");
  const panel = page.locator(".configuration-drafts");
  await panel.getByText("配置草稿与操作记录", { exact: true }).click();
  await panel.getByRole("button", { name: "另存当前表单为草稿" }).click();
  await expect(panel.getByRole("status")).toContainText("草稿已保存");
  const id = await panel.getByLabel("选择配置草稿").inputValue();
  expect(id).toMatch(/^[0-9a-f-]{36}$/);
  expect(
    (await (await page.request.get("/api/admin/v1/config")).json()).revision,
  ).toBe(original.revision);
  await panel.getByRole("button", { name: "校验草稿", exact: true }).click();
  await expect(
    panel.getByText("字段校验通过；连接状态以操作记录为准。", { exact: true }),
  ).toBeVisible();
  await panel
    .getByRole("button", { name: "检查草稿连接", exact: true })
    .click();
  await expect(panel.locator(".draft-operation > strong")).toHaveText(
    "检查完成",
  );
  await page.reload();
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "模型与运行", exact: true })
    .click();
  await panel.getByText("配置草稿与操作记录", { exact: true }).click();
  await panel.getByLabel("选择配置草稿").selectOption(id);
  await panel.getByRole("button", { name: "载入草稿到表单" }).click();
  await expect(page.getByLabel("模型名称", { exact: true })).toHaveValue(
    "draft-browser-model",
  );
  expect(
    (
      await page.request.put("/api/admin/v1/config", {
        headers: { ...headers, "if-match": `"${original.revision}"` },
        data: { config: original.config },
      })
    ).status(),
  ).toBe(200);
  await panel.getByRole("button", { name: "刷新草稿与操作" }).click();
  await expect(
    panel.getByRole("button", { name: "应用已保存草稿" }),
  ).toBeDisabled();
  await expect(
    panel.getByText("生效配置已变化", { exact: false }),
  ).toBeVisible();
  page.once("dialog", (dialog) => dialog.accept());
  await panel.getByRole("button", { name: "核对差异并重新定位草稿" }).click();
  await panel.getByRole("button", { name: "校验草稿", exact: true }).click();
  await panel
    .getByRole("button", { name: "检查草稿连接", exact: true })
    .click();
  await expect(panel.locator(".draft-operation > strong")).toHaveText(
    "检查完成",
  );
  page.once("dialog", (dialog) => dialog.accept());
  await panel.getByRole("button", { name: "应用已保存草稿" }).click();
  await expect(page.getByLabel("模型名称", { exact: true })).toHaveValue(
    "draft-browser-model",
  );
  await expect
    .poll(
      async () =>
        (await (await page.request.get("/api/admin/v1/config")).json())
          .revision,
    )
    .toBe(original.revision + 2);
  // Parent refresh reconstructs the panel; persisted receipts remain available.
  if ((await panel.locator("details").first().getAttribute("open")) === null)
    await panel.getByText("配置草稿与操作记录", { exact: true }).click();
  await panel.getByLabel("选择配置草稿").selectOption(id);
  await expect(
    panel.getByText(`当前生效 v${original.revision + 2}`, { exact: false }),
  ).toBeVisible();
  const receipts = await (
    await page.request.get("/api/admin/v1/config/operations")
  ).json();
  const applied = receipts.items.find(
    (item: any) => item.kind === "apply" && item.draft_id === id,
  );
  await panel.getByLabel("查看配置操作").selectOption(applied.id);
  await expect(panel.locator(".draft-operation > strong")).toHaveText(
    "应用完成",
  );
  await panel.getByText("操作回执", { exact: true }).click();
  await expect(panel.locator(".draft-operation pre")).toContainText(
    '"applied_revision"',
  );
  await expect(panel.locator(".draft-operation pre")).not.toContainText(
    "synthetic-diary-browser-key",
  );
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.screenshot({
    path: resolve(
      "../../.local/prometheus/screenshots/config-drafts-mobile.png",
    ),
    fullPage: true,
    animations: "disabled",
  });
});

test("relationship mappings can be split without moving independent profile or score data", async ({
  page,
}) => {
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "人际关系", exact: true })
    .click();
  await page.getByText("核对并合并重复人物", { exact: true }).click();
  await page
    .getByLabel("待合并人物", { exact: true })
    .selectOption({ label: "甲" });
  await page
    .getByLabel("保留人物", { exact: true })
    .selectOption({ label: "乙" });
  await page
    .getByLabel("合并依据", { exact: true })
    .fill("浏览器核对的合成重复档案");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "确认合并人物", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("独立账本保留");
  await expect(
    page.locator(".record-card").filter({ hasText: "甲" }).last(),
  ).toContainText("映射并入：乙");
  await page.getByText("核对并拆分人物映射", { exact: true }).click();
  await page
    .getByLabel("待拆分人物", { exact: true })
    .selectOption({ label: "甲" });
  await page.getByLabel("拆分依据", { exact: true }).fill("重新核对为不同人物");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "确认拆分人物", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("人物映射已拆分");
  await page.getByText("好感规则", { exact: true }).click();
  await page.getByLabel("每日衰减", { exact: true }).fill("0.5");
  await page
    .getByLabel("规则修改依据", { exact: true })
    .fill("仅调整未来的衰减区间");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "保存好感规则", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("好感规则已保存");
  await page.getByText("好感策略历史", { exact: true }).click();
  await expect(
    page.getByText("仅调整未来的衰减区间", { exact: false }),
  ).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await page.screenshot({
    path: resolve(
      "../../.local/prometheus/screenshots/relationships-ledger-mobile.png",
    ),
    fullPage: true,
    animations: "disabled",
  });
});

test("civil calendar, scoped edits, authorized reading and work goals operate through management pages", async ({
  page,
}) => {
  await page
    .getByRole("navigation")
    .getByRole("button", { name: "日程", exact: true })
    .click();
  const editor = page.locator(".schedule-editor"),
    work = page.locator(".schedule-work");
  await page.getByLabel("日历日期", { exact: true }).fill("2027-01-31");
  await page.getByLabel("日历日期", { exact: true }).press("Tab");
  await editor.getByRole("button", { name: "新建活动", exact: true }).click();
  await editor.getByLabel("活动标题", { exact: true }).fill("浏览器月度系列");
  await editor.getByLabel("活动类型", { exact: true }).selectOption("video");
  await editor
    .getByLabel("活动目的", { exact: true })
    .fill("仅核对合成日历与能力门禁");
  await editor.getByLabel("活动开始", { exact: true }).fill("2027-01-31T09:00");
  await editor.getByLabel("活动结束", { exact: true }).fill("2027-01-31T10:00");
  await editor.getByLabel("活动重复", { exact: true }).selectOption("monthly");
  await editor.getByLabel("重复次数（0表示不指定）", { exact: true }).fill("3");
  await editor.getByRole("button", { name: "预演活动", exact: true }).click();
  await expect(editor.getByText("只读预演", { exact: true })).toBeVisible();
  await editor.getByRole("button", { name: "保存活动", exact: true }).click();
  await expect(editor.getByRole("status")).toContainText("活动已保存");
  await page
    .locator(".calendar-grid")
    .getByRole("button", { name: "浏览器月度系列", exact: true })
    .click();
  await expect(editor.getByLabel("活动修改范围", { exact: true })).toHaveValue(
    "single",
  );
  await editor.getByLabel("活动开始", { exact: true }).fill("2027-02-08T09:00");
  await editor.getByLabel("活动结束", { exact: true }).fill("2027-02-08T10:00");
  await editor
    .getByRole("button", { name: "保存活动更改", exact: true })
    .click();
  await expect(editor.getByRole("status")).toContainText("更改已保存");
  await page.getByLabel("日历日期", { exact: true }).fill("2027-02-08");
  await page.getByLabel("日历日期", { exact: true }).press("Tab");
  await expect(
    page
      .locator(".calendar-grid")
      .getByRole("button", { name: "浏览器月度系列", exact: true }),
  ).toBeVisible();
  await page
    .locator(".calendar-grid")
    .getByRole("button", { name: "浏览器月度系列", exact: true })
    .dragTo(page.locator('.calendar-day:has(time[datetime="2027-02-09"])'));
  await expect(editor.getByRole("status")).toContainText("拖动改期已预演");
  await expect(editor.getByLabel("活动开始", { exact: true })).toHaveValue(
    "2027-02-09T09:00",
  );
  const unmoved = await (
    await page.request.get(
      `/api/admin/v1/schedule/calendar?start=${Date.parse("2027-02-08T00:00+08:00") / 1000}&end=${Date.parse("2027-02-09T00:00+08:00") / 1000}`,
    )
  ).json();
  expect(
    unmoved.items.some((item: any) => item.title === "浏览器月度系列"),
  ).toBe(true);
  await editor
    .getByRole("button", { name: "保存活动更改", exact: true })
    .click();
  await expect(editor.getByRole("status")).toContainText("更改已保存");
  await page.getByLabel("日历日期", { exact: true }).fill("2027-03-31");
  await page.getByLabel("日历日期", { exact: true }).press("Tab");
  await page.getByLabel("日历视图").selectOption("day");
  await page
    .locator(".calendar-grid")
    .getByRole("button", { name: "浏览器月度系列", exact: true })
    .click();
  await editor
    .getByLabel("活动修改范围", { exact: true })
    .selectOption("following");
  await expect(
    editor.getByLabel("重复次数（0表示不指定）", { exact: true }),
  ).toHaveValue("1");
  await editor.getByLabel("活动开始", { exact: true }).fill("2027-03-31T11:00");
  await editor.getByLabel("活动结束", { exact: true }).fill("2027-03-31T12:00");
  await editor
    .getByRole("button", { name: "保存活动更改", exact: true })
    .click();
  await expect(editor.getByRole("status")).toContainText("后续系列已另建");
  await editor
    .getByRole("button", { name: "关闭活动编辑", exact: true })
    .click();
  await work.getByText("授权文本与阅读进度", { exact: true }).click();
  await work.getByLabel("授权文本标题", { exact: true }).fill("浏览器合成书架");
  await work
    .getByLabel("授权文本正文", { exact: true })
    .fill("浏览器明确授权的合成片段。");
  await work
    .getByLabel("明确允许当前模型服务阅读这份私人文本", { exact: true })
    .check();
  await work
    .getByRole("button", { name: "保存并授权文本", exact: true })
    .click();
  await expect(work.getByRole("status")).toContainText("尚不代表已阅读");
  await work.getByText("自主安排范围", { exact: true }).click();
  await work.getByLabel("授权名称", { exact: true }).fill("浏览器阅读范围");
  await work.getByLabel("范围内可用：浏览器合成书架", { exact: true }).check();
  await work
    .getByLabel("明确授予以上范围，不授予消息发送或程序执行权限", {
      exact: true,
    })
    .check();
  await work.getByRole("button", { name: "保存自主授权", exact: true }).click();
  await expect(work.getByRole("status")).toContainText("自主范围已保存");
  await work.getByText("工作目标与规划", { exact: true }).click();
  await work.getByLabel("工作目标", { exact: true }).fill("浏览器规则工作目标");
  await work
    .getByLabel("工作目标授权", { exact: true })
    .selectOption({ label: "浏览器阅读范围" });
  await work.getByLabel("目标采用：浏览器合成书架", { exact: true }).check();
  await work
    .getByLabel("启用目标，在已有范围内自动安排", { exact: true })
    .check();
  await work.getByRole("button", { name: "保存工作目标", exact: true }).click();
  await expect(work.getByRole("status")).toContainText("目标已启用");
  await work
    .getByRole("button", { name: "预演目标空闲段", exact: true })
    .click();
  await expect(work.getByText("只读候选时段", { exact: false })).toBeVisible();
  const local = (time: number) =>
    new Date(time + 8 * 3600000).toISOString().slice(0, 16);
  await page
    .getByLabel("日历日期", { exact: true })
    .fill(local(Date.now()).slice(0, 10));
  await page.getByLabel("日历日期", { exact: true }).press("Tab");
  await editor.getByRole("button", { name: "新建活动", exact: true }).click();
  await editor
    .getByLabel("活动标题", { exact: true })
    .fill("浏览器实际片段请求");
  await editor
    .getByLabel("活动目的", { exact: true })
    .fill("只读取明确提供的合成片段");
  await editor
    .getByLabel("活动开始", { exact: true })
    .fill(local(Date.now() - 60000));
  await editor
    .getByLabel("活动结束", { exact: true })
    .fill(local(Date.now() + 120000));
  await editor
    .getByLabel("活动阅读资料", { exact: true })
    .selectOption({ label: "浏览器合成书架" });
  await editor.getByRole("button", { name: "保存活动", exact: true }).click();
  await expect(editor.getByRole("status")).toContainText("活动已保存");
  await editor
    .getByRole("button", { name: "关闭活动编辑", exact: true })
    .click();
  let book: any;
  await expect
    .poll(async () => {
      book = (
        await (
          await page.request.get("/api/admin/v1/schedule/resources")
        ).json()
      ).items.find((item: any) => item.title === "浏览器合成书架");
      return book.progress?.checkpoint;
    })
    .toBe([..."浏览器明确授权的合成片段。"].length);
  await work
    .getByRole("button", { name: "刷新书架与目标", exact: true })
    .click();
  await work
    .getByRole("button", { name: "查看覆盖与笔记", exact: true })
    .click();
  await expect(work.locator("pre")).toContainText("合成浏览器回复");
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await work.screenshot({
    path: resolve(
      "../../.local/prometheus/screenshots/schedule-reading-mobile.png",
    ),
    animations: "disabled",
  });
  page.once("dialog", (dialog) => dialog.accept());
  await work.getByRole("button", { name: "撤销文本授权", exact: true }).click();
  await expect(work.getByRole("status")).toContainText("派生笔记已撤销");
  const revoked = await (
    await page.request.get(`/api/admin/v1/schedule/resources/${book.id}`)
  ).json();
  expect(revoked.notes).toHaveLength(0);
  expect(revoked.progress).toBeNull();
});

test("diary events, immutable period coverage, explicit model permission and recursive forgetting work in management pages", async ({
  page,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.getByRole("button", { name: "日记", exact: true }).click();
  const ledger = page.locator(".diary-ledger");
  await expect(
    ledger.getByRole("heading", { name: "日记周期与来源覆盖", exact: true }),
  ).toBeVisible();
  await ledger
    .getByLabel("发生的民用时间", { exact: true })
    .fill("2026-09-15T12:00");
  await ledger
    .getByLabel("事件正文", { exact: true })
    .fill("浏览器明确保存并核对的九月事件。");
  await ledger
    .getByRole("button", { name: "保存明确事件", exact: true })
    .click();
  await expect(ledger.getByRole("status")).toContainText("事件已保存");
  await ledger.getByLabel("整理日期", { exact: true }).fill("2026-09-15");
  await ledger
    .getByRole("button", { name: "准备这个日记周期", exact: true })
    .click();
  const detail = ledger.locator(".diary-detail");
  await expect(detail).toContainText("浏览器明确保存并核对的九月事件");
  await expect(detail).toContainText("1条真实来源");
  let p: any, entry: any;
  const state = await (
    await page.request.get("/api/admin/v1/diary/policies")
  ).json();
  p = state.items.find(
    (p: any) => p.source_key.startsWith("self:") && p.visibility === "private",
  );
  entry = (
    await (
      await page.request.get(
        `/api/admin/v1/diary/entries?policy_id=${p.id}&date=2026-09-15&period=day`,
      )
    ).json()
  ).items[0];
  expect(entry.version.generator).toBe("evidence_excerpt_v1");
  await ledger.getByText("日期整理与模型许可", { exact: true }).click();
  await ledger
    .getByLabel("允许本分区的资料发送给模型整理", { exact: true })
    .check();
  await ledger
    .getByRole("button", { name: "保存日期分区许可", exact: true })
    .click();
  await expect(ledger.getByRole("status")).toContainText("分区范围已保存");
  await ledger
    .getByLabel("同时请求已获授权的模型整理", { exact: true })
    .check();
  await ledger
    .getByRole("button", { name: "准备这个日记周期", exact: true })
    .click();
  await expect
    .poll(async () => {
      entry = await (
        await page.request.get(`/api/admin/v1/diary/entries/${entry.id}`)
      ).json();
      return entry.task.state;
    })
    .toBe("completed");
  await detail
    .getByRole("button", { name: "刷新所选周期状态", exact: true })
    .click();
  await expect(detail).toContainText("浏览器固定来源的完整日记归并");
  expect(entry.coverage.complete_raw).toBe(true);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page
      .locator("body")
      .evaluate((node) => node.scrollWidth <= innerWidth),
  ).toBe(true);
  await detail.screenshot({
    path: resolve(
      "../../.local/prometheus/screenshots/diary-coverage-mobile.png",
    ),
    animations: "disabled",
  });
  await ledger
    .getByRole("button", { name: "更正这条事件", exact: true })
    .filter({ visible: true })
    .first()
    .click();
  await ledger
    .getByLabel("事件正文", { exact: true })
    .fill("浏览器人工更正后的九月事件。");
  await ledger
    .getByRole("button", { name: "保存事件更正", exact: true })
    .click();
  await expect(ledger.getByRole("status")).toContainText("事件已保存");
  const invalid = await (
    await page.request.get(`/api/admin/v1/diary/entries/${entry.id}`)
  ).json();
  expect(invalid.state).toBe("invalidated");
  expect(
    invalid.versions.every((v: any) => v.facts === null && v.feelings === null),
  ).toBe(true);
  await ledger
    .getByRole("button", { name: "准备这个日记周期", exact: true })
    .click();
  await expect
    .poll(async () => {
      const r = await (
        await page.request.get(`/api/admin/v1/diary/entries/${entry.id}`)
      ).json();
      return r.task.state;
    })
    .toBe("completed");
  page.once("dialog", (dialog) => dialog.accept());
  await ledger
    .locator(".job-card")
    .filter({ hasText: "浏览器人工更正后的九月事件。" })
    .getByRole("button", { name: "遗忘这条事件", exact: true })
    .click();
  await expect(ledger.getByRole("status")).toContainText("事件已遗忘");
  const forgotten = await (
    await page.request.get(`/api/admin/v1/diary/entries/${entry.id}`)
  ).json();
  expect(forgotten.state).toBe("invalidated");
  expect(forgotten.version.facts).toBeNull();
  await page.setViewportSize({ width: 1280, height: 900 });
  await ledger
    .getByLabel("事件正文", { exact: true })
    .fill("不会保存的临时草稿");
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "运行总览", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "运行总览", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "日记", exact: true }).click();
  await expect(
    page.locator(".diary-ledger").getByLabel("事件正文", { exact: true }),
  ).toHaveValue("");
  expect(errors).toEqual([]);
});
