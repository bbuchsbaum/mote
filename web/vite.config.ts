import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The built bundle is embedded into the mote binary with include_bytes!, so it
// must be a fixed set of filenames with no hashing and no code splitting.
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: "dist",
    assetsDir: ".",
    rollupOptions: {
      output: {
        entryFileNames: "console.js",
        chunkFileNames: "console.js",
        assetFileNames: "console.[ext]",
      },
    },
  },
  server: {
    port: 5173,
    proxy: { "/api": "http://127.0.0.1:7717" },
  },
});
