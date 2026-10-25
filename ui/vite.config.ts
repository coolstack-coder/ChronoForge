import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// dev: the Rust server runs on :7420, vite proxies /api to it
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: { "/api": "http://127.0.0.1:7420" },
  },
});
