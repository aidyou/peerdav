import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import Components from "unplugin-vue-components/vite";
import { ElementPlusResolver } from "unplugin-vue-components/resolvers";
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// WebKitGTK sends absolute-form request targets (for example
// `GET http://127.0.0.1:1421/src/App.vue HTTP/1.1`) when the webview goes
// through a proxy path. Vite matches raw `request.url` against files, so such
// requests would 404 even though the dev server is healthy. Normalize them
// back to origin-form before Vite's middlewares run.
function normalizeDevServerRequestUrl() {
  return {
    name: "normalize-dev-server-request-url",
    configureServer(server) {
      server.middlewares.use((request, _response, next) => {
        if (request.url && /^https?:\/\//i.test(request.url)) {
          try {
            const url = new URL(request.url);
            request.url = `${url.pathname}${url.search}`;
          } catch {
            // Leave malformed targets untouched so Vite can report them.
          }
        }
        next();
      });
    },
  };
}

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [
    normalizeDevServerRequestUrl(),
    vue(),
    Components({ resolvers: [ElementPlusResolver()], dts: false }),
  ],

  test: {
    environment: "jsdom",
    include: ["src/**/*.test.js"],
    // Element Plus ships CSS alongside each component; inline it so Vitest
    // transforms style imports instead of loading them through Node.
    server: { deps: { inline: ["element-plus"] } },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1421,
    strictPort: true,
    host: host || "127.0.0.1",
    // Desktop state must not be torn down by Vite's full-reload fallback or
    // websocket reconnect. Source changes are applied by an explicit refresh.
    hmr: false,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri` and development state.
      // `dev_data` contains test fixtures with intentionally looping symlinks.
      ignored: ["**/src-tauri/**", "**/dev_data/**", "**/.codegraph/**", "**/.cs/**", "**/dist/**"],
    },
  },
}));
