// Arcs on the map (docs/specs/U11-linked-views.md, Phase 1) against the mock's `flows`
// dataset (mock/geo.mjs): ten people in Lyon and four in Paris who work for an organization
// in Paris, one person without a geometry, and routes between airports, two of which cross
// the antimeridian. Commit 1 also has an eleventh person in Lyon whom commit 2 removes, to
// tell `at` reads apart.

import { devices, expect as baseExpect, test, type Locator, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { expectNoSidewaysScroll } from '../phone';

// the first visit to a page compiles it in the dev server, and a map loads MapLibre
test.describe.configure({ timeout: 90_000 });
test.use({ actionTimeout: 15_000 });

const expect = baseExpect.configure({ timeout: 15_000 });

const FLOWS = 'http://example.org/flows/';
const ACME = encodeURIComponent(`${FLOWS}acme`);
const SHOTS = resolve(import.meta.dirname, '../../../target/audits/u11-phase1');
mkdirSync(SHOTS, { recursive: true });

const PREFIXES = `PREFIX ex: <${FLOWS}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
`;

type Fc = {
  features: {
    geometry: { type: string; coordinates: unknown };
    properties: Record<string, unknown>;
  }[];
};

/** The GeoJSON the page last gave one source of a map region. */
async function sourceData(region: Locator, source: string): Promise<Fc> {
  return region.evaluate(
    (el, src) =>
      (
        el as unknown as { __map: { getSource(s: string): { serialize(): { data: unknown } } } }
      ).__map
        .getSource(src)
        .serialize().data as never,
    source,
  );
}

async function openExploreMap(page: Page, extra = '', iri = ACME) {
  await page.goto(`/ui/explore?ds=flows&iri=${iri}${extra}`);
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible();
  await page
    .getByRole('group', { name: 'Show the graph as' })
    .getByRole('button', { name: 'Map' })
    .click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  const region = page.getByRole('region', { name: 'Graph map' });
  await expect(region).toHaveAttribute('data-state', 'ready');
  return { arcmap, region };
}

async function runQuery(page: Page, query: string) {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'flows'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(query);
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  return results;
}

test('A1. Explore draws one arc from Lyon to Paris labelled 10, and the Paris people as a local cluster', async ({
  page,
}) => {
  const asked: string[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().includes('/sparql')) asked.push(r.postData() ?? '');
  });
  const { arcmap, region } = await openExploreMap(page);
  // 14 people, the organization and its geometry node are located; the person without a
  // geometry and the class of the organization are not
  await expect(arcmap).toHaveAttribute('data-places', '16');
  await expect(arcmap).toContainText('16 located');
  await expect(arcmap).toContainText('2 not');
  // two markers: the people of Lyon, and the people of Paris with their employer
  await expect(arcmap).toHaveAttribute('data-clusters', '2');
  await expect(arcmap).toHaveAttribute('data-arcs', '1');
  await expect(arcmap).toHaveAttribute('data-arc-summary', /\+9>.*:worksIn:10$/);
  const lines = await sourceData(region, 'o-arcs');
  expect(lines.features).toHaveLength(1);
  expect(lines.features[0].properties.label).toBe('10');
  expect(lines.features[0].properties.count).toBe(10);
  // an arrowhead at the target
  expect((await sourceData(region, 'o-heads')).features).toHaveLength(1);
  // the Paris cluster holds the four people, the organization and its geometry node, with
  // their links local to it
  const markers = (await sourceData(region, 'o-markers')).features.map((f) => f.properties);
  expect(markers.map((m) => m.size as number).sort((a, b) => a - b)).toEqual([6, 10]);
  expect(markers.find((m) => m.size === 6)!.local).toBe(5);
  // the person without a geometry is listed, and her link is not drawn
  const missing = arcmap.locator('details.undrawn');
  await missing.locator('summary').click();
  await expect(missing).toContainText('Ghost');
  await expect(missing).toContainText('ex:Org');
  // the map only asks where entities are: it makes no query about their relations
  const lookups = asked.filter((q) => q.includes('VALUES ?s'));
  expect(lookups.length).toBeGreaterThan(0);
  expect(lookups.every((q) => !q.includes('worksIn'))).toBe(true);
  await page.setViewportSize({ width: 1440, height: 900 });
  await arcmap.scrollIntoViewIfNeeded();
  await page.screenshot({ path: `${SHOTS}/explore-map-desktop.png` });
});

