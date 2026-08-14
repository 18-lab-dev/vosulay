import { defineConfig, externalizeDepsPlugin } from 'electron-vite';
import { resolve } from 'node:path';

export default defineConfig({
    main: {
        plugins: [externalizeDepsPlugin()],
        build: {
            rollupOptions: {
                input: resolve(__dirname, 'src/main/index.ts'),
                // The Linux addon is loaded from extraResources at runtime; keep
                // its package name external when developing outside a packaged app.
                external: ['@vosu/overlay-node']
            }
        }
    },
    preload: {
        plugins: [externalizeDepsPlugin()],
        build: {
            rollupOptions: {
                input: resolve(__dirname, 'src/preload/index.ts')
            }
        }
    }
});
