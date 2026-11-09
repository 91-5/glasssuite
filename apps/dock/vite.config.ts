import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// dock 与 island 各自独立构建，产物落进同一个 dist/ 的两个子目录：
//   dist/dock/index.html  → dock 窗口加载
//   dist/island/index.html → island 窗口加载
// 这样 Rust 侧只需要一个 frontendDist，且两窗可以是完全不同的前端应用。
export default defineConfig({
  plugins: [react()],
  base: "./",
  server: { port: 1421, strictPort: true },
  build: {
    outDir: "../../dist/dock",
    emptyOutDir: true,
    target: "es2022",
  },
});
