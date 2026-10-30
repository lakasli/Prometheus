import { writeFileSync } from "node:fs";
const commit = process.env.VITE_BUILD_SHA || "development";
if (commit !== "development" && !/^[0-9a-f]{40}$/.test(commit))
  throw new Error("VITE_BUILD_SHA must be a complete Git SHA");
writeFileSync(
  new URL("../dist/build.json", import.meta.url),
  JSON.stringify({ name: "Prometheus", version: "0.1.0", commit }, null, 2) +
    "\n",
);
