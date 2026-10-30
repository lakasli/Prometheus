import { parseArgs } from "node:util";
import { resolve, join, relative, isAbsolute, sep } from "node:path";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  lstatSync,
  readFileSync,
  copyFileSync,
  writeFileSync,
  unlinkSync,
  rmdirSync,
} from "node:fs";
import { createHash, randomUUID } from "node:crypto";
import { DatabaseSync, backup } from "node:sqlite";
import { privateDirectory } from "../../apps/server/dist/security.js";
const { values } = parseArgs({
  options: {
    "data-dir": { type: "string" },
    "source-commit": { type: "string" },
    "backup-root": { type: "string", default: ".local/prometheus/backups" },
  },
});
if (
  !values["data-dir"] ||
  !/^[0-9a-f]{40}$/.test(values["source-commit"] ?? "")
)
  throw new Error(
    "Supply --data-dir and the running full --source-commit after stopping that version.",
  );
const source = resolve(values["data-dir"]);
if (
  !existsSync(join(source, "prometheus.db")) ||
  !existsSync(join(source, "secrets", "master.key"))
)
  throw new Error("A complete Prometheus data directory is required.");
privateDirectory(source);
const target = resolve(
    values["backup-root"],
    `instance-${new Date().toISOString().replace(/[:.]/g, "-")}-${randomUUID()}`,
  ),
  rel = relative(source, target);
if (!rel || (!isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${sep}`)))
  throw new Error("Keep the backup outside the live data directory.");
const lock = join(source, ".prometheus.lock"),
  owner = join(lock, "owner.json");
mkdirSync(lock); // The same atomic lock as the CLI prevents startup during the snapshot.
writeFileSync(
  owner,
  JSON.stringify({
    pid: process.pid,
    operation: "backup",
    commit: values["source-commit"],
  }),
  { flag: "wx" },
);
try {
  privateDirectory(target);
  const files = [];
  function copy(directory, destination) {
    for (const entry of readdirSync(directory)) {
      if (
        entry === ".prometheus.lock" ||
        (["prometheus.db", "prometheus.db-wal", "prometheus.db-shm"].includes(
          entry,
        ) &&
          directory === source)
      )
        continue;
      const from = join(directory, entry),
        to = join(destination, entry),
        info = lstatSync(from);
      if (info.isSymbolicLink())
        throw new Error(
          "Backup refuses symbolic links; inspect the source before continuing.",
        );
      if (info.isDirectory()) {
        privateDirectory(to);
        copy(from, to);
      } else if (info.isFile()) {
        copyFileSync(from, to);
        files.push(relative(target, to).replaceAll("\\", "/"));
      } else throw new Error("Unsupported data entry.");
    }
  }
  copy(source, target);
  const database = new DatabaseSync(join(source, "prometheus.db"), {
    readOnly: true,
  });
  try {
    await backup(database, join(target, "prometheus.db"));
  } finally {
    database.close();
  }
  const snapshot = new DatabaseSync(join(target, "prometheus.db"), {
    readOnly: true,
  });
  let identity, schema;
  try {
    if (
      snapshot.prepare("PRAGMA integrity_check").get().integrity_check !== "ok"
    )
      throw new Error("Backup integrity check failed.");
    identity = snapshot
      .prepare("SELECT value FROM metadata WHERE key='assistant.identity'")
      .get().value;
    schema = snapshot
      .prepare("SELECT value FROM metadata WHERE key='prometheus.schema'")
      .get().value;
  } finally {
    snapshot.close();
  }
  files.push("prometheus.db");
  privateDirectory(target);
  const manifest = {
    created_at: new Date().toISOString(),
    source_directory: source,
    source_commit: values["source-commit"],
    schema,
    assistant_identity: identity,
    integrity_check: "ok",
    files: files.map((path) => ({
      path,
      sha256: createHash("sha256")
        .update(readFileSync(join(target, path)))
        .digest("hex"),
    })),
    restore:
      "Stop the runtime. Restore ALL files into a new private directory and select the matching Windows artifact. Never restore over the live instance.",
  };
  writeFileSync(
    join(target, "backup.json"),
    JSON.stringify(manifest, null, 2),
    { flag: "wx" },
  );
  console.log(
    JSON.stringify({
      backup: target,
      files: files.length,
      integrity_check: "ok",
      source_commit: values["source-commit"],
    }),
  );
} finally {
  unlinkSync(owner);
  rmdirSync(lock);
}
