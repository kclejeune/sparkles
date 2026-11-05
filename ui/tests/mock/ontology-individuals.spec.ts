import { expect, test } from '@playwright/test';

const name = (iri: string, label: string) => ({
  iri,
  label,
  labelLang: 'en',
  shortForm: null,
  kinds: ['class'],
  deprecated: false,
});

test('changing graph drops a delayed Individuals response and its labels', async ({ page }) => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let received!: () => void;
  const started = new Promise<void>((resolve) => {
    received = resolve;
  });
  await page.route('**/$/ontology/uni/individuals?*', async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('graph') === 'default') {
      received();
      await held;
      await route
        .fulfill({
          json: {
            commit: 1,
            groups: [{ type: name('http://example.org/stale', 'Stale type'), total: 10000 }],
            individuals: [],
            total: 1,
            next: null,
          },
        })
        .catch(() => {});
    } else {
      await route.fulfill({
        json: {
          commit: 2,
          groups: [{ type: name('http://example.org/fresh', 'Fresh type'), total: 1 }],
          individuals: [],
          total: 1,
          next: null,
        },
      });
    }
  });
  await page.goto('/ui/workbench/ontology?ds=uni&kind=NamedIndividual');
  await started;
  await page.goto(
    '/ui/workbench/ontology?ds=uni&kind=NamedIndividual&graph=http%3A%2F%2Fexample.org%2FfreshGraph',
  );
  release();
  const tree = page.getByRole('tree', { name: 'Individuals (asserted)' });
  await expect(tree.getByRole('button', { name: 'Fresh type', exact: true })).toBeVisible();
  await expect(tree).not.toContainText('Stale type');
});

test('the first Individuals page waits for and pins the displayed summary commit', async ({
  page,
}) => {
  let release!: () => void;
  const held = new Promise<void>((r) => {
    release = r;
  });
  let received!: () => void;
  const started = new Promise<void>((r) => {
    received = r;
  });
  const reads: URL[] = [];
  await page.route('**/$/ontology/uni?*', async (route) => {
    const response = await route.fetch();
    const json = await response.json();
    received();
    await held;
    await route.fulfill({ response, json: { ...json, commit: 101 } });
  });
  await page.route('**/$/ontology/uni/individuals?*', async (route) => {
    const url = new URL(route.request().url());
    reads.push(url);
    await held;
    await route.fulfill({
      json: {
        commit: url.searchParams.get('at') === 'commit:101' ? 101 : 100,
        groups: [{ type: name('http://example.org/fresh', 'Fresh type'), total: 1 }],
        individuals: [],
        total: 1,
        next: null,
      },
    });
  });
  await page.goto('/ui/workbench/ontology?ds=uni&kind=NamedIndividual');
  await started;
  await expect(page.getByText('Reading the individuals…', { exact: true })).toBeVisible();
  const early = reads.length;
  release();
  await expect(
    page
      .getByRole('tree', { name: 'Individuals (asserted)' })
      .getByRole('button', { name: 'Fresh type', exact: true }),
  ).toBeVisible();
  expect(early).toBe(0);
  expect(reads).toHaveLength(1);
  expect(reads[0].searchParams.get('at')).toBe('commit:101');
});

test('a delayed reveal of the previous selection cannot open or scroll its type', async ({
  page,
}) => {
  let release!: () => void;
  const held = new Promise<void>((r) => {
    release = r;
  });
  let received!: () => void;
  const started = new Promise<void>((r) => {
    received = r;
  });
  await page.route('**/$/ontology/uni/individuals?*', async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('iri')?.endsWith('#ada')) {
      received();
      await held;
    }
    await route.fallback();
  });
  await page.goto(
    '/ui/workbench/ontology?ds=uni&kind=NamedIndividual&iri=http%3A%2F%2Fexample.org%2Funi%23ada',
  );
  await started;
  const tree = page.getByRole('tree', { name: 'Individuals (asserted)' });
  await tree.getByRole('button', { name: 'Expand Graduate student', exact: true }).click();
  await tree.getByRole('button', { name: 'Alan Turing', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Alan Turing', exact: true })).toBeVisible();
  const memberPage = page.waitForResponse(
    (r) =>
      new URL(r.url()).pathname.endsWith('/individuals') &&
      new URL(r.url()).searchParams.get('type')?.endsWith('#Professor') === true,
  );
  release();
  await (await memberPage).finished();
  await page.evaluate(
    () => new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r()))),
  );
  await expect(tree.getByRole('button', { name: 'Expand Professor', exact: true })).toBeVisible();
  await expect(tree.getByRole('button', { name: 'Collapse Professor', exact: true })).toHaveCount(
    0,
  );
  await expect(page.getByRole('heading', { name: 'Alan Turing', exact: true })).toBeVisible();
});

