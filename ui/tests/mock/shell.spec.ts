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
      new RegExp(`/ui/browse\\?ds=uni&iri=${encodeURIComponent(`${UNI}Student`)}`),
    );
    await expect(list).toHaveCount(0);
  });

  test('a query that matches nothing says so', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/admin/server');
    await search(page).click();
    await page.keyboard.type('zzzzqqq');
    await expect(page.getByRole('listbox', { name: 'Search results' })).toContainText(
      'Nothing in uni matches “zzzzqqq”.',
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
    await visit(page, '/ui/explore?ds=uni&tab=search');
    const find = page.getByRole('textbox', { name: 'Full-text query' });
    await find.click();
    await page.keyboard.type('a/b');
    await expect(find).toHaveValue('a/b');
    await expect(search(page)).not.toBeFocused();
  });

  test("Explore's resource finder is the top bar's search", async ({ page }) => {
    await onDataset(page, 'uni');
    // the text-search tab has a field of its own, which `/` focuses
    await visit(page, '/ui/explore?ds=uni&tab=search');
    await page.locator('body').click();
    await page.keyboard.press('/');
    await expect(page.getByRole('textbox', { name: 'Full-text query' })).toBeFocused();
    // the graph view has none: `/` opens the top bar, which names what it finds
    await visit(page, '/ui/explore?ds=uni');
    await page.locator('body').click();
    await page.keyboard.press('/');
    const find = page.getByPlaceholder(/^Find a resource in uni/);
    await expect(find).toBeFocused();
    await find.fill('Professor');
    const hits = page.getByRole('group', { name: 'Resources in uni' });
    await expect(hits.getByRole('option').first()).toBeVisible();
    await hits.getByRole('option').first().click();
    await expect(page.getByRole('complementary', { name: 'Resource details' })).toBeVisible();
  });

  test('the resource finder keeps Open when the label search fails, and offers it once late prefixes arrive', async ({
    page,
  }) => {
    await onDataset(page, 'uni');
    // the prefixes answer late, with one the mock does not otherwise know
    let release = () => {};
    const ready = new Promise<void>((r) => (release = r));
    await page.route('**/$/prefixes/uni', async (route) => {
      await ready;
      await route.fulfill({ json: { prefixes: { late: UNI } } });
    });
    // and the label search fails
    await page.route('**/uni/sparql*', async (route) => {
      if (/LCASE/.test(route.request().postData() ?? '')) await route.fulfill({ status: 500 });
      else await route.continue();
    });
    await visit(page, '/ui/explore?ds=uni');
    await page.locator('body').click();
    await page.keyboard.press('/');
    const find = page.getByPlaceholder(/^Find a resource in uni/);
    await find.fill(`<${UNI}Professor>`);
    const hits = page.getByRole('group', { name: 'Resources in uni' });
    await expect(hits.getByRole('option', { name: /^Open / })).toBeVisible();
    // a prefixed name opens once its prefix is known, without typing again
    await find.fill('late:Professor');
    await expect(hits.getByRole('option', { name: /^Open / })).toHaveCount(0);
    release();
    await expect(hits.getByRole('option', { name: /^Open / })).toBeVisible();
  });

  test('the Options button says when a query leaves out the inferences', async ({ page }) => {
    await onDataset(page, 'uni');
    // uni reasons, as far as the page knows
    await page.route('**/$/datasets', async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      for (const d of body.datasets ?? [])
        if (d['ds.name'] === '/uni' || d.name === 'uni')
          d.reasoning = { profile: 'rdfs', inferred: 12, at: new Date().toISOString() };
      await route.fulfill({ response: res, json: body });
    });
    await visit(page, '/ui/query?ds=uni');
    const options = page.getByRole('button', { name: /^Options\b/ });
    await expect(options).not.toContainText('no inferences');
    await options.click();
    await page.getByRole('checkbox', { name: 'Use inferences' }).uncheck();
    await page.keyboard.press('Escape');
    await expect(options).toContainText('no inferences');
  });

  test('Enter waits for the results of what the field now says', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/datasets');
    await page.keyboard.press('Control+k');
    await page.keyboard.type('pers');
    await expect(page.getByRole('option', { name: /Person/ }).first()).toBeVisible();
    // the next answer is slow, so the rows of `pers` are still shown when Enter is pressed
    await page.route('**/$/ontology/uni/search?*', async (r) => {
      await new Promise((f) => setTimeout(f, 1200));
      await r.continue();
    });
    await search(page).fill('stud');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(
      new RegExp(`/ui/browse\\?ds=uni&iri=${encodeURIComponent(`${UNI}Student`)}`),
    );
  });

  test('a pending Enter is dropped when the query changes or the search closes', async ({
    page,
  }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/datasets');
    await page.route('**/$/ontology/uni/search?*', async (r) => {
      await new Promise((f) => setTimeout(f, 1200));
      await r.continue();
    });
    await page.keyboard.press('Control+k');
    await page.keyboard.type('stud');
    await page.keyboard.press('Enter');
    // an edit asks again, and its results wait for another Enter
    await page.keyboard.type('ent');
    await expect(page.getByRole('option', { name: /^Student\b/ })).toBeVisible();
    await page.waitForTimeout(400);
    await expect(page).toHaveURL(/\/ui\/datasets$/);

    await search(page).fill('pers');
    await page.keyboard.press('Enter');
    // a click outside the search, away from its results
    await page.mouse.click(1400, 860);
    await expect(page.getByRole('listbox', { name: 'Search results' })).toHaveCount(0);
    await page.waitForTimeout(1800);
    await expect(page).toHaveURL(/\/ui\/datasets$/);
  });

  test('the arrow keys keep the active result in view', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 340 });
    await onDataset(page, 'uni');
    await visit(page, '/ui/datasets');
    await page.keyboard.press('Control+k');
    // entities only, eight of them
    await page.keyboard.type('@a');
    const options = page.getByRole('listbox', { name: 'Search results' }).getByRole('option');
    await expect(options).toHaveCount(8);
    for (let i = 0; i < 7; i++) await page.keyboard.press('ArrowDown');
    await expect(options.nth(7)).toHaveAttribute('aria-selected', 'true');
    await expect(options.nth(7)).toBeInViewport({ ratio: 1 });
  });

  test('a provider can suggest items before anything is typed', async ({ page }) => {
    await visit(page, '/ui/admin/server');
    // the mock runs under Vite, which serves the app's own instance of the module
    await page.evaluate(async () => {
      const path = '/ui/src/lib/launcher.svelte.ts';
      const { launcher } = await import(/* @vite-ignore */ path);
      launcher.register({
        id: 'recent',
        label: 'Recent',
        order: 0,
        search: async (q: string) =>
          q ? [] : [{ id: 'r1', title: 'Datasets', href: '/ui/datasets' }],
      });
    });
    await page.keyboard.press('Control+k');
    const list = page.getByRole('listbox', { name: 'Search results' });
    await expect(list.getByRole('group', { name: 'Recent' })).toBeVisible();
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/datasets$/);
  });

  test("Open in Explore from an entity's page shows it in the graph", async ({ page }) => {
    await visit(page, '/ui/explore?ds=uni&tab=schema');
    await expect(page.getByRole('tab', { name: /Schema/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await page.keyboard.press('Control+k');
    await page.keyboard.type('ada love');
    // Explore's own resources come first; the entity search opens the entity's page
    await page
      .getByRole('group', { name: /^Entities/ })
      .getByRole('option', { name: /^Ada Lovelace/ })
      .click();
    await expect(page).toHaveURL(/\/ui\/browse\?ds=uni&iri=/);
    await page.getByRole('link', { name: 'Open in Explore' }).click();
    await expect(page).toHaveURL(/\/ui\/explore\?ds=uni&iri=/);
    await expect(page).not.toHaveURL(/tab=schema/);
    await expect(page.getByRole('tab', { name: /Graph/ })).toHaveAttribute('aria-selected', 'true');
  });

  test('Ctrl+K reaches the search from the query editor', async ({ page }) => {
    await visit(page, '/ui/query');
    await page.locator('.cm-content').click();
    await page.keyboard.press('Control+k');
    await expect(search(page)).toBeFocused();
  });

  test('the rail names every page and marks the open one', async ({ page }) => {
    await visit(page, '/ui/admin/server');
    const rail = page.getByRole('navigation', { name: 'Pages' });
    for (const name of ['Query', 'Explore', 'Ontology', 'Thesaurus', 'Similar', 'Memory'])
      await expect(rail.getByRole('link', { name, exact: true })).toBeVisible();
    await expect(rail.getByRole('link', { name: 'Admin', exact: true })).toHaveAttribute(
      'aria-current',
      'page',
    );
    await expect(rail.getByText('Admin', { exact: true })).toBeVisible();
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
    await visit(page, '/ui/admin/server');
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
    await visit(page, '/ui/admin/server');
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
    await expect(page).toHaveURL(/\/ui\/browse\?ds=uni&iri=/);
    await expect(search(page)).toBeHidden();
  });

  test('the drawer holds the dataset switcher and every page', async ({ page }) => {
    await visit(page, '/ui/admin/server');
    await expect(page.getByRole('navigation', { name: 'Pages' })).toBeHidden();
    await page.getByRole('button', { name: 'Menu' }).click();
    const drawer = page.getByRole('dialog', { name: 'Menu' });
    await expect(drawer).toBeVisible();
    await expectNoSidewaysScroll(page, 'the drawer');
    await drawer.locator('.ds-button').click();
    await expect(drawer.getByRole('listbox', { name: 'Datasets' })).toBeVisible();
    // the first Escape closes the switcher's list, the drawer stays open
    await page.keyboard.press('Escape');
    await expect(drawer.getByRole('listbox', { name: 'Datasets' })).toBeHidden();
    await expect(drawer).toBeVisible();
    await expect(drawer.locator('.ds-button')).toBeFocused();
    for (const name of [
      'Query',
      'Explore',
      'Browse',
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
  '/ui/browse?ds=uni',
  '/ui/browse?ds=uni&iri=http%3A%2F%2Fexample.org%2Funi%23Professor',
  '/ui/ontology?ds=uni',
  '/ui/thesaurus?ds=uni',
  '/ui/similar?ds=foaf',
  '/ui/memory?ds=foaf',
  '/ui/embedding?ds=foaf',
  '/ui/datasets',
  '/ui/datasets/foaf',
  '/ui/admin/backups',
  '/ui/admin/server',
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
