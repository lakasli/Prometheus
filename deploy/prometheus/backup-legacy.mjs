import { backup, DatabaseSync } from "node:sqlite";
import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import {
  basename,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { createHash } from "node:crypto";
import { privateDirectory } from "../../apps/server/dist/security.js";

const repo = resolve(process.cwd());
const gitRoot = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
  cwd: repo,
}).trim();
if (resolve(gitRoot).toLowerCase() !== repo.toLowerCase())
  throw new Error("Run this script from the repository root");
const stamp = new Date().toISOString().replace(/[:.]/g, "-");
const destination = privateDirectory(
  join(repo, ".local", "prometheus", "backups", `legacy-${stamp}`),
);
const inside = (path) => {
  const rel = relative(repo, resolve(path));
  if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel))
    throw new Error("Backup source must stay inside this repository");
  return resolve(path);
};
const sha256 = (path) =>
  createHash("sha256").update(readFileSync(path)).digest("hex");
const manifest = {
  created_at: new Date().toISOString(),
  source_commit: execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: repo,
    encoding: "utf8",
  }).trim(),
  source_branch: execFileSync("git", ["branch", "--show-current"], {
    cwd: repo,
    encoding: "utf8",
  }).trim(),
  code_bundle: "legacy-source.bundle",
  dirty_patch: "working-tree.patch",
  copied_untracked: [],
  data_snapshots: [],
  retained_in_place: [],
  restoration:
    "Restore the bundle into another directory; inspect the patch before applying. Restore each data snapshot into its own directory while the legacy runtime is stopped. Prometheus does not import it.",
};
execFileSync(
  "git",
  ["bundle", "create", join(destination, manifest.code_bundle), "--all"],
  { cwd: repo, stdio: "pipe" },
);
writeFileSync(
  join(destination, manifest.dirty_patch),
  execFileSync("git", ["diff", "--binary", "HEAD"], {
    cwd: repo,
    maxBuffer: 64 * 1024 * 1024,
  }),
  { flag: "wx", mode: 0o600 },
);
const priorUntracked = [
  "crates/dshd/src/breeze_speech.rs",
  "deploy/breeze-runtime.json",
  "deploy/package-windows.ps1",
  "deploy/start-breeze.ps1",
  "deploy/start-windows.ps1",
  "tools/breeze-speech-smoke.py",
  "web/admin/tests/e2e/breeze-speech.spec.ts",
  "web/admin/tests/speech.test.ts",
];
for (const name of priorUntracked) {
  const source = inside(join(repo, name));
  if (!existsSync(source)) continue;
  const target = join(destination, "untracked", name);
  mkdirSync(dirname(target), { recursive: true });
  cpSync(source, target, { force: false, errorOnExist: true });
  manifest.copied_untracked.push({ path: name, sha256: sha256(target) });
}
const candidates = [join(repo, "data"), join(repo, ".local", "dev")];
if (existsSync(join(repo, ".local")))
  for (const item of readdirSync(join(repo, ".local"), {
    withFileTypes: true,
  })) {
    if (item.isDirectory() && item.name !== "prometheus")
      candidates.push(join(repo, ".local", item.name, "data"));
  }
for (const candidate of [...new Set(candidates)]) {
  const source = inside(candidate),
    databasePath = join(source, "dsh.db");
  if (!existsSync(databasePath)) continue;
  const name = relative(repo, source),
    target = join(destination, "data", ...name.split(/[\\/]/));
  mkdirSync(target, { recursive: true });
  // Copy auxiliary files without copying SQLite journals as a database snapshot.
  for (const item of readdirSync(source, { withFileTypes: true })) {
    if (item.isSymbolicLink())
      throw new Error("Refusing a linked legacy data item");
    if (
      ["dsh.db", "dsh.db-wal", "dsh.db-shm", "daemon.lock"].includes(item.name)
    )
      continue;
    cpSync(join(source, item.name), join(target, item.name), {
      recursive: true,
      force: false,
      errorOnExist: true,
    });
  }
  const database = new DatabaseSync(databasePath, { readOnly: true });
  try {
    await backup(database, join(target, "dsh.db"));
  } finally {
    database.close();
  }
  const check = new DatabaseSync(join(target, "dsh.db"), { readOnly: true });
  try {
    if (check.prepare("PRAGMA integrity_check").get().integrity_check !== "ok")
      throw new Error("Backup integrity check failed");
  } finally {
    check.close();
  }
  manifest.data_snapshots.push({
    source: name,
    backup: relative(destination, target),
    sha256: sha256(join(target, "dsh.db")),
    bytes: statSync(join(target, "dsh.db")).size,
    integrity: "ok",
  });
}
for (const name of ["library", "source"])
  if (existsSync(join(repo, name)))
    manifest.retained_in_place.push({
      path: name,
      duplicated: false,
      reason:
        "Existing third-party runtimes and media materials are preserved in place; they are not legacy application databases.",
    });
writeFileSync(
  join(destination, "manifest.json"),
  JSON.stringify(manifest, null, 2) + "\n",
  { flag: "wx", mode: 0o600 },
);
privateDirectory(destination);
console.log(
  JSON.stringify(
    {
      backup_directory: destination,
      source_commit: manifest.source_commit,
      database_snapshots: manifest.data_snapshots.length,
      source_bundle_sha256: sha256(join(destination, manifest.code_bundle)),
      manifest: join(destination, "manifest.json"),
    },
    null,
    2,
  ),
);
