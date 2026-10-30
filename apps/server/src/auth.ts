import {
  existsSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import { ApiError, fields, string } from "./errors.ts";
import {
  hash,
  now,
  passwordHash,
  privateDirectory,
  secureEqual,
  token,
  verifyPassword,
} from "./security.ts";
import type { Store } from "./store.ts";

export interface Admin {
  id: string;
  username: string;
  role: "admin";
  csrf_token: string;
}
const cookieName = "prometheus_session";
function readBootstrap(path: string): { token: string; expires_at: number } {
  try {
    const record = JSON.parse(readFileSync(path, "utf8"));
    if (
      !record ||
      typeof record.token !== "string" ||
      !/^[0-9a-f]{64}$/.test(record.token) ||
      !Number.isSafeInteger(record.expires_at)
    )
      throw new Error();
    return { token: record.token, expires_at: record.expires_at };
  } catch {
    throw new ApiError(
      403,
      "bootstrap_invalid",
      "当前实例的初始化凭证文件无效，请按页面命令重新生成",
    );
  }
}
export function bootstrapToken(dataDir: string): string {
  privateDirectory(dataDir);
  const path = join(dataDir, "bootstrap-token.json"),
    temporary = join(dataDir, `.bootstrap-${randomUUID()}.tmp`);
  writeFileSync(
    temporary,
    JSON.stringify({ token: token(), expires_at: now() + 900 }),
    { flag: "wx", mode: 0o600 },
  );
  renameSync(temporary, path);
  return path;
}
export class Authentication {
  private attempts: number[] = [];
  constructor(
    private store: Store,
    private secureCookie: boolean,
  ) {}
  private attempt(): void {
    this.attempts = this.attempts.filter((value) => value > Date.now() - 60000);
    if (this.attempts.length >= 10)
      throw new ApiError(429, "rate_limited", "登录尝试过于频繁，请稍后再试");
    this.attempts.push(Date.now());
  }
  status() {
    const initialized = !!this.store.get("SELECT id FROM admins LIMIT 1");
    const path = join(this.store.dataDir, "bootstrap-token.json");
    let state: "missing" | "invalid" | "expired" | "ready" = "missing",
      expires: number | null = null;
    if (!initialized && existsSync(path)) {
      try {
        const record = readBootstrap(path);
        expires = record.expires_at;
        state = expires <= now() ? "expired" : "ready";
      } catch {
        state = "invalid";
      }
    }
    return {
      initialized,
      bootstrap_required: !initialized,
      name: "Prometheus",
      version: "0.1.0",
      ...(!initialized
        ? {
            bootstrap_hint: {
              file_path: path,
              generate_command: `npm.cmd run bootstrap -- --data-dir '${this.store.dataDir.replaceAll("'", "''")}'`,
              state,
              expires_at: expires,
            },
          }
        : {}),
    };
  }
  require(request: IncomingMessage, mutation = false): Admin {
    const session = request.headers.cookie
      ?.split(";")
      .map((item) => item.trim())
      .find((item) => item.startsWith(`${cookieName}=`))
      ?.slice(cookieName.length + 1);
    if (!session || !/^[0-9a-f]{64}$/.test(session))
      throw new ApiError(401, "unauthorized", "请先登录");
    const row = this.store.get(
      "SELECT a.id,a.username,s.csrf FROM sessions s JOIN admins a ON a.id=s.user_id WHERE s.token_hash=? AND s.expires_at>?",
      hash(session),
      now(),
    );
    if (!row) throw new ApiError(401, "unauthorized", "登录已过期，请重新登录");
    if (
      mutation &&
      !secureEqual(
        String(request.headers["x-csrf-token"] ?? ""),
        String(row.csrf),
      )
    )
      throw new ApiError(403, "csrf_forbidden", "请求校验未通过，请重新登录");
    return {
      id: String(row.id),
      username: String(row.username),
      role: "admin",
      csrf_token: String(row.csrf),
    };
  }
  private session(id: string, username: string, response: ServerResponse) {
    const sessionToken = token(),
      csrf = token();
    this.store.run("DELETE FROM sessions WHERE expires_at<=?", now());
    this.store.run(
      "INSERT INTO sessions VALUES(?,?,?,?)",
      hash(sessionToken),
      id,
      csrf,
      now() + 8 * 3600,
    );
    response.setHeader(
      "set-cookie",
      `${cookieName}=${sessionToken}; HttpOnly; SameSite=Strict; Path=/api/admin/v1; Max-Age=28800${this.secureCookie ? "; Secure" : ""}`,
    );
    return { user: { id, username, role: "admin" }, csrf_token: csrf };
  }
  async bootstrap(value: unknown, response: ServerResponse) {
    this.attempt();
    const body = fields(value, ["username", "password", "bootstrap_token"]);
    const username = string(body.username, "用户名", 64, 3),
      password = string(body.password, "密码", 256, 12);
    if (!/^[a-zA-Z0-9_.-]+$/.test(username))
      throw new ApiError(
        400,
        "invalid_username",
        "用户名只能包含字母、数字、下划线、点和连字符",
      );
    if (this.status().initialized)
      throw new ApiError(409, "already_initialized", "实例已初始化，请登录");
    const path = join(this.store.dataDir, "bootstrap-token.json");
    if (!existsSync(path))
      throw new ApiError(
        403,
        "bootstrap_required",
        "当前实例还没有初始化凭证，请按页面命令在本机生成",
      );
    const record = readBootstrap(path);
    if (record.expires_at <= now())
      throw new ApiError(
        403,
        "bootstrap_expired",
        "当前实例的初始化凭证已过期，请按页面命令重新生成",
      );
    if (
      !secureEqual(
        record.token,
        string(body.bootstrap_token, "初始化凭证", 64, 64),
      )
    )
      throw new ApiError(
        403,
        "bootstrap_forbidden",
        "初始化凭证与当前运行实例不匹配，请使用页面显示文件中的token字段",
      );
    const encoded = await passwordHash(password),
      id = randomUUID();
    this.store.transaction(() => {
      if (this.status().initialized)
        throw new ApiError(409, "already_initialized", "实例已初始化，请登录");
      const current = readBootstrap(path);
      if (current.token !== record.token || current.expires_at <= now())
        throw new ApiError(
          403,
          "bootstrap_forbidden",
          "初始化凭证已变化或过期",
        );
      this.store.run(
        "INSERT INTO admins VALUES(?,?,?,?)",
        id,
        username,
        encoded,
        now(),
      );
      this.store.audit(id, "authentication", "completed", "管理账户已创建", id);
    });
    unlinkSync(path);
    return this.session(id, username, response);
  }
  async login(value: unknown, response: ServerResponse) {
    this.attempt();
    const body = fields(value, ["username", "password"]);
    const username = string(body.username, "用户名", 64),
      password = string(body.password, "密码", 256);
    const row = this.store.get(
      "SELECT id,password_hash FROM admins WHERE username=?",
      username,
    );
    // Unknown users still incur a password derivation to avoid a cheap timing oracle.
    const valid = row
      ? await verifyPassword(password, String(row.password_hash))
      : (await passwordHash(password), false);
    if (!row || !valid)
      throw new ApiError(401, "unauthorized", "用户名或密码错误");
    this.store.audit(
      String(row.id),
      "authentication",
      "completed",
      "管理账户已登录",
      String(row.id),
    );
    return this.session(String(row.id), username, response);
  }
  logout(request: IncomingMessage, response: ServerResponse) {
    const sessionToken = request.headers.cookie
      ?.split(";")
      .map((item) => item.trim())
      .find((item) => item.startsWith(`${cookieName}=`))
      ?.slice(cookieName.length + 1);
    if (sessionToken)
      this.store.run(
        "DELETE FROM sessions WHERE token_hash=?",
        hash(sessionToken),
      );
    response.setHeader(
      "set-cookie",
      `${cookieName}=; HttpOnly; SameSite=Strict; Path=/api/admin/v1; Max-Age=0${this.secureCookie ? "; Secure" : ""}`,
    );
    return { ok: true };
  }
}
