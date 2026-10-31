import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  existsSync,
  lstatSync,
  realpathSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { privateDirectory } from "./security.ts";

export interface ProbeResult {
  execution_available: false;
  status: "passed" | "blocked";
  events: Record<string, unknown>[];
  launcher_exit_code: number | null;
}
let probeBusy = false;
/** Only the bundled, fixed prerequisite probe can be launched. No user code. */
export async function workerProbe(options: {
  directory: string;
  port: number;
  signal?: AbortSignal;
  mode?: "permissions" | "hang" | "exit" | "flood" | "cpu";
  timeoutMs?: number;
  onStarted?: (pid: number, launcher: number) => void;
}): Promise<ProbeResult> {
  const executable = fileURLToPath(
    new URL("./native/worker-probe.exe", import.meta.url),
  );
  if (process.platform !== "win32" || !existsSync(executable))
    return {
      execution_available: false,
      status: "blocked",
      events: [{ event: "blocked", stage: "native_probe_unavailable" }],
      launcher_exit_code: null,
    };
  const root = privateDirectory(resolve(options.directory));
  if (
    lstatSync(root).isSymbolicLink() ||
    realpathSync(root).toLowerCase() !== root.toLowerCase()
  )
    throw new Error("Worker probe root must not traverse symbolic links");
  if (
    !Number.isSafeInteger(options.port) ||
    options.port < 1 ||
    options.port > 65535
  )
    throw new Error("A listening loopback probe port is required");
  const timeout = options.timeoutMs ?? 10000;
  if (!Number.isSafeInteger(timeout) || timeout < 100 || timeout > 15000)
    throw new Error("Worker probe timeout must be 100–15000 ms");
  const sentinel = join(root, "private-sentinel.txt");
  if (!existsSync(sentinel))
    writeFileSync(sentinel, "synthetic-private-sentinel", { flag: "wx" });
  // privateDirectory verifies existing files too. Never pass any real credential.
  privateDirectory(root);
  const sourceSentinel = fileURLToPath(new URL("./server.js", import.meta.url));
  const id = randomUUID();
  const events: Record<string, unknown>[] = [];
  if (probeBusy)
    throw new Error("Only one worker prerequisite probe may run at a time");
  probeBusy = true;
  return new Promise<ProbeResult>((resolveResult) => {
    const task = spawn(
      executable,
      [
        "--probe",
        root,
        id,
        options.mode ?? "permissions",
        sentinel,
        String(options.port),
        sourceSentinel,
        String(timeout),
      ],
      {
        windowsHide: true,
        shell: false,
        cwd: dirname(executable),
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
          PROMETHEUS_PROBE_SECRET: "synthetic-must-not-reach-child",
        },
      },
    );
    let childPid = 0;
    let pending = "",
      bytes = 0,
      invalid = false;
    const cancel = () => {
      if (!task.stdin.destroyed) task.stdin.end("cancel\n");
    };
    task.stdin.on("error", () => {});
    const timer = setTimeout(cancel, timeout + 5000);
    // Killing the trusted launcher closes its non-inherited Job Object handle.
    // The kernel kills the assigned child even if a native API stopped responding.
    const hardDeadline = setTimeout(() => task.kill(), timeout + 10000);
    options.signal?.addEventListener("abort", cancel, { once: true });
    if (options.signal?.aborted) cancel();
    task.stdout.setEncoding("utf8");
    task.stdout.on("data", (chunk: string) => {
      bytes += Buffer.byteLength(chunk);
      if (bytes > 32768) {
        invalid = true;
        cancel();
        return;
      }
      pending += chunk;
      let index: number;
      while ((index = pending.indexOf("\n")) >= 0) {
        const line = pending.slice(0, index).trim();
        pending = pending.slice(index + 1);
        if (!line) continue;
        try {
          const event = JSON.parse(line) as Record<string, unknown>;
          events.push(event);
          if (
            event.event === "started" &&
            Number.isSafeInteger(event.pid) &&
            task.pid
          ) {
            childPid = Number(event.pid);
            options.onStarted?.(childPid, task.pid);
          }
        } catch {
          invalid = true;
          events.push({ event: "blocked", stage: "invalid_probe_frame" });
          cancel();
        }
      }
    });
    task.stderr.on("data", () => {
      invalid = true;
      cancel();
    });
    task.on("error", (error: NodeJS.ErrnoException) => {
      events.push({
        event: "blocked",
        stage: "launch",
        code: error.code ?? "unknown",
      });
    });
    task.once("close", async (code) => {
      clearTimeout(timer);
      clearTimeout(hardDeadline);
      options.signal?.removeEventListener("abort", cancel);
      const finish = events.find((event) => event.event === "finished");
      const probe = finish?.probe as Record<string, unknown> | undefined;
      const lifecycle =
        !invalid &&
        !events.some((event) => event.event === "blocked") &&
        !pending.trim() &&
        code === 0 &&
        finish &&
        finish.active_processes === 0 &&
        Number(finish.peak_memory_bytes) <= 64 * 1024 * 1024;
      const permissions =
        finish?.reason === "exited" &&
        finish.exit_code === 0 &&
        probe &&
        probe.app_container === 1 &&
        probe.capabilities === 0 &&
        Number(probe.integrity_rid) <= 4096 &&
        [
          "in_job",
          "private_read_denied",
          "private_write_denied",
          "source_read_denied",
          "parent_access_denied",
          "environment_absent",
          "memory_denied",
          "child_process_denied",
        ].every((key) => probe[key] === 1) &&
        [10013, 10060].includes(Number(probe.loopback_error)) &&
        probe.public_network_error === 10013;
      // Verify the only external write target remained unchanged.
      let unchanged = false;
      try {
        unchanged =
          readFileSync(sentinel, "utf8") === "synthetic-private-sentinel";
      } catch {
        /* fail closed */
      }
      let childExited = !childPid;
      for (let n = 0; childPid && n < 200; n++) {
        try {
          process.kill(childPid, 0);
        } catch {
          childExited = true;
          break;
        }
        await new Promise((r) => setTimeout(r, 25));
      }
      if (!childExited)
        events.push({ event: "blocked", stage: "child_still_running" });
      if (childExited && existsSync(join(root, id))) {
        await new Promise<void>((done) => {
          const cleanup = spawn(executable, ["--cleanup-probe", root, id], {
            windowsHide: true,
            stdio: "ignore",
          });
          const timer = setTimeout(() => cleanup.kill(), 10000);
          cleanup.once("error", () =>
            events.push({ event: "blocked", stage: "cleanup_launch" }),
          );
          cleanup.once("close", (c) => {
            clearTimeout(timer);
            if (c !== 0 || existsSync(join(root, id)))
              events.push({ event: "blocked", stage: "cleanup_failed" });
            done();
          });
        });
      }
      const mode = options.mode ?? "permissions",
        expected = options.signal?.aborted
          ? finish?.reason === "cancelled" && finish.exit_code === 93
          : mode === "permissions"
            ? permissions
            : mode === "exit"
              ? finish?.reason === "exited" && finish.exit_code === 7
              : finish?.reason ===
                  (mode === "flood" ? "output_limit" : "timeout") &&
                finish.exit_code === 93;
      probeBusy = false;
      resolveResult({
        execution_available: false,
        status:
          lifecycle &&
          expected &&
          unchanged &&
          childExited &&
          !events.some((e) => e.event === "blocked")
            ? "passed"
            : "blocked",
        events,
        launcher_exit_code: code,
      });
    });
  }).catch((error: NodeJS.ErrnoException) => {
    probeBusy = false;
    return {
      execution_available: false,
      status: "blocked",
      events: [
        { event: "blocked", stage: "launch", code: error.code ?? "unknown" },
      ],
      launcher_exit_code: null,
    };
  });
}
