// The GeoSPARQL surfaces against the mock (mock/geo.mjs and its `places` dataset): the
// query results' Map tab, the explorer's map card with Nearby, and the dataset page's
// Spatial index panel.

import { expect, test, type Page } from '@playwright/test';

const PLACES = 'http://example.org/places/';

const QUERY = `PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?f ?label ?w WHERE {
  ?f rdfs:label ?label ; geo:hasDefaultGeometry|geo:hasGeometry ?g .
  ?g geo:asWKT|geo:asGeoJSON ?w .
} ORDER BY ?label`;

async function run(page: Page, query: string) {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'places'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(query);
  await page.getByRole('button', { name: /^Run\b/ }).click();
}

test('the Map tab draws the geometry column; a row click highlights it', async ({ page }) => {
  await run(page, QUERY);
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toContainText('Eiffel Tower');
  await results.getByRole('tab', { name: 'Map' }).click();

  const map = page.getByRole('region', { name: 'Result map' });
  await expect(map).toHaveAttribute('data-state', 'ready');
  // six drawn: CRS84, EPSG:4326 swapped, GeoJSON, a polygon and a UTM point the server
  // converted; the malformed one is listed instead
  await expect(map).toHaveAttribute('data-features', '6');
  await expect(results.getByText('6 drawn, 1 not drawn')).toBeVisible();
  const undrawn = results.locator('details.undrawn');
  await expect(undrawn).toContainText('POINT(1)');
  await expect(undrawn).toContainText('malformed');

  // ordered by label: Atlantis (not drawn), Bois de Boulogne, Eiffel Tower, …
  const rows = results.getByRole('button', { name: /Eiffel Tower/ });
  await rows.click();
  await expect(rows).toHaveAttribute('aria-pressed', 'true');
  await expect(map).toHaveAttribute('data-selected', '2');
  const popup = page.locator('.maplibregl-popup');
  await expect(popup).toContainText('?label');
  await expect(popup).toContainText('Eiffel Tower');
});

test('the Map tab draws GML and KML literals through the server', async ({ page }) => {
  await run(
    page,
    `PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?label ?w WHERE {
  ?f rdfs:label ?label ; geo:hasGeometry ?g .
  ?g geo:asGML|geo:asKML ?w .
} ORDER BY ?label`,
  );
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toContainText('Notre-Dame');
  const converted = page.waitForRequest((r) => r.url().endsWith('/$/geo/convert'));
  await results.getByRole('tab', { name: 'Map' }).click();
  const literals = (await converted)
    .postDataJSON()
    .literals.map((l: { datatype: string }) => l.datatype);
  expect(literals.sort()).toEqual([
    'http://www.opengis.net/ont/geosparql#gmlLiteral',
    'http://www.opengis.net/ont/geosparql#kmlLiteral',
  ]);
  const map = page.getByRole('region', { name: 'Result map' });
  await expect(map).toHaveAttribute('data-state', 'ready');
  await expect(map).toHaveAttribute('data-features', '2');
  await expect(results.getByText('2 drawn')).toBeVisible();
});

test('an explored feature shows its map card and lists nearby features', async ({ page }) => {
  await page.goto(`/ui/explore?ds=places&iri=${encodeURIComponent(`${PLACES}paris`)}`);
  await expect(page.getByRole('heading', { name: 'Location' })).toBeVisible();
  const map = page.getByRole('region', { name: 'Location map' });
  await expect(map).toHaveAttribute('data-features', '1');

  await page.getByRole('combobox', { name: 'Radius' }).selectOption('10');
  await page.getByRole('button', { name: 'Nearby' }).click();
  const hits = page.locator('table.hits tbody tr');
  // within 10 km of Paris: the Louvre first, Lyon is not
  await expect(hits.first()).toContainText('Louvre');
  await expect(page.locator('table.hits')).toContainText('Eiffel Tower');
  await expect(page.locator('table.hits')).not.toContainText('Lyon');
  await expect(map).toHaveAttribute('data-features', String(1 + (await hits.count())));
  await hits.first().click();
  await expect(hits.first()).toHaveClass(/sel/);
  await expect(map).toHaveAttribute('data-selected', '0');
});

test('the Spatial index panel shows the index and starts its tasks', async ({ page }) => {
  await page.goto('/ui/datasets/places');
  const panel = page
    .locator('section.panel')
    .filter({ has: page.getByRole('heading', { name: 'Spatial index' }) });
  await expect(panel.locator('.panel-head .badge')).toContainText('ready');
  await expect(panel).toContainText('EPSG:32631');
  await expect(panel).toContainText('1 malformed');

  await panel.getByRole('button', { name: 'Rebuild' }).click();
  await expect(page.getByText('Spatial index rebuild started')).toBeVisible();
  await expect(panel.locator('.panel-head .badge')).toContainText('building');
  await expect(page.getByText('Spatial index built')).toBeVisible({ timeout: 15_000 });

  // the indexed geometries in view
  await panel.getByRole('button', { name: 'Map' }).click();
  const indexed = panel.getByRole('region', { name: 'Indexed geometries' });
  await expect(indexed).toHaveAttribute('data-state', 'ready');
  // the world: every geometry the index holds
  await expect(indexed).toHaveAttribute('data-features', '6');
  await expect(panel).toContainText('6 geometries in view');

  await panel.getByRole('button', { name: 'Disable' }).click();
  await page.getByRole('button', { name: 'Disable and delete index' }).click();
  await expect(panel.locator('.panel-head .badge')).toHaveText('off');

  await panel.getByRole('button', { name: 'Enable…' }).click();
  const dialog = page.getByRole('dialog', { name: 'Enable the spatial index' });
  await expect(dialog.getByRole('textbox').first()).toHaveValue(/geo:asWKT/);
  await dialog.getByRole('checkbox', { name: /Basic Geo/ }).check();
  await dialog.getByRole('button', { name: 'Enable' }).click();
  await expect(page.getByText('Enabling the spatial index')).toBeVisible();
  await expect(panel.locator('.panel-head .badge')).toContainText(/building|ready/);
  await expect(panel).toContainText('Basic Geo points', { timeout: 15_000 });
});
