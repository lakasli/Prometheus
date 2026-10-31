import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import { resolve, join, dirname, sep } from "node:path";
import {
  readFileSync,
  writeFileSync,
  existsSync,
  mkdirSync,
  lstatSync,
  readdirSync,
  copyFileSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
const { values } = parseArgs({
  options: { output: { type: "string" }, cache: { type: "string" } },
});
if (process.platform !== "win32" || !values.output || !values.cache)
  throw Error(
    "Supply a new Windows runtime output directory and a verified artifact cache.",
  );
const lock = JSON.parse(
    readFileSync(new URL("./media-runtime-lock.json", import.meta.url), "utf8"),
  ),
  output = resolve(values.output),
  cache = resolve(values.cache);
if (existsSync(output))
  throw Error("Keep existing runtime directories; use a fresh output.");
const sha = (b) => createHash("sha256").update(b).digest("hex");
mkdirSync(cache, { recursive: true });
for (const f of [...lock.artifacts, ...lock.wheels]) {
  const path = resolve(cache, f.file);
  if (!path.startsWith(cache + sep) || f.file.includes(".."))
    throw Error("Invalid cache boundary");
  if (!existsSync(path)) {
    const u = new URL(f.url);
    if (
      u.protocol !== "https:" ||
      ![
        "www.python.org",
        "github.com",
        "huggingface.co",
        "files.pythonhosted.org",
      ].includes(u.hostname)
    )
      throw Error("Untrusted dependency host");
    const r = await fetch(u, { signal: AbortSignal.timeout(120000) });
    if (!r.ok) throw Error("Artifact download failed");
    const b = Buffer.from(await r.arrayBuffer());
    if (b.length !== f.bytes || sha(b) !== f.sha256)
      throw Error("Artifact digest mismatch");
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, b, { flag: "wx" });
  }
  if (lstatSync(path).isSymbolicLink() || sha(readFileSync(path)) !== f.sha256)
    throw Error("Cached artifact digest mismatch");
}
function unpack(file, target, members = [], strip = 0) {
  const paths = execFileSync("tar.exe", ["-tf", file], {
    encoding: "utf8",
    windowsHide: true,
    maxBuffer: 4 * 1024 * 1024,
  })
    .trim()
    .split(/\r?\n/);
  if (
    paths.length > 20000 ||
    paths.some(
      (p) =>
        p.includes("\\") ||
        p.includes(":") ||
        p.startsWith("/") ||
        p.split("/").some((s) => s === ".."),
    )
  )
    throw Error("Unsafe dependency archive");
  mkdirSync(target, { recursive: true });
  execFileSync(
    "tar.exe",
    [
      "-xf",
      file,
      "-C",
      target,
      ...(strip ? ["--strip-components", String(strip)] : []),
      ...members,
    ],
    {
      windowsHide: true,
      stdio: "pipe",
    },
  );
}
unpack(join(cache, "python.zip"), output);
for (const f of lock.wheels) unpack(join(cache, f.file), join(output, "Lib"));
const models = join(output, "models");
mkdirSync(models);
for (const name of ["huayan.onnx", "huayan.onnx.json"])
  copyFileSync(join(cache, name), join(models, name));
const prefix = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17";
// Extract only runtime weights, tokens and attribution; no upstream export programs.
unpack(
  join(cache, "sensevoice.tar.bz2"),
  models,
  [prefix + "/model.int8.onnx", prefix + "/tokens.txt", prefix + "/LICENSE"],
  1,
);
copyFileSync(
  fileURLToPath(
    new URL("../../apps/server/native/media-worker.py", import.meta.url),
  ),
  join(output, "media-worker.py"),
);
mkdirSync(join(output, "licenses"));
copyFileSync(
  fileURLToPath(new URL("./licenses/silk-sdk.txt", import.meta.url)),
  join(output, "licenses/silk-sdk.txt"),
);
writeFileSync(
  join(output, "ATTRIBUTION.json"),
  JSON.stringify(
    { artifacts: lock.artifacts, wheels: lock.wheels, licenses: lock.licenses },
    null,
    2,
  ),
);
const files = [];
function walk(dir) {
  for (const f of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, f.name);
    if (f.isSymbolicLink()) throw Error("Runtime links are forbidden");
    if (f.isDirectory()) walk(path);
    else
      files.push({
        path: path
          .slice(output.length + 1)
          .split(sep)
          .join("/"),
        sha256: sha(readFileSync(path)),
      });
  }
}
walk(output);
writeFileSync(
  join(output, "manifest.json"),
  JSON.stringify({ lock_sha256: sha(JSON.stringify(lock)), files }, null, 2),
);
console.log(
  JSON.stringify({
    output,
    files: files.length,
    lock_sha256: sha(JSON.stringify(lock)),
  }),
);
