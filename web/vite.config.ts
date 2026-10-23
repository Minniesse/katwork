/// <reference types="vitest/config" />
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  build: {
    // The server's CSP is `default-src 'self'`, so no asset may be inlined as a data: URI.
    assetsInlineLimit: 0,
  },
  server: {
    proxy: { '/api': 'http://localhost:3000' },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test-setup.ts'],
    include: ['src/**/*.test.{ts,tsx}', 'scripts/**/*.test.ts'],
    // Role queries in jsdom are slow; leave headroom for loaded CI runners.
    testTimeout: 20_000,
  },
});
