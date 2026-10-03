// The dataset page's upload form with a CSV file, against the mock: the table fields appear
// once a table is chosen, the default mapping waits for a base IRI, a key column goes with
// it, and a mapping file rules the key out. The mock maps tables with the default mapping
// only; the server's own tests cover CSVW mappings and templates.

import { expect, test } from '@playwright/test';

const PEOPLE = 'id,name\n7,Ann\n8,Bob\n';

test('a CSV upload sends its base IRI, key column and mapping', async ({ page }) => {
  await page.goto('/ui/datasets/scratch');
  const panel = page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'Upload data' }),
  });
  const upload = panel.getByRole('button', { name: /^Upload/ });

  // an RDF file alone shows no table fields
  await panel.locator('input[type=file][multiple]').setInputFiles({
    name: 'extra.ttl',
    mimeType: 'text/turtle',
    buffer: Buffer.from('<urn:x> <urn:p> "rdf" .'),
  });
  await expect(panel.getByRole('group', { name: 'CSV and TSV tables' })).toHaveCount(0);

  await panel.locator('input[type=file][multiple]').setInputFiles({
    name: 'people.csv',
    mimeType: 'text/csv',
    buffer: Buffer.from(PEOPLE),
  });
  const tables = panel.getByRole('group', { name: 'CSV and TSV tables' });
  await expect(tables).toBeVisible();
  await expect(tables).toContainText('needs a base IRI');
  await expect(upload).toBeDisabled();

  // a mapping file makes the base optional and rules out the key column
  await tables.getByLabel('Mapping or template file').setInputFiles({
    name: 'people.csv-metadata.json',
    mimeType: 'application/json',
    buffer: Buffer.from('{"@context": "http://www.w3.org/ns/csvw", "url": "people.csv"}'),
  });
  await expect(tables.getByText('people.csv-metadata.json')).toBeVisible();
  await expect(tables.getByRole('textbox', { name: /Key column/ })).toBeDisabled();
  await expect(upload).toBeEnabled();
  await tables.getByRole('button', { name: 'Remove people.csv-metadata.json' }).click();

  // the default mapping with a base and a key
  await tables.getByRole('textbox', { name: /Base IRI/ }).fill('http://example.org/people/');
  await tables.getByRole('textbox', { name: /Key column/ }).fill('id');
  await expect(upload).toBeEnabled();
  const sent = page.waitForRequest((r) => r.url().includes('/scratch/upload'));
  await upload.click();
  const req = await sent;
  const url = new URL(req.url());
  expect(url.searchParams.get('base')).toBe('http://example.org/people/');
  expect(url.searchParams.get('key')).toBe('id');
  await expect(page.getByText(/2 rows from 1 table/)).toBeVisible();

  // the rows are in the dataset under the base IRI
  const ask = 'ASK { <http://example.org/people/7> <http://example.org/people/name> "Ann" }';
  const r = await page.request.get(`/scratch/sparql?query=${encodeURIComponent(ask)}`, {
    headers: { Accept: 'application/sparql-results+json' },
  });
  expect((await r.json()).boolean).toBe(true);
});
