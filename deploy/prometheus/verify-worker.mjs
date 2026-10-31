import assert from "node:assert/strict";
import { createServer, createConnection } from "node:net";
import { randomUUID, createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { release, arch, cpus } from "node:os";

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
let accepted = 0;
const server = createServer((socket) => {
  accepted++;
  socket.destroy();
});
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
  await new Promise((ready, reject) => {
    const socket = createConnection({ host: "127.0.0.1", port }, () => {
      socket.destroy();
      ready();
    });
    socket.on("error", reject);
  });
  assert.equal(accepted, 1);
  accepted = 0;
  const proof = await workerProbe({ directory, port });
  assert.equal(
    accepted,
    0,
    "An isolated connection reached the listening host",
  );
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
      assert.equal(result.status, "passed");
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
    assert.equal(result.status, "passed");
    const cpuStart = Date.now(),
      cpu = await workerProbe({
        directory,
        port,
        mode: "cpu",
        timeoutMs: 2000,
      }),
      cpuFinish = cpu.events.find((e) => e.event === "finished"),
      seconds = (Date.now() - cpuStart) / 1000;
    assert.equal(cpu.status, "passed");
    assert.ok(Number(cpuFinish.cpu_100ns) > 1000000);
    assert.ok(
      Number(cpuFinish.cpu_100ns) / 10000000 < seconds * cpus().length * 0.32,
    );
    report.checks.push({
      name: "saturated-cpu-cap",
      wall_seconds: seconds,
      ...cpu,
    });
    let killed = 0;
    const death = await workerProbe({
      directory,
      port,
      mode: "hang",
      onStarted(child, launcher) {
        killed = child;
        process.kill(launcher);
      },
    });
    assert.ok(killed);
    assert.throws(() => process.kill(killed, 0));
    assert.ok(
      !death.events.some(
        (e) =>
          e.stage === "child_still_running" || e.stage === "cleanup_failed",
      ),
    );
    report.checks.push({
      name: "launcher-death-kills-child-and-cleans",
      status: "passed",
      events: death.events,
    });
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
