import assert from "node:assert/strict";
import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { randomUUID, createHash } from "node:crypto";
import { release, arch } from "node:os";

// Explicit public/synthetic fixtures only. Never point these arguments at user media.
const [rootArg, audioArg, silkArg, videoArg] = process.argv.slice(2);
assert.ok(
  rootArg && audioArg && silkArg && videoArg,
  "Usage: node verify-media-worker.mjs RUNTIME PUBLIC_WAV SYNTHETIC_SILK SYNTHETIC_MP4",
);
const root = resolve(rootArg);
const { LocalWorker } = await import(
  pathToFileURL(join(root, "apps/server/dist/local-worker.js"))
);
const { privateDirectory } = await import(
  pathToFileURL(join(root, "apps/server/dist/security.js"))
);
const build = JSON.parse(
  readFileSync(join(root, "apps/server/dist/assets/build.json")),
);
if (build.commit !== "development") {
  const manifest = JSON.parse(
    readFileSync(join(root, "release.json"), "utf8").replace(/^\ufeff/, ""),
  );
  assert.equal(build.commit, manifest.commit);
  for (const f of manifest.files.filter((f) =>
    f.path.startsWith("apps/server/dist/"),
  ))
    assert.equal(
      createHash("sha256")
        .update(readFileSync(join(root, f.path)))
        .digest("hex"),
      f.sha256,
      f.path,
    );
}
const directory = privateDirectory(
  resolve(
    ".local/prometheus/media-worker-validation",
    build.commit,
    randomUUID(),
  ),
);
const worker = new LocalWorker(join(directory, "instance"));
const fixtures = {
  wav: readFileSync(audioArg),
  silk: readFileSync(silkArg),
  video: readFileSync(videoArg),
};
const report = {
  commit: build.commit,
  time: new Date().toISOString(),
  windows: release(),
  arch: arch(),
  fixtures: Object.fromEntries(
    Object.entries(fixtures).map(([k, b]) => [
      k,
      { bytes: b.length, sha256: createHash("sha256").update(b).digest("hex") },
    ]),
  ),
  general_execution_available: false,
  checks: [],
};
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};
function clean() {
  assert.equal(worker.running, false);
  assert.equal(readdirSync(join(worker.root, ".requests")).length, 0);
  assert.equal(
    readdirSync(worker.root).filter((n) => /^[a-f0-9-]{36}$/.test(n)).length,
    0,
  );
  assert.equal(existsSync(join(worker.root, ".lease")), false);
}
async function execute(name, kind, input, options, verify) {
  const result = await worker.run(kind, input, options);
  assert.equal(result.isolation.active_processes, 0);
  assert.ok(result.isolation.peak_memory_bytes <= 1536 * 1024 * 1024);
  assert.deepEqual(result.receipt.isolation, {
    app_container: 1,
    capabilities: 0,
    in_job: 1,
  });
  verify(result);
  clean();
  report.checks.push({
    name,
    status: "passed",
    receipt: result.receipt,
    isolation: result.isolation,
    elapsed_ms: result.elapsed_ms,
    files: [...result.files].map(([n, b]) => ({ name: n, bytes: b.length })),
  });
}
async function startedLease(task) {
  const end = Date.now() + 30000;
  while (Date.now() < end) {
    for (const id of readdirSync(join(worker.root, ".requests"))) {
      const path = join(worker.root, ".requests", id, "lease.json");
      if (existsSync(path)) {
        try {
          const lease = JSON.parse(readFileSync(path));
          if (lease.child && alive(lease.child)) return lease;
        } catch {}
      }
    }
    if (
      await Promise.race([
        task.then(() => true),
        new Promise((r) => setTimeout(() => r(false), 20)),
      ])
    )
      throw Error("Worker finished before lifecycle intervention");
  }
  throw Error("Worker did not start");
}
try {
  await execute(
    "actual-1536MiB-job-and-readonly-boundary",
    "selftest",
    Buffer.alloc(0),
    {},
    (r) => {
      for (const k of [
        "small_allocation_ok",
        "allocation_over_limit_denied",
        "runtime_write_denied",
        "input_write_denied",
        "other_job_read_denied",
      ])
        assert.equal(r.receipt.result[k], true, k);
    },
  );
  for (const encoding of ["wav", "mp3"])
    await execute(
      "Piper-" + encoding,
      "tts",
      Buffer.alloc(0),
      { text: "你好，今天阳光很好。", encoding },
      (r) => {
        assert.ok(r.files.get("speech." + encoding)?.length > 1000);
        if (encoding === "wav")
          assert.equal(
            r.files.get("speech.wav").toString("ascii", 0, 4),
            "RIFF",
          );
      },
    );
  for (const format of ["wav", "silk"])
    await execute("SenseVoice-" + format, "asr", fixtures[format], {}, (r) => {
      assert.ok(r.receipt.result.text.length > 2);
      assert.equal(r.receipt.result.confidence, null);
      assert.equal(r.receipt.result.timestamps, null);
      assert.ok(r.receipt.result.seconds > 1 && r.receipt.result.seconds < 60);
    });
  for (const transcribe_audio of [false, true])
    await execute(
      "41-second-video-audio-" + transcribe_audio,
      "video",
      fixtures.video,
      { transcribe_audio },
      (r) => {
        assert.deepEqual(r.receipt.result.frames_at, [0, 10, 20, 30, 40]);
        assert.ok(
          r.receipt.result.decoded_until >= 40 &&
            r.receipt.result.decoded_until < 42,
        );
        assert.equal(
          r.receipt.result.audio.state,
          transcribe_audio ? "transcribed" : "not_processed",
        );
        if (transcribe_audio) assert.ok(r.receipt.result.audio.text.length > 2);
        for (let n = 0; n < 5; n++)
          assert.equal(
            r.files.get(`frame-${n}.png`).subarray(1, 4).toString(),
            "PNG",
          );
      },
    );
  for (const mode of ["cancel", "launcher-death", "shutdown"]) {
    const controller = new AbortController();
    const task = worker.run("asr", fixtures.wav, {}, controller.signal).then(
      () => ({ code: "unexpected_success" }),
      (e) => ({ code: e.code }),
    );
    const lease = await startedLease(task);
    assert.equal(worker.running, true);
    const start = Date.now();
    if (mode === "launcher-death") process.kill(lease.launcher);
    else if (mode === "shutdown") await worker.stop();
    else controller.abort();
    const result = await task;
    assert.equal(
      result.code,
      mode === "launcher-death"
        ? "media_worker_failed"
        : "media_worker_cancelled",
    );
    assert.equal(alive(lease.child), false);
    assert.equal(alive(lease.launcher), false);
    clean();
    report.checks.push({
      name: "actual-media-" + mode,
      status: "passed",
      child_exited: true,
      launcher_exited: true,
      staged_and_request_directories_removed: true,
      elapsed_ms: Date.now() - start,
    });
  }
} catch (error) {
  report.failure = {
    code: error.code,
    message: error.message,
    diagnostic: worker.lastDiagnostic,
  };
  process.exitCode = 1;
} finally {
  await worker.stop();
  writeFileSync(
    join(directory, "report.json"),
    JSON.stringify(report, null, 2),
  );
  console.log(
    JSON.stringify({ report: join(directory, "report.json"), ...report }),
  );
}
