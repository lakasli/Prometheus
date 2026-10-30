import { resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";
import { readFileSync, writeFileSync } from "node:fs";
const root = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? ".");
const { startServer } = await import(
  pathToFileURL(resolve(root, "apps/server/dist/server.js")).href
);
const { bootstrapToken } = await import(
  pathToFileURL(resolve(root, "apps/server/dist/auth.js")).href
);
const dataDir = resolve(`.local/prometheus/e2e/${randomUUID()}`);
const app = await startServer({
  dataDir,
  webDir: resolve(root, "apps/admin/dist"),
  host: "127.0.0.1",
  port: 18449,
  origin: "http://127.0.0.1:18449",
  secureCookie: false,
  allowLoopbackModel: true,
});
const tokenFile = bootstrapToken(dataDir);
const stateFile = resolve(".local/prometheus/e2e-state.json");
// This state only contains fixture file locations, never token values.
writeFileSync(
  stateFile,
  JSON.stringify({
    dataDir,
    tokenFile,
    commit: JSON.parse(
      readFileSync(resolve(root, "apps/server/dist/assets/build.json"), "utf8"),
    ).commit,
  }),
);
const close = async () => {
  await app.close();
  process.exitCode = 0;
};
process.once("SIGINT", close);
process.once("SIGTERM", close);
console.log("Prometheus synthetic browser fixture ready on loopback.");
