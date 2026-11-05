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
