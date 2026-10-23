// The path finder in Explore (docs/specs/U09-path-finder.md, Phase 2: acceptance examples
// A4, A5 and A7 to A10, and the empty states). The dataset `paths9` has two commits: 1
// loads U03 §7's people, organizations and cities, every one typed, and 2 adds
// ex:alice ex:knows ex:bob. The mock answers `SERVICE path:search` itself (mock/paths.mjs).

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';
const SHOTS = '../target/audits/u09-path-finder';

const update = (request: APIRequestContext, sparql: string, message: string) =>
  request
    .post('/paths9/update', {
      headers: { 'Content-Type': 'application/sparql-update', 'Sparkles-Commit-Message': message },
      data: `PREFIX ex: <${EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ${sparql}`,
    })
    .then((r) => expect(r.ok(), `update: ${message}`).toBe(true));

test.beforeAll(async ({ request }) => {
  mkdirSync(SHOTS, { recursive: true });
  // a second dataset to switch to
  const other = await request.post('/$/datasets', { data: { dbName: 'paths9b', dbType: 'mem' } });
  expect(other.ok() || other.status() === 409).toBe(true);
  const r = await request.post('/$/datasets', { data: { dbName: 'paths9', dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  await update(
    request,
    `INSERT DATA {
      ex:alice a ex:Person ; rdfs:label "Alice" ; ex:worksFor ex:acme .
      ex:bob a ex:Person ; rdfs:label "Bob" ; ex:worksFor ex:beta .
      ex:carol a ex:Person ; rdfs:label "Carol" ; ex:knows ex:alice ; ex:worksFor ex:acme .
      ex:acme a ex:Org ; rdfs:label "Acme" ; ex:locatedIn ex:lyon .
      ex:beta a ex:Org ; rdfs:label "Beta" ; ex:locatedIn ex:lyon .
      ex:lyon a ex:City ; rdfs:label "Lyon" .
      ex:loner rdfs:label "Loner" .
    }`,
    'Load the people',
  );
  await update(request, 'INSERT DATA { ex:alice ex:knows ex:bob }', 'Alice knows Bob');
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'paths9'));
});

type Drawn = { nodes: [string, string][]; edges: [string, string][] };
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const edgeLabels = async (page: Page) => (await drawn(page)).edges.map(([l]) => l);

const panel = (page: Page) => page.getByRole('region', { name: 'Paths' });
const list = (page: Page) => panel(page).getByRole('listbox', { name: 'Paths found' });

async function openAlice(page: Page, extra = '') {
  await page.goto(`/ui/explore?ds=paths9&iri=${encodeURIComponent(`${EX}alice`)}${extra}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(1);
}

/** Choose an end by typing its label and picking the option. */
async function choose(page: Page, end: 'From' | 'To', text: string, option: string | RegExp) {
  const box = panel(page).getByRole('combobox', { name: end });
  await box.fill(text);
  await panel(page).getByRole('option', { name: option }).first().click();
}

test('A4: k shortest paths from Alice to Lyon, listed and drawn in order', async ({ page }) => {
  await openAlice(page);
  await page.getByRole('button', { name: 'Paths', exact: true }).click();
  // the selected node fills From
  await expect(panel(page).locator('#path-from').locator('..')).toContainText('Alice');
  await choose(page, 'To', 'Lyon', /Lyon/);
  await panel(page).getByLabel('k shortest').check();
  const req = page.waitForRequest(
    (r) =>
      r.url().includes('/paths9/sparql') &&
      r.method() === 'POST' &&
      (r.postData() ?? '').includes('path:search'),
  );
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  const body = (await req).postData() ?? '';
  // the query is bounded: k paths of at most 8 edges, a tenth of the default visit limit
  expect(body).toContain('path:k 3 ;');
  expect(body).toContain('path:maxVisited 1000000 ; path:limit 3 ;');
  expect(body).toContain('path:excludePredicate');
  const rows = list(page).getByRole('option');
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toHaveAccessibleName(
    'path 1, length 2, Alice works for Acme, Acme located in Lyon',
  );
  const lengths = await rows.locator('.len').allTextContents();
  expect(lengths).toEqual([...lengths].sort());
  // the graph carries the path numbers on the edges
  await expect.poll(() => edgeLabels(page)).toContain('worksFor · 1');
  await expect(panel(page)).toContainText('at commit 2 · main');
  // the animation draws one edge every 250 ms
  await page.waitForTimeout(2500);
  await page.screenshot({ path: `${SHOTS}/desktop-k-shortest.png` });
  // the number keys and the list highlight a path
  await list(page).focus();
  await page.keyboard.press('2');
  await expect(page.locator('.wrap[data-path-focus]')).toHaveAttribute('data-path-focus', '2');
  await expect(rows.nth(1)).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('ArrowDown');
  await expect(page.locator('.wrap[data-path-focus]')).toHaveAttribute('data-path-focus', '3');
  await page.screenshot({ path: `${SHOTS}/desktop-highlight.png` });
  await page.keyboard.press('a');
  await expect(page.locator('.wrap[data-path-focus]')).toHaveAttribute('data-path-focus', '');
  // the paths animate: elements were revealed one step at a time
  expect(Number(await page.locator('.wrap').getAttribute('data-animations'))).toBeGreaterThan(0);
  // Add to graph keeps them as ordinary elements and closes the panel
  await panel(page).getByRole('button', { name: 'Add to graph' }).click();
  await expect(panel(page)).toHaveCount(0);
  await expect.poll(() => edgeLabels(page)).toContain('locatedIn');
  expect((await drawn(page)).nodes.map(([l]) => l)).toContain('Lyon');
});

test('A5: with reduced motion the paths appear at once', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await openAlice(page);
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Lyon', /Lyon/);
  await panel(page).getByLabel('k shortest').check();
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option')).toHaveCount(3);
  await expect.poll(() => edgeLabels(page)).toContain('worksFor · 1');
  await expect(page.locator('.wrap')).toHaveAttribute('data-animations', '0');
});

test('A7: a past state has no path through ex:knows, and the header names it', async ({ page }) => {
  await openAlice(page, '&at=commit:1');
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Bob', /Bob/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  const row = list(page).getByRole('option').first();
  await expect(row).toHaveAccessibleName(/length 4/);
  await expect(row).not.toContainText('knows');
  await expect(panel(page)).toContainText('at commit 1 · main');
  // at the head the path through ex:knows is one edge
  await openAlice(page);
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Bob', /Bob/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option').first()).toHaveAccessibleName(
    'path 1, length 1, Alice knows Bob',
  );
});

test('A8: a search that reaches the visit limit says so', async ({ page }) => {
  await openAlice(page);
  await page.route('**/paths9/sparql**', async (route) => {
    if (!(route.request().postData() ?? '').includes('path:search')) return route.fallback();
    await route.fulfill({
      status: 400,
      contentType: 'application/json',
      body: JSON.stringify({
        error:
          'path:search: a search visited more than 1000000 nodes; raise path:maxVisited, lower path:maxLength or add predicates',
      }),
    });
  });
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Lyon', /Lyon/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(panel(page).getByRole('alert')).toContainText(
    'The search stopped after visiting 1,000,000 nodes',
  );
  await expect(panel(page).getByRole('alert')).toContainText('lower the hub limit');
  // a timeout is told apart
  await page.unroute('**/paths9/sparql**');
  await page.route('**/paths9/sparql**', async (route) => {
    if (!(route.request().postData() ?? '').includes('path:search')) return route.fallback();
    await route.fulfill({
      status: 408,
      contentType: 'application/json',
      body: JSON.stringify({ error: 'the query did not finish within its 10 s timeout' }),
    });
  });
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(panel(page).getByRole('alert')).toContainText('The search took longer than 10 s');
});

test('A9: the keyboard alone finds and walks the paths', async ({ page }) => {
  await openAlice(page);
  await page.locator('body').click({ position: { x: 5, y: 5 } });
  await page.keyboard.press('Shift+P');
  const from = panel(page).getByRole('combobox', { name: 'From' });
  await expect(from).toBeFocused();
  await page.keyboard.press('Backspace');
  await page.keyboard.type('Alice');
  await expect(panel(page).getByRole('option', { name: /Alice/ }).first()).toBeVisible();
  await page.keyboard.press('Enter');
  await panel(page).getByRole('combobox', { name: 'To' }).focus();
  await page.keyboard.type('Lyon');
  await expect(panel(page).getByRole('option', { name: /Lyon/ }).first()).toBeVisible();
  await page.keyboard.press('Enter');
  await page.keyboard.press('Enter');
  await expect(panel(page).locator('[aria-live="polite"]')).toHaveText(
    '1 path found, the shortest of length 2',
  );
  await list(page).focus();
  await page.keyboard.press('1');
  await expect(page.locator('.wrap[data-path-focus]')).toHaveAttribute('data-path-focus', '1');
  await page.keyboard.press('Escape');
  await expect(page.locator('.wrap[data-path-focus]')).toHaveAttribute('data-path-focus', '');
  await page.keyboard.press('Escape');
  await expect(panel(page)).toHaveCount(0);
});

test('the empty states', async ({ page }) => {
  await openAlice(page);
  await page.keyboard.press('Shift+P');
  // the same entity twice
  await choose(page, 'To', 'Alice', /Alice/);
  await expect(panel(page).getByRole('button', { name: 'Find paths' })).toBeDisabled();
  await expect(panel(page)).toContainText('Choose two different entities');
  // no path within the length, with the offer of a longer one
  await panel(page).getByRole('button', { name: 'Clear To' }).click();
  await choose(page, 'To', 'Loner', /Loner/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(panel(page)).toContainText('No path of up to 6 edges.');
  await panel(page).getByRole('button', { name: 'Allow 8' }).click();
  await expect(panel(page)).toContainText('No path of up to 8 edges.');
  await expect(panel(page).getByRole('button', { name: 'Allow 8' })).toHaveCount(0);
  // a literal may end a path but not start one
  await panel(page).getByRole('button', { name: 'Clear To' }).click();
  await panel(page).getByRole('combobox', { name: 'To' }).fill('"Lyon"');
  await panel(page).getByRole('option', { name: /Use/ }).click();
  await expect(panel(page).getByRole('button', { name: 'Swap from and to' })).toBeDisabled();
});

test('Open in editor takes the search to the query page', async ({ page }) => {
  await openAlice(page);
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Lyon', /Lyon/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option')).toHaveCount(1);
  await panel(page).getByRole('button', { name: 'Open in editor' }).click();
  await expect(page).toHaveURL(/\/ui\/query/);
  await expect(page.locator('.cm-content')).toContainText('path:search');
});

/** Holds the requests that `match` picks until the returned function is called. */
async function hold(page: Page, match: (body: string) => boolean) {
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  let seen!: () => void;
  const arrived = new Promise<void>((r) => (seen = r));
  await page.route('**/paths9/sparql**', async (route) => {
    if (!match(route.request().postData() ?? '')) return route.fallback();
    seen();
    await gate;
    await route.fallback();
  });
  return { arrived, release };
}
const isSearch = (b: string) => b.includes('path:search');
const isLabels = (b: string) => b.includes('VALUES ?s') && !b.includes('path:search');

test('a search keeps the state it ran at, and Open in editor runs it again', async ({ page }) => {
  await openAlice(page, '&at=commit:1');
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Bob', /Bob/);
  const held = await hold(page, isSearch);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await held.arrived;
  // the slider moves to the head while the search runs at commit 1
  const bar = page.getByRole('region', { name: 'Graph history time slider' });
  await bar.getByRole('slider', { name: 'Commit' }).focus();
  await page.keyboard.press('End');
  held.release();
  await expect(list(page).getByRole('option').first()).toHaveAccessibleName(/length 4/);
  await expect(panel(page)).toContainText('at commit 1 · main');
  await expect(panel(page).getByRole('button', { name: /^Search again at/ })).toBeVisible();
  // a change to the form after the search does not change the query that is opened
  await panel(page).getByRole('button', { name: 'Clear To' }).click();
  await choose(page, 'To', 'Lyon', /Lyon/);
  await panel(page).getByRole('button', { name: 'Open in editor' }).click();
  await expect(page).toHaveURL(/\/ui\/query/);
  await expect(page.locator('.cm-content')).toContainText('bob');
  await expect(page.locator('.cm-content')).not.toContainText('lyon');
});

test('Clear during the label lookup leaves nothing drawn', async ({ page }) => {
  await openAlice(page);
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Lyon', /Lyon/);
  // the paths arrive, and the labels of their nodes are held
  await page.waitForTimeout(300);
  const held = await hold(page, isLabels);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await held.arrived;
  await panel(page).getByRole('button', { name: 'Close paths' }).click();
  held.release();
  await page.waitForTimeout(1000);
  expect((await edgeLabels(page)).some((l) => l.includes(' · '))).toBe(false);
  expect((await drawn(page)).nodes.map(([l]) => l)).not.toContain('Lyon');
});

test('switching datasets clears the paths', async ({ page }) => {
  await openAlice(page);
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Lyon', /Lyon/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect.poll(() => edgeLabels(page)).toContain('worksFor · 1');
  await page.locator('.ds-button').click();
  await page
    .getByRole('listbox', { name: 'Datasets' })
    .getByRole('option', { name: /paths9b/ })
    .click();
  await expect(panel(page)).toHaveCount(0);
  // the empty dataset opens on its overview, with no paths left to draw
  await expect(page.getByText('No classes to draw')).toBeVisible();
  await expect(page.locator('.wrap[data-graph]')).toHaveCount(0);
});

test('compare mode marks the path edges the earlier state lacks', async ({ page }) => {
  // Carol's neighbourhood does not draw ex:alice ex:knows ex:bob, which commit 2 added
  await page.goto(`/ui/explore?ds=paths9&iri=${encodeURIComponent(`${EX}carol`)}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(1);
  await page.getByRole('button', { name: 'Graph history' }).click();
  const bar = page.getByRole('region', { name: 'Graph history time slider' });
  await bar.getByRole('slider', { name: /^(Commit|Compared to)$/ }).focus();
  await page.keyboard.press('c');
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Bob', /Bob/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option').first()).toHaveAccessibleName(
    'path 1, length 2, Carol knows Alice, Alice knows Bob',
  );
  // Carol knows Alice is drawn and unchanged, Alice knows Bob is the overlay's and added
  const marks = async () =>
    (await drawn(page)).edges
      .filter(([l]) => l.endsWith('knows · 1'))
      .map(([, c]) => c)
      .sort();
  await expect.poll(marks).toEqual(['', 'added']);
  // from the empty dataset to commit 2 both edges are added
  await bar.getByRole('slider', { name: 'Compared from' }).focus();
  await page.keyboard.press('Home');
  await expect.poll(marks).toEqual(['added', 'added']);
  // from the empty dataset to commit 1 only Carol knows Alice is. Neither state holds Alice
  // knows Bob, which the paths found at commit 2 still draw, so it is not marked
  await bar.getByRole('slider', { name: 'Compared to' }).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(bar).toContainText('from commit 0 to commit 1');
  await expect.poll(marks).toEqual(['', 'added']);
});

