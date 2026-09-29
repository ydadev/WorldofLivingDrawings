import { defineConfig } from 'vite';

const csp = [
  "default-src 'none'",
  "script-src 'self' 'wasm-unsafe-eval'",
  "style-src 'self'",
  "img-src 'self' data: blob:",
  "connect-src 'self'",
  "worker-src 'self'",
  "object-src 'none'",
  "base-uri 'none'",
  "frame-ancestors 'none'",
].join('; ');

export default defineConfig({
  base: './',
  build: { target: 'es2020', rolldownOptions: { input: ['index.html', 'fish.html', 'paint.html', 'capture.html', 'interaction.html', 'core.html'] } },
  server: { headers: { 'Content-Security-Policy': csp } },
  preview: {
    port: 4173,
    strictPort: true,
    headers: { 'Content-Security-Policy': csp },
  },
});
