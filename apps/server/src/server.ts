import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { extname, join, relative, resolve, sep } from "node:path";
import { freemem, totalmem } from "node:os";
import { randomUUID } from "node:crypto";
import { Authentication } from "./auth.ts";
import {
  ApiError,
  bad,
  fields,
  integer,
  missing,
  revision,
  string,
  unavailable,
} from "./errors.ts";
import { validateConfig, validatePersonality } from "./config.ts";
import { complete } from "./model.ts";
import { kind } from "./records.ts";
import { Runtime } from "./runtime.ts";
import { checkHostOrigin, networkOptions, now } from "./security.ts";
import { Store } from "./store.ts";

export interface ServerOptions {
  dataDir: string;
  webDir: string;
  host: string;
  port: number;
  origin: string;
  secureCookie: boolean;
  allowLoopbackModel?: boolean;
}
export const build = JSON.parse(
  readFileSync(new URL("./assets/build.json", import.meta.url), "utf8"),
) as { name: string; version: string; commit: string };
export const capabilityStatus = {
  native_text: {
    available: true,
    description: "有界模型对话、私有会话、持久队列与取消",
  },
  memory: { available: true, description: "显式资料、共享确认、纠正与遗忘" },
  diary: {
    available: true,
    description: "人工日记、周期与来源引用；模型自动整理待迁移",
  },
  schedule: { available: true, description: "人工日程与进度；自主执行待迁移" },
  relationships: {
    available: true,
    description: "人工审核的人物和有向关系；好感规则待迁移",
  },
  skills: {
    available: true,
    description: "显式选择的文字方法；不授予执行权限",
  },
  qq: { available: false, description: "QQ 协议与投递迁移待完成" },
  weixin: { available: false, description: "微信扫码和投递迁移待完成" },
  speech: {
    available: false,
    description: "Breeze / Piper worker 的 Windows 隔离待验证",
  },
  vision: { available: false, description: "图片理解与媒体来源门禁待迁移" },
  rest: { available: false, description: "作息与多批整理待迁移" },
  plugins: {
    available: false,
    description: "独立 worker 权限、资源限制及退出验证待完成",
  },
  harness: { available: false, description: "Windows 隔离与权限桥待完成" },
};
const prefix = "/api/admin/v1";
function json(response: ServerResponse, value: unknown, status = 200) {
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
  });
  response.end(JSON.stringify(value));
}
async function body(request: IncomingMessage): Promise<unknown> {
  if (
    !/^application\/json(?:\s*;|$)/i.test(request.headers["content-type"] ?? "")
  )
    throw new ApiError(415, "json_required", "请使用 application/json");
  const length = Number(request.headers["content-length"] ?? 0);
  if (!Number.isSafeInteger(length) || length > 128 * 1024)
    throw new ApiError(413, "body_too_large", "请求正文过大");
  let size = 0;
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    size += Buffer.byteLength(chunk);
    if (size > 128 * 1024)
      throw new ApiError(413, "body_too_large", "请求正文过大");
    chunks.push(Buffer.from(chunk));
  }
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw bad("JSON 格式无效");
  }
}
export async function startServer(options: ServerOptions) {
  const network = networkOptions(
    options.host,
    options.port,
    options.origin,
    options.secureCookie,
  );
  const web = resolve(options.webDir);
  if (!existsSync(join(web, "index.html")))
    throw new Error("Build the Prometheus admin frontend before startup");
  if (build.commit !== "development") {
    const frontendBuild = JSON.parse(
      readFileSync(join(web, "build.json"), "utf8"),
    ) as { commit: string };
    if (frontendBuild.commit !== build.commit)
      throw new Error("Frontend/server commit mismatch");
  }
  const store = new Store(options.dataDir),
    runtime = new Runtime(store, options.allowLoopbackModel),
    auth = new Authentication(store, options.secureCookie),
    started = Date.now();
  let checking = false;
  const connectionChecks = new Set<AbortController>();
  const server = createServer(async (request, response) => {
    response.setHeader("x-content-type-options", "nosniff");
    response.setHeader("referrer-policy", "no-referrer");
    response.setHeader("x-frame-options", "DENY");
    response.setHeader(
      "content-security-policy",
      "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    );
    try {
      const method = request.method ?? "GET",
        mutation = !["GET", "HEAD"].includes(method);
      checkHostOrigin(request.headers, network, mutation);
      const url = new URL(request.url ?? "/", network.origin),
        path = url.pathname;
      if (path === "/health/live" && method === "GET")
        return json(response, { ...build, status: "ok" });
      if (path === "/health/ready" && method === "GET") {
        store.get("SELECT 1");
        return json(response, { ...build, status: "ready" });
      }
      if (path === `${prefix}/auth/status` && method === "GET")
        return json(response, auth.status());
      if (path === `${prefix}/auth/bootstrap` && method === "POST")
        return json(
          response,
          await auth.bootstrap(await body(request), response),
          201,
        );
      if (path === `${prefix}/auth/login` && method === "POST")
        return json(response, await auth.login(await body(request), response));
      if (path.startsWith("/api/")) {
        const admin = auth.require(request, mutation),
          apiPath = path.slice(prefix.length);
        if (!path.startsWith(`${prefix}/`)) throw missing();
        if (apiPath === "/auth/me" && method === "GET")
          return json(response, {
            user: { id: admin.id, username: admin.username, role: admin.role },
            csrf_token: admin.csrf_token,
          });
        if (apiPath === "/auth/logout" && method === "POST")
          return json(response, auth.logout(request, response));
        if (apiPath === "/capabilities" && method === "GET")
          return json(response, {
            name: "Prometheus",
            capabilities: capabilityStatus,
            historical_dsh_validation_applies: false,
          });
        if (apiPath === "/status" && method === "GET")
          return json(response, {
            ...build,
            uptime_seconds: Math.floor((Date.now() - started) / 1000),
            platform: {
              os: process.platform,
              arch: process.arch,
              node: process.version,
              memory_total_bytes: totalmem(),
              memory_available_bytes: freemem(),
            },
            queue: {
              running: runtime.runningCount,
              queued: Number(
                store.get(
                  "SELECT count(*) AS count FROM runs WHERE status='queued'",
                )!.count,
              ),
              failed: Number(
                store.get(
                  "SELECT count(*) AS count FROM runs WHERE status IN ('failed','interrupted')",
                )!.count,
              ),
            },
            config: {
              desired_revision: store.config().revision,
              applied_revision: store.config().applied_revision,
            },
            assistant_id: store.assistantId,
          });
        if (apiPath === "/config" && method === "GET") {
          response.setHeader("etag", `"${store.config().revision}"`);
          return json(response, store.config());
        }
        if (apiPath === "/config" && method === "PUT") {
          const input = fields(await body(request), ["config"]);
          const config = validateConfig(
            input.config,
            options.allowLoopbackModel,
          );
          return json(
            response,
            store.applyConfig(
              revision(request.headers["if-match"]),
              config,
              admin.id,
            ),
          );
        }
        if (apiPath === "/config/check" && method === "POST") {
          if (checking)
            throw new ApiError(429, "checks_busy", "连接检查正在执行");
          const input = fields(await body(request), ["config"]);
          const config = validateConfig(
            input.config,
            options.allowLoopbackModel,
          );
          store.checkCredentials(config);
          if (!config.model.credential_ref) throw bad("请先选择模型凭据");
          checking = true;
          const controller = new AbortController();
          connectionChecks.add(controller);
          let release: (() => void) | undefined;
          try {
            release = runtime.beginCheck();
            await complete(
              config,
              store.secret(config.model.credential_ref, "model_api_key"),
              [{ role: "user", content: "请只回复 OK。" }],
              AbortSignal.any([controller.signal, AbortSignal.timeout(20000)]),
              options.allowLoopbackModel,
            );
            store.audit(admin.id, "model", "completed", "模型连接检查成功");
            return json(response, {
              ok: true,
              message: "模型实际返回成功；配置尚未保存",
            });
          } finally {
            release?.();
            checking = false;
            connectionChecks.delete(controller);
          }
        }
        if (apiPath === "/config/revisions" && method === "GET")
          return json(response, {
            items: store.all(
              "SELECT revision,created_at FROM config_revisions ORDER BY revision DESC LIMIT 50",
            ),
          });
        const rollback = /^\/config\/revisions\/(\d+)\/rollback$/.exec(apiPath);
        if (rollback && method === "POST") {
          await body(request);
          const row = store.get(
            "SELECT config_json FROM config_revisions WHERE revision=?",
            Number(rollback[1]),
          );
          if (!row) throw missing();
          return json(
            response,
            store.applyConfig(
              revision(request.headers["if-match"]),
              validateConfig(
                JSON.parse(String(row.config_json)),
                options.allowLoopbackModel,
              ),
              admin.id,
            ),
          );
        }
        if (apiPath === "/assistant/personality" && method === "GET") {
          response.setHeader("etag", `"${store.personality().revision}"`);
          return json(response, store.personality());
        }
        if (apiPath === "/assistant/personality" && method === "PUT")
          return json(
            response,
            store.savePersonality(
              revision(request.headers["if-match"]),
              validatePersonality(
                fields(await body(request), ["personality"]).personality,
              ),
              admin.id,
            ),
          );
        if (apiPath === "/assistant/personality/history" && method === "GET")
          return json(response, {
            items: store
              .all(
                "SELECT revision,personality_json,created_at FROM personality_versions ORDER BY revision DESC LIMIT 30",
              )
              .map((row) => ({
                revision: row.revision,
                created_at: row.created_at,
                personality: JSON.parse(String(row.personality_json)),
              })),
          });
        if (apiPath === "/secrets" && method === "GET")
          return json(response, {
            items: store
              .all(
                "SELECT id,kind,created_at FROM secrets ORDER BY created_at DESC",
              )
              .map((row) => ({ ...row, configured: true })),
          });
        if (apiPath === "/secrets" && method === "POST") {
          const input = fields(await body(request), ["kind", "value"]);
          return json(
            response,
            store.addSecret(
              string(input.kind, "凭据类型", 64, 1),
              string(input.value, "凭据", 8192, 1),
              admin.id,
            ),
            201,
          );
        }
        const secret = /^\/secrets\/([a-f0-9-]{36})$/.exec(apiPath);
        if (secret && method === "DELETE")
          return json(response, store.removeSecret(secret[1]!, admin.id));
        if (apiPath === "/conversations" && method === "GET")
          return json(response, {
            items: store.all(
              "SELECT id,title,created_at FROM conversations WHERE owner_id=? ORDER BY rowid DESC LIMIT 100",
              admin.id,
            ),
          });
        if (apiPath === "/conversations" && method === "POST") {
          const input = fields(await body(request), ["title"]);
          return json(
            response,
            runtime.createConversation(
              admin.id,
              string(input.title ?? "新对话", "标题", 200, 1),
            ),
            201,
          );
        }
        const messages = /^\/conversations\/([a-f0-9-]{36})\/messages$/.exec(
          apiPath,
        );
        if (messages) {
          runtime.ownConversation(messages[1]!, admin.id);
          if (method === "GET")
            return json(response, {
              items: store
                .all(
                  "SELECT id,run_id,role,content,created_at FROM messages WHERE conversation_id=? ORDER BY rowid DESC LIMIT 200",
                  messages[1]!,
                )
                .reverse()
                .map((row) => ({
                  ...row,
                  content: store.redact(String(row.content)),
                })),
            });
          if (method === "POST") {
            const input = fields(await body(request), [
              "content",
              "client_message_id",
              "skill_id",
            ]);
            return json(
              response,
              runtime.enqueue(
                messages[1]!,
                admin.id,
                string(input.content, "消息", 16000, 1),
                string(
                  input.client_message_id ?? randomUUID(),
                  "消息标识",
                  128,
                  1,
                ),
                input.skill_id === undefined
                  ? undefined
                  : string(input.skill_id, "Skill ID", 36, 36),
              ),
              202,
            );
          }
        }
        const run = /^\/runs\/([a-f0-9-]{36})$/.exec(apiPath);
        if (run && method === "GET")
          return json(response, runtime.getRun(run[1]!, admin.id));
        if (run && method === "DELETE")
          return json(response, runtime.cancel(run[1]!, admin.id));
        if (apiPath === "/runs" && method === "GET")
          return json(response, {
            items: store.all(
              "SELECT id,conversation_id,status,created_at,updated_at,error FROM runs WHERE owner_id=? ORDER BY rowid DESC LIMIT 100",
              admin.id,
            ),
          });
        const records = /^\/records\/([a-z]+)(?:\/([a-f0-9-]{36}))?$/.exec(
          apiPath,
        );
        if (records) {
          const type = kind(records[1]!);
          const id = records[2];
          if (!id && method === "GET")
            return json(
              response,
              runtime.records.list(type, admin.id, url.searchParams),
            );
          if (!id && method === "POST")
            return json(
              response,
              runtime.records.save(type, admin.id, await body(request)),
              201,
            );
          if (id && method === "GET")
            return json(response, runtime.records.get(type, id, admin.id));
          if (id && method === "PUT")
            return json(
              response,
              runtime.records.save(
                type,
                admin.id,
                await body(request),
                id,
                revision(request.headers["if-match"]),
              ),
            );
          if (id && method === "DELETE")
            return json(
              response,
              runtime.records.forget(
                type,
                id,
                admin.id,
                revision(request.headers["if-match"]),
              ),
            );
        }
        if (apiPath === "/runtime-logs" && method === "GET") {
          const cursor = integer(
            Number(url.searchParams.get("cursor") ?? Number.MAX_SAFE_INTEGER),
            "日志游标",
            1,
            Number.MAX_SAFE_INTEGER,
          );
          const rows = store.all(
            "SELECT * FROM audit_events WHERE owner_id=? AND id<? ORDER BY id DESC LIMIT 51",
            admin.id,
            cursor,
          );
          return json(response, {
            items: rows.slice(0, 50),
            next_cursor: rows.length > 50 ? rows[49]!.id : null,
            retention: { days: 30, max_events: 10000 },
          });
        }
        if (apiPath === "/export" && method === "GET")
          return json(response, {
            name: "Prometheus",
            schema: 1,
            assistant_id: store.assistantId,
            exported_at: now(),
            config: store.config(),
            personality: store.personality(),
            records: store
              .all(
                "SELECT * FROM records WHERE owner_id=? AND deleted_at IS NULL",
                admin.id,
              )
              .map((row) => ({
                id: row.id,
                kind: row.kind,
                visibility: row.visibility,
                revision: row.revision,
                data: JSON.parse(String(row.payload)),
              })),
            secrets: "excluded",
          });
        if (
          /^\/(plugins|harness|speech|weixin|qq|rest|vision)(?:\/|$)/.test(
            apiPath,
          )
        )
          throw unavailable(apiPath.split("/")[1]!);
        throw missing();
      }
      if (path === "/" || path === "/admin") {
        response.writeHead(302, { location: "/admin/" });
        response.end();
        return;
      }
      if (path.startsWith("/admin/") && ["GET", "HEAD"].includes(method)) {
        const name =
          decodeURIComponent(path.slice("/admin/".length)) || "index.html";
        if (
          name.includes("\\") ||
          name.includes("\0") ||
          name.split("/").some((part) => part.startsWith("."))
        )
          throw missing();
        let file = resolve(web, name),
          rel = relative(web, file);
        if (rel.startsWith(`..${sep}`) || rel === ".." || rel.includes(":"))
          throw missing();
        if (!existsSync(file)) {
          if (extname(name)) throw missing();
          file = join(web, "index.html");
        }
        rel = relative(realpathSync(web), realpathSync(file));
        if (
          rel === ".." ||
          rel.startsWith(`..${sep}`) ||
          rel.includes(":") ||
          !(await stat(file)).isFile()
        )
          throw missing();
        const types: Record<string, string> = {
          ".html": "text/html; charset=utf-8",
          ".js": "text/javascript; charset=utf-8",
          ".css": "text/css; charset=utf-8",
          ".svg": "image/svg+xml",
          ".png": "image/png",
          ".json": "application/json",
        };
        const data = await readFile(file);
        response.writeHead(200, {
          "content-type": types[extname(file)] ?? "application/octet-stream",
          "cache-control": name.startsWith("assets/")
            ? "public, max-age=31536000, immutable"
            : "no-store",
          "content-length": data.length,
        });
        response.end(method === "HEAD" ? undefined : data);
        return;
      }
      throw missing();
    } catch (error) {
      const apiError =
        error instanceof ApiError
          ? error
          : new ApiError(500, "internal_error", "请求未完成，请查看运行状态");
      if (!response.headersSent)
        json(
          response,
          { error: { code: apiError.code, message: apiError.message } },
          apiError.status,
        );
      else response.destroy();
    }
  });
  server.requestTimeout = 30000;
  server.headersTimeout = 15000;
  server.keepAliveTimeout = 5000;
  server.maxHeadersCount = 100;
  try {
    await runtime.start();
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(network.port, network.host, () => {
        server.off("error", reject);
        resolve();
      });
    });
  } catch (error) {
    await runtime.stop();
    store.close();
    throw error;
  }
  let closed = false;
  return {
    server,
    store,
    runtime,
    async close() {
      if (closed) return;
      closed = true;
      const closing = new Promise<void>((resolve) =>
        server.close(() => resolve()),
      );
      server.closeIdleConnections();
      for (const controller of connectionChecks) controller.abort();
      await runtime.stop();
      await closing;
      store.close();
    },
  };
}