test('the predicate boxes switch arcs off, and the legend names the colours', async ({ page }) => {
  // a Lyon person: her knows links, in both directions, join the graph
  const { arcmap, region } = await openExploreMap(page, '', encodeURIComponent(`${FLOWS}lyon1`));
  const preds = arcmap.getByRole('group', { name: 'Predicates' });
  await expect(preds.locator('label.pred')).toHaveCount(3);
  const worksIn = preds.locator('label.pred', { hasText: 'worksIn' }).getByRole('checkbox');
  // works for (Lyon to Paris) and knows both ways; the geometry link stays in Lyon
  await expect(arcmap).toHaveAttribute('data-arcs', '3');
  await worksIn.uncheck();
  await expect(arcmap).toHaveAttribute('data-arcs', '2');
  await worksIn.check();
  await expect(arcmap).toHaveAttribute('data-arcs', '3');
  // knows goes both ways between Lyon and Paris: two arcs, bowed to opposite sides
  const lines = (await sourceData(region, 'o-arcs')).features;
  const knows = lines.filter((f) => String(f.properties.id).includes('knows'));
  expect(knows).toHaveLength(2);
  const mid = (f: (typeof knows)[0]) => {
    const part = (f.geometry.coordinates as number[][][])[0];
    return part[Math.floor(part.length / 2)];
  };
  const [a, b] = knows.map(mid);
  expect(Math.hypot(a[0] - b[0], a[1] - b[1])).toBeGreaterThan(0.5);
  // each predicate has its own colour
  const colours = new Set(lines.map((f) => f.properties.color));
  expect(colours.size).toBe(2);
});