const individualAnswer = (commit: number, label: string) => ({
  commit,
  groups: [{ type: name('http://example.org/Type', 'Pinned type'), total: null }],
  individuals: [
    {
      ...name('http://example.org/ada', label),
      types: [name('http://example.org/Type', 'Pinned type')],
    },
  ],
  total: 1,
  next: null,
});

async function pinnedSummary(page: import('@playwright/test').Page) {
  await page.route('**/$/ontology/uni?*', async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, json: { ...(await response.json()), commit: 101 } });
  });
}

test('filter membership stays at the displayed commit after a failed root read', async ({
  page,
}) => {
  await pinnedSummary(page);
  const lookups: URL[] = [];
  await page.route('**/$/ontology/uni/search?*', (route) =>
    route.fulfill({ json: { results: [{ iri: 'http://example.org/ada' }] } }),
  );
  await page.route('**/$/ontology/uni/individuals?*', async (route) => {
    const url = new URL(route.request().url());
    if (!url.searchParams.has('iri'))
      return route.fulfill({ status: 500, body: 'root read failed' });
    lookups.push(url);
    const pinned = url.searchParams.get('at') === 'commit:101';
    await route.fulfill({
      json: individualAnswer(pinned ? 101 : 102, pinned ? 'Pinned Ada' : 'Head-only Ada'),
    });
  });
  await page.goto('/ui/workbench/ontology?ds=uni&kind=NamedIndividual');
  await expect(page.getByRole('alert')).toContainText('root read failed');
  await page.getByRole('searchbox', { name: 'Filter', exact: true }).fill('ada');
  await expect.poll(() => lookups.length).toBe(1);
  expect(lookups[0].searchParams.get('at')).toBe('commit:101');
  const tree = page.getByRole('tree', { name: 'Individuals (asserted)' });
  await expect(tree.getByRole('button', { name: 'Pinned Ada', exact: true })).toBeVisible();
  await expect(tree).not.toContainText('Head-only Ada');
});

for (const status of [404, 500]) {
  test(`filter retains pinned matches when another head candidate returns ${status}`, async ({
    page,
  }) => {
    await pinnedSummary(page);
    await page.route('**/$/ontology/uni/search?*', (route) =>
      route.fulfill({
        json: { results: [{ iri: 'http://example.org/ada' }, { iri: 'http://example.org/new' }] },
      }),
    );
    await page.route('**/$/ontology/uni/individuals?*', async (route) => {
      const url = new URL(route.request().url());
      if (!url.searchParams.has('iri'))
        return route.fulfill({
          json: {
            commit: 101,
            groups: [{ type: name('http://example.org/Type', 'Pinned type'), total: 1 }],
            individuals: [],
            total: 1,
            next: null,
          },
        });
      expect(url.searchParams.get('at')).toBe('commit:101');
      if (url.searchParams.get('iri')?.endsWith('/new'))
        return route.fulfill({
          status,
          body: status === 404 ? 'not present at pinned revision' : 'candidate read failed',
        });
      await route.fulfill({ json: individualAnswer(101, 'Pinned Ada') });
    });
    await page.goto('/ui/workbench/ontology?ds=uni&kind=NamedIndividual');
    await expect(page.getByRole('tree', { name: 'Individuals (asserted)' })).toBeVisible();
    await page.getByRole('searchbox', { name: 'Filter', exact: true }).fill('ada');
    if (status === 500)
      await expect(page.getByRole('alert')).toContainText('candidate read failed');
    const tree = page.getByRole('tree', { name: 'Individuals (asserted)' });
    await expect(tree.getByRole('button', { name: 'Pinned Ada', exact: true })).toBeVisible();
    await expect(page.getByText(/Nothing matches/)).toHaveCount(0);
    if (status === 404)
      await expect(
        page.getByText('Some search matches are absent at the displayed revision.', {
          exact: true,
        }),
      ).toBeVisible();
  });
}
