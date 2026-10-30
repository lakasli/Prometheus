import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
export default defineConfig({
  plugins: [vue()],
  base: "/admin/",
  build: { sourcemap: false },
  server: {
    host: "127.0.0.1",
    proxy: {
      "/api": "http://127.0.0.1:8419",
      "/health": "http://127.0.0.1:8419",
    },
  },
});