test('keys: Tab walks the markers, Enter selects, Escape clears; the graph shows the same selection', async ({
  page,
}) => {
  const { arcmap, region } = await openExploreMap(page);
  const canvas = region.locator('canvas.maplibregl-canvas');
  await canvas.focus();
  await page.keyboard.press('Tab');
  // the largest marker first: the ten people of Lyon
  await expect(arcmap.locator('.detail')).toContainText('2 of 2'.replace('2 of 2', '1 of 2'));
  await expect(arcmap.locator('.detail')).toContainText('10 places');
  await page.keyboard.press('Enter');
  const bar = page.getByRole('region', { name: 'Selection' });
  await expect(bar).toContainText('10 selected by the map');
  // the selection stands out: the arc and marker are drawn again in the accent colour
  expect((await sourceData(region, 'o-hot-arcs')).features).toHaveLength(1);
  expect((await sourceData(region, 'o-hot-markers')).features.length).toBeGreaterThanOrEqual(1);
  await expect(page.locator('.sr-only[aria-live="polite"]').last()).toContainText(
    '10 selected by the map',
  );
  // the second marker, with the organization, added with Shift+Enter
  await page.keyboard.press('Tab');
  await expect(arcmap.locator('.detail')).toContainText('2 of 2');
  await page.keyboard.press('Shift+Enter');
  await expect(bar).toContainText('16 selected');
  // adding what is selected already keeps it
  await page.keyboard.press('Shift+Enter');
  await expect(bar).toContainText('16 selected');
  // Tab past the last marker moves on to the zoom buttons, whose keys are their own
  await page.keyboard.press('Tab');
  await expect(region.getByRole('button', { name: 'Zoom in' })).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(region.getByRole('button', { name: 'Zoom out' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(bar).toContainText('16 selected');
  // the canvas shows the same selection: what is not selected is dimmed (the organization,
  // the person without a geometry, and the literals and classes around them)
  await page
    .getByRole('group', { name: 'Show the graph as' })
    .getByRole('button', { name: 'Graph' })
    .click();
  await expect(page.getByRole('region', { name: 'Selection' })).toContainText('16 selected');
  await expect(page.locator('.wrap[data-muted]')).not.toHaveAttribute('data-muted', '0');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('region', { name: 'Selection' })).toHaveCount(0);
  await expect(page.locator('.wrap[data-muted]')).toHaveAttribute('data-muted', '0');
});

test('a click on the map selects, Clear and Open as graph work', async ({ page }) => {
  const { arcmap, region } = await openExploreMap(page);
  // list alternative: the same markers and arcs as text
  const list = arcmap.locator('details.list');
  await list.locator('summary').click();
  await expect(list.getByRole('list', { name: 'Markers in view' }).getByRole('button')).toHaveCount(
    2,
  );
  const arcs = list.getByRole('list', { name: 'Arcs in view' }).getByRole('button');
  await expect(arcs).toHaveCount(1);
  await expect(arcs.first()).toContainText('worksIn');
  await expect(arcs.first()).toContainText('10');
  await arcs.first().click();
  const bar = page.getByRole('region', { name: 'Selection' });
  await expect(bar).toContainText('16 selected by the map');
  await expect(arcmap.locator('.detail')).toContainText('worksIn');
  // dimming the rest
  const dimmed = await region.evaluate((el) =>
    (
      el as unknown as { __map: { getPaintProperty(l: string, p: string): number } }
    ).__map.getPaintProperty('o-arc', 'line-opacity'),
  );
  expect(dimmed).toBeLessThan(0.5);
  // click a marker on the canvas: the people of Lyon
  const lyon = (await sourceData(region, 'o-markers')).features.find(
    (f) => f.properties.size === 10,
  )!;
  // the list opened above changed the page layout, so centre the map on the marker first
  const at = await region.evaluate((el, c) => {
    const m = (
      el as unknown as {
        __map: {
          jumpTo(o: { center: number[] }): void;
          project(c: number[]): { x: number; y: number };
        };
      }
    ).__map;
    m.jumpTo({ center: c as number[] });
    return m.project(c as number[]);
  }, lyon.geometry.coordinates as number[]);
  const canvas = region.locator('canvas.maplibregl-canvas');
  await canvas.scrollIntoViewIfNeeded();
  const box = (await canvas.boundingBox())!;
  await page.mouse.click(box.x + at.x, box.y + at.y);
  await expect(bar).toContainText('10 selected by the map');
  await bar.getByRole('button', { name: 'Clear' }).click();
  await expect(bar).toHaveCount(0);
  // Open as graph: select the Lyon people and draw them
  await arcmap.locator('details.list').getByRole('button', { name: /\+9/ }).first().click();
  await page
    .getByRole('region', { name: 'Selection' })
    .getByRole('button', { name: 'Open as graph' })
    .click();
  await expect(
    page.getByRole('group', { name: 'Show the graph as' }).getByRole('button', { name: 'Graph' }),
  ).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('.wrap[data-graph]')).toHaveAttribute('data-graph', /Lyon 7/);
});

test('Shift+M toggles the map', async ({ page }) => {
  await page.goto(`/ui/explore?ds=flows&iri=${ACME}`);
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible();
  await page.locator('body').click({ position: { x: 5, y: 5 } });
  await page.keyboard.press('Shift+M');
  await expect(page.locator('.arcmap')).toBeVisible();
  await expect(page).toHaveURL(/map=1/);
  await page.keyboard.press('Shift+M');
  await expect(page.locator('.arcmap')).toHaveCount(0);
});

test('a map without any located entity says so and what it looks for', async ({ page }) => {
  await page.goto(
    `/ui/explore?ds=foaf&iri=${encodeURIComponent('http://example.org/ontology#Person')}`,
  );
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible();
  await page
    .getByRole('group', { name: 'Show the graph as' })
    .getByRole('button', { name: 'Map' })
    .click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toContainText('None of these entities has a geometry');
  await expect(arcmap).toContainText('geo:hasGeometry');
  await expect(arcmap).toHaveAttribute('data-places', '0');
});

test('an entity whose geometry the caller cannot read is not located and its links are not drawn', async ({
  page,
}) => {
  // the server answers the geometry lookups as it would for a caller who cannot see the
  // geometry of the people of Lyon
  await page.route('**/flows/sparql*', async (route, request) => {
    const body = request.postData() ?? '';
    if (!body.includes('VALUES ?s')) return route.continue();
    const res = await route.fetch();
    const json = await res.json();
    json.rows = json.rows.filter((r: { value?: string }[]) => !JSON.stringify(r).includes('lyon'));
    json.meta.totalRows = json.rows.length;
    json.meta.sentRows = json.rows.length;
    await route.fulfill({ response: res, json });
  });
  const { arcmap } = await openExploreMap(page);
  // the four people of Paris, the organization and its geometry node
  await expect(arcmap).toHaveAttribute('data-places', '6');
  const summary = (await arcmap.getAttribute('data-arc-summary')) ?? '';
  expect(summary).toContain('worksIn');
  expect(summary).not.toContain('Lyon');
  await arcmap.locator('details.undrawn summary').click();
  await expect(arcmap.locator('details.undrawn')).toContainText('Lyon 1');
});

test('the map reads the state the graph slider names (at)', async ({ page }) => {
  const reads: { at: string | null; body: string }[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().includes('/flows/sparql'))
      reads.push({ at: new URL(r.url()).searchParams.get('at'), body: r.postData() ?? '' });
  });
  // the head has ten people in Lyon
  const head = await openExploreMap(page);
  await expect(head.arcmap).toHaveAttribute('data-arc-summary', /:worksIn:10$/);
  // commit 1 had eleven
  reads.length = 0;
  const past = await openExploreMap(page, '&at=1');
  await expect(past.arcmap).toHaveAttribute('data-arc-summary', /:worksIn:11$/);
  const lookups = reads.filter((r) => r.body.includes('VALUES ?s'));
  expect(lookups.length).toBeGreaterThan(0);
  expect(lookups.every((r) => r.at === '1')).toBe(true);
});

