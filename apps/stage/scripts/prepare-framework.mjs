import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const release = "5-r.5";
const sdk = `CubismSdkForWeb-${release}`;
const archiveSha256 =
  "67064a7fb1812cf502f5c4a03bfe12cc638c75a621bb4acf06bb28763df06ba0";
const cache = fileURLToPath(
  new URL("../node_modules/.cubism/", import.meta.url),
);
const framework = join(cache, release);
if (existsSync(framework)) process.exit(0);

await mkdir(cache, { recursive: true });
const temporary = await mkdtemp(join(cache, `${release}-`));
try {
  const response = await fetch(
    `https://cubism.live2d.com/sdk-web/bin/${sdk}.zip`,
    {
      signal: AbortSignal.timeout(120_000),
    },
  );
  if (!response.ok)
    throw new Error(
      `Live2D Cubism Web SDK download failed (${response.status})`,
    );
  const archive = Buffer.from(await response.arrayBuffer());
  if (createHash("sha256").update(archive).digest("hex") !== archiveSha256)
    throw new Error("Live2D Cubism Web SDK SHA-256 mismatch");

  const archivePath = join(temporary, `${sdk}.zip`);
  await writeFile(archivePath, archive);
  execFileSync(
    process.platform === "win32" ? "tar.exe" : "tar",
    [
      "-xf",
      archivePath,
      "-C",
      temporary,
      `${sdk}/Framework`,
      `${sdk}/Core/live2dcubismcore.d.ts`,
    ],
    { stdio: "inherit" },
  );
  try {
    await rename(join(temporary, sdk), framework);
  } catch (error) {
    if (!existsSync(framework)) throw error;
  }
} finally {
  await rm(temporary, { recursive: true, force: true });
}
