import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { request } from "node:http";
import { fixture } from "./helpers.mjs";
import { bootstrapToken } from "../dist/auth.js";
import {
  networkOptions,
  passwordHash,
  verifyPassword,
} from "../dist/security.js";
import { publicAddress } from "../dist/model.js";

test("Argon2 password verification supports the former Rust PHC encoding", async () => {
  const encoded = await passwordHash("Synthetic-password-2026!");
  assert.match(encoded, /^\$argon2id\$v=19\$m=19456,t=2,p=1\$/);
  assert.equal(await verifyPassword("Synthetic-password-2026!", encoded), true);
  assert.equal(await verifyPassword("wrong", encoded), false);
  assert.equal(
    await verifyPassword(
      "wrong",
      "$argon2id$v=19$m=999999999,t=2,p=1$AAAA$AAAA",
    ),
    false,
  );
});
test("network entry and model address policies reject unsafe endpoints", () => {
  for (const origin of [
    "http://example.com",
    "http://127.0.0.1:8419/admin/",
    "http://user:pass@127.0.0.1:8419",
    "http://127.0.0.1:8419?x=1",
  ])
    assert.throws(() => networkOptions("127.0.0.1", 8419, origin, false));
  assert.throws(() =>
    networkOptions("0.0.0.0", 8419, "http://127.0.0.1:8419", false),
  );
  for (const ip of [
    "127.0.0.1",
    "10.1.2.3",
    "172.16.2.3",
    "192.168.1.1",
    "169.254.169.254",
    "100.64.1.2",
    "0.0.0.0",
    "224.0.0.1",
    "::1",
    "::ffff:127.0.0.1",
    "fc00::1",
    "fe80::1",
    "2001:db8::1",
    "2001::1",
    "2001:0000::1",
    "2002:a00:1::1",
    "3fff::1",
    "192.88.99.1",
  ])
    assert.equal(publicAddress(ip), false, ip);
  for (const ip of ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"])
    assert.equal(publicAddress(ip), true, ip);
});
test("authentication, exact Host/Origin, CSRF, bootstrap expiry and cookie properties", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  assert.equal(
    (await app.call("/status", "GET", undefined, { auth: false })).status,
    401,
  );
  const hostileHostStatus = await new Promise((resolve, reject) => {
    const req = request(
      `${app.origin}/api/admin/v1/auth/status`,
      { headers: { host: "evil.test" } },
      (response) => {
        response.resume();
        resolve(response.statusCode);
      },
    );
    req.on("error", reject);
    req.end();
  });
  assert.equal(hostileHostStatus, 403);
  assert.equal(
    (
      await app.call("/auth/status", "GET", undefined, {
        headers: { origin: "https://evil.test" },
      })
    ).status,
    403,
  );
  const path = bootstrapToken(app.dataDir),
    expired = JSON.parse(readFileSync(path, "utf8"));
  writeFileSync(path, JSON.stringify({ ...expired, expires_at: 1 }));
  assert.equal(
    (
      await app.call("/auth/bootstrap", "POST", {
        username: "tester",
        password: "Synthetic-password-2026!",
        bootstrap_token: expired.token,
      })
    ).status,
    403,
  );
  const created = await app.bootstrap();
  assert.match(
    created.headers.get("set-cookie"),
    /HttpOnly; SameSite=Strict; Path=\/api\/admin\/v1/,
  );
  assert.equal(existsSync(path), false);
  assert.equal(
    (
      await app.call("/auth/bootstrap", "POST", {
        username: "tester",
        password: "Synthetic-password-2026!",
        bootstrap_token: expired.token,
      })
    ).status,
    409,
  );
  assert.equal(
    (
      await app.call(
        "/conversations",
        "POST",
        {},
        { headers: { "x-csrf-token": "" } },
      )
    ).status,
    403,
  );
  assert.equal(
    (
      await app.call(
        "/conversations",
        "POST",
        {},
        { headers: { origin: "null" } },
      )
    ).status,
    403,
  );
  assert.equal((await app.call("/auth/logout", "POST", {})).status, 200);
  assert.equal((await app.call("/status")).status, 401);
  assert.equal(
    (
      await app.call("/auth/login", "POST", {
        username: "tester",
        password: "wrong",
      })
    ).status,
    401,
  );
  assert.equal(
    (
      await app.call("/auth/login", "POST", {
        username: "tester",
        password: "Synthetic-password-2026!",
      })
    ).status,
    200,
  );
});
test("secrets are encrypted, write-only, type-checked, and excluded from exports", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const secretValue = "synthetic-secret-never-export-me";
  const saved = await app.call("/secrets", "POST", {
    kind: "model_api_key",
    value: secretValue,
  });
  assert.equal(saved.status, 201);
  assert.equal(JSON.stringify(saved.data).includes(secretValue), false);
  assert.equal(
    readFileSync(join(app.dataDir, "secrets", saved.data.id)).includes(
      Buffer.from(secretValue),
    ),
    false,
  );
  assert.equal(
    JSON.stringify((await app.call("/secrets")).data).includes(secretValue),
    false,
  );
  assert.equal(
    JSON.stringify((await app.call("/export")).data).includes(secretValue),
    false,
  );
  assert.equal(
    (await app.call("/secrets", "POST", { kind: "unknown", value: "bad" }))
      .status,
    400,
  );
  const config = (await app.call("/config")).data;
  config.config.model.credential_ref = saved.data.id;
  assert.equal(
    (
      await app.call(
        "/config",
        "PUT",
        { config: config.config },
        { headers: { "if-match": `"${config.revision}"` } },
      )
    ).status,
    200,
  );
  assert.equal(
    (await app.call(`/secrets/${saved.data.id}`, "DELETE")).status,
    409,
  );
});
test("configuration conflicts, immutable history, rollback and fail-closed capabilities", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  const state = (await app.call("/config")).data;
  assert.equal(
    (await app.call("/config", "PUT", { config: state.config })).status,
    428,
  );
  const saved = await app.call(
    "/config",
    "PUT",
    { config: state.config },
    { headers: { "if-match": '"1"' } },
  );
  assert.equal(saved.status, 200);
  assert.equal(saved.data.applied_revision, 2);
  assert.equal(
    (
      await app.call(
        "/config",
        "PUT",
        { config: state.config },
        { headers: { "if-match": '"1"' } },
      )
    ).status,
    409,
  );
  assert.equal(
    (
      await app.call(
        "/config/revisions/1/rollback",
        "POST",
        {},
        { headers: { "if-match": '"2"' } },
      )
    ).data.revision,
    3,
  );
  for (const route of [
    "/plugins/execute",
    "/harness/start",
    "/speech/synthesize",
    "/weixin/login",
    "/qq/send",
  ])
    assert.equal((await app.call(route, "POST", {})).status, 503, route);
  state.config.qq.enabled = true;
  assert.equal(
    (
      await app.call(
        "/config",
        "PUT",
        { config: state.config },
        { headers: { "if-match": '"3"' } },
      )
    ).status,
    400,
  );
  state.config.qq.enabled = false;
  state.config.runtime.max_concurrent_runs = 3;
  assert.equal(
    (
      await app.call(
        "/config",
        "PUT",
        { config: state.config },
        { headers: { "if-match": '"3"' } },
      )
    ).status,
    400,
  );
});
test("static assets are same-origin, traversal is refused, and unsafe content is never executed", async (t) => {
  const app = await fixture();
  t.after(() => app.close());
  const page = await fetch(`${app.origin}/admin/`);
  assert.equal(page.status, 200);
  assert.match(await page.text(), /Prometheus/);
  assert.match(
    page.headers.get("content-security-policy"),
    /frame-ancestors 'none'/,
  );
  assert.equal((await fetch(`${app.origin}/admin/missing.js`)).status, 404);
  assert.equal(
    (await fetch(`${app.origin}/admin/%2e%2e%5cpackage.json`)).status,
    404,
  );
  assert.equal((await fetch(`${app.origin}/admin/.env`)).status, 404);
});
