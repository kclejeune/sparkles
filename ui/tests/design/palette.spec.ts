// The command palette (ROADMAP item 631) on every page of the seeded server
// (`node scripts/seed-design.mjs`, SPARKLES_DESIGN_URL): open at the large, medium and phone
// widths in both themes with no sideways overflow, shots of each to SPARKLES_DESIGN_OUT, and
// each source answering on real data. Run with `pnpm exec playwright test --config
// playwright.design.config.ts palette`.

import { expect, test, type Page } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const BASE = process.env.SPARKLES_DESIGN_URL ?? 'http://127.0.0.1:36320';
const OUT = resolve(
  process.env.SPARKLES_DESIGN_OUT ?? join(import.meta.dirname, '../../../.artifacts/design'),
  'palette',
);
// the pages of capture.spec.ts, which a spec cannot import from another spec
const PAGES = [
  ['query', '/ui/query?ds=demo'],
  ['explore', '/ui/explore?ds=demo'],
  ['ontology', '/ui/ontology?ds=go'],
  ['thesaurus', '/ui/thesaurus?ds=unesco'],
  ['similar', '/ui/similar?ds=demo'],
  ['memory', '/ui/memory?ds=demo'],
  ['embedding', '/ui/similar/map?ds=demo'],
  ['datasets', '/ui/datasets'],
  ['dataset', '/ui/datasets/demo'],
  ['merge', '/ui/datasets/demo/merge?source=draft-places'],
  ['backups', '/ui/admin/backups'],
  ['server', '/ui/admin/server'],
  ['tokens', '/ui/admin/tokens'],
].map(([name, path]) => ({ name, path }));
const WIDTHS = { large: [1440, 900], medium: [900, 1000], phone: [390, 844] } as const;

const search = (page: Page) => page.locator('.scope-search input[role="combobox"]');
const list = (page: Page) => page.getByRole('listbox', { name: 'Search results' });

async function open(page: Page, phone: boolean, text = '') {
  if (phone) await page.getByRole('button', { name: 'Open search' }).click();
  else await page.keyboard.press('ControlOrMeta+k');
  await expect(search(page)).toBeFocused();
  if (text) await page.keyboard.type(text);
  await expect(list(page).getByRole('option').first()).toBeVisible({ timeout: 90_000 });
}

test.describe.configure({ mode: 'serial' });

test('opens on every page at every width', async ({ browser }) => {
  test.setTimeout(600_000);
  mkdirSync(OUT, { recursive: true });
  const report: Record<string, { docOverflow: number; options: number }> = {};
  for (const p of PAGES)
    for (const [w, [width, height]] of Object.entries(WIDTHS))
      for (const theme of ['light', 'dark'] as const) {
        const ctx = await browser.newContext({
          viewport: { width, height },
          colorScheme: theme,
          isMobile: w === 'phone',
          hasTouch: w === 'phone',
        });
        await ctx.addInitScript((t) => {
          localStorage.setItem('sparkles.theme', t);
          localStorage.setItem('sparkles.dataset', 'demo');
        }, theme);
        const page = await ctx.newPage();
        await page.goto(BASE + p.path);
        await expect(page.locator('main h1')).toBeAttached({ timeout: 60_000 });
        await open(page, w === 'phone');
        const m = await page.evaluate(() => ({
          docOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
          options: document.querySelectorAll('[role="listbox"] [role="option"]').length,
        }));
        report[`${p.name}-${w}-${theme}`] = m;
        expect(m.docOverflow, `${p.name} ${w} ${theme}`).toBeLessThanOrEqual(0);
        await page.screenshot({ path: join(OUT, `${p.name}-${w}-${theme}.png`) });
        await ctx.close();
      }
  writeFileSync(join(OUT, 'report.json'), JSON.stringify(report, null, 2));
});

test('each source answers on real data', async ({ browser }) => {
  test.setTimeout(240_000);
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await ctx.addInitScript(() => localStorage.setItem('sparkles.dataset', 'demo'));
  const page = await ctx.newPage();
  const shot = (name: string) => page.screenshot({ path: join(OUT, `source-${name}.png`) });

  // GO's classes through the server's typo-tolerant ranking; the first search builds the
  // index, which takes a while on a debug build
  await page.request.get(`${BASE}/$/ontology/go/search?q=mito&limit=1`, { timeout: 120_000 });
  await page.goto(`${BASE}/ui/ontology?ds=go`);
  await expect(page.locator('main h1')).toBeAttached({ timeout: 60_000 });
  await open(page, false, '@mitochondrion inher');
  await expect(
    list(page).getByRole('option', { name: /^mitochondrion inheritance/ }),
  ).toBeVisible();
  await shot('entities-go');
  await page.keyboard.press('Escape');
  // the ontology editor's own commands and its branches
  await open(page, false, '>new class');
  await expect(list(page).getByRole('option', { name: /^New class/ })).toBeVisible();
  await page.keyboard.press('Escape');
  await page.keyboard.press('Escape');

  // UNESCO's concepts
  await page.goto(`${BASE}/ui/thesaurus?ds=unesco`);
  await expect(page.locator('main h1')).toBeAttached({ timeout: 60_000 });
  await open(page, false, '@educaton');
  await expect(
    list(page)
      .getByRole('option', { name: /^Education/ })
      .first(),
  ).toBeVisible();
  await shot('entities-unesco');
  await page.keyboard.press('Escape');

  // the demo dataset's stored queries and branches
  await page.goto(`${BASE}/ui/query?ds=demo`);
  await expect(page.locator('main h1')).toBeAttached({ timeout: 60_000 });
  await open(page, false, '?');
  await expect(list(page).getByRole('option', { name: /^people-by-city/ })).toBeVisible();
  await shot('saved-demo');
  // the first Escape leaves the `?` mode, the second closes the palette
  await page.keyboard.press('Escape');
  await page.keyboard.press('Escape');
  await open(page, false, '#');
  await expect(list(page).getByRole('group', { name: 'Branches of demo' })).toBeVisible();
  await expect(list(page).getByRole('option', { name: /^draft-places/ })).toBeVisible();
  await shot('datasets-branches');
  await ctx.close();
});
