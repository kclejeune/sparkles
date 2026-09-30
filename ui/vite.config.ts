import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, type ProxyOptions } from 'vite';

// Where the Sparkles backend (or `pnpm mock`) is listening.
const target = process.env.SPARKLES_API ?? 'http://localhost:3030';

// Everything that is not the UI itself (/ui/...) or Vite internals goes to the backend:
// the admin API at /$/... and every dataset endpoint at /{ds}[/...].
const proxy: Record<string, ProxyOptions> = {
  '^/(?!ui(/|$)|@|node_modules|src/|\\.svelte-kit|__vite|favicon).+': {
    target,
    changeOrigin: true,
  },
};

export default defineConfig({
  plugins: [sveltekit()],
  server: { port: 5173, proxy },
  preview: { port: 4173, proxy },
});
