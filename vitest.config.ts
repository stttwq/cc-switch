import path from "node:path";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./tests/setupGlobals.ts", "./tests/setupTests.ts"],
    globals: true,
    // jsdom + userEvent 的交互用例在 CI runner 和本机并发负载下会越过默认
    // 5s；一旦超时，同文件残留的 DOM 还会让后续用例以"Found multiple
    // elements"二次失败，红了也看不出真因。
    testTimeout: 15000,
    hookTimeout: 15000,
    coverage: {
      reporter: ["text", "lcov"],
    },
  },
});
