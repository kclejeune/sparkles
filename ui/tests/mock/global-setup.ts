// Refuses to start the mock UI tests without the browser module of the formatter. The
// Pattern pane, the visual query builder and the facets that read a query's pattern have no
// server fallback, so without the module every one of those tests fails on a missing
// pattern, far from the cause. The module is git-ignored and built by `mise run ui:wasm`,
// which `mise run ui:e2e:mock` runs first. `SPARKLES_E2E_NO_WASM=1` runs the suite without
// it anyway, and the tests that need the module then fail.

import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export default function globalSetup() {
  const module = fileURLToPath(
    new URL('../../src/lib/wasm/sparkles_fmt_wasm_bg.wasm', import.meta.url),
  );
  if (existsSync(module) || process.env.SPARKLES_E2E_NO_WASM === '1') return;
  throw new Error(
    `The browser module of the formatter is missing (${module}). ` +
      'The Pattern pane and the facets tests need it. Build it with `mise run ui:wasm`, ' +
      'or run `mise run ui:e2e:mock`, which builds it first.',
  );
}
