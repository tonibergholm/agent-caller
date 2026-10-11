/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  // assetsInlineLimit 0: fonts stay files (CSP font-src 'self'), never data: URLs.
  build: { target: "safari16", outDir: "dist", emptyOutDir: true, assetsInlineLimit: 0 },
  resolve: process.env.VITEST ? { conditions: ["browser"] } : undefined,
  test: { environment: "jsdom", include: ["src/**/*.test.ts"], setupFiles: ["src/test-setup.ts"] },
});
