// Every tab and control of the merged pages is an address (ROADMAP item 633): a deep link
// opens it, a reload keeps it and Back returns to the view before. Browse and the Workbench
// link to each other at the same IRI.

import { expect, test, type Locator, type Page } from '@playwright/test';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';

type View = {
  url: string;
  /** What the view shows when it is open. */
  shows: (page: Page) => Locator;
  /** Leaves the view for another one of the same page. */
  leave: (page: Page) => Promise<void>;
};

const tab = (page: Page, list: string, name: string) =>
  page.getByRole('tablist', { name: list }).getByRole('tab', { name, exact: true });

const VIEWS: Record<string, View> = {
  'the ontology DL Query tab': {
    url: '/ui/workbench/ontology?ds=uni&tab=dlquery',
    shows: (p) => p.getByRole('textbox', { name: 'Class expression' }),
    leave: (p) => tab(p, 'Ontology views', 'Tree').click(),
  },
  'the ontology header tab': {
    url: '/ui/workbench/ontology?ds=uni&tab=header',
    shows: (p) => p.getByRole('heading', { name: 'Ontology header' }),
    leave: (p) => tab(p, 'Ontology views', 'Tree').click(),
  },
  'the ontology history tab': {
    url: '/ui/workbench/ontology?ds=uni&tab=history',
    shows: (p) => p.getByRole('list', { name: 'Changes' }),
    leave: (p) => tab(p, 'Ontology views', 'Tree').click(),
  },
  'the object property tree with a usage view': {
    url: `/ui/workbench/ontology?ds=uni&kind=ObjectProperty&iri=${encodeURIComponent(UNI + 'headOf')}&view=usage`,
    shows: (p) => p.getByRole('button', { name: /^Usage/, pressed: true }),
    leave: (p) => p.getByRole('button', { name: 'Frame', exact: true }).click(),
  },
  'the thesaurus validation tab': {
    url: '/ui/workbench/thesaurus?ds=thes&tab=validation',
    shows: (p) => p.getByRole('list', { name: 'Check results' }),
    leave: (p) => tab(p, 'Thesaurus views', 'Tree').click(),
  },
  'the thesaurus collections list': {
    url: '/ui/workbench/thesaurus?ds=thes&kind=collections',
    shows: (p) => p.getByRole('list', { name: 'Collections' }),
    leave: (p) => p.getByRole('combobox', { name: 'List' }).selectOption('concepts').then(),
  },
  'the thesaurus history tab': {
    url: '/ui/workbench/thesaurus?ds=thes&tab=history',
    shows: (p) => p.getByRole('list', { name: 'Changes' }),
    leave: (p) => tab(p, 'Thesaurus views', 'Validation').click(),
  },
  "the backups page's repositories tab": {
    url: '/ui/admin/backups?tab=repositories',
    shows: (p) => p.getByRole('tab', { name: /Repositories/, selected: true }),
    leave: (p) => p.getByRole('tab', { name: /Snapshots/ }).click(),
  },
  "Similar's map": {
    url: '/ui/similar/map?ds=foaf',
    shows: (p) => p.getByRole('heading', { name: 'Embedding map' }),
    leave: (p) =>
      p
        .getByRole('navigation', { name: 'Pages' })
        .getByRole('link', { name: 'Search', exact: true })
        .click(),
  },
};

for (const [name, v] of Object.entries(VIEWS)) {
  test(`${name} opens from its address, survives a reload and comes back on Back`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(v.url);
    await expect(v.shows(page)).toBeVisible({ timeout: 30_000 });
    await page.reload();
    await expect(v.shows(page)).toBeVisible({ timeout: 30_000 });
    const at = page.url();
    await v.leave(page);
    await expect(page).not.toHaveURL(at);
    await page.goBack();
    await expect(page).toHaveURL(at);
    await expect(v.shows(page)).toBeVisible({ timeout: 30_000 });
  });
}

test('a term page opens in the Workbench at the same IRI, and back to its page', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
  await page.getByRole('link', { name: 'Open in Ontology' }).click();
  await expect(page).toHaveURL(/\/ui\/workbench\/ontology\?ds=uni&iri=.*Professor/);
  // the tree opens the path to the selection and marks it
  const tree = page.getByRole('tree', { name: 'Classes (asserted)' });
  await expect(tree.locator('[aria-selected="true"]')).toContainText('Professor', {
    timeout: 30_000,
  });
  await page.getByRole('link', { name: 'Open page' }).click();
  await expect(page).toHaveURL(/\/ui\/browse\?ds=uni&iri=.*Professor/);
  await expect(page.getByRole('heading', { level: 1 })).toContainText(/Professor/);
});

