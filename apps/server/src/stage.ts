import type { IncomingMessage, ServerResponse } from "node:http";
import { createHash } from "node:crypto";
import { readFileSync, realpathSync, statSync } from "node:fs";
import { open, realpath } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import { pipeline } from "node:stream/promises";
import { missing } from "./errors.ts";
import type { NetworkOptions } from "./security.ts";

export interface StageDirectories {
  stageDir?: string;
  stageModelDir?: string;
  stageCoreDir?: string;
}

const modelFiles = new Set([
  "大肥鱼.model3.json",
  "大肥鱼.moc3",
  "大肥鱼.physics3.json",
  "大肥鱼.cdi3.json",
  "大肥鱼.4096/texture_00.png",
  "大肥鱼.4096/texture_01.png",
  "大肥鱼.4096/texture_02.png",
  "大肥鱼.4096/texture_03.png",
  "大肥鱼.4096/texture_04.png",
  "Animation/IDLE.motion3.json",
  "Exp3/脸红.exp3.json",
  "Exp3/爱心眼.exp3.json",
  "Exp3/圈圈眼.exp3.json",
  "Exp3/流泪.exp3.json",
  "Exp3/＞＜.exp3.json",
  "Exp3/黑脸.exp3.json",
  "Exp3/加载.exp3.json",
  "Exp3/流冷汗.exp3.json",
  "Exp3/屑屑眼.exp3.json",
  "Exp3/兔牙.exp3.json",
  "Exp3/单片眼镜.exp3.json",
  "Exp3/黑框眼镜.exp3.json",
  "Exp3/钢盆.exp3.json",
  "Exp3/裙子切换.exp3.json",
  "Exp3/脱鞋.exp3.json",
  "Exp3/胸口衣服.exp3.json",
  "Exp3/默认体型.exp3.json",
  "Exp3/体型1.exp3.json",
  "Exp3/体型2.exp3.json",
  "Exp3/前倾.exp3.json",
  "Exp3/尾巴开关.exp3.json",
  "Exp3/竖食指开.exp3.json",
  "Exp3/竖食指关.exp3.json",
]);
const coreScript = "live2dcubismcore.min.js";
// Core is loaded as same-origin JavaScript. Only serve the R5 binary that
// matches the version of the Web Framework used by the stage.
const coreBytes = 228_042;
const coreSha256 =
  "8741f739779b5d5210872bd3d7d99f0f1e56e6c87409e7d26d6bb4b80aa1ef47";
const shaderPath = /^assets\/cubism-shaders\/[\w-]+\.(?:vert|frag|glsl)$/;
const contentTypes: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".json": "application/json",
  ".moc3": "application/octet-stream",
  ".md": "text/plain; charset=utf-8",
  ".vert": "text/plain; charset=utf-8",
  ".frag": "text/plain; charset=utf-8",
  ".glsl": "text/plain; charset=utf-8",
};

function stageDirectory(path: string | undefined, label: string) {
  if (path === undefined) return undefined;
  try {
    if (!path.trim()) throw new Error();
    const root = realpathSync(path);
    if (!statSync(root).isDirectory()) throw new Error();
    return root;
  } catch {
    throw new Error(`${label} must be an existing directory`);
  }
}

async function stageFile(root: string, name: string) {
  const target = resolve(root, name);
  try {
    // The route has already matched an allowlisted name. Reject links even
    // when they point to a different file under the same configured root.
    if ((await realpath(target)) !== target) throw missing();
    return await open(target, "r");
  } catch {
    throw missing();
  }
}

