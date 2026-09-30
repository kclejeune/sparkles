import { expect, test } from './fixtures';
import { DATASET, EX } from './data';

test('the query page runs a query and shows the results', async ({ page }) => {
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(
    `PREFIX ex: <${EX}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?person ?name WHERE { ?person a ex:Person ; rdfs:label ?name } ORDER BY ?name`,
  );
  await page.getByRole('button', { name: /^Run\b/ }).click();

  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('tab', { name: /Table/ })).toContainText('4');
  const rows = results.getByRole('grid').getByRole('row');
  await expect(rows).toHaveCount(5); // the header and four people
  await expect(rows.nth(1)).toContainText('Ada Lovelace');
  await expect(rows.nth(4)).toContainText('Grace Hopper');
});

test('the explorer finds a resource by label and shows its properties', async ({ page }) => {
  await page.goto(`/ui/explore?ds=${DATASET}`);
  await page.getByRole('combobox').fill('lovelace');
  await page.getByRole('option', { name: /Ada Lovelace/ }).click();

  const side = page.getByRole('complementary', { name: 'Resource details' });
  await expect(side.getByRole('heading', { name: 'Ada Lovelace' })).toBeVisible();
  await expect(side).toContainText('analytical engine');
  await expect(side.getByRole('heading', { name: /^Properties/ })).toBeVisible();
  // ex:knows links to Grace Hopper
  await expect(page).toHaveURL(new RegExp(`iri=${encodeURIComponent(`${EX}ada`)}`));
});

test('Similar lists the nearest entities by vector', async ({ page }) => {
  await page.goto(`/ui/explore?ds=${DATASET}&iri=${encodeURIComponent(`${EX}ada`)}`);
  const side = page.getByRole('complementary', { name: 'Resource details' });
  await expect(side.getByRole('heading', { name: /^Similar/ })).toBeVisible();

  const hits = side.locator('table.sim tbody tr');
  await expect(hits).toHaveCount(3); // everyone else, the entity itself left out
  await expect(hits.first()).toContainText('Grace Hopper');
  await expect(hits.first()).toContainText('0.99');

  // euclidean: lower is closer, and the nearest stays first
  await side.getByRole('radio', { name: 'euclidean' }).click();
  await expect(side.locator('table.sim thead')).toContainText('distance');
  await expect(hits.first()).toContainText('Grace Hopper');

  await hits.first().getByRole('link').click();
  await expect(side.getByRole('heading', { name: 'Grace Hopper' })).toBeVisible();
});

test('text search ranks literals and opens the subject', async ({ page }) => {
  await page.goto(`/ui/explore?ds=${DATASET}&tab=search`);
  await page.getByLabel('Full-text query').fill('compiler');
  await page.getByRole('button', { name: 'Search', exact: true }).click();

  const hits = page.locator('ol.hits > li');
  await expect(hits).toHaveCount(1);
  await expect(hits.first()).toContainText('Grace Hopper');
  await expect(hits.first()).toContainText('Built the first compiler');

  await page.getByLabel('Full-text query').fill('first');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(hits).toHaveCount(2);

  await hits.filter({ hasText: 'Ada Lovelace' }).getByRole('button', { name: /Ada/ }).click();
  const side = page.getByRole('complementary', { name: 'Resource details' });
  await expect(side.getByRole('heading', { name: 'Ada Lovelace' })).toBeVisible();
});

test('the dataset page shows the commit history', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}`);
  const history = page.locator('section.panel').filter({
    has: page.getByRole('heading', { name: 'History', exact: true }),
  });
  await expect(history).toBeVisible();
  const commits = history.locator('table.commits tbody tr');
  // the two loads (and the empty dataset's creation, if it is recorded)
  await expect(commits.first()).toBeVisible();
  expect(await commits.count()).toBeGreaterThanOrEqual(2);
  await expect(history.locator('.ins').first()).toContainText('+');
});
