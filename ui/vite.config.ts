import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  // '..' lets the dev server read ../CHANGELOG.md (bundled for the What's new dialog)
  server: { port: 1420, strictPort: true, watch: { ignored: ['**/src-tauri/**'] }, fs: { allow: ['..'] } },
  build: { outDir: 'dist', target: 'es2022', sourcemap: false },
  test: { environment: 'node', include: ['src/**/*.test.ts'] },
} as never);
