// The GeoSPARQL surfaces against the real server without auth: the Map tab of query
// results (MapLibre and its worker under the page's Content Security Policy, a UTM
// literal converted by the server), the explorer's map card with Nearby
// (`spatial:nearbyGeom`), and the Spatial index panel (tasks, `GET /{ds}/geo`). Each test
// loads the places into a dataset of its own.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

const EX = 'http://example.org/places/';
const PLACES = `@prefix ex: <${EX}> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:paris a ex:City ; rdfs:label "Paris" ; geo:hasDefaultGeometry ex:parisGeom .
ex:parisGeom geo:asWKT "POINT(2.3522 48.8566)"^^geo:wktLiteral .
ex:lyon a ex:City ; rdfs:label "Lyon" ; geo:hasDefaultGeometry ex:lyonGeom .
ex:lyonGeom geo:asWKT "POINT(4.8357 45.764)"^^geo:wktLiteral .
ex:louvre rdfs:label "Louvre" ; geo:hasGeometry ex:louvreGeom .
ex:louvreGeom geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.8606 2.3376)"^^geo:wktLiteral .
ex:eiffel rdfs:label "Eiffel Tower" ; geo:hasGeometry ex:eiffelGeom .
ex:eiffelGeom geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/32631> POINT(448252 5411935)"^^geo:wktLiteral .
ex:seine rdfs:label "Seine" ; geo:hasGeometry ex:seineGeom .
ex:seineGeom geo:asGeoJSON "{\\"type\\":\\"LineString\\",\\"coordinates\\":[[2.25,48.84],[2.3,48.86],[2.36,48.85]]}"^^geo:geoJSONLiteral .
ex:atlantis rdfs:label "Atlantis" ; geo:hasGeometry ex:atlantisGeom .
ex:atlantisGeom geo:asWKT "POINT(1)"^^geo:wktLiteral .
`;

async function dataset(request: APIRequestContext, name: string, index = false) {
  expect((await request.post('/$/datasets', { data: { dbName: name, dbType: 'mem' } })).ok()).toBe(
    true,
  );
  const loaded = await request.post(`/${name}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: PLACES,
  });
  expect(loaded.ok()).toBe(true);
  if (!index) return;
  expect((await request.put(`/$/geo/${name}`, { data: {} })).ok()).toBe(true);
  await expect
    .poll(async () => (await (await request.get(`/$/geo/${name}`)).json()).state, {
      timeout: 15_000,
    })
    .toBe('ready');
}

async function run(page: Page, ds: string, query: string) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(query);
  await page.getByRole('button', { name: /^Run\b/ }).click();
}

open(
  'the Map tab draws geometry literals of every CRS; a row opens its popup',
  async ({ page, request }) => {
    await dataset(request, 'geo-map');
    await run(
      page,
      'geo-map',
      `PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?f ?label ?w WHERE {
  ?f rdfs:label ?label ; geo:hasDefaultGeometry|geo:hasGeometry ?g .
  ?g geo:asWKT|geo:asGeoJSON ?w .
} ORDER BY ?label`,
    );
    const results = page.getByRole('region', { name: 'Results' });
    await results.getByRole('tab', { name: 'Map' }).click();
    const map = page.getByRole('region', { name: 'Result map' });
    await expect(map).toHaveAttribute('data-state', 'ready');
    await expect(map).toHaveAttribute('data-features', '5');
    await expect(results.getByText('5 drawn, 1 not drawn')).toBeVisible();
    await expect(results.locator('details.undrawn')).toContainText('POINT(1)');

    await results.getByRole('button', { name: /Eiffel Tower/ }).click();
    const popup = page.locator('.maplibregl-popup');
    await expect(popup).toContainText('Eiffel Tower');
    await expect(popup).toContainText('ex:eiffel');
  },
);

open(
  'the explorer shows a feature on a map and lists what is nearby',
  async ({ page, request }) => {
    await dataset(request, 'geo-near', true);
    await page.goto(`/ui/explore?ds=geo-near&iri=${encodeURIComponent(`${EX}paris`)}`);
    const map = page.getByRole('region', { name: 'Location map' });
    await expect(map).toHaveAttribute('data-state', 'ready');
    await expect(map).toHaveAttribute('data-features', '1');
    await page.getByRole('combobox', { name: 'Radius' }).selectOption('10');
    await page.getByRole('button', { name: 'Nearby' }).click();
    const hits = page.locator('table.hits tbody tr');
    // the Seine passes closest (its distance is to the line), then the Louvre
    await expect(hits.first()).toContainText('Seine');
    await expect(hits.nth(1)).toContainText('Louvre');
    await expect(page.locator('table.hits')).toContainText('Eiffel Tower');
    await expect(page.locator('table.hits')).not.toContainText('Lyon');
    const km = (await hits.locator('td.num').allInnerTexts()).map(Number);
    expect(km).toEqual([...km].sort((a, b) => a - b));
    // the UTM point is drawn too: the server converted it
    await expect(map).toHaveAttribute('data-features', String(1 + (await hits.count())));
  },
);

open('the Spatial index panel enables, shows and rebuilds the index', async ({ page, request }) => {
  await dataset(request, 'geo-panel');
  await page.goto('/ui/datasets/geo-panel');
  const panel = page
    .locator('section.panel')
    .filter({ has: page.getByRole('heading', { name: 'Spatial index' }) });
  await expect(panel.locator('.panel-head .badge')).toHaveText('off');
  await panel.getByRole('button', { name: 'Enable…' }).click();
  await page
    .getByRole('dialog', { name: 'Enable the spatial index' })
    .getByRole('button', { name: 'Enable' })
    .click();
  await expect(panel.locator('.panel-head .badge')).toContainText('ready', { timeout: 15_000 });
  await expect(panel).toContainText('EPSG:32631');
  await expect(panel).toContainText('1 malformed');

  // the first build's toast goes away, the rebuild brings another
  await expect(page.getByText('Spatial index built')).toHaveCount(0, { timeout: 10_000 });
  await panel.getByRole('button', { name: 'Rebuild' }).click();
  await expect(page.getByText('Spatial index built')).toBeVisible({ timeout: 15_000 });

  await panel.getByRole('button', { name: 'Map' }).click();
  const indexed = panel.getByRole('region', { name: 'Indexed geometries' });
  await expect(indexed).toHaveAttribute('data-state', 'ready');
  await expect(indexed).toHaveAttribute('data-features', '5');
});
