import { expect, test } from '@playwright/test';

test('metrics read on demand and display scoped counts, the bound and bounded profile details', async ({
  page,
}) => {
  let reads = 0;
  await page.route('**/$/ontology/uni?*', async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('metrics') !== 'true') return route.fallback();
    reads++;
    expect(url.searchParams.get('at')).toMatch(/^commit:\d+$/);
    await route.fulfill({
      json: {
        metrics: {
          scopeComplete: false,
          commit: 7,
          graphs: ['default'],
          warnings: [],
          axiomCount: 3,
          entities: { Class: 2, NamedIndividual: 1, Datatype: 0 },
          entityCount: 3,
          axioms: { SubClassOf: 2, ClassAssertion: 1 },
          terminologyAxioms: { SubClassOf: 2 },
          assertionOccurrences: { ClassAssertion: 1 },
          expressivity: {
            notation: null,
            observedBound: 'ALC',
            reasons: ['An import could not be resolved to a graph of this dataset.'],
          },
          reports: [
            {
              profile: 'el',
              title: 'OWL 2 EL',
              conforms: false,
              checked: 3,
              total: 120,
              truncated: true,
              violations: [
                {
                  rule: 'OWL 2 Profiles',
                  message: 'A universal restriction is outside EL.',
                  graph: 'default',
                },
              ],
            },
          ],
        },
      },
    });
  });
  await page.goto('/ui/workbench/ontology?ds=uni&tab=header');
  const panel = page.getByRole('region', { name: 'Ontology metrics', exact: true });
  await expect(panel.getByRole('button', { name: 'Read metrics' })).toBeVisible();
  expect(reads).toBe(0);
  await panel.getByRole('button', { name: 'Read metrics' }).click();
  await expect(panel).toContainText('DL expressivity bound: Unknown');
  await expect(panel).toContainText('Observed mapped fragment bound: ALC');
  await expect(panel).toContainText('Streamed assertions count occurrences');
  await panel.locator('summary').click();
  await expect(panel).toContainText('Showing 1 of 120 violations');
  await expect(panel).toContainText('A universal restriction is outside EL');
  expect(reads).toBe(1);
});

test('leaving the Header clears a pending report before it can repaint', async ({ page }) => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => (release = resolve));
  let received!: () => void;
  const started = new Promise<void>((resolve) => (received = resolve));
  await page.route('**/$/ontology/uni?*', async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get('metrics') !== 'true') return route.fallback();
    received();
    await held;
    await route.fulfill({ status: 500, body: 'stale metrics failure' }).catch(() => {});
  });
  await page.goto('/ui/workbench/ontology?ds=uni&tab=header');
  await page.getByRole('button', { name: 'Read metrics', exact: true }).click();
  await started;
  await page.getByRole('tab', { name: 'Tree', exact: true }).click();
  release();
  await page.getByRole('tab', { name: 'Header', exact: true }).click();
  const panel = page.getByRole('region', { name: 'Ontology metrics', exact: true });
  await expect(panel.getByRole('button', { name: 'Read metrics', exact: true })).toBeVisible();
  await expect(panel).not.toContainText('stale metrics failure');
});
