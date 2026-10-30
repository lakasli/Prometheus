import { parseArgs } from "node:util";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  unlinkSync,
  writeFileSync,
  rmdirSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { createServer } from "node:net";
import { workerProbe } from "./worker.ts";
import { bootstrapToken } from "./auth.ts";
import { privateDirectory } from "./security.ts";
import { build, startServer } from "./server.ts";

const usage = `Prometheus ${build.version} (${build.commit})

prometheus serve [--data-dir <directory>] [--listen 127.0.0.1:8419] [--origin http://127.0.0.1:8419] [--web-dir apps/admin/dist]
prometheus bootstrap-token [--data-dir <directory>]
prometheus chat --message <text> --password-file <private-file> [--username admin] [--conversation <id>] [--url http://127.0.0.1:8419]
prometheus recover-lock [--data-dir <directory>]
prometheus worker-check [--data-dir <isolated-probe-directory>]

Default data: .local/prometheus/dev; historical DSH data is never opened.
Initialization tokens and passwords are not printed. Windows starts in the foreground; Ctrl+C exits.
`;
async function main() {
  const args = parseArgs({
    allowPositionals: true,
    options: {
      "data-dir": { type: "string" },
      listen: { type: "string" },
      origin: { type: "string" },
      "web-dir": { type: "string" },
      "secure-cookie": { type: "boolean", default: false },
      "allow-loopback-model": { type: "boolean", default: false },
      help: { type: "boolean", default: false },
      version: { type: "boolean", default: false },
      message: { type: "string" },
      "password-file": { type: "string" },
      username: { type: "string" },
      conversation: { type: "string" },
      url: { type: "string" },
    },
  });
  if (args.values.version) {
    console.log(`Prometheus ${build.version} ${build.commit}`);
    return;
  }
  if (args.values.help || !args.positionals.length) {
    console.log(usage);
    return;
  }
  const command = args.positionals[0];
  const dataDir = resolve(
    args.values["data-dir"] ??
      process.env.PROMETHEUS_DATA_DIR ??
      ".local/prometheus/dev",
  );
  if (command === "worker-check") {
    const listener = createServer((socket) => socket.destroy());
    await new Promise<void>((ready, reject) => {
      listener.once("error", reject);
      listener.listen(0, "127.0.0.1", ready);
    });
    try {
      const address = listener.address();
      if (!address || typeof address === "string")
        throw new Error("Probe listener unavailable");
      const result = await workerProbe({
        directory: join(dataDir, "worker-probe"),
        port: address.port,
      });
      console.log(
        JSON.stringify({
          ...result,
          time: new Date().toISOString(),
          commit: build.commit,
        }),
      );
      if (result.status !== "passed") process.exitCode = 2;
    } finally {
      await new Promise<void>((closed) => listener.close(() => closed()));
    }
    return;
  }
  if (command === "bootstrap-token") {
    console.log(
      `初始化凭证文件：${bootstrapToken(dataDir)}（15分钟有效；在本机受保护终端读取，不要写入日志）。`,
    );
    return;
  }
  if (command === "chat") {
    const passwordFile = args.values["password-file"];
    if (!passwordFile || !args.values.message)
      throw new Error("chat requires --message and --password-file");
    const base = new URL(args.values.url ?? "http://127.0.0.1:8419");
    if (
      base.username ||
      base.password ||
      base.pathname !== "/" ||
      base.search ||
      base.hash ||
      (base.protocol !== "https:" &&
        !(
          base.protocol === "http:" &&
          ["127.0.0.1", "[::1]", "localhost"].includes(base.hostname)
        ))
    )
      throw new Error("Use a trusted HTTPS or loopback URL");
    const password = readFileSync(passwordFile, "utf8").replace(/[\r\n]+$/, "");
    let cookie = "",
      csrf = "";
    const call = async (path: string, method = "GET", input?: unknown) => {
      const response = await fetch(`${base.origin}/api/admin/v1${path}`, {
        method,
        redirect: "error",
        headers: {
          origin: base.origin,
          "content-type": "application/json",
          cookie,
          "x-csrf-token": csrf,
        },
        body: input === undefined ? undefined : JSON.stringify(input),
        signal: AbortSignal.timeout(150000),
      });
      const data = (await response.json()) as Record<string, unknown>;
      if (!response.ok)
        throw new Error(`Prometheus request failed (${response.status})`);
      if (response.headers.get("set-cookie"))
        cookie = response.headers.get("set-cookie")!.split(";")[0]!;
      if (typeof data.csrf_token === "string") csrf = data.csrf_token;
      return data;
    };
    await call("/auth/login", "POST", {
      username: args.values.username ?? "admin",
      password,
    });
    try {
      const conversation =
        args.values.conversation ??
        String(
          (await call("/conversations", "POST", { title: "终端对话" })).id,
        );
      const run = await call(
        `/conversations/${encodeURIComponent(conversation)}/messages`,
        "POST",
        { content: args.values.message, client_message_id: randomUUID() },
      );
      const deadline = Date.now() + 150000;
      while (Date.now() < deadline) {
        const status = await call(`/runs/${String(run.run_id)}`);
        if (status.status === "completed") {
          console.log(String(status.response));
          return;
        }
        if (!["queued", "running"].includes(String(status.status)))
          throw new Error(
            `Run ${String(status.status)}: ${String(status.error)}`,
          );
        await new Promise((resolve) => setTimeout(resolve, 250));
      }
      throw new Error("Run did not finish within the CLI deadline");
    } finally {
      await call("/auth/logout", "POST", {});
    }
  }
  if (command !== "serve" && command !== "recover-lock")
    throw new Error("Unknown command; use --help");
  privateDirectory(dataDir);
  const lock = join(dataDir, ".prometheus.lock"),
    ownerPath = join(lock, "owner.json");
  if (command === "recover-lock") {
    if (!existsSync(lock)) {
      console.log("No data-directory lock exists.");
      return;
    }
    const owner = JSON.parse(readFileSync(ownerPath, "utf8")) as {
      pid: number;
    };
    if (!Number.isSafeInteger(owner.pid) || owner.pid < 1)
      throw new Error("Invalid lock owner; inspect manually");
    try {
      process.kill(owner.pid, 0);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      unlinkSync(ownerPath);
      rmdirSync(lock);
      console.log("Removed lock belonging to a confirmed exited process.");
      return;
    }
    throw new Error("Lock owner is still alive; stop that process first");
  }
  try {
    mkdirSync(lock);
  } catch {
    throw new Error(
      "Data directory is already locked; use recover-lock only after the previous process exits",
    );
  }
  writeFileSync(
    ownerPath,
    JSON.stringify({
      pid: process.pid,
      started_at: new Date().toISOString(),
      commit: build.commit,
    }),
    { flag: "wx", mode: 0o600 },
  );
  const release = () => {
    unlinkSync(ownerPath);
    rmdirSync(lock);
  };
  const listen =
    args.values.listen ?? process.env.PROMETHEUS_LISTEN ?? "127.0.0.1:8419";
  const match = /^(127\.0\.0\.1|\[::1\]):([0-9]+)$/.exec(listen);
  if (!match) {
    release();
    throw new Error("Listen must use 127.0.0.1:<port> or [::1]:<port>");
  }
  try {
    const app = await startServer({
      dataDir,
      webDir:
        args.values["web-dir"] ??
        process.env.PROMETHEUS_WEB_DIR ??
        "apps/admin/dist",
      host: match[1]!.replace(/^\[|\]$/g, ""),
      port: Number(match[2]),
      origin:
        args.values.origin ??
        process.env.PROMETHEUS_ORIGIN ??
        `http://${listen}`,
      secureCookie: args.values["secure-cookie"]!,
      allowLoopbackModel: args.values["allow-loopback-model"],
    });
    console.log(
      JSON.stringify({
        event: "ready",
        ...build,
        time: new Date().toISOString(),
        pid: process.pid,
        address: listen,
        data_directory: dataDir,
      }),
    );
    let stopping = false;
    const stop = async () => {
      if (stopping) return;
      stopping = true;
      await app.close();
      release();
      console.log("Prometheus stopped.");
    };
    process.once("SIGINT", () => {
      void stop().catch(() => {
        process.exitCode = 1;
      });
    });
    process.once("SIGTERM", () => {
      void stop().catch(() => {
        process.exitCode = 1;
      });
    });
  } catch (error) {
    release();
    throw error;
  }
}
main().catch((error) => {
  console.error(error instanceof Error ? error.message : "Prometheus failed");
  process.exitCode = 1;
});