test('compare mode keeps the marks of path edges when the graph changes under them', async ({
  page,
}) => {
  // Alice's neighbourhood draws Alice knows Bob, and the comparison marks it
  await openAlice(page);
  await page.getByRole('button', { name: 'Graph history' }).click();
  const bar = page.getByRole('region', { name: 'Graph history time slider' });
  await bar.getByRole('slider', { name: /^(Commit|Compared to)$/ }).focus();
  await page.keyboard.press('c');
  await page.keyboard.press('Shift+P');
  await choose(page, 'To', 'Bob', /Bob/);
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  await expect(list(page).getByRole('option').first()).toHaveAccessibleName(
    'path 1, length 1, Alice knows Bob',
  );
  const knows = async () =>
    (await drawn(page)).edges.filter(([l]) => l.endsWith('knows · 1')).map(([, c]) => c);
  await expect.poll(knows).toEqual(['added']);
  // the graph moves to Carol, so the overlay draws Alice knows Bob itself
  const search = page.getByRole('combobox', { name: /Find a resource/ });
  await search.fill('Carol');
  await page.locator('#search-results').getByRole('option', { name: /Carol/ }).first().click();
  await expect.poll(async () => (await drawn(page)).nodes.map(([l]) => l)).toContain('Acme');
  await expect(panel(page)).toBeVisible();
  await expect.poll(knows).toEqual(['added']);
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test('A10: taps alone find the paths, and the page does not scroll sideways', async ({
    page,
  }) => {
    await openAlice(page);
    await page.getByRole('button', { name: 'Paths', exact: true }).tap();
    await expect(panel(page)).toBeVisible();
    await expectNoSidewaysScroll(page, 'the path finder');
    await page.screenshot({ path: `${SHOTS}/phone-form.png` });
    await panel(page).getByRole('combobox', { name: 'To' }).tap();
    await panel(page).getByRole('combobox', { name: 'To' }).fill('Lyon');
    await panel(page).getByRole('option', { name: /Lyon/ }).first().tap();
    await panel(page).getByLabel('k shortest').tap();
    await panel(page).getByRole('button', { name: 'Find paths' }).tap();
    const rows = list(page).getByRole('option');
    await expect(rows).toHaveCount(3);
    // the sheet folds to the first path, and the handle shows the rest
    await expect(panel(page)).toHaveClass(/folded/);
    await page.waitForTimeout(2500);
    await expectNoSidewaysScroll(page, 'the path list');
    await page.screenshot({ path: `${SHOTS}/phone-folded.png` });
    await panel(page).getByRole('button', { name: 'Show every path' }).tap();
    await expect(panel(page)).not.toHaveClass(/folded/);
    await rows.nth(1).tap();
    await expect(page.locator('.wrap[data-path-focus]')).toHaveAttribute('data-path-focus', '2');
    await expectNoSidewaysScroll(page, 'the path list');
    await page.screenshot({ path: `${SHOTS}/phone-list.png` });
  });
});
