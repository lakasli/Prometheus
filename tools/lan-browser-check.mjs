// Read-only production login-page check. No password, login POST, or data write.
import fs from "node:fs";
import path from "node:path";
import https from "node:https";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(path.join(root, "web/admin/package.json"));
const { chromium } = require("@playwright/test");
const origin = "https://192.168.189.191:8086";
const directory = path.resolve(process.argv[2] || path.join(root, ".local/lan-https"));
const ca = fs.readFileSync(path.join(directory, "DSH-LAN-Root-CA.crt"));
// Verify the actual chain and IP before opening a browser. The temporary
// browser context does not install this CA into the user's OS trust store.
const status = await new Promise((resolve, reject) => {
  const request = https.get(origin + "/health/ready", { ca, rejectUnauthorized: true }, (response) => {
    response.resume();
    resolve(response.statusCode);
  });
  request.setTimeout(10000, () => request.destroy(new Error("TLS check timed out")));
  request.on("error", reject);
});
if (status !== 200) throw new Error("LAN TLS readiness failed");
const browser = await chromium.launch({ headless: true });
const report = {
  origin, status: "running", read_only: true, production_login_attempted: false,
  tls_chain_and_ip_verified_by_node: true,
  browser_context_ignores_uninstalled_ca: true, system_trust_store_modified: false,
  viewports: [], errors: [],
};
try {
  const context = await browser.newContext({ ignoreHTTPSErrors: true });
  await context.route("**/*", async (route) => {
    if (route.request().method() !== "GET") {
      report.errors.push("unexpected non-GET request");
      await route.abort();
    } else await route.continue();
  });
  const page = await context.newPage();
  page.on("pageerror", (error) => report.errors.push(error.name));
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(origin + "/admin/", { waitUntil: "networkidle" });
    await page.getByLabel("管理员用户名", { exact: true }).waitFor();
    await page.getByRole("button", { name: "登录控制台", exact: true }).waitFor();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
    if (overflow) throw new Error("login page overflows viewport");
    await page.screenshot({ path: path.join(directory, `lan-login-${width}.png`), fullPage: true });
    report.viewports.push(width);
  }
  if (report.errors.length) throw new Error("browser errors occurred");
  report.status = "passed";
} catch (error) {
  report.status = "failed";
  report.error_type = error.name;
  throw error;
} finally {
  await browser.close();
  fs.writeFileSync(path.join(directory, "browser-check.json"), JSON.stringify(report, null, 2));
}
console.log(JSON.stringify(report));
