// Vector search against the mock (mock/vector.mjs): the dataset page's index cards with
// recall, create, edit, rebuild and drop, and the Similar page from an entity and from
// a pasted vector, with its ef and exact controls and the link from the explorer.

import { expect, test, type Page } from '@playwright/test';

const RES = 'http://example.org/resource/';

const card = (page: Page, name: string) =>
  page.getByRole('article', { name: `Vector index ${name}` });

test('the dataset page lists the index, measures recall and manages indexes', async ({ page }) => {
  await page.goto('/ui/datasets/foaf');
  const panel = page.getByRole('region', { name: 'Vector indexes' });
  const emb = card(page, 'embedding');
  await expect(emb).toContainText('ex:embedding');
  await expect(emb).toContainText('8 dimensions');
  await expect(emb).toContainText('cosine');
  await expect(emb).toContainText('mock-embed-8');
  await expect(emb).toContainText('M 16 · efConstruction 128 · efSearch 128');
  await expect(emb).toContainText('1 malformed');
  await expect(emb.locator('.badge')).toHaveText('ready');
  await expect(emb).toContainText('not measured');

  // recall@k against the exact search, kept for the next visit
  await emb.getByLabel('Recall ef').fill('64');
  await emb.getByRole('button', { name: 'Measure recall' }).click();
  await expect(emb).toContainText('recall@10 at ef 64');
  await expect(emb.locator('.recall strong')).toHaveText(/%$/);

  // a new index over the 4-dimensional bio embeddings: the dimension is read from the data
  await panel.getByRole('button', { name: 'New index…' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Name', { exact: true }).fill('bio');
  await dialog.getByLabel(/^Predicate/).fill('ex:bioEmbedding');
  await expect(dialog.getByLabel('Dimension')).toHaveValue('4');
  await dialog.getByLabel('Metric').selectOption('euclidean');
  await dialog.getByLabel(/^Model/).fill('bio-4');
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog).toBeHidden();
  const bio = card(page, 'bio');
  await expect(bio).toContainText('ex:bioEmbedding');
  await expect(bio).toContainText('euclidean');
  await expect(bio.locator('.badge')).toHaveText('ready', { timeout: 15_000 });

  // another index of the same predicate is refused
  await panel.getByRole('button', { name: 'New index…' }).click();
  await dialog.getByLabel('Name', { exact: true }).fill('bio2');
  await dialog.getByLabel(/^Predicate/).fill('ex:bioEmbedding');
  await expect(dialog.getByLabel('Dimension')).toHaveValue('4');
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog).toContainText('already indexed by vector index bio');
  await dialog.getByRole('button', { name: 'Cancel' }).click();

  // a new efSearch keeps the build; a new M builds again
  await bio.getByRole('button', { name: 'Edit…' }).click();
  await dialog.getByLabel('efSearch').fill('256');
  await expect(dialog).toContainText('The build is kept');
  await dialog.getByLabel('M', { exact: true }).fill('24');
  await expect(dialog).toContainText('These changes build the index again');
  await dialog.getByLabel('M', { exact: true }).fill('16');
  await dialog.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(bio).toContainText('efSearch 256');

  await bio.getByRole('button', { name: 'Rebuild' }).click();
  await expect(bio.locator('.badge')).toHaveText(/building/);
  await expect(bio.locator('.badge')).toHaveText('ready', { timeout: 15_000 });

  await bio.getByRole('button', { name: 'Drop' }).click();
  await dialog.getByRole('button', { name: 'Drop index' }).click();
  await expect(bio).toHaveCount(0);
  await expect(emb).toBeVisible();
});

test('the explorer links to Similar, which ranks the neighbours of an entity', async ({ page }) => {
  await page.goto(`/ui/explore?ds=foaf&iri=${encodeURIComponent(`${RES}Ada_Lovelace`)}`);
  await page.getByRole('link', { name: 'Open in Similar' }).click();
  await expect(page).toHaveURL(/\/ui\/similar\?/);

  const results = page.getByRole('region', { name: 'Similar results' });
  const rows = results.locator('tbody tr');
  await expect(rows).toHaveCount(10);
  await expect(results).not.toContainText('Ada Lovelace');
  await expect(results).toContainText('similarity ↑');
  await expect(results).toContainText('The query entity is left out.');
  // the explorer's first vector predicate, ex:bioEmbedding, has no index
  await expect(page).toHaveURL(/predicate=http%3A%2F%2Fexample\.org%2Fontology%23bioEmbedding/);
  await expect(results.locator('.foot')).toContainText('· exact ·');
  await expect(rows.first().locator('td.vec')).toContainText('vector(4)');

  // through the index of ex:embedding
  const query = page.getByRole('region', { name: 'Query' });
  await query.getByLabel('Index').selectOption('index:embedding');
  await query.getByRole('button', { name: 'Search' }).click();
  await expect(results.locator('.foot')).toContainText('hnsw ef=128');
  await expect(rows.first().locator('td.vec')).toContainText('vector(8)');
  await expect(page).toHaveURL(/index=embedding/);

  // ef and exact go into the search
  await query.getByLabel('ef', { exact: true }).fill('40');
  await query.getByRole('button', { name: 'Search' }).click();
  await expect(results.locator('.foot')).toContainText('hnsw ef=40');
  await query.getByLabel('exact', { exact: true }).check();
  await query.getByRole('button', { name: 'Search' }).click();
  await expect(results.locator('.foot')).toContainText('exact (exact:true)');
  await expect(page).toHaveURL(/exact=true/);

  // the next search starts from a result
  const second = rows.nth(1);
  const label = (await second.locator('td.ent a').textContent())!.trim();
  await second.getByRole('button', { name: /^Search from/ }).click();
  await expect(results).not.toContainText(label);
  await expect(query.getByLabel(/^Entity/)).toHaveValue(/^http:\/\/example\.org\/resource\//);

  // and a result opens in the explorer
  const link = rows.first().locator('td.ent a');
  const target = await link.getAttribute('href');
  expect(target).toContain('/ui/explore?ds=foaf&iri=');
});

test('Similar searches from a pasted vector and checks it first', async ({ page }) => {
  await page.goto('/ui/similar?ds=foaf&index=embedding');
  const query = page.getByRole('region', { name: 'Query' });
  await query.getByRole('radio', { name: 'Vector' }).click();
  const box = query.getByLabel(/^Vector/);

  await box.fill('[1, 2, x]');
  await expect(query).toContainText('Expected a number. At character 8.');
  await box.fill('[1, 2, 3]');
  await expect(query).toContainText('the index holds 8');
  await expect(query.getByRole('button', { name: 'Search' })).toBeDisabled();

  await box.fill('"[0.9, 0.1, -0.7, -1.1, -0.6, 0.4, 1.1, 0.9]"^^spk:vector');
  await expect(query).toContainText('8 dimensions');
  await query.getByRole('radio', { name: 'euclidean' }).click();
  await query.getByLabel('Results').selectOption('5');
  await query.getByRole('button', { name: 'Search' }).click();

  const results = page.getByRole('region', { name: 'Similar results' });
  await expect(results.locator('tbody tr')).toHaveCount(5);
  await expect(results).toContainText('distance ↓');
  // another metric than the index's is searched exactly
  await expect(results.locator('.foot')).toContainText("the query's metric is not the index's");

  await results.getByRole('button', { name: 'Open in query editor' }).click();
  await expect(page).toHaveURL(/\/ui\/query/);
  await expect(page.locator('.cm-content')).toContainText('spk:vectorSearch');
  await expect(page.locator('.cm-content')).toContainText('"metric:euclidean"');
});
