// The app shell (ROADMAP item 632): the top bar's search and its keys, the rail at large and
// medium widths, the phone's expanding search and drawer, keyboard focus, and no sideways
// scroll on any page at 1440, 900 and 390 pixels.

import { expect, test, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

const UNI = 'http://example.org/uni#';
const LARGE = { width: 1440, height: 900 };
const MEDIUM = { width: 900, height: 900 };
const PHONE = { width: 390, height: 844 };

const search = (page: Page) => page.getByRole('combobox', { name: /^Search/ });

async function onDataset(page: Page, ds: string) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
}

/** Opens `url` and waits for the app to start, so that its keys answer. */
async function visit(page: Page, url: string) {
  await page.goto(url);
  await expect(page.locator('main h1')).toBeAttached();
}

test.describe('at 1440 pixels', () => {
  test.use({ viewport: LARGE });

  test('Ctrl+K searches the entities of the open dataset and opens a hit', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/datasets');
    await expect(page.locator('.ds-button')).toContainText('uni');
    await page.keyboard.press('Control+k');
    await expect(search(page)).toBeFocused();
    await page.keyboard.type('stud');
    const list = page.getByRole('listbox', { name: 'Search results' });
    await expect(list.getByRole('group', { name: 'Entities in uni' })).toBeVisible();
    const hit = list.getByRole('option', { name: /^Student\b/ });
    await expect(hit).toHaveAttribute('aria-selected', 'true');
    await expect(hit.locator('mark')).toHaveText('Stud');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(
      new RegExp(`/ui/ontology\\?ds=uni&iri=${encodeURIComponent(`${UNI}Student`)}`),
    );
    await expect(list).toHaveCount(0);
  });

  test('a query that matches nothing says so', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/server');
    await search(page).click();
    await page.keyboard.type('zzzzqqq');
    await expect(page.getByRole('listbox', { name: 'Search results' })).toContainText(
      'No entity in uni matches',
    );
  });

  test('/ focuses the search outside a text field, Escape gives the focus back', async ({
    page,
  }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/datasets');
    const refresh = page.getByRole('button', { name: 'Refresh' });
    await refresh.focus();
    await page.keyboard.press('/');
    await expect(search(page)).toBeFocused();
    await expect(search(page)).toHaveValue('');
    await page.keyboard.type('pers');
    await expect(page.getByRole('option', { name: /Person/ }).first()).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(refresh).toBeFocused();
    await expect(page.getByRole('listbox', { name: 'Search results' })).toHaveCount(0);

    // in a text field `/` is a character
    await visit(page, '/ui/explore?ds=uni');
    const find = page.getByRole('combobox', { name: /Find a resource/ });
    await find.click();
    await page.keyboard.type('a/b');
    await expect(find).toHaveValue('a/b');
    await expect(search(page)).not.toBeFocused();
  });

  test('Ctrl+K reaches the search from the query editor', async ({ page }) => {
    await visit(page, '/ui/query');
    await page.locator('.cm-content').click();
    await page.keyboard.press('Control+k');
    await expect(search(page)).toBeFocused();
  });

  test('the rail names every page and marks the open one', async ({ page }) => {
    await visit(page, '/ui/server');
    const rail = page.getByRole('navigation', { name: 'Pages' });
    for (const name of ['Query', 'Explore', 'Ontology', 'Thesaurus', 'Similar', 'Memory'])
      await expect(rail.getByRole('link', { name, exact: true })).toBeVisible();
    await expect(rail.getByRole('link', { name: 'Server', exact: true })).toHaveAttribute(
      'aria-current',
      'page',
    );
    await expect(rail.getByText('Server', { exact: true })).toBeVisible();
  });

  test('choosing a dataset reopens a page that names its dataset', async ({ page }) => {
    await visit(page, '/ui/ontology?ds=uni');
    await expect(page.getByRole('heading', { name: 'Ontology', exact: true })).toBeVisible();
    await page.locator('.ds-button').click();
    await page
      .getByRole('listbox', { name: 'Datasets' })
      .getByRole('option', { name: /^foaf / })
      .click();
    await expect(page).toHaveURL(/\/ui\/ontology\?ds=foaf$/);
    await expect(page.locator('.ds-button')).toContainText('foaf');
  });

  test('Tab reaches the top bar and the rail with a visible focus', async ({ page }) => {
    await visit(page, '/ui/datasets');
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    const seen: string[] = [];
    for (let i = 0; i < 18; i++) {
      await page.keyboard.press('Tab');
      const f = await page.evaluate(() => {
        const e = document.activeElement as HTMLElement | null;
        if (!e || e === document.body) return null;
        const cs = getComputedStyle(e);
        const shown =
          (cs.boxShadow && cs.boxShadow !== 'none') ||
          (cs.outlineStyle !== 'none' && cs.outlineWidth !== '0px') ||
          getComputedStyle(e.closest('.field') ?? e).boxShadow !== 'none';
        return {
          name: e.getAttribute('aria-label') ?? e.textContent?.trim() ?? e.tagName,
          shown,
        };
      });
      if (!f) continue;
      expect(f.shown, `the focus on ${f.name} is visible`).toBe(true);
      seen.push(f.name);
    }
    for (const name of ['Skip to content', 'Sparkles home', 'Datasets'])
      expect(
        seen.some((n) => n.includes(name)),
        `${name} in ${seen.join(', ')}`,
      ).toBe(true);
    expect(seen.some((n) => n.startsWith('Theme'))).toBe(true);
  });
});

