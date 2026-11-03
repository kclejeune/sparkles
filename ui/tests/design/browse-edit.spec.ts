// Edit mode on the browser's pages (ROADMAP item 630) on the seeded GO and UNESCO data
// (`node scripts/seed-design.mjs`): the preview of a synonym's addition and of a parent's,
// as dry runs on the whole of GO, at the large, medium and phone widths in both themes.
// Not a test suite: `pnpm exec playwright test --config playwright.design.config.ts
// browse-edit` writes the images and a JSON report of each page's horizontal overflow and
// the dry runs' times to SPARKLES_DESIGN_OUT (default ../.artifacts/design). Nothing is
// committed.

import { expect, test, type Page } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const BASE = process.env.SPARKLES_DESIGN_URL ?? 'http://127.0.0.1:36320';
const OUT = resolve(
  process.env.SPARKLES_DESIGN_OUT ?? join(import.meta.dirname, '../../../.artifacts/design'),
);
const GO = (id: string) =>
  `/ui/browse?ds=go&iri=${encodeURIComponent(`http://purl.obolibrary.org/obo/GO_${id}`)}&edit=1`;
const UNESCO = `/ui/browse?ds=unesco&iri=${encodeURIComponent('http://vocabularies.unesco.org/thesaurus/concept4011')}&edit=1`;
const WIDTHS = { large: [1440, 900], medium: [900, 1000], phone: [390, 844] } as const;
const THEMES = ['light', 'dark'] as const;

const overflow = (page: Page) =>
  page.evaluate(() => {
    const doc = document.documentElement;
    const main = document.querySelector('main');
    const dialog = document.querySelector('dialog[open]');
    return {
      docOverflow: doc.scrollWidth - doc.clientWidth,
      mainOverflow: main ? main.scrollWidth - main.clientWidth : 0,
      dialogOverflow: dialog
        ? Math.max(0, dialog.getBoundingClientRect().right - doc.clientWidth)
        : 0,
    };
  });

/** Opens the preview of `act`, waits for the dry run's outcome and answers its time. */
async function previewOf(page: Page, act: () => Promise<void>) {
  const t0 = Date.now();
  await act();
  await expect(page.getByRole('dialog').locator('[data-outcome]')).toBeVisible({
    timeout: 60_000,
  });
  return Date.now() - t0;
}

test.describe.configure({ mode: 'serial' });

test('edit mode previews on GO and UNESCO', async ({ browser }) => {
  test.setTimeout(600_000);
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
      const dir = join(OUT, wname);
      mkdirSync(dir, { recursive: true });
      const shot = async (name: string) => {
        await page.screenshot({ path: join(dir, `${name}-${theme}.png`) });
        report[`${name}-${wname}-${theme}`] = await overflow(page);
      };

      await page.goto(BASE + GO('0005739'));
      await expect(page.getByRole('toolbar', { name: 'Edit mode' })).toBeVisible({
        timeout: 60_000,
      });
      await page
        .getByRole('textbox', { name: 'New synonym', exact: true })
        .fill('mitochondrial organelle');
      const synMs = await previewOf(page, () =>
        page
          .getByRole('form', { name: 'Add a synonym' })
          .getByRole('button', { name: 'Add' })
          .click(),
      );
      report[`synonym-dry-run-ms-${wname}-${theme}`] = synMs;
      await shot('browse-edit-preview');
      await page.getByRole('dialog').getByRole('button', { name: 'Cancel' }).click();

      const tree = page.getByRole('complementary', { name: 'Hierarchy' });
      await tree.getByRole('combobox', { name: 'Add a parent class' }).fill('membrane-bounded');
      await tree.getByRole('option').first().waitFor({ timeout: 30_000 });
      await shot('browse-edit-parent');
      await page.keyboard.press('Escape');

      await page.goto(BASE + UNESCO);
      await expect(page.getByRole('region', { name: 'Labels', exact: true })).toBeVisible({
        timeout: 60_000,
      });
      await shot('browse-edit-unesco');
      await ctx.close();
    }
  }
  mkdirSync(OUT, { recursive: true });
  writeFileSync(join(OUT, 'browse-edit.json'), JSON.stringify(report, null, 2));
});
