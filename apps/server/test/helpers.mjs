import { createServer } from "node:net";
import { resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
const sourceRoot = fileURLToPath(new URL("../../../", import.meta.url));
const runtimeRoot = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? sourceRoot);
const { startServer } = await import(
  pathToFileURL(resolve(runtimeRoot, "apps/server/dist/server.js"))
);
const { bootstrapToken } = await import(
  pathToFileURL(resolve(runtimeRoot, "apps/server/dist/auth.js"))
);

export async function port() {
  const socket = createServer();
  await new Promise((resolve, reject) => {
    socket.once("error", reject);
    socket.listen(0, "127.0.0.1", resolve);
  });
  const value = socket.address().port;
  await new Promise((resolve) => socket.close(resolve));
  return value;
}
export async function fixture(options = {}) {
  const root = fileURLToPath(new URL("../../../", import.meta.url));
  const number = await port(),
    origin = `http://127.0.0.1:${number}`;
  const dataDir = resolve(
    root,
    options.dataDir ?? `.local/prometheus/tests/${randomUUID()}`,
  );
  const app = await startServer({
    dataDir,
    webDir: resolve(runtimeRoot, "apps/admin/dist"),
    host: "127.0.0.1",
    port: number,
    origin,
    secureCookie: false,
    allowLoopbackModel: true,
    ...options,
  });
  let cookie = "",
    csrf = "";
  async function call(
    path,
    method = "GET",
    body,
    { auth = true, headers = {} } = {},
  ) {
    const response = await fetch(`${origin}/api/admin/v1${path}`, {
      method,
      redirect: "manual",
      headers: {
        origin,
        ...(body === undefined ? {} : { "content-type": "application/json" }),
        ...(auth ? { cookie, "x-csrf-token": csrf } : {}),
        ...headers,
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(10000),
    });
    const data = await response.json();
    if (response.headers.get("set-cookie"))
      cookie = response.headers.get("set-cookie").split(";")[0];
    if (data.csrf_token) csrf = data.csrf_token;
    return { status: response.status, data, headers: response.headers };
  }
  async function bootstrap() {
    const path = bootstrapToken(dataDir),
      token = JSON.parse(readFileSync(path, "utf8")).token;
    const response = await call("/auth/bootstrap", "POST", {
      username: "tester",
      password: "Synthetic-password-2026!",
      bootstrap_token: token,
    });
    if (response.status !== 201)
      throw new Error(
        `Fixture bootstrap failed: ${response.status}: ${JSON.stringify(response.data)}`,
      );
    return response;
  }
  async function configure(url) {
    const secret = await call("/secrets", "POST", {
      kind: "model_api_key",
      value: "synthetic-provider-credential",
    });
    const state = await call("/config");
    const config = state.data.config;
    config.model = {
      base_url: url,
      model: "synthetic-model",
      credential_ref: secret.data.id,
    };
    const saved = await call(
      "/config",
      "PUT",
      { config },
      { headers: { "if-match": `"${state.data.revision}"` } },
    );
    if (saved.status !== 200)
      throw new Error(
        `Fixture model configuration failed: ${JSON.stringify(saved.data)}`,
      );
    return saved.data;
  }
  return {
    ...app,
    origin,
    dataDir,
    call,
    bootstrap,
    configure,
    get cookie() {
      return cookie;
    },
    get csrf() {
      return csrf;
    },
  };
}
export async function until(operation, predicate, ms = 5000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const value = await operation();
    if (predicate(value)) return value;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error("Test condition did not become true within deadline");
}
