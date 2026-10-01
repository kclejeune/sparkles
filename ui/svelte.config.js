import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    // The Rust server embeds ui/build and serves it under /ui/ (and redirects / → /ui/).
    // Dataset endpoints live at the root (/{ds}/sparql), so the UI must stay under a prefix.
    adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html', strict: false }),
    paths: { base: '/ui', relative: false },
    alias: { $components: 'src/lib/components' },
    // type-check the Playwright configuration too (tests/ is already included), but not the
    // generated bindings of the browser formatter (`mise run ui:wasm`)
    typescript: {
      config: (tsconfig) => {
        tsconfig.include.push('../playwright.config.ts', '../playwright.mock.config.ts');
        tsconfig.exclude = [...(tsconfig.exclude ?? []), '../src/lib/wasm/**'];
      },
    },
  },
};

export default config;
