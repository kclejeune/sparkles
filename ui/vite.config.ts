import { sveltekit } from '@sveltejs/kit/vite';
import type { ProxyOptions } from 'vite';
import { defineConfig } from 'vitest/config';
import { licenses } from './scripts/licenses.js';

// Where the Sparkles backend (or `pnpm mock`) is listening.
const target = process.env.SPARKLES_API ?? 'http://localhost:3030';

// Everything that is not the UI itself (/ui/...) or Vite internals goes to the backend:
// the admin API at /$/... and every dataset endpoint at /{ds}[/...]. The backend refuses
// unsafe requests from other origins, so they arrive with its own `Host` and `Origin`.
const proxy: Record<string, ProxyOptions> = {
  '^/(?!ui(/|$)|@|node_modules|src/|\\.svelte-kit|__vite|favicon).+': {
    target,
    changeOrigin: true,
    headers: { origin: new URL(target).origin },
  },
};

export default defineConfig({
  // licenses.txt: the license notices of the npm packages in the bundle
  plugins: [sveltekit(), licenses()],
  server: { port: 5173, proxy },
  preview: { port: 4173, proxy },
  // Unit tests for the pure modules under src/lib and the build scripts (`pnpm test`).
  test: { include: ['src/**/*.test.ts', 'scripts/**/*.test.ts'], environment: 'node' },
});
