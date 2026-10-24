// The path finder's Phase 3 (docs/specs/U09-path-finder.md): weights from reifiers in
// Explore, and the panel on the query page's result graph, drawn by the canvas renderer
// and by the WebGL view. The dataset `roads9` is a small road map whose reifiers carry
// `ex:km`: the fewest roads from Alpha to Delta go through Bravo (20 km), and the shortest distance
// through Charlie and Echo (6 km).

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';
const DS = 'roads9';

const update = (request: APIRequestContext, sparql: string) =>
  request
    .post(`/${DS}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <${EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ${sparql}`,
    })
    .then((r) => expect(r.ok(), 'update').toBe(true));

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  await update(
    request,
    `INSERT DATA {
      ex:a rdfs:label "Alpha" . ex:b rdfs:label "Bravo" . ex:c rdfs:label "Charlie" .
      ex:d rdfs:label "Delta" . ex:e rdfs:label "Echo" .
      ex:a ex:road ex:b {| ex:km 10 |} . ex:b ex:road ex:d {| ex:km 10 |} .
      ex:a ex:road ex:c {| ex:km 2 |} . ex:c ex:road ex:e {| ex:km 2 |} .
      ex:e ex:road ex:d {| ex:km 2 |} .
    }`,
  );
  // commit 2 adds a second weight property, so the state decides which ones there are
  await update(request, `INSERT DATA { ex:e ex:road ex:d {| ex:mins 5 |} . }`);
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), DS);
});

type Drawn = { nodes: [string, string][]; edges: [string, string][] };
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const panel = (page: Page) => page.getByRole('region', { name: 'Paths' });
const list = (page: Page) => panel(page).getByRole('listbox', { name: 'Paths found' });

async function choose(page: Page, end: 'From' | 'To', text: string, option: string | RegExp) {
  const box = panel(page).getByRole('combobox', { name: end });
  await box.fill(text);
  await panel(page).getByRole('option', { name: option }).first().click();
}

/** Where the canvas draws the node labelled `label`, from the graph's own instance. */
async function nodeOn(page: Page, label: string) {
  return await page
    .locator('.wrap canvas')
    .first()
    .evaluate((canvas, label) => {
      type Node = { data(k: string): unknown; renderedPosition(): { x: number; y: number } };
      let el: HTMLElement | null = canvas.parentElement;
      while (el && !('_cyreg' in el)) el = el.parentElement;
      const cy = (el as unknown as { _cyreg: { cy: { nodes(): Node[] } } })._cyreg.cy;
      const n = [...cy.nodes()].find((x) => x.data('label') === label)!;
      const p = n.renderedPosition();
      const box = el!.getBoundingClientRect();
      return { x: box.left + p.x, y: box.top + p.y };
    }, label);
}

async function runQuery(page: Page, query: string) {
  await page.addInitScript((q) => {
    localStorage.setItem(
      'sparkles.queryTabs',
      JSON.stringify({ tabs: [{ id: 't1', title: 'Roads', query: q }], active: 't1' }),
    );
  }, query);
  await page.goto(`/ui/query?ds=${DS}`);
  await page.getByRole('button', { name: /^Run/ }).click();
  await page.getByRole('tab', { name: 'Graph' }).click();
}

/** Holds the requests to the dataset whose body matches until `release` is called. */
async function hold(page: Page, match: (body: string) => boolean) {
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  let seen!: () => void;
  const arrived = new Promise<void>((r) => (seen = r));
  await page.route(`**/${DS}/sparql**`, async (route) => {
    if (!match(route.request().postData() ?? '')) return route.fallback();
    seen();
    await gate;
    await route.fallback().catch(() => {});
  });
  return { arrived, release };
}
const isWeights = (b: string) => b.includes('rdf:reifies');

const ROADS_FROM_A = `PREFIX ex: <${EX}>
CONSTRUCT { ?s ex:road ?o } WHERE { ?s ex:road ?o FILTER(?s = ex:a) }`;

