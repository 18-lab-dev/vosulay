import { defineConfig, externalizeDepsPlugin } from "electron-vite";
import { resolve } from "node:path";

export default defineConfig({
    main: {
        build: {
            rollupOptions: {
                // The Linux addon is loaded from extraResources at runtime; keep
                // Its package name external when developing outside a packaged app.
                external: ["@vosu/overlay-node"],
                input: resolve(__dirname, "src/main/index.ts"),
            },
        },
        plugins: [externalizeDepsPlugin()],
    },
    preload: {
        build: {
            rollupOptions: {
                input: {
                    "frame-rate": resolve(__dirname, "src/preload/frame-rate.ts"),
                    index: resolve(__dirname, "src/preload/index.ts"),
                },
            },
        },
        plugins: [externalizeDepsPlugin()],
    },
});