export function createStageAccessCheck(network: NetworkOptions) {
  const listenHost = network.host === "::1" ? "[::1]" : network.host;
  const allowedHosts = new Set(
    [listenHost, "localhost"].map(
      (host) => new URL(`http://${host}:${network.port}`).host,
    ),
  );
  return (
    request: IncomingMessage,
    rawPath: string,
    allowLinkMutation = false,
  ) => {
    const { headers, method } = request;
    const host = headers.host;
    const topLevelNavigation =
      (rawPath === "/stage" ||
        rawPath === "/stage/" ||
        rawPath === "/stage/index.html") &&
      headers["sec-fetch-mode"] === "navigate" &&
      headers["sec-fetch-dest"] === "document";
    if (
      (method !== "GET" &&
        method !== "HEAD" &&
        !(allowLinkMutation && (method === "POST" || method === "DELETE"))) ||
      request.socket.remoteAddress !== network.host ||
      !host ||
      !allowedHosts.has(host) ||
      (headers.origin !== undefined && headers.origin !== `http://${host}`) ||
      (headers["sec-fetch-site"] !== undefined &&
        headers["sec-fetch-site"] !== "same-origin" &&
        headers["sec-fetch-site"] !== "none" &&
        !topLevelNavigation) ||
      Object.keys(headers).some(
        (key) =>
          key === "forwarded" ||
          key === "via" ||
          key === "x-real-ip" ||
          key.startsWith("x-forwarded-") ||
          key.startsWith("x-original-"),
      )
    )
      throw missing();
  };
}

export function createStageHandler(
  directories: StageDirectories,
  checkAccess: ReturnType<typeof createStageAccessCheck>,
  commit: string,
) {
  const stage = stageDirectory(directories.stageDir, "Stage frontend");
  const model = stageDirectory(directories.stageModelDir, "Stage model");
  const core = stageDirectory(directories.stageCoreDir, "Stage Core");
  if (!stage && (model || core))
    throw new Error("Build the Prometheus stage frontend before startup");
  if (
    stage &&
    !statSync(join(stage, "index.html"), { throwIfNoEntry: false })?.isFile()
  )
    throw new Error("Build the Prometheus stage frontend before startup");
  if (stage && commit !== "development") {
    const frontendBuild = JSON.parse(
      readFileSync(join(stage, "build.json"), "utf8"),
    ) as { commit: string };
    if (frontendBuild.commit !== commit)
      throw new Error("Stage frontend/server commit mismatch");
  }
  return async (
    request: IncomingMessage,
    response: ServerResponse,
    rawPath: string,
  ) => {
    if (!stage) throw missing();
    checkAccess(request, rawPath);

    response.setHeader("cross-origin-resource-policy", "same-origin");
    if (rawPath === "/stage") {
      response.writeHead(302, {
        location: "/stage/",
        "cache-control": "no-store",
      });
      response.end();
      return;
    }
    let name: string;
    try {
      name =
        decodeURIComponent(rawPath.slice("/stage/".length)) || "index.html";
    } catch {
      throw missing();
    }
    let root: string | undefined = stage;
    if (name.startsWith("model/")) {
      root = model;
      name = name.slice("model/".length);
      if (!modelFiles.has(name)) throw missing();
    } else if (name.startsWith("core/")) {
      root = core;
      name = name.slice("core/".length);
      if (name !== coreScript) throw missing();
    } else if (
      name !== "index.html" &&
      name !== "build.json" &&
      name !== "cubism-LICENSE.md" &&
      !shaderPath.test(name) &&
      !/^assets\/[\w-]+\.(?:js|css|png|svg)$/.test(name)
    ) {
      throw missing();
    }
    if (!root) throw missing();
    const file = await stageFile(root, name);
    try {
      const info = await file.stat();
      if (!info.isFile()) throw missing();
      let verifiedCore: Buffer | undefined;
      if (name === coreScript) {
        if (info.size !== coreBytes) throw missing();
        const buffer = Buffer.alloc(coreBytes);
        const { bytesRead } = await file.read({ buffer, position: 0 });
        if (
          bytesRead !== coreBytes ||
          createHash("sha256").update(buffer).digest("hex") !== coreSha256
        )
          throw missing();
        verifiedCore = buffer;
      }
      response.writeHead(200, {
        "content-type": contentTypes[extname(name)]!,
        "cache-control":
          name.startsWith("assets/") && !shaderPath.test(name)
            ? "private, max-age=31536000, immutable"
            : "no-store",
        "content-length": info.size,
      });
      if (request.method === "HEAD") response.end();
      else if (verifiedCore) response.end(verifiedCore);
      else
        await pipeline(file.createReadStream({ autoClose: false }), response);
    } finally {
      await file.close();
    }
  };
}