test.describe('at 900 pixels', () => {
  test.use({ viewport: MEDIUM });

  test('the rail shows icons and keeps each page name', async ({ page }) => {
    await visit(page, '/ui/server');
    const rail = page.getByRole('navigation', { name: 'Pages' });
    const query = rail.getByRole('link', { name: 'Query', exact: true });
    await expect(query).toBeVisible();
    await expect(query).toHaveAttribute('title', 'Query');
    const box = await rail.boundingBox();
    expect(box!.width).toBeLessThan(60);
    await expect(search(page)).toBeVisible();
  });
});

test.describe('at 390 pixels', () => {
  test.use({ viewport: PHONE, hasTouch: true, isMobile: true });

  test('the search button expands the search over the bar', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/datasets');
    const opener = page.getByRole('button', { name: 'Open search' });
    await expect(opener).toBeVisible();
    await expect(search(page)).toBeHidden();
    await opener.click();
    await expect(search(page)).toBeFocused();
    const field = await search(page).boundingBox();
    expect(field!.width).toBeGreaterThan(260);
    await page.keyboard.type('stud');
    await expect(page.getByRole('option', { name: /^Student\b/ })).toBeVisible();
    await expectNoSidewaysScroll(page, 'the expanded search');
    await page.getByRole('button', { name: 'Close search' }).click();
    await expect(search(page)).toBeHidden();
    await expect(opener).toBeFocused();
  });

  test('the keys expand the search, Escape collapses it', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/server');
    await page.keyboard.press('Control+k');
    await expect(search(page)).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(search(page)).toBeHidden();
    await page.keyboard.press('/');
    await expect(search(page)).toBeFocused();
    await page.keyboard.type('pers');
    await page
      .getByRole('option', { name: /Person/ })
      .first()
      .click();
    await expect(page).toHaveURL(/\/ui\/ontology\?ds=uni&iri=/);
    await expect(search(page)).toBeHidden();
  });

  test('the drawer holds the dataset switcher and every page', async ({ page }) => {
    await visit(page, '/ui/server');
    await expect(page.getByRole('navigation', { name: 'Pages' })).toBeHidden();
    await page.getByRole('button', { name: 'Menu' }).click();
    const drawer = page.getByRole('dialog', { name: 'Menu' });
    await expect(drawer).toBeVisible();
    await expectNoSidewaysScroll(page, 'the drawer');
    await drawer.locator('.ds-button').click();
    await expect(drawer.getByRole('listbox', { name: 'Datasets' })).toBeVisible();
    await drawer.locator('.ds-button').click();
    for (const name of [
      'Query',
      'Explore',
      'Ontology',
      'Thesaurus',
      'Similar',
      'Memory',
      'Datasets',
    ])
      await expect(drawer.getByRole('link', { name, exact: true })).toBeVisible();
    await expect(drawer.getByRole('img', { name: /Connected|Connecting/ })).toBeVisible();
    await drawer.getByRole('link', { name: 'Datasets', exact: true }).click();
    await expect(page).toHaveURL(/\/ui\/datasets$/);
    await expect(drawer).toBeHidden();
    await page.getByRole('button', { name: 'Menu' }).click();
    await page.keyboard.press('Escape');
    await expect(drawer).toBeHidden();
    await expect(page.getByRole('button', { name: 'Menu' })).toBeFocused();
  });
});

const PAGES = [
  '/ui/query',
  '/ui/explore?ds=foaf',
  '/ui/ontology?ds=uni',
  '/ui/thesaurus?ds=uni',
  '/ui/similar?ds=foaf',
  '/ui/memory?ds=foaf',
  '/ui/embedding?ds=foaf',
  '/ui/datasets',
  '/ui/datasets/foaf',
  '/ui/backups',
  '/ui/server',
];

for (const [name, viewport] of Object.entries({ large: LARGE, medium: MEDIUM, phone: PHONE })) {
  test(`no page scrolls sideways at ${viewport.width} pixels (${name})`, async ({ page }) => {
    test.setTimeout(120_000);
    await page.setViewportSize(viewport);
    for (const url of PAGES) {
      await visit(page, url);
      await expect(page.locator('main .spinner')).toHaveCount(0, { timeout: 15_000 });
      await expectNoSidewaysScroll(page, url);
    }
  });
}
