import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Dev: a SPA roda em :5173 e encaminha /api para o servidor Rust (127.0.0.1:3000).
// O guard do servidor aceita só Host 127.0.0.1:3000, localhost:3000 ou [::1]:3000 e Origin
// igual a http://<Host>. Por isso `changeOrigin: true` reescreve o Host para o alvo e o
// `Origin` (enviado pelo navegador como http://localhost:5173) é reescrito para o alvo.
// SSE (/api/events) também passa pelo proxy: o Vite faz streaming de respostas HTTP sem
// buffer. Em produção a SPA é same-origin e não há proxy.
const API_TARGET = "http://127.0.0.1:3000";

export default defineConfig({
  base: "/",
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist",
    sourcemap: false,
    assetsDir: "assets",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api": {
        target: API_TARGET,
        changeOrigin: true,
        configure: (proxy) => {
          proxy.on("proxyReq", (proxyReq) => {
            if (proxyReq.getHeader("origin")) {
              proxyReq.setHeader("origin", API_TARGET);
            }
          });
        },
      },
    },
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    // O jsdom se apresenta em en-US: fixa pt-BR para a suíte (spec 101, C-05).
    setupFiles: ["src/test-setup.ts"],
    restoreMocks: true,
    // A suíte roda 49+ arquivos em paralelo: 5 s por teste é justo sob carga.
    testTimeout: 20_000,
    hookTimeout: 20_000,
  },
});
