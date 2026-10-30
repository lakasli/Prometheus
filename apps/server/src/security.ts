import {
  argon2,
  createHash,
  randomBytes,
  randomUUID,
  timingSafeEqual,
} from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { ApiError, bad } from "./errors.ts";

export const now = () => Math.floor(Date.now() / 1000);
export const token = () => randomBytes(32).toString("hex");
export const hash = (value: string) =>
  createHash("sha256").update(value).digest("hex");
export function secureEqual(left: string, right: string): boolean {
  const a = Buffer.from(left),
    b = Buffer.from(right);
  return a.length === b.length && timingSafeEqual(a, b);
}
function derive(
  password: string,
  salt: Buffer,
  memory = 19456,
  passes = 2,
  parallelism = 1,
  tagLength = 32,
): Promise<Buffer> {
  return new Promise((resolve, reject) =>
    argon2(
      "argon2id",
      {
        message: password,
        nonce: salt,
        memory,
        passes,
        parallelism,
        tagLength,
      },
      (error, key) => (error ? reject(error) : resolve(key)),
    ),
  );
}
export async function passwordHash(password: string): Promise<string> {
  const salt = randomBytes(16),
    key = await derive(password, salt);
  return `$argon2id$v=19$m=19456,t=2,p=1$${salt.toString("base64").replace(/=+$/, "")}$${key.toString("base64").replace(/=+$/, "")}`;
}
export async function verifyPassword(
  password: string,
  encoded: string,
): Promise<boolean> {
  const match =
    /^\$argon2id\$v=19\$m=(\d+),t=(\d+),p=(\d+)\$([A-Za-z0-9+/]+)\$([A-Za-z0-9+/]+)$/.exec(
      encoded,
    );
  if (!match) return false;
  const [, m, t, p, s, k] = match;
  const memory = Number(m),
    passes = Number(t),
    parallelism = Number(p);
  const salt = Buffer.from(s!, "base64"),
    key = Buffer.from(k!, "base64");
  if (
    memory < 8 * parallelism ||
    memory > 65536 ||
    passes < 1 ||
    passes > 10 ||
    parallelism < 1 ||
    parallelism > 4 ||
    salt.length < 8 ||
    salt.length > 64 ||
    key.length < 16 ||
    key.length > 64
  )
    return false;
  try {
    return timingSafeEqual(
      await derive(password, salt, memory, passes, parallelism, key.length),
      key,
    );
  } catch {
    return false;
  }
}

let currentSid: string | undefined;
function windowsSid(): string {
  if (!currentSid)
    currentSid = execFileSync("whoami.exe", ["/user", "/fo", "csv", "/nh"], {
      windowsHide: true,
      encoding: "utf8",
    }).match(/S-1-5-[0-9-]+/)?.[0];
  if (!currentSid) throw new Error("Cannot determine Windows runtime SID");
  return currentSid;
}
/** Protect only a newly created task directory; existing directories are verified. */
export function privateDirectory(path: string): string {
  const full = resolve(path),
    created = !existsSync(full);
  if (created) mkdirSync(full, { recursive: true, mode: 0o700 });
  if (
    lstatSync(full).isSymbolicLink() ||
    realpathSync(full).toLowerCase() !== full.toLowerCase()
  )
    throw new Error("Private data directory must not be a link or junction");
  if (process.platform !== "win32") {
    if (created) chmodSync(full, 0o700);
    if (lstatSync(full).mode & 0o077)
      throw new Error("Private data directory has broad permissions");
    return full;
  }
  const sid = windowsSid();
  if (created)
    execFileSync(
      "icacls.exe",
      [
        full,
        "/inheritance:r",
        "/grant:r",
        `*${sid}:(OI)(CI)F`,
        "*S-1-5-18:(OI)(CI)F",
        "*S-1-5-32-544:(OI)(CI)F",
      ],
      { windowsHide: true, stdio: "pipe" },
    );
  const snapshot = join(tmpdir(), `prometheus-acl-${randomUUID()}.txt`);
  writeFileSync(snapshot, "", { flag: "wx", mode: 0o600 });
  try {
    // /save emits SDDL, so this check is independent of localized account names.
    // It checks existing files too; no ACL is rewritten for an existing directory.
    execFileSync("icacls.exe", [full, "/save", snapshot, "/t", "/q"], {
      windowsHide: true,
      stdio: "pipe",
    });
    const raw = readFileSync(snapshot, "utf16le").replace(/^\ufeff/, "");
    const descriptors = raw
      .split(/\r?\n/)
      .filter((line) => line.startsWith("D:"));
    const allowed = new Set([sid, "SY", "BA", "S-1-5-18", "S-1-5-32-544"]);
    if (!descriptors.length) throw new Error("Cannot inspect private NTFS ACL");
    for (const descriptor of descriptors) {
      const rules = [...descriptor.matchAll(/\(([^()]*)\)/g)].map((match) =>
        match[1]!.split(";"),
      );
      if (
        !rules.length ||
        rules.some(
          (rule) =>
            !["A", "D"].includes(rule[0]!) ||
            (rule[0] === "A" && !allowed.has(rule[5]!)),
        )
      )
        throw new Error(
          "Private data ACL grants another principal access; review NTFS ACL before startup",
        );
    }
  } finally {
    unlinkSync(snapshot);
  }
  return full;
}

export interface NetworkOptions {
  host: string;
  port: number;
  origin: string;
  secureCookie: boolean;
}
export function networkOptions(
  host: string,
  port: number,
  origin: string,
  secureCookie: boolean,
): NetworkOptions {
  if (!["127.0.0.1", "::1"].includes(host))
    throw bad("后端必须监听 loopback 地址");
  if (!Number.isInteger(port) || port < 1 || port > 65535)
    throw bad("端口无效");
  let url: URL;
  try {
    url = new URL(origin);
  } catch {
    throw bad("Origin 无效");
  }
  if (
    url.origin !== origin ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash
  )
    throw bad("必须配置精确 Origin，不含路径、凭据或查询");
  const loopback = ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname);
  if (!(
    (url.protocol === "https:" && secureCookie) ||
    (url.protocol === "http:" && loopback && !secureCookie)
  ))
    throw bad("HTTPS 入口必须使用 Secure cookie；HTTP 只允许 loopback");
  return { host, port, origin, secureCookie };
}
export function checkHostOrigin(
  headers: Record<string, string | string[] | undefined>,
  options: NetworkOptions,
  mutation: boolean,
): void {
  const expected = new URL(options.origin);
  if (headers.host !== expected.host)
    throw new ApiError(403, "host_forbidden", "Host 不在管理入口内");
  if (
    (headers.origin !== undefined && headers.origin !== options.origin) ||
    (mutation && headers.origin !== options.origin)
  )
    throw new ApiError(403, "origin_forbidden", "Origin 不在管理入口内");
  const site = headers["sec-fetch-site"];
  if (site === "cross-site")
    throw new ApiError(403, "origin_forbidden", "禁止跨站请求");
}
