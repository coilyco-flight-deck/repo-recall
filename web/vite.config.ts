import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// JSON API + MCP host run as a separate Rust process. The dev server
// proxies those paths to it so a single browser origin works during dev.
// Production: the static bundle is served by caddy in its own container;
// the ingress routes /api, /openapi.json, and /mcp to the Rust container.
const API_TARGET = process.env.VITE_API_TARGET || "http://127.0.0.1:7777";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": { target: API_TARGET, changeOrigin: true },
      "/openapi.json": { target: API_TARGET, changeOrigin: true },
      "/mcp": { target: API_TARGET, changeOrigin: true, ws: true },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
});
