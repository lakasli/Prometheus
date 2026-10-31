import { spawn } from "node:child_process";
import {
  existsSync,
  lstatSync,
  readFileSync,
  writeFileSync,
  mkdirSync,
  readdirSync,
  realpathSync,
  rmSync,
  unlinkSync,
  rmdirSync,
  statfsSync,
} from "node:fs";
import { dirname, isAbsolute, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
import { createServer, createConnection } from "node:net";
import { ApiError } from "./errors.ts";
import { hash, privateDirectory } from "./security.ts";
import { workerProbe } from "./worker.ts";

type Json = Record<string, any>;
export type LocalKind = "tts" | "asr" | "video" | "selftest";
export interface LocalResult {
  receipt: Json;
  files: Map<string, Buffer>;
  isolation: Json;
  elapsed_ms: number;
}
export type LocalTestRunner = (
  kind: LocalKind,
  input: Buffer,
  options: Json,
  signal: AbortSignal,
) => Promise<LocalResult>;
const error = (code: string) =>
  new ApiError(503, code, "本地媒体处理未完成，未读取未经确认的结果。");
const alive = (pid: number) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};
const uuid = (id: string) =>
  /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(id);
let busy = false;
export class LocalWorker {
  lastDiagnostic: Json | undefined;
  readonly bundle = fileURLToPath(new URL("./native/media/", import.meta.url));
  readonly executable = fileURLToPath(
    new URL("./native/worker-probe.exe", import.meta.url),
  );
  readonly root: string;
  private ready = false;
  private preparing?: Promise<void>;
  private stopping = false;
  private active?: { abort: AbortController; task: Promise<LocalResult> };
  constructor(
    dataDir: string,
    private testRunner?: LocalTestRunner,
  ) {
    const cache = process.env.LOCALAPPDATA;
    if (process.platform === "win32" && (!cache || !isAbsolute(cache)))
      throw error("worker_directory_invalid");
    // Python's bootstrap and native codecs still have path-length limits even
    // with extended Win32 paths. Keep the job namespace short and independent
    // of arbitrarily deep data/recovery directories; never relax OS policy.
    this.root =
      process.platform === "win32"
        ? resolve(
            cache!,
            "Prometheus",
            "workers",
            hash(resolve(dataDir)).slice(0, 32),
          )
        : resolve(
            dirname(dataDir),
            ".prometheus-workers-" + hash(resolve(dataDir)).slice(0, 32),
          );
    if (process.platform === "win32" && this.root.length > 120)
      throw error("worker_path_too_long");
  }
  get installed() {
    return (
      !!this.testRunner ||
      (process.platform === "win32" &&
        existsSync(join(this.bundle, "manifest.json")) &&
        existsSync(this.executable))
    );
  }
  get verified() {
    return this.ready && !this.stopping;
  }
  get running() {
    return !!this.active;
  }
  status() {
    return {
      installed: this.installed,
      verified: this.verified,
      running: this.running,
      network: false,
      max_concurrent: 1,
      memory_bytes: 1536 * 1024 * 1024,
      cpu_rate: 2000,
      timeout_seconds: 60,
      output_bytes: 8 * 1024 * 1024,
      engines: {
        asr: "sherpa-onnx/SenseVoice-int8",
        tts: "Piper/huayan",
        video: "PyAV/FFmpeg",
      },
      ...(this.testRunner ? { test_mode: true } : {}),
    };
  }
  private verifyFiles() {
    const expected = JSON.parse(
      readFileSync(
        new URL("./assets/media-runtime-lock.json", import.meta.url),
        "utf8",
      ),
    );
    const manifest = JSON.parse(
      readFileSync(join(this.bundle, "manifest.json"), "utf8"),
    );
    if (
      manifest.lock_sha256 !== hash(JSON.stringify(expected)) ||
      !Array.isArray(manifest.files) ||
      manifest.files.length > 10000
    )
      throw error("media_runtime_integrity");
    const paths = new Set<string>();
    for (const f of manifest.files) {
      if (
        typeof f.path !== "string" ||
        f.path.includes("\\") ||
        f.path.split("/").some((s: string) => !s || s === "." || s === "..") ||
        !/^[a-f\d]{64}$/.test(f.sha256)
      )
        throw error("media_runtime_integrity");
      const path = resolve(this.bundle, f.path);
      if (
        !path.startsWith(resolve(this.bundle) + sep) ||
        paths.has(f.path) ||
        lstatSync(path).isSymbolicLink() ||
        realpathSync(path).toLowerCase() !== path.toLowerCase() ||
        hash(readFileSync(path)) !== f.sha256
      )
        throw error("media_runtime_integrity");
      paths.add(f.path);
    }
    const walk = (dir: string) => {
      for (const e of readdirSync(dir, { withFileTypes: true })) {
        const path = join(dir, e.name);
        if (e.isSymbolicLink()) throw error("media_runtime_integrity");
        if (e.isDirectory()) walk(path);
        else {
          const relative = path
            .slice(resolve(this.bundle).length + 1)
            .split(sep)
            .join("/");
          if (relative !== "manifest.json" && !paths.has(relative))
            throw error("media_runtime_integrity");
        }
      }
    };
    walk(resolve(this.bundle));
    for (const required of [
      "pythonw.exe",
      "media-worker.py",
      "models/huayan.onnx",
      "models/huayan.onnx.json",
      "models/model.int8.onnx",
      "models/tokens.txt",
    ])
      if (!paths.has(required)) throw error("media_runtime_integrity");
    if (
      hash(readFileSync(join(this.bundle, "media-worker.py"))) !==
      hash(readFileSync(new URL("./assets/media-worker.py", import.meta.url)))
    )
      throw error("media_runtime_integrity");
  }
  private requestPath(id: string) {
    if (!uuid(id)) throw error("worker_directory_invalid");
    const path = resolve(this.root, ".requests", id);
    if (!path.startsWith(resolve(this.root, ".requests") + sep))
      throw error("worker_directory_invalid");
    return path;
  }
  private removeRequest(id: string) {
    const path = this.requestPath(id);
    if (existsSync(path)) {
      if (
        lstatSync(path).isSymbolicLink() ||
        realpathSync(path).toLowerCase() !== path.toLowerCase()
      )
        throw error("worker_directory_invalid");
      rmSync(path, { recursive: true });
    }
  }
  private async cleanup(id: string) {
    const target = resolve(this.root, id);
    if (!uuid(id) || !target.startsWith(this.root + sep))
      throw error("worker_directory_invalid");
    await new Promise<void>((done, reject) => {
      const c = spawn(this.executable, ["--cleanup-media", this.root, id], {
        windowsHide: true,
        shell: false,
        stdio: ["ignore", "pipe", "ignore"],
      });
      let output = "";
      c.stdout.on("data", (b) => {
        if (output.length < 8192) output += b;
      });
      const timer = setTimeout(() => c.kill(), 15000);
      c.once("error", () => reject(error("worker_cleanup_failed")));
      c.once("close", (code) => {
        clearTimeout(timer);
        if (code !== 0 || output.includes("blocked") || existsSync(target))
          reject(error("worker_cleanup_failed"));
        else done();
      });
    });
  }
  private async recover() {
    privateDirectory(this.root, false);
    const disk = statfsSync(this.root, { bigint: true });
    if (disk.bavail * disk.bsize < 1024n * 1024n * 1024n)
      throw error("worker_storage_capacity");
    const marker = join(this.root, "owner.json"),
      identity = hash(this.root);
    if (existsSync(marker)) {
      if (JSON.parse(readFileSync(marker, "utf8")).identity !== identity)
        throw error("worker_directory_invalid");
    } else {
      if (readdirSync(this.root).length)
        throw error("worker_directory_invalid");
      writeFileSync(marker, JSON.stringify({ identity }), { flag: "wx" });
    }
    const lock = join(this.root, ".lease");
    if (existsSync(lock)) {
      const lease = JSON.parse(readFileSync(join(lock, "owner.json"), "utf8"));
      if (!Number.isSafeInteger(lease.pid) || alive(lease.pid))
        throw error("media_worker_busy");
      unlinkSync(join(lock, "owner.json"));
      rmdirSync(lock);
    }
    const requests = join(this.root, ".requests");
    if (existsSync(requests))
      for (const id of readdirSync(requests)) {
        const leaseFile = join(this.requestPath(id), "lease.json");
        if (existsSync(leaseFile)) {
          const lease = JSON.parse(readFileSync(leaseFile, "utf8"));
          for (const pid of [lease.launcher, lease.child])
            if (Number.isSafeInteger(pid) && pid > 0) {
              for (let n = 0; alive(pid) && n < 400; n++)
                await new Promise((r) => setTimeout(r, 25));
              if (alive(pid)) throw error("worker_previous_process_active");
            }
        }
        await this.cleanup(id);
        this.removeRequest(id);
      }
    for (const e of readdirSync(this.root, { withFileTypes: true })) {
      if (["owner.json", ".requests", ".lease", "proof"].includes(e.name))
        continue;
      if (!e.isDirectory() || !uuid(e.name))
        throw error("worker_directory_invalid");
      await this.cleanup(e.name);
    }
    privateDirectory(this.root);
  }
  async ensureReady() {
    if (this.ready && !this.stopping) return;
    if (!this.installed || this.stopping)
      throw error("local_media_unavailable");
    if (this.preparing) return this.preparing;
    this.preparing = (async () => {
      if (this.testRunner) {
        this.ready = true;
        return;
      }
      this.verifyFiles();
      await this.recover();
      let accepted = 0;
      const server = createServer((s) => {
        accepted++;
        s.destroy();
      });
      await new Promise<void>((r, j) => {
        server.once("error", j);
        server.listen(0, "127.0.0.1", r);
      });
      try {
        const port = (server.address() as { port: number }).port;
        await new Promise<void>((r, j) => {
          const s = createConnection({ host: "127.0.0.1", port }, () => {
            s.destroy();
            r();
          });
          s.once("error", j);
        });
        if (accepted !== 1) throw error("worker_probe_failed");
        accepted = 0;
        const directory = privateDirectory(join(this.root, "proof")),
          proof = await workerProbe({ directory, port });
        if (proof.status !== "passed" || accepted)
          throw error("worker_probe_failed");
      } finally {
        await new Promise<void>((r) => server.close(() => r()));
      }
      this.ready = true;
    })();
    try {
      await this.preparing;
    } finally {
      this.preparing = undefined;
    }
  }
  async run(
    kind: LocalKind,
    input: Buffer,
    options: Json = {},
    signal = new AbortController().signal,
  ): Promise<LocalResult> {
    if (busy || this.active) throw error("media_worker_busy");
    if (
      input.length > 20 * 1024 * 1024 ||
      !["tts", "asr", "video", "selftest"].includes(kind)
    )
      throw error("media_worker_input");
    await this.ensureReady();
    if (busy || this.active || signal.aborted || this.stopping)
      throw error("media_worker_cancelled");
    busy = true;
    const abort = new AbortController(),
      combined = AbortSignal.any([signal, abort.signal]);
    const task = this.testRunner
      ? this.testRunner(kind, input, options, combined)
      : this.execute(kind, input, options, combined);
    this.active = { abort, task };
    try {
      return await task;
    } finally {
      this.active = undefined;
      busy = false;
    }
  }
  private async execute(
    kind: LocalKind,
    input: Buffer,
    options: Json,
    signal: AbortSignal,
  ) {
    const started = Date.now(),
      id = randomUUID(),
      directory = this.requestPath(id),
      lock = join(this.root, ".lease");
    this.verifyFiles();
    mkdirSync(lock);
    let childPid = 0,
      launcher = 0,
      childExited = true;
    try {
      writeFileSync(
        join(lock, "owner.json"),
        JSON.stringify({ pid: process.pid }),
        { flag: "wx" },
      );
      privateDirectory(directory);
      writeFileSync(
        join(directory, "request.json"),
        JSON.stringify({ ...options, kind }),
        { flag: "wx" },
      );
      writeFileSync(join(directory, "input.bin"), input, { flag: "wx" });
      const events = await new Promise<Json[]>((done, reject) => {
        const c = spawn(
          this.executable,
          [
            "--media",
            this.root,
            id,
            kind,
            resolve(this.bundle),
            "1",
            "unused",
            "60000",
          ],
          {
            windowsHide: true,
            shell: false,
            stdio: ["pipe", "pipe", "pipe"],
            env: {
              SystemRoot: process.env.SystemRoot,
              WINDIR: process.env.WINDIR,
              USERPROFILE: process.env.USERPROFILE,
              LOCALAPPDATA: process.env.LOCALAPPDATA,
              APPDATA: process.env.APPDATA,
              ALLUSERSPROFILE: process.env.ALLUSERSPROFILE,
              ProgramData: process.env.ProgramData,
              SystemDrive: process.env.SystemDrive,
              TEMP: process.env.TEMP,
              TMP: process.env.TMP,
            },
          },
        );
        launcher = c.pid ?? 0;
        writeFileSync(
          join(directory, "lease.json"),
          JSON.stringify({ launcher }),
        );
        const frames: Json[] = [];
        let pending = "",
          bytes = 0,
          invalid = false;
        const cancel = () => {
          if (!c.stdin.destroyed) c.stdin.end("cancel\n");
        };
        c.stdin.on("error", () => {});
        signal.addEventListener("abort", cancel, { once: true });
        if (signal.aborted) cancel();
        const deadline = setTimeout(cancel, 75000),
          hard = setTimeout(() => c.kill(), 90000);
        c.stdout.on("data", (b: Buffer) => {
          bytes += b.length;
          if (bytes > 32768) {
            invalid = true;
            cancel();
            return;
          }
          pending += b.toString();
          for (let at; (at = pending.indexOf("\n")) >= 0;) {
            const line = pending.slice(0, at).trim();
            pending = pending.slice(at + 1);
            if (!line) continue;
            try {
              const f = JSON.parse(line);
              frames.push(f);
              if (f.event === "started" && Number.isSafeInteger(f.pid)) {
                childPid = f.pid;
                writeFileSync(
                  join(directory, "lease.json"),
                  JSON.stringify({ launcher, child: childPid }),
                );
              }
            } catch {
              invalid = true;
              cancel();
            }
          }
        });
        c.stderr.on("data", () => {
          invalid = true;
          cancel();
        });
        c.once("error", () => {
          invalid = true;
        });
        c.once("close", (code) => {
          clearTimeout(deadline);
          clearTimeout(hard);
          signal.removeEventListener("abort", cancel);
          const finish = frames.find((f) => f.event === "finished");
          const started = frames.find((f) => f.event === "started");
          this.lastDiagnostic = {
            events: frames,
            launcher_exit_code: code,
            invalid,
            pending_bytes: Buffer.byteLength(pending),
          };
          if (
            code !== 0 ||
            invalid ||
            pending.trim() ||
            frames.some((f) => f.event === "blocked") ||
            frames.length !== 2 ||
            !started ||
            started.memory_bytes !== 1536 * 1024 * 1024 ||
            started.cpu_rate !== 2000 ||
            started.active_process_limit !== 1 ||
            !Number.isSafeInteger(started.pid) ||
            started.pid < 1 ||
            frames[0] !== started ||
            !finish ||
            finish.reason !== "exited" ||
            finish.exit_code !== 0 ||
            finish.active_processes !== 0 ||
            Number(finish.peak_memory_bytes) > 1536 * 1024 * 1024 ||
            signal.aborted
          )
            reject(
              error(
                signal.aborted
                  ? "media_worker_cancelled"
                  : "media_worker_failed",
              ),
            );
          else done(frames);
        });
      });
      const output = join(directory, "output"),
        files = new Map<string, Buffer>();
      let size = 0;
      for (const name of readdirSync(output)) {
        if (
          !/^(result\.json|speech\.(wav|mp3)|frame-[0-5]\.png)$/.test(name) ||
          lstatSync(join(output, name)).isSymbolicLink()
        )
          throw error("media_worker_output");
        const stat = lstatSync(join(output, name));
        if (!stat.isFile() || (size += stat.size) > 8 * 1024 * 1024)
          throw error("media_worker_output");
        files.set(name, readFileSync(join(output, name)));
      }
      const receipt = JSON.parse(
        files.get("result.json")?.toString("utf8") ?? "null",
      );
      if (
        receipt?.status !== "completed" ||
        receipt.kind !== kind ||
        receipt.isolation?.app_container !== 1 ||
        receipt.isolation?.capabilities !== 0 ||
        receipt.isolation?.in_job !== 1
      )
        throw error("media_engine_failed");
      return {
        receipt,
        files,
        isolation: events.find((f) => f.event === "finished")!,
        elapsed_ms: Date.now() - started,
      };
    } finally {
      for (let n = 0; childPid && alive(childPid) && n < 200; n++)
        await new Promise((r) => setTimeout(r, 25));
      childExited = !childPid || !alive(childPid);
      if (childExited) {
        try {
          await this.cleanup(id);
        } catch (e) {
          this.ready = false;
          throw e;
        }
        this.removeRequest(id);
        if (existsSync(join(lock, "owner.json")))
          unlinkSync(join(lock, "owner.json"));
        rmdirSync(lock);
      } else {
        this.ready = false;
        throw error("worker_process_not_exited");
      }
    }
  }
  async stop() {
    this.stopping = true;
    this.active?.abort.abort();
    await this.preparing?.catch(() => {});
    await this.active?.task.catch(() => {});
  }
}
