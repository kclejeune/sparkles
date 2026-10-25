import { expect, test, type Page } from '@playwright/test';

const NS = 'https://example.test/catalog#';
const datasets = ['prefix-label-first', 'prefix-label-second'];

const drawn = async (page: Page) =>
  JSON.parse((await page.locator('.wrap[data-graph]').getAttribute('data-graph')) ?? '{}') as {
    nodes: [string, string][];
    edges: [string, string][];
  };

test('late dataset prefixes update fallback captions and keep RDF labels across dataset switches', async ({
  page,
  request,
}) => {
  const release: (() => void)[] = [];
  for (const [i, ds] of datasets.entries()) {
    const made = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'mem' } });
    expect(made.ok()).toBe(true);
    const update = await request.post(`/${ds}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `INSERT DATA {
        <${NS}Unlabelled> <${NS}linked> <${NS}Labelled> .
        <${NS}Labelled> <http://www.w3.org/2000/01/rdf-schema#label> "A human name" .
      }`,
    });
    expect(update.ok()).toBe(true);
    const ready = new Promise<void>((resolve) => release.push(resolve));
    await page.route(`**/$/prefixes/${ds}`, async (route) => {
      await ready;
      await route.fulfill({ json: { prefixes: { [i ? 'second' : 'first']: NS } } });
    });
  }

  try {
    await page.addInitScript((ds) => localStorage.setItem('sparkles.dataset', ds), datasets[0]);
    await page.goto(`/ui/explore?ds=${datasets[0]}&iri=${encodeURIComponent(`${NS}Unlabelled`)}`);
    for (const [i, ds] of datasets.entries()) {
      if (i) {
        // Keep the same app instance, including the first dataset's loaded prefixes.
        await page.locator('.ds-button').click();
        await page.getByRole('option', { name: new RegExp(`^${ds} `) }).click();
        const search = page.getByPlaceholder(`Find a resource in ${ds} by label or IRI`);
        await search.fill(`${NS}Unlabelled`);
        await page.getByRole('option', { name: /^Open / }).click();
      }
      await expect
        .poll(async () => (await drawn(page)).nodes?.map(([label]) => label))
        .toEqual(expect.arrayContaining(['Unlabelled', 'A human name']));
      const title = page
        .getByRole('complementary', { name: 'Resource details' })
        .locator('h2')
        .first();
      await expect(title).toHaveText('Unlabelled');
      release[i]();
      const prefix = i ? 'second' : 'first';
      await expect
        .poll(async () => (await drawn(page)).nodes.map(([label]) => label))
        .toEqual(expect.arrayContaining([`${prefix}:Unlabelled`, 'A human name']));
      await expect
        .poll(async () => (await drawn(page)).edges.map(([label]) => label))
        .toContain(`${prefix}:linked`);
      await expect(title).toHaveText(`${prefix}:Unlabelled`);
    }
  } finally {
    for (const done of release) done();
  }
});
