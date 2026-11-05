import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import vue from "@vitejs/plugin-vue";

const base = "/stage/";
const framework = fileURLToPath(
  new URL("./node_modules/.cubism/5-r.5/Framework/", import.meta.url),
);

function stageAssets(): Plugin {
  const shaderDir = join(framework, "Shaders", "WebGL");
  const assets = new Map<string, Buffer>();
  for (const name of readdirSync(shaderDir)) {
    if (!/\.(?:vert|frag)$/.test(name)) continue;
    assets.set(
      `assets/cubism-shaders/${name}`,
      readFileSync(join(shaderDir, name)),
    );
  }
  assets.set("cubism-LICENSE.md", readFileSync(join(framework, "LICENSE.md")));

  return {
    name: "prometheus-stage-assets",
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const pathname = request.url?.split("?", 1)[0];
        if (!pathname?.startsWith(base)) return next();
        const body = assets.get(pathname.slice(base.length));
        if (!body) return next();
        response.setHeader("Content-Type", "text/plain; charset=utf-8");
        response.end(body);
      });
    },
    generateBundle() {
      const commit = process.env.VITE_BUILD_SHA || "development";
      if (commit !== "development" && !/^[0-9a-f]{40}$/.test(commit))
        throw new Error("VITE_BUILD_SHA must be a complete Git SHA");
      for (const [fileName, source] of assets) {
        this.emitFile({ type: "asset", fileName, source });
      }
      this.emitFile({
        type: "asset",
        fileName: "build.json",
        source:
          JSON.stringify({ name: "Prometheus Stage", commit }, null, 2) + "\n",
      });
    },
  };
}

export default defineConfig({
  plugins: [vue(), stageAssets()],
  base,
  resolve: { alias: { "@cubism": join(framework, "compiled") } },
  build: {
    sourcemap: false,
    rolldownOptions: {
      output: {
        comments: { legal: true },
        banner:
          "/*! Includes Live2D Cubism Web Framework. Copyright(c) Live2D Inc. All rights reserved. See cubism-LICENSE.md. */",
      },
    },
  },
  server: {
    host: "127.0.0.1",
    proxy: {
      "/stage/model/": { target: "http://127.0.0.1:8419", changeOrigin: true },
      "/stage/core/": { target: "http://127.0.0.1:8419", changeOrigin: true },
    },
  },
});
