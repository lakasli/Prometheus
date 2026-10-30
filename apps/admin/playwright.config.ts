import { defineConfig } from "@playwright/test";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(new URL("../../", import.meta.url));
export default defineConfig({
  testDir: "./test",
  testMatch: "*.spec.ts",
  workers: 1,
  timeout: 30000,
  use: {
    baseURL: "http://127.0.0.1:18449",
    browserName: "chromium",
    screenshot: "only-on-failure",
    trace: "off",
  },
  outputDir: "../../.local/prometheus/e2e-results",
  webServer: {
    command: "node apps/admin/test/server.mjs",
    cwd: root,
    url: "http://127.0.0.1:18449/health/live",
    reuseExistingServer: false,
    timeout: 15000,
  },
});
