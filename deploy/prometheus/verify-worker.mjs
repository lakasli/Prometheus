import assert from "node:assert/strict";
import { createServer } from "node:net";
import { randomUUID, createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { release, arch } from "node:os";

const root = resolve(process.argv[2] ?? ".");
const { workerProbe } = await import(
  pathToFileURL(join(root, "apps/server/dist/worker.js"))
);
const { privateDirectory } = await import(
  pathToFileURL(join(root, "apps/server/dist/security.js"))
);
const build = JSON.parse(
  readFileSync(join(root, "apps/server/dist/assets/build.json"), "utf8"),
);
if (build.commit !== "development") {
  const manifest = JSON.parse(
    readFileSync(join(root, "release.json"), "utf8").replace(/^\ufeff/, ""),
  );
  assert.equal(manifest.commit, build.commit);
  for (const name of ["worker-probe.exe", "probe-child.exe"]) {
    const path = `apps/server/dist/native/${name}`;
    const file = manifest.files.find((entry) => entry.path === path);
    assert.ok(file, `Missing fixed probe: ${name}`);
    assert.equal(
      createHash("sha256")
        .update(readFileSync(join(root, path)))
        .digest("hex"),
      file.sha256,
    );
  }
}
const directory = privateDirectory(
  resolve(
    `.local/prometheus/worker-validation/${build.commit}/${randomUUID()}`,
  ),
);
const server = createServer((socket) => socket.destroy());
await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
const report = {
  commit: build.commit,
  time: new Date().toISOString(),
  windows: release(),
  arch: arch(),
  execution_available: false,
  checks: [],
};
try {
  const port = server.address().port;
  const proof = await workerProbe({ directory, port });
  report.checks.push({ name: "empty-capability-permissions", ...proof });
  if (proof.status === "passed") {
    for (const [mode, reason] of [
      ["hang", "timeout"],
      ["flood", "output_limit"],
      ["exit", "exited"],
    ]) {
      const result = await workerProbe({
        directory,
        port,
        mode,
        timeoutMs: 500,
      });
      const finish = result.events.find((event) => event.event === "finished");
      assert.equal(finish?.reason, reason);
      assert.equal(finish.active_processes, 0);
      if (mode === "exit") assert.equal(finish.exit_code, 7);
      report.checks.push({ name: mode, ...result });
    }
    const abort = new AbortController();
    const result = await workerProbe({
      directory,
      port,
      mode: "hang",
      signal: abort.signal,
      onStarted() {
        abort.abort();
      },
    });
    const finish = result.events.find((event) => event.event === "finished");
    assert.equal(finish?.reason, "cancelled");
    assert.equal(finish.active_processes, 0);
    report.checks.push({ name: "cancel", ...result });
  } else {
    report.unexecuted = [
      "timeout",
      "memory-and-cpu-enforcement",
      "output-limit",
      "cancel",
      "launcher-death",
      "process-tree-exit",
    ];
    process.exitCode = 2;
  }
} finally {
  await new Promise((closed) => server.close(closed));
  const file = join(directory, "report.json");
  writeFileSync(file, JSON.stringify(report, null, 2));
  console.log(JSON.stringify({ report: file, ...report }));
}
