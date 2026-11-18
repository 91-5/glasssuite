import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 控制中心独立构建，产物落进 dist/control/index.html（与 dock/island 同级）。
// Rust 侧 WebviewUrl::App("control/index.html") 加载。
export default defineConfig({
  plugins: [react()],
  base: "./",
  server: { port: 1422, strictPort: true },
  build: {
    outDir: "../../dist/control",
    emptyOutDir: true,
    target: "es2022",
  },
});
