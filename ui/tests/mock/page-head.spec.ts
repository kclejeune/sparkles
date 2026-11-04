// The shared page head (ROADMAP item 633): at 1024 and 1440 px a page's actions start level
// with its title's first line, never under it, and leave the title most of the head. At 1024
// a Browse term page keeps Open in Explore and Open in Ontology and folds the rest into More,
// whose menu runs from the keyboard.

import { expect, test, type Page } from '@playwright/test';

test.describe.configure({ timeout: 120_000 });

const UNI = 'http://example.org/uni#';

const PAGES: [string, string][] = [
  ['memory', '/ui/memory?ds=org'],
  ['similar', '/ui/similar?ds=foaf&branch=dev&index=embedding'],
  ['similar/map', '/ui/similar/map?ds=foaf'],
  ['datasets', '/ui/datasets'],
  ['datasets/[name]', '/ui/datasets/uni'],
  ['merge', '/ui/datasets/orchard/merge?source=dev&target=main'],
  ['review/[branch]', '/ui/datasets/org/review/ingest.standup-2026-10-08-1'],
  ['workbench/ontology', '/ui/workbench/ontology?ds=uni'],
  ['workbench/thesaurus', '/ui/workbench/thesaurus?ds=thes'],
  ['admin/server', '/ui/admin/server'],
  ['admin/backups', '/ui/admin/backups'],
  ['admin/tokens', '/ui/admin/tokens'],
  ['browse term', `/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`],
  ['browse landing', '/ui/browse?ds=uni'],
];

/** The head's geometry: its title's first line, its actions and its width. */
async function head(page: Page) {
  return page
    .locator('.page-head')
    .first()
    .evaluate((h) => {
      const h1 = h.querySelector('h1')!.getBoundingClientRect();
      const lh = parseFloat(getComputedStyle(h.querySelector('h1')!).lineHeight) || h1.height;
      const a = h.querySelector(':scope > .page-actions')?.getBoundingClientRect() ?? null;
      return {
        width: h.getBoundingClientRect().width,
        h1: { top: h1.top, line: lh },
        actions: a && a.width > 0 ? { top: a.top, left: a.left, width: a.width } : null,
        headLeft: h.getBoundingClientRect().left,
      };
    });
}

for (const width of [1024, 1440]) {
  test(`at ${width} px every page's actions sit level with its title`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    for (const [name, url] of PAGES) {
      await page.goto(url);
      await expect(page.locator('.page-head h1').first()).toBeVisible({ timeout: 30_000 });
      // live pages never go quiet on the network, so the head gets a moment to settle and fold
      await page.waitForTimeout(800);
      const g = await head(page);
      if (!g.actions) continue;
      // the actions start within the title's first line
      expect(g.actions.top, `${name}: actions under the title`).toBeLessThan(
        g.h1.top + g.h1.line - 4,
      );
      expect(g.actions.top, `${name}: actions above the title`).toBeGreaterThan(g.h1.top - 12);
      // and leave the title column at least 55% of the head
      expect(g.actions.left - g.headLeft, `${name}: title squeezed`).toBeGreaterThan(
        g.width * 0.55,
      );
    }
  });
}

test('at 1024 px a term page folds Ask and Edit into More, which the keyboard runs', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1024, height: 900 });
  await page.goto(PAGES.find(([n]) => n === 'browse term')![1]);
  const actions = page.locator('.page-head > .page-actions');
  await expect(actions.getByRole('link', { name: 'Open in Explore' })).toBeVisible({
    timeout: 30_000,
  });
  await expect(actions.getByRole('link', { name: 'Open in Ontology' })).toBeVisible();
  await expect(actions.getByRole('button', { name: 'Ask about this' })).toBeHidden();
  await expect(actions.getByRole('button', { name: 'Edit', exact: true })).toBeHidden();
  const more = actions.getByRole('button', { name: 'More actions' });
  await more.focus();
  await page.keyboard.press('Enter');
  const menu = page.getByRole('menu', { name: 'More actions' });
  await expect(menu.getByRole('menuitem')).toHaveText(['Ask about this', 'Edit']);
  await expect(menu.getByRole('menuitem', { name: 'Ask about this' })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(menu.getByRole('menuitem', { name: 'Edit' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
  await expect(more).toBeFocused();
  // the menu's Edit runs the page's Edit
  await more.click();
  await menu.getByRole('menuitem', { name: 'Edit' }).click();
  await expect(page).toHaveURL(/edit=1/);
  // at 1440 nothing folds
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(PAGES.find(([n]) => n === 'browse term')![1]);
  await expect(actions.getByRole('button', { name: 'Ask about this' })).toBeVisible({
    timeout: 30_000,
  });
  await expect(actions.getByRole('button', { name: 'More actions' })).toHaveCount(0);
});
