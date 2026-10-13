import { defaultClientConditions } from "vite";
import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const devHost = process.env.OPENPISO_WEB_ADMIN_DEV_HOST ?? true;

export default defineConfig(({ mode }) => ({
  plugins: [tailwindcss(), svelte()],
  base: "/admin/",
  resolve: {
    alias: {
      $lib: path.resolve(__dirname, "./src/lib"),
    },
    // Vitest resolves with SSR conditions by default, which hands component
    // tests Svelte's server build. Add "browser" for tests only, and keep
    // Vite's defaults in every mode — dropping them makes the production
    // bundle resolve `svelte` to its server entry and the app never mounts.
    conditions:
      mode === "test"
        ? ["browser", ...defaultClientConditions]
        : [...defaultClientConditions],
  },
  // Hot-reload dev only: the app's API base is "" (see src/lib/api.ts), so it
  // fetches `/web-admin/*` from the Vite origin. Forward those to the owner
  // server on :8080 (run `bun run dev:admin` alongside) so login, dashboard,
  // and mutations work the same as the production single-origin build.
  // `changeOrigin` stays false: the owner server accepts a mutation only when
  // `Origin` matches the request `Host` (ADR-0074), and rewriting `Host` to
  // the target would break that check for every dev mutation.
  server: {
    // Expose the dev server to phones and other devices on the shop LAN.
    // Set OPENPISO_WEB_ADMIN_DEV_HOST to the admin interface address when the
    // host also connects to an untrusted/customer network.
    host: devHost,
    proxy: {
      "/web-admin": {
        target: "http://localhost:8080",
        changeOrigin: false,
      },
      "/api": {
        target: "http://localhost:8080",
        changeOrigin: false,
      },
    },
  },
  clearScreen: false,
  test: {
    setupFiles: ["./src/test/setup.ts"],
    // Must stay above the setup file's `asyncUtilTimeout`, or a slow first
    // paint trips the test timeout before the async utility can retry.
    testTimeout: 15_000,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
}));