test('Open page keeps the session branch the editor works on', async ({ page, request }) => {
  const made = await request.post('/$/branches/uni', { data: { name: 'dr-link' } });
  expect(made.status()).toBe(201);
  try {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(
      `/ui/workbench/ontology?ds=uni&branch=dr-link&iri=${encodeURIComponent(UNI + 'Professor')}`,
    );
    const open = page.getByRole('link', { name: 'Open page' });
    await expect(open).toBeVisible({ timeout: 30_000 });
    await expect(open).toHaveAttribute('href', /[?&]branch=dr-link(&|$)/);
    await open.click();
    await expect(page).toHaveURL(/\/ui\/browse\?.*branch=dr-link/);
  } finally {
    await request.delete('/$/branches/uni/dr-link?force=true');
  }
});

test('Similar terms on a session branch link to Similar by dataset and branch', async ({
  page,
  request,
}) => {
  const made = await request.post('/$/branches/uni', { data: { name: 'dr-sim' } });
  expect(made.status()).toBe(201);
  try {
    // the mock has no vector index on uni, so the index and its neighbours are stubbed
    await page.route('**/$/vector/uni**', (route) =>
      route.fulfill({
        json: {
          indexes: [
            {
              name: 'terms',
              predicate: 'http://example.org/uni#embedding',
              dimension: 8,
              metric: 'cosine',
              state: 'ready',
              generation: 'g1',
              rows: 3,
              overlay: { inserts: 0, deletes: 0 },
            },
          ],
        },
      }),
    );
    await page.route(/\/uni((@|%40)[^/]*)?\/(sparql|query)/, (route) =>
      route.fulfill({
        json: {
          vars: ['s', 'score'],
          rows: [
            [
              { type: 'uri', value: UNI + 'Teacher' },
              { type: 'literal', value: '0.9' },
            ],
          ],
        },
      }),
    );
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/ui/browse?ds=uni&branch=dr-sim&iri=${encodeURIComponent(UNI + 'Professor')}`);
    const more = page.getByRole('link', { name: /More in Similar/ });
    await expect(more).toBeVisible({ timeout: 30_000 });
    const href = new URL((await more.getAttribute('href'))!, 'http://x');
    expect(href.pathname).toMatch(/\/similar$/);
    expect(href.searchParams.get('ds')).toBe('uni');
    expect(href.searchParams.get('branch')).toBe('dr-sim');
  } finally {
    await request.delete('/$/branches/uni/dr-sim?force=true');
  }
});

test('the rail groups the Workbench editors and marks the open one', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto('/ui/workbench/thesaurus?ds=thes');
  const rail = page.getByRole('navigation', { name: 'Pages' });
  await expect(rail.getByRole('button', { name: 'Close Workbench' })).toHaveAttribute(
    'aria-expanded',
    'true',
  );
  await expect(rail.getByRole('link', { name: 'Thesaurus', exact: true })).toHaveAttribute(
    'aria-current',
    'page',
  );
  await expect(rail.getByRole('link', { name: 'Ontology', exact: true })).toBeVisible();
});

test.describe('a reader', () => {
  test.beforeEach(async ({ context }) => {
    await context.addCookies([
      { name: 'mock-principal', value: 'reader', domain: '127.0.0.1', path: '/' },
    ]);
  });

  test('has Browse but no Workbench, and a Workbench link opens read-only', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
    const rail = page.getByRole('navigation', { name: 'Pages' });
    await expect(rail.getByRole('link', { name: 'Browse', exact: true })).toBeVisible();
    await expect(rail.getByRole('link', { name: 'Workbench', exact: true })).toHaveCount(0);
    await expect(page.getByRole('link', { name: 'Open in Ontology' })).toHaveCount(0);

    await page.goto(`/ui/workbench/ontology?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
    await expect(page.getByRole('status').filter({ hasText: 'Read-only.' })).toBeVisible({
      timeout: 30_000,
    });
    const tree = page.getByRole('tree', { name: 'Classes (asserted)' });
    await expect(tree.locator('[aria-selected="true"]')).toContainText('Professor');
    await expect(page.getByRole('button', { name: 'New', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: /Rename/ })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Start session' })).toHaveCount(0);
    await tab(page, 'Ontology views', 'DL Query').click();
    await expect(page.getByRole('textbox', { name: 'Class expression' })).toBeVisible();
  });
});
