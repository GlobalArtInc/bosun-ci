import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

const server = process.env.BOSUN_WEB_SERVER ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: { '/api': { target: server } },
  },
  build: {
    outDir: 'dist/interface',
    emptyOutDir: true,
    assetsDir: 'assets',
    chunkSizeWarningLimit: 1536,
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['src/test/setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
    css: false,
    restoreMocks: true,
  },
});