test('A2. a result with two geometry columns draws one arc per row, split at the antimeridian', async ({
  page,
}) => {
  const results = await runQuery(
    page,
    `${PREFIXES}SELECT ?label ?from ?to WHERE {
  ?r a ex:Route ; rdfs:label ?label ; ex:fromGeom ?from ; ex:toGeom ?to .
} ORDER BY ?label`,
  );
  await results.getByRole('tab', { name: 'Map' }).click();
  await results.getByRole('button', { name: 'Arcs' }).click();
  const region = page.getByRole('region', { name: 'Result arcs' });
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  await expect(region).toHaveAttribute('data-state', 'ready');
  // from and to default to the two geometry columns; label by the route's name
  await expect(results.getByLabel('Column the arcs start in')).toHaveValue('from');
  await expect(results.getByLabel('Column the arcs end in')).toHaveValue('to');
  await results.getByLabel('Column that labels the arcs').selectOption('label');
  await expect(arcmap).toHaveAttribute('data-arcs', '3');
  await expect(results.getByText('4 of 4 rows drawn')).toBeVisible();
  const lines = (await sourceData(region, 'o-arcs')).features;
  const parts = lines.map((f) => (f.geometry.coordinates as unknown[]).length).sort();
  // Paris to New York stays whole; Tokyo to San Francisco and Auckland to Honolulu are cut.
  // Paris and Lyon are within 20 px at this zoom, so their route is inside one marker.
  expect(parts).toEqual([1, 2, 2]);
  for (const f of lines) {
    for (const part of f.geometry.coordinates as number[][][])
      for (const [lon] of part) expect(Math.abs(lon)).toBeLessThanOrEqual(180);
  }
  const cut = lines.find((f) => (f.geometry.coordinates as unknown[]).length === 2)!;
  const [west, east] = cut.geometry.coordinates as number[][][];
  expect(west.at(-1)![0]).toBe(180);
  expect(east[0][0]).toBe(-180);
  expect(lines.map((f) => f.properties.label).sort()).toEqual([
    'Auckland to Honolulu',
    'Paris to New York',
    'Tokyo to San Francisco',
  ]);
  await page.setViewportSize({ width: 1440, height: 1100 });
  await arcmap.scrollIntoViewIfNeeded();
  await results.screenshot({ path: `${SHOTS}/result-arcs-desktop.png` });
  // zoomed in, Paris and Lyon part and their route is an arc of its own
  await region.evaluate((el) =>
    (el as unknown as { __map: { jumpTo(o: object): void } }).__map.jumpTo({
      center: [3.6, 47.3],
      zoom: 7,
    }),
  );
  await expect(arcmap).toHaveAttribute('data-arcs', '4');
  // a row in the list highlights its arc
  const row = results.getByRole('button', { name: /Tokyo/ }).last();
  await row.click();
  await expect(row).toHaveAttribute('aria-pressed', 'true');
  expect((await sourceData(region, 'o-hot-arcs')).features).toHaveLength(1);
  // a marker chosen from the list stays selected and does not start the lookups again
  const lookups: string[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().includes('/sparql')) lookups.push(r.url());
  });
  const bar = page.getByRole('region', { name: 'Selection' });
  await region.evaluate((el) =>
    (el as unknown as { __map: { jumpTo(o: object): void } }).__map.jumpTo({
      center: [3.6, 47.3],
      zoom: 4,
    }),
  );
  await arcmap.locator('details.list summary').click();
  await arcmap.getByRole('list', { name: 'Markers in view' }).getByRole('button').first().click();
  await expect(bar).toContainText('selected by the map');
  await page.waitForTimeout(700);
  await expect(bar).toContainText('selected by the map');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  expect(lookups).toHaveLength(0);
});

