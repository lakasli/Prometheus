import assert from "node:assert/strict";
import { mkdir, readFile, symlink, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { get as requestGet } from "node:http";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { fixture } from "./helpers.mjs";

test("stage serves approved assets only on direct loopback and keeps admin policy", async (t) => {
  const root = fileURLToPath(new URL("../../../", import.meta.url));
  const runtime = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? root);
  const scratch = join(root, ".local/prometheus/tests", randomUUID());
  const stageDir = join(scratch, "stage");
  const stageModelDir = join(scratch, "model");
  const stageCoreDir = join(scratch, "core");
  const outside = join(scratch, "outside");
  const shaderDir = join(stageDir, "assets/cubism-shaders");
  await Promise.all(
    [shaderDir, stageModelDir, stageCoreDir, outside].map((dir) =>
      mkdir(dir, { recursive: true }),
    ),
  );
  await Promise.all([
    writeFile(
      join(stageDir, "index.html"),
      "<!doctype html><title>Stage</title>",
    ),
    writeFile(
      join(stageDir, "build.json"),
      await readFile(join(runtime, "apps/admin/dist/build.json")),
    ),
    writeFile(join(stageDir, "cubism-LICENSE.md"), "License fixture"),
    writeFile(join(shaderDir, "Fixture.frag"), "Shader fixture"),
    writeFile(join(stageModelDir, "大肥鱼.moc3"), "Moc fixture"),
    writeFile(join(stageModelDir, "大肥鱼.vtube.json"), "Excluded asset"),
    writeFile(
      join(stageCoreDir, "live2dcubismcore.min.js"),
      Buffer.alloc(228_042),
    ),
    writeFile(join(outside, "texture_00.png"), "Outside the model directory"),
    symlink(outside, join(stageModelDir, "大肥鱼.4096"), "junction"),
  ]);
  const app = await fixture({ stageDir, stageModelDir, stageCoreDir });
  t.after(() => app.close());
  const get = (path, init) => fetch(`${app.origin}${path}`, init);
  const getStatus = (path, headers) =>
    new Promise((resolve, reject) => {
      requestGet(new URL(path, app.origin), { headers }, (response) => {
        response.resume();
        response.once("end", () => resolve(response.statusCode));
      }).once("error", reject);
    });

  const redirect = await get("/stage", { redirect: "manual" });
  assert.equal(redirect.status, 302);
  assert.equal(redirect.headers.get("location"), "/stage/");
  const page = await get("/stage/");
  assert.equal(page.status, 200);
  assert.equal(page.headers.get("cross-origin-resource-policy"), "same-origin");
  assert.match(await page.text(), /<title>Stage<\/title>/);
  const navigation = {
    "sec-fetch-site": "cross-site",
    "sec-fetch-mode": "navigate",
    "sec-fetch-dest": "document",
  };
  assert.equal(await getStatus("/stage/", navigation), 200);
  assert.equal(
    await getStatus("/stage/", { ...navigation, "sec-fetch-dest": "iframe" }),
    404,
  );
  const admin = await get("/admin/");
  assert.equal(admin.status, 200);
  assert.equal(
    admin.headers.get("content-security-policy"),
    page.headers.get("content-security-policy"),
  );
  await admin.text();
  assert.equal(
    (await app.call("/status", "GET", undefined, { auth: false })).status,
    401,
  );

  const shader = await get("/stage/assets/cubism-shaders/Fixture.frag");
  assert.equal(shader.headers.get("content-type"), "text/plain; charset=utf-8");
  assert.equal(shader.headers.get("cache-control"), "no-store");
  assert.equal(await shader.text(), "Shader fixture");
  const license = await get("/stage/cubism-LICENSE.md");
  assert.equal(
    license.headers.get("content-type"),
    "text/plain; charset=utf-8",
  );
  assert.equal(await license.text(), "License fixture");

  const mocPath = "/stage/model/大肥鱼.moc3";
  const moc = await get(mocPath);
  assert.equal(moc.status, 200);
  assert.equal(moc.headers.get("content-type"), "application/octet-stream");
  assert.equal(moc.headers.get("cache-control"), "no-store");
  assert.equal(await moc.text(), "Moc fixture");
  const head = await get(mocPath, { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(head.headers.get("content-length"), "11");
  assert.equal(await head.text(), "");

  for (const path of [
    "/stage/model/大肥鱼.vtube.json",
    "/stage/model/大肥鱼.4096/texture_00.png",
    "/stage/model/%2e%2e%2findex.html",
    "/stage/model/%5c大肥鱼.moc3",
    "/stage/core/live2dcubismcore.min.js",
  ]) {
    const response = await get(path);
    assert.equal(response.status, 404, path);
    await response.text();
  }
  for (const headers of [
    { forwarded: "for=203.0.113.1" },
    { "x-forwarded-host": "public.example" },
    { host: "public.example" },
    { origin: "https://public.example" },
    { "sec-fetch-site": "same-site" },
    navigation,
  ]) {
    assert.equal(
      await getStatus(mocPath, headers),
      404,
      JSON.stringify(headers),
    );
  }
});
