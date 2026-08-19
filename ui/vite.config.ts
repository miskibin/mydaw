import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vitest/config';
import solid from 'vite-plugin-solid';

// Tauri v2 conventions: fixed dev port, never fall back to another one,
// and keep the Rust/cargo output visible in the terminal.
export default defineConfig({
  plugins: [tailwindcss(), solid()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: false,
    watch: {
      // The Rust side of the workspace is owned by other tooling; ignore it.
      ignored: ['**/src-tauri/**', '**/target/**', '**/crates/**', '**/plugins/**'],
    },
  },
  build: {
    outDir: 'dist',
    target: 'chrome110',
    sourcemap: true,
    emptyOutDir: true,
  },
  test: {
    include: ['src/**/*.test.ts'],
    environment: 'node',
  },
});
