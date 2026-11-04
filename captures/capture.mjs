// Captures of the Browse term page for the two open questions of item 734. This file and its
// PNGs live on the captures branch and are never merged.
//
// Start the mock (`PORT=23310 node ui/mock/server.mjs`) and the UI
// (`SPARKLES_API=http://127.0.0.1:23310 VITE_FMT_WASM=off pnpm exec vite dev --port 23311`), then
// run this inside `nix develop` from the repository root: `node captures/capture.mjs`.
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, '../ui/'));
const { chromium } = require('@playwright/test');

const BASE = process.env.CAPTURE_BASE ?? 'http://127.0.0.1:23311';
const term = (id) =>
  `${BASE}/ui/browse?ds=cells&iri=${encodeURIComponent('http://purl.obolibrary.org/obo/' + id)}`;
const TERMS = { heavy: 'CL_0009001', deep: 'GO_0000432' };
const VARIANTS = {
  'cards-today': '',
  'cards-declutter': '&declutter=1',
  'flat-declutter': '&flat=1&declutter=1',
};
const VIEWS = { tree: '', graph: '&view=graph' };

const browser = await chromium.launch({ channel: 'chromium' });

/** Open the page and shoot all of it: the page scrolls inside main, so the window takes its height. */
async function shoot(url, file, width, { before, height = 900, scale = 2, whole = true } = {}) {
  const ctx = await browser.newContext({
    viewport: { width, height },
    deviceScaleFactor: scale,
    colorScheme: 'light',
  });
  const page = await ctx.newPage();
  await page.goto(url);
  await page.waitForSelector('h1.term-title', { timeout: 60_000 });
  await page.waitForLoadState('networkidle');
  // the tree's scroll limit is half the window, which a tall window would lift
  await page.addStyleTag({ content: `.tree-body { max-height: ${height / 2}px !important; }` });
  if (before) await before(page);
  if (whole) {
    const h = await page.evaluate(() =>
      Math.max(document.querySelector('main')?.scrollHeight ?? 0, document.documentElement.scrollHeight),
    );
    await page.setViewportSize({ width, height: Math.min(h + 60, 8000) });
  }
  // the graph lays itself out and fits
  await page.waitForTimeout(1800);
  await page.screenshot({ path: join(here, file) });
  await ctx.close();
  console.log(file);
}

const fitGraph = async (page) => {
  const fit = page.locator('.b-tree').getByRole('button', { name: 'Fit' });
  if (await fit.count()) {
    await page.waitForTimeout(1200);
    await fit.click();
  }
};
const openPhoneTree = async (page) => {
  const b = page.getByRole('button', { name: 'Show the hierarchy' });
  if (await b.isVisible()) await b.click();
};

for (const [variant, q] of Object.entries(VARIANTS))
  for (const [name, id] of Object.entries(TERMS))
    for (const [view, v] of Object.entries(VIEWS))
      await shoot(term(id) + q + v, `${variant}_${name}_${view}_1440.png`, 1440, {
        before: fitGraph,
      });

for (const width of [1024, 390])
  for (const [view, v] of Object.entries(VIEWS))
    await shoot(term(TERMS.heavy) + v, `implementation_heavy_${view}_${width}.png`, width, {
      before: async (page) => {
        await openPhoneTree(page);
        await fitGraph(page);
      },
    });

const expandDialog = async (page) => {
  await page.locator('.b-tree').getByRole('button', { name: 'Expand' }).click();
  await page.getByRole('dialog').locator('canvas').first().waitFor();
};
await shoot(term(TERMS.heavy) + '&view=graph', 'implementation_heavy_expand_1440.png', 1440, {
  before: expandDialog,
  whole: false,
});
await shoot(term(TERMS.heavy) + '&view=graph', 'implementation_heavy_expand_390.png', 390, {
  height: 844,
  before: expandDialog,
  whole: false,
});

await browser.close();
