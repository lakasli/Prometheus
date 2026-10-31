import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
const root = new URL("../../../", import.meta.url);
const output = new URL("../dist/assets/", import.meta.url);
mkdirSync(output, { recursive: true });
copyFileSync(
  new URL("docs/examples/liu-jiahui-personality-v3.json", root),
  new URL("personality.json", output),
);
copyFileSync(
  new URL("crates/dshd/qq-api-catalog.json", root),
  new URL("qq-api-catalog.json", output),
);
copyFileSync(
  new URL("../src/schema.sql", import.meta.url),
  new URL("schema.sql", output),
);
const manifestPath = new URL("build.json", output);
const sha = process.env.PROMETHEUS_BUILD_SHA || "development";
if (sha !== "development" && !/^[0-9a-f]{40}$/.test(sha))
  throw new Error("PROMETHEUS_BUILD_SHA must be a complete Git SHA");
writeFileSync(
  manifestPath,
  JSON.stringify(
    {
      name: "Prometheus",
      version: JSON.parse(
        readFileSync(new URL("../package.json", import.meta.url), "utf8"),
      ).version,
      commit: sha,
      data_schema: 15,
    },
    null,
    2,
  ) + "\n",
);
console.log(
  `Prometheus server built (${sha}); assets: ${fileURLToPath(output)}`,
);