test('a result with two entity columns offers Arcs when the entities have geometries', async ({
  page,
}) => {
  const asked: { at: string | null; path: string; body: string }[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().includes('/sparql'))
      asked.push({
        at: new URL(r.url()).searchParams.get('at'),
        path: new URL(r.url()).pathname,
        body: r.postData() ?? '',
      });
  });
  const results = await runQuery(
    page,
    `${PREFIXES}SELECT ?person ?org WHERE { ?person ex:worksIn ?org } ORDER BY ?person`,
  );
  // no geometry column, but the people have geometries: the tab is there
  await results.getByRole('tab', { name: 'Map' }).click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  // fifteen rows: ten people of Lyon, four of Paris, one without a geometry
  await expect(arcmap).toHaveAttribute('data-arc-summary', /:\?person → \?org:10/);
  await expect(arcmap).toContainText('15 located');
  await expect(results.getByText('14 of 15 rows drawn')).toBeVisible();
  // the lookups ran on the dataset, at the head, in batches of VALUES
  const lookups = asked.filter((r) => r.body.includes('VALUES ?s'));
  expect(lookups.length).toBeGreaterThan(0);
  expect(lookups.every((r) => r.at === null && r.path.endsWith('/flows/sparql'))).toBe(true);
});

test('on a branch the lookups ask the branch, and the arcs show what the branch has', async ({
  page,
  request,
}) => {
  // a branch with an eleventh person in Lyon; main keeps ten
  const made = await request.post('/$/branches/flows', { data: { name: 'rerouted' } });
  expect([201, 409]).toContain(made.status());
  if (made.status() === 201) {
    const u = await request.post('/flows@rerouted/update', {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <${FLOWS}> PREFIX geo: <http://www.opengis.net/ont/geosparql#>
INSERT DATA { ex:lyon12 ex:worksIn ex:acme ; geo:hasDefaultGeometry ex:lyon12Geom .
  ex:lyon12Geom geo:asWKT "POINT(4.8457 45.7690)"^^geo:wktLiteral }`,
    });
    expect(u.ok()).toBe(true);
  }
  const asked: string[] = [];
  page.on('request', (r) => {
    if (
      r.method() === 'POST' &&
      r.url().includes('/sparql') &&
      (r.postData() ?? '').includes('VALUES ?s')
    )
      asked.push(new URL(r.url()).pathname);
  });
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'flows'));
  await page.goto('/ui/query');
  await page.getByRole('combobox', { name: 'Branch' }).selectOption('rerouted');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(
    `${PREFIXES}SELECT ?person ?org WHERE { ?person ex:worksIn ?org }`,
  );
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await results.getByRole('tab', { name: 'Map' }).click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  // eleven people of Lyon on the branch
  await expect(arcmap).toHaveAttribute('data-arc-summary', /:\?person → \?org:11/);
  expect(asked.length).toBeGreaterThan(0);
  expect(asked.every((p) => p.endsWith('/flows@rerouted/sparql'))).toBe(true);
});

test('a result read at a past state looks its entities up at that state', async ({ page }) => {
  const asked: (string | null)[] = [];
  page.on('request', (r) => {
    if (
      r.method() === 'POST' &&
      r.url().includes('/sparql') &&
      (r.postData() ?? '').includes('VALUES ?s')
    )
      asked.push(new URL(r.url()).searchParams.get('at'));
  });
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'flows'));
  await page.goto('/ui/query?ds=flows&at=1');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(
    `${PREFIXES}SELECT ?person ?org WHERE { ?person ex:worksIn ?org }`,
  );
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  // the lookup that offers the Map tab waits while the time slider is open, so close the bar
  // and keep the result read at the commit
  await expect(results.getByRole('tab', { name: 'Map' })).toHaveCount(0);
  await page.getByRole('button', { name: 'History' }).click();
  await results.getByRole('tab', { name: 'Map' }).click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  // commit 1 had eleven people in Lyon, the head ten
  await expect(arcmap).toHaveAttribute('data-arc-summary', /:\?person → \?org:11/);
  expect(asked.length).toBeGreaterThan(0);
  expect(asked.every((a) => a === 'commit:1')).toBe(true);
});

test('in a comparison the entities of removed rows are located at the earlier state', async ({
  page,
}) => {
  const asked: { at: string | null; body: string }[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().includes('/sparql'))
      asked.push({ at: new URL(r.url()).searchParams.get('at'), body: r.postData() ?? '' });
  });
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'flows'));
  await page.goto('/ui/query?ds=flows&from=1');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  // the organization's geometry column gives the result its Map tab
  await page.keyboard.insertText(
    `${PREFIXES}PREFIX geo: <http://www.opengis.net/ont/geosparql#>
SELECT ?person ?org ?g WHERE { ?person ex:worksIn ?org . ?org geo:hasDefaultGeometry/geo:asWKT ?g }`,
  );
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await results.getByRole('tab', { name: 'Map' }).click();
  await results.getByRole('button', { name: 'Arcs' }).click();
  const arcmap = page.locator('.arcmap');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  await results.getByLabel('Column the arcs start in').selectOption('person');
  await results.getByLabel('Column the arcs end in').selectOption('org');
  await expect(arcmap).toHaveAttribute('data-loading', 'false');
  const region = page.getByRole('region', { name: 'Result arcs' });
  await expect(region).toHaveAttribute('data-state', 'ready');
  // the eleventh person of Lyon is gone at the head and only the earlier state knows where
  // they were, so their link is drawn, as a removed one
  const lines = (await sourceData(region, 'o-arcs')).features;
  expect(lines.filter((f) => f.properties.change === 'removed')).toHaveLength(1);
  const lookups = asked.filter((a) => a.body.includes('VALUES ?s'));
  expect(lookups.some((a) => a.at === 'commit:1' && a.body.includes('lyon11'))).toBe(true);
  expect(lookups.filter((a) => a.at !== 'commit:1').every((a) => !a.body.includes('lyon11'))).toBe(
    true,
  );
});

test('a result with one entity column has no Map tab', async ({ page }) => {
  const results = await runQuery(
    page,
    `${PREFIXES}SELECT ?person WHERE { ?person ex:worksIn ?org }`,
  );
  await expect(results.getByRole('tab', { name: 'Table' })).toBeVisible();
  await expect(results.getByRole('tab', { name: 'Map' })).toHaveCount(0);
});

test('a result of entities that have no geometry has no Map tab', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'foaf'));
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(
    'SELECT ?a ?b WHERE { ?a <http://www.w3.org/2000/01/rdf-schema#subClassOf> ?b } LIMIT 20',
  );
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toBeVisible();
  await expect(results.getByRole('tab', { name: 'Table' })).toBeVisible();
  // give the lookup time to answer
  await page.waitForTimeout(800);
  await expect(results.getByRole('tab', { name: 'Map' })).toHaveCount(0);
});

test.describe('at 390 px', () => {
  const { defaultBrowserType: _ignored, ...iphone } = devices['iPhone 12'];
  test.use(iphone);

  test('the map and its controls fit the screen, and taps do the work of keys', async ({
    page,
  }) => {
    const { arcmap, region } = await openExploreMap(page);
    await expectNoSidewaysScroll(page, 'the Explore map at 390 px');
    const stage = await arcmap.locator('.stage').boundingBox();
    expect(stage!.width).toBeGreaterThan(300);
    expect(stage!.height).toBeGreaterThanOrEqual(300);
    // the controls are 44 px targets
    for (const target of [
      arcmap.locator('label.pred').first(),
      page.getByRole('group', { name: 'Show the graph as' }).getByRole('button', { name: 'Map' }),
      page.getByRole('group', { name: 'Show the graph as' }).getByRole('button', { name: 'Graph' }),
    ]) {
      const b = (await target.boundingBox())!;
      expect(b.height).toBeGreaterThanOrEqual(43.5);
    }
    await arcmap.locator('.stage').scrollIntoViewIfNeeded();
    await page.screenshot({ path: `${SHOTS}/explore-map-390.png` });
    // tap a list entry instead of Tab and Enter
    const list = arcmap.locator('details.list');
    await list.locator('summary').click();
    const item = list.getByRole('list', { name: 'Markers in view' }).getByRole('button').first();
    expect((await item.boundingBox())!.height).toBeGreaterThanOrEqual(44);
    await item.tap();
    const bar = page.getByRole('region', { name: 'Selection' });
    await expect(bar).toContainText('selected by the map');
    for (const b of await bar.getByRole('button').all()) {
      const box = (await b.boundingBox())!;
      expect(box.height).toBeGreaterThanOrEqual(44);
    }
    await expectNoSidewaysScroll(page, 'the Explore map with a selection at 390 px');
    await page.locator('main').evaluate((el) => (el.scrollTop = 0));
    await page.screenshot({ path: `${SHOTS}/explore-map-390-selected.png` });
    await bar.getByRole('button', { name: 'Clear' }).tap();
    await expect(bar).toHaveCount(0);
    await expect(region).toBeVisible();
  });

  test('result arcs fit the screen', async ({ page }) => {
    const results = await runQuery(
      page,
      `${PREFIXES}SELECT ?label ?from ?to WHERE {
  ?r a ex:Route ; rdfs:label ?label ; ex:fromGeom ?from ; ex:toGeom ?to .
} ORDER BY ?label`,
    );
    await results.getByRole('tab', { name: 'Map' }).tap();
    await results.getByRole('button', { name: 'Arcs' }).tap();
    await expect(page.locator('.arcmap')).toHaveAttribute('data-arcs', '3');
    await expectNoSidewaysScroll(page, 'result arcs at 390 px');
    for (const b of await results
      .getByRole('group', { name: 'Draw the result as' })
      .getByRole('button')
      .all()) {
      expect((await b.boundingBox())!.height).toBeGreaterThanOrEqual(43.5);
    }
    await page.locator('.arcmap .stage').scrollIntoViewIfNeeded();
    await page.screenshot({ path: `${SHOTS}/result-arcs-390.png` });
  });
});
