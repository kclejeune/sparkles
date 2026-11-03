// Layout review shots of every page at the large, medium and phone widths in both themes,
// against a server that `node scripts/seed-design.mjs` serves (SPARKLES_DESIGN_URL). Not a
// test suite: `pnpm design:capture` writes the images to SPARKLES_DESIGN_OUT (default
// ../.artifacts/design) and a JSON report of each page's horizontal overflow and timing.

import { test, type Page } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const BASE = process.env.SPARKLES_DESIGN_URL ?? 'http://127.0.0.1:36320';
const OUT = resolve(
  process.env.SPARKLES_DESIGN_OUT ?? join(import.meta.dirname, '../../../.artifacts/design'),
);
const ONLY = process.env.SPARKLES_DESIGN_PAGES?.split(',');

export const PAGES: { name: string; path: string; ready?: string }[] = [
  { name: 'query', path: '/ui/query?ds=demo' },
  { name: 'explore', path: '/ui/explore?ds=demo' },
  { name: 'ontology', path: '/ui/ontology?ds=go' },
  { name: 'thesaurus', path: '/ui/thesaurus?ds=unesco' },
  { name: 'similar', path: '/ui/similar?ds=demo' },
  { name: 'memory', path: '/ui/memory?ds=demo' },
  { name: 'embedding', path: '/ui/embedding?ds=demo' },
  { name: 'datasets', path: '/ui/datasets' },
  { name: 'dataset', path: '/ui/datasets/demo' },
  { name: 'merge', path: '/ui/datasets/demo/merge?source=draft-places' },
  { name: 'backups', path: '/ui/backups' },
  { name: 'server', path: '/ui/server' },
  { name: 'tokens', path: '/ui/tokens' },
  { name: 'login', path: '/ui/login' },
];

const WIDTHS = { large: [1440, 900], medium: [900, 1000], phone: [390, 844] } as const;
const THEMES = ['light', 'dark'] as const;

async function measure(page: Page) {
  return page.evaluate(() => {
    const doc = document.documentElement;
    const main = document.querySelector('main');
    const fcp = performance.getEntriesByName('first-contentful-paint')[0]?.startTime ?? null;
    return {
      docOverflow: doc.scrollWidth - doc.clientWidth,
      mainOverflow: main ? main.scrollWidth - main.clientWidth : 0,
      fcp,
    };
  });
}

test.describe.configure({ mode: 'serial' });

for (const p of PAGES) {
  if (ONLY && !ONLY.includes(p.name)) continue;
  test(p.name, async ({ browser }) => {
    test.setTimeout(240_000);
    const report: Record<string, unknown> = {};
    for (const [wname, [w, h]] of Object.entries(WIDTHS)) {
      for (const theme of THEMES) {
        const ctx = await browser.newContext({
          viewport: { width: w, height: h },
          colorScheme: theme,
          deviceScaleFactor: 1,
          hasTouch: wname === 'phone',
          isMobile: wname === 'phone',
        });
        const page = await ctx.newPage();
        await page.goto(BASE + p.path);
        await page.waitForLoadState('networkidle', { timeout: 20_000 }).catch(() => {});
        await page.waitForTimeout(800);
        const dir = join(OUT, wname);
        mkdirSync(dir, { recursive: true });
        await page.screenshot({ path: join(dir, `${p.name}-${theme}.png`), fullPage: false });
        report[`${wname}-${theme}`] = await measure(page);
        await ctx.close();
      }
    }
    mkdirSync(OUT, { recursive: true });
    writeFileSync(join(OUT, `${p.name}.json`), JSON.stringify(report, null, 2));
  });
}