test('weights: the cheapest path by a reifier property comes first, with its cost', async ({
  page,
}) => {
  await page.goto(`/ui/explore?ds=${DS}&iri=${encodeURIComponent(`${EX}a`)}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(1);
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Delta', /^Delta\b/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  // counted in edges, the way through B is the shortest
  await expect(list(page).getByRole('option').first()).toHaveAccessibleName(
    'path 1, length 2, Alpha road Bravo, Bravo road Delta',
  );
  // the picker offers the numeric properties of the reifiers once it is used
  const weight = panel(page).getByRole('combobox', { name: 'Weight' });
  await expect(panel(page)).toContainText('Choose a property of the reifiers');
  await weight.focus();
  await expect(weight.locator(`option[value="${EX}km"]`)).toHaveCount(1);
  await expect(panel(page)).toContainText('2 properties on reifiers can weigh the edges');
  await weight.selectOption(`${EX}km`);
  const req = page.waitForRequest(
    (r) => r.method() === 'POST' && (r.postData() ?? '').includes('path:weight'),
  );
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  expect((await req).postData()).toContain(`path:weight <${EX}km> ;`);
  await expect(list(page).getByRole('option').first()).toHaveAccessibleName(
    'path 1, length 3, cost 6, Alpha road Charlie, Charlie road Echo, Echo road Delta',
  );
  await expect(panel(page).locator('[aria-live="polite"]')).toHaveText(
    '1 path found, the cheapest of cost 6',
  );
  await expect(list(page).locator('.cost')).toHaveText('· cost 6');
  // k shortest by cost
  await panel(page).getByLabel('k shortest').check();
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  const rows = list(page).getByRole('option');
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(1)).toHaveAccessibleName(
    'path 2, length 2, cost 20, Alpha road Bravo, Bravo road Delta',
  );
});

test('the weight list follows the state, and a weight the state lacks is dropped', async ({
  page,
}) => {
  await page.goto(`/ui/explore?ds=${DS}&iri=${encodeURIComponent(`${EX}a`)}&at=commit:2`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(1);
  await page.keyboard.press('Shift+P');
  const weight = panel(page).getByRole('combobox', { name: 'Weight' });
  const note = panel(page).locator('#path-weight-note');
  const bar = page.getByRole('region', { name: 'Graph history time slider' });
  const previous = bar.getByRole('button', { name: 'Previous commit' });
  const next = bar.getByRole('button', { name: 'Next commit' });
  // a list read at commit 2 that arrives after the slider moved to commit 1 is not shown
  const held = await hold(page, isWeights);
  await weight.focus();
  await held.arrived;
  await previous.click();
  held.release();
  await page.unroute(`**/${DS}/sparql**`);
  await page.waitForTimeout(300);
  await expect(note).toContainText('Choose a property of the reifiers');
  // at commit 2 there are two weight properties: choose the one commit 1 lacks
  await next.click();
  await weight.focus();
  await expect(note).toContainText('2 properties on reifiers can weigh the edges');
  await weight.selectOption(`${EX}mins`);
  await expect(note).toContainText('mins on their edges');
  // back at commit 1 the weight is checked at once and dropped
  await previous.click();
  await expect(note).toContainText('1 property on reifiers can weigh the edges');
  await expect(weight.locator(`option[value="${EX}mins"]`)).toHaveCount(0);
});

test('the query page draws the paths on its result graph, adding what it lacks', async ({
  page,
}) => {
  await runQuery(page, ROADS_FROM_A);
  await expect.poll(async () => (await drawn(page)).edges.length).toBe(2);
  await page.getByRole('button', { name: 'Paths', exact: true }).click();
  await choose(page, 'From', 'Alpha', /^Alpha\b/);
  await choose(page, 'To', 'Delta', /^Delta\b/);
  await panel(page).getByLabel('k shortest').check();
  await panel(page).getByRole('combobox', { name: 'Weight' }).focus();
  await panel(page).getByRole('combobox', { name: 'Weight' }).selectOption(`${EX}km`);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option')).toHaveCount(2);
  // the result's two roads carry their paths, and the paths' other roads are added
  await expect
    .poll(async () => (await drawn(page)).edges.map(([l]) => l).sort())
    .toEqual(['road · 1', 'road · 1', 'road · 1', 'road · 2', 'road · 2']);
  // the result names its nodes by their IRIs, and the nodes it lacks come with their labels
  expect((await drawn(page)).nodes.map(([l]) => l).sort()).toEqual([
    'Delta',
    'Echo',
    'a',
    'b',
    'c',
  ]);
  // the list's keys highlight a path on the graph
  await list(page).focus();
  await page.keyboard.press('2');
  await expect(page.locator('.wrap[data-path-focus]')).toHaveAttribute('data-path-focus', '2');
  // Explore's Add to graph has no place on a result
  await expect(panel(page).getByRole('button', { name: 'Add to graph' })).toHaveCount(0);
  // a node the paths added, which the result lacks, has the node menu too
  const at = await nodeOn(page, 'Delta');
  await page.mouse.click(at.x, at.y, { button: 'right' });
  await expect(page.getByRole('menuitem', { name: 'Paths to here' })).toBeVisible();
  await page.keyboard.press('Escape');
  await page.mouse.click(1, 1);
  // closing the panel leaves the result as it was
  await panel(page).getByRole('button', { name: 'Close paths' }).click();
  await expect
    .poll(async () => (await drawn(page)).edges.map(([l]) => l))
    .toEqual(['road', 'road']);
});

test('a node menu on the result graph starts a search, and a new result clears the paths', async ({
  page,
}) => {
  await runQuery(page, ROADS_FROM_A);
  await expect.poll(async () => (await drawn(page)).edges.length).toBe(2);
  // right-click on the node of ex:a
  const at = await nodeOn(page, 'a');
  await page.mouse.click(at.x, at.y, { button: 'right' });
  await page.getByRole('menuitem', { name: 'Paths from here' }).click();
  await choose(page, 'To', 'Delta', /^Delta\b/);
  const req = page.waitForRequest(
    (r) => r.method() === 'POST' && (r.postData() ?? '').includes('path:search'),
  );
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  expect((await req).postData()).toContain(`path:source <${EX}a> ;`);
  await expect.poll(async () => (await drawn(page)).edges.map(([l]) => l)).toContain('road · 1');
  // running the query again is a new result, which the old paths do not belong to
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect
    .poll(async () => (await drawn(page)).edges.map(([l]) => l))
    .toEqual(['road', 'road']);
  // the panel keeps its ends, and Open in editor opens the paths' query in a new tab here
  await page.getByRole('button', { name: 'Paths', exact: true }).click();
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option')).toHaveCount(1);
  await panel(page).getByRole('button', { name: 'Open in editor' }).click();
  await expect(page.getByRole('tab', { name: /^Paths/ })).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('.cm-content')).toContainText('path:search');
});

test('the WebGL view draws the paths over the result and focuses one', async ({ page }) => {
  await runQuery(page, ROADS_FROM_A);
  await page.getByRole('combobox', { name: 'Renderer' }).selectOption('webgl');
  await expect(page.locator('.lg-host canvas').first()).toBeVisible();
  const overlayEl = page.locator('.lg-overlay');
  await expect(overlayEl).toHaveAttribute('data-layout-runs', /\d/);
  await page.waitForTimeout(500);
  const runs = await overlayEl.getAttribute('data-layout-runs');
  await page.getByRole('button', { name: 'Paths', exact: true }).click();
  await choose(page, 'From', 'Alpha', /^Alpha\b/);
  await choose(page, 'To', 'Delta', /^Delta\b/);
  await panel(page).getByLabel('k shortest').check();
  await panel(page).getByRole('combobox', { name: 'Weight' }).focus();
  await panel(page).getByRole('combobox', { name: 'Weight' }).selectOption(`${EX}km`);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option')).toHaveCount(2);
  const overlay = page.locator('.lg-overlay');
  // five roads lie on the two paths: three on the first, two on the second
  await expect.poll(async () => overlay.getAttribute('data-path-edges')).toBe('5');
  await list(page).focus();
  await page.keyboard.press('1');
  await expect(overlay).toHaveAttribute('data-path-focus', '1');
  await expect.poll(async () => overlay.getAttribute('data-path-edges')).toBe('3');
  await page.keyboard.press('a');
  await expect(overlay).toHaveAttribute('data-path-focus', '');
  // the keys work with focus on the graph as well
  await page.locator('.lg-host').focus();
  await page.keyboard.press('2');
  await expect(overlay).toHaveAttribute('data-path-focus', '2');
  await page.keyboard.press('a');
  await expect(overlay).toHaveAttribute('data-path-focus', '');
  // showing and clearing paths keeps the layout: no new one starts
  await panel(page).getByRole('button', { name: 'Clear', exact: true }).click();
  await expect.poll(async () => overlay.getAttribute('data-path-edges')).toBe('0');
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect.poll(async () => overlay.getAttribute('data-path-edges')).toBe('5');
  expect(await overlay.getAttribute('data-layout-runs')).toBe(runs);
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test('the panel is a sheet over the result graph, and the page does not scroll sideways', async ({
    page,
  }) => {
    await runQuery(page, ROADS_FROM_A);
    await expect.poll(async () => (await drawn(page)).edges.length).toBe(2);
    await page.getByRole('button', { name: 'Paths', exact: true }).tap();
    await expect(panel(page)).toBeVisible();
    await expectNoSidewaysScroll(page, 'the query page with the path finder');
    await panel(page).getByRole('combobox', { name: 'From' }).fill('Alpha');
    await panel(page)
      .getByRole('option', { name: /^Alpha\b/ })
      .first()
      .tap();
    await panel(page).getByRole('combobox', { name: 'To' }).fill('Delta');
    await panel(page)
      .getByRole('option', { name: /^Delta\b/ })
      .first()
      .tap();
    await panel(page).getByRole('button', { name: 'Find paths' }).tap();
    await expect(list(page).getByRole('option')).toHaveCount(1);
    await expectNoSidewaysScroll(page, 'the query page with paths found');
  });
});
