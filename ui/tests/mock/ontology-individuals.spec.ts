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
