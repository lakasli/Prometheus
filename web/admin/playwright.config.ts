import { defineConfig } from "@playwright/test";
import path from "node:path";

const baseURL = process.env.DSH_E2E_BASE_URL || "http://127.0.0.1:18420";
if (!["localhost", "127.0.0.1", "[::1]"].includes(new URL(baseURL).hostname)) {
  throw new Error("E2E tests must target an isolated localhost DSH instance.");
}

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  reporter: "list",
  outputDir: path.resolve("../../.local/browser-e2e/results"),
  use: {
    baseURL,
    browserName: "chromium",
    headless: true,
    viewport: { width: 1440, height: 1000 },
    trace: "off",
    video: "off",
    screenshot: "off",
  },
});
