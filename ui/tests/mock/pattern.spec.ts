// The Pattern pane of the query page (spec U08 Phase 1, with the splice core of Phase 2):
// the drawing of the editor's query, locked blocks, canvas edits as splices of the text
// with one undo history, the stable layout, the keyboard, the error states, the result's
// Pattern tab and the 390 px layout. The pane needs the browser module of the formatter
// (`mise run ui:wasm`), which the mock UI loads for the builder even though it formats
// through the mock's endpoint.

import { expect, test, type Locator, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, selectAll } from '../phone';

const editor = (page: Page) => page.locator('.cm-content');

async function setText(page: Page, text: string) {
  await editor(page).click();
  await selectAll(page);
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

const textOf = async (page: Page) =>
  (await editor(page).locator('.cm-line').allInnerTexts())
    .map((l) => l.replace(/\n$/, ''))
    .join('\n');

const pane = (page: Page) => page.getByRole('region', { name: 'Pattern', exact: true });
const canvas = (page: Page) => pane(page).locator('.pcanvas');

type Graph = {
  nodes: string[][];
  edges: [string, string, string, boolean][];
  hulls: number;
  locked: string[];
};
const graph = async (c: Locator): Promise<Graph> =>
  JSON.parse((await c.getAttribute('data-graph')) ?? '{"nodes":[],"edges":[]}');
const positions = async (c: Locator): Promise<Record<string, [number, number]>> =>
  JSON.parse((await c.getAttribute('data-positions')) ?? '{}');
const settled = async (c: Locator) => Number(await c.getAttribute('data-settled'));

const A3 = `PREFIX ex: <http://example.org/>
SELECT ?person ?org WHERE {
  ?person a ex:Person ; ex:worksFor ?org .
  ?org a ex:Org ; ex:locatedIn ex:lyon .
}
LIMIT 1000`;

const A2 = `PREFIX ex: <http://example.org/>
SELECT * WHERE {
  ?person ex:born ?b .
  BIND(YEAR(NOW()) AS ?y) # year
  ?person ex:name ?name .
}`;

async function openPane(page: Page, text: string) {
  await page.goto('/ui/query');
  await setText(page, text);
  await page.getByRole('button', { name: 'Pattern', exact: true }).click();
  await expect(pane(page)).toHaveAttribute('data-state', 'ok');
}

/** Wait for the canvas to settle after the text changed. */
async function redrawn(c: Locator, before: number) {
  await expect.poll(() => settled(c)).toBeGreaterThan(before);
}

test.describe.configure({ timeout: 60_000 });

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    if (!sessionStorage.getItem('pattern-test')) {
      localStorage.removeItem('sparkles.patternPane');
      localStorage.removeItem('sparkles.patternLayouts');
      sessionStorage.setItem('pattern-test', '1');
    }
  });
});

test('A1: the pattern of U03 A3 draws three nodes, with classes as subtitles', async ({ page }) => {
  await openPane(page, A3);
  await expect
    .poll(async () => (await graph(canvas(page))).nodes)
    .toEqual([['?person', 'ex:Person'], ['?org', 'ex:Org'], ['ex:lyon']]);
  const g = await graph(canvas(page));
  expect(g.edges.map((e) => e[0])).toEqual(['ex:worksFor', 'ex:locatedIn']);
  await expect(pane(page).getByTestId('pattern-counts')).toHaveText('4 triples · 0 locked');
  // the list alternative has the same rows
  await pane(page).getByRole('button', { name: 'List' }).click();
  await expect(
    pane(page).getByRole('list', { name: 'Pattern elements in text order' }).locator('li .desc'),
  ).toHaveText([
    'variable ?person, ex:Person',
    'edge ex:worksFor from ?person to ?org',
    'variable ?org, ex:Org',
    'edge ex:locatedIn from ?org to ex:lyon',
    'IRI ex:lyon',
  ]);
  // the strip shows the projection and LIMIT
  await expect(pane(page).getByLabel('Projection and modifiers')).toContainText('?person');
  await expect(pane(page).getByLabel('LIMIT', { exact: true })).toHaveValue('1000');
});

test('A2: a BIND is a locked block, comment included, joined to ?y', async ({ page }) => {
  await openPane(page, A2);
  await expect
    .poll(async () => (await graph(canvas(page))).locked)
    .toEqual(['BIND(YEAR(NOW()) AS ?y) # year']);
  const g = await graph(canvas(page));
  expect(g.nodes.map((n) => n[0])).toContain('?y');
});

test('A6 and A9: optional on an edge is one splice, and one undo in either view reverts it', async ({
  page,
}) => {
  await openPane(page, A3);
  await pane(page).getByRole('button', { name: 'List' }).click();
  const rows = pane(page).locator('li');
  await rows
    .filter({ hasText: 'edge ex:locatedIn' })
    .getByRole('button', { name: 'Optional' })
    .click();
  await expect.poll(() => textOf(page)).toBe(`PREFIX ex: <http://example.org/>
SELECT ?person ?org WHERE {
  ?person a ex:Person ; ex:worksFor ?org .
  ?org a ex:Org .
  OPTIONAL { ?org ex:locatedIn ex:lyon . }
}
LIMIT 1000`);
  await expect(pane(page).getByTestId('pattern-live')).toContainText(
    'Toggled optional on ?org ex:locatedIn ex:lyon',
  );
  await pane(page).getByRole('button', { name: 'Canvas' }).click();
  await expect.poll(async () => (await graph(canvas(page))).hulls).toBe(1);

  // undo in the editor restores the text and the hull goes
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe(A3);
  await expect.poll(async () => (await graph(canvas(page))).hulls).toBe(0);
  // redo from the canvas, then undo from the canvas: one history for both views
  await canvas(page).focus();
  await page.keyboard.press('ControlOrMeta+Shift+Z');
  await expect.poll(() => textOf(page)).toContain('OPTIONAL { ?org ex:locatedIn ex:lyon . }');
  await page.keyboard.press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe(A3);
  // a typed edit and a canvas edit undo alike, one step each
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type(' OFFSET 5');
  await pane(page).getByRole('button', { name: 'List' }).click();
  await rows
    .filter({ hasText: 'edge ex:worksFor' })
    .getByRole('button', { name: /^Delete/ })
    .click();
  await expect.poll(() => textOf(page)).not.toContain('ex:worksFor');
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe(`${A3} OFFSET 5`);
});

test('locked blocks survive canvas edits byte for byte', async ({ page }) => {
  const text = `PREFIX ex: <http://example.org/>
SELECT * WHERE {
  ?person ex:born ?b .
  BIND(YEAR(NOW())   AS ?y) # year
  { SELECT ?person WHERE { ?person ex:p   ?q } }   # sub
  ?person ex:name ?name .
}`;
  await openPane(page, text);
  await pane(page).getByRole('button', { name: 'List' }).click();
  // add a triple, make one optional, delete one
  await pane(page).getByRole('button', { name: 'Edge', exact: true }).click();
  await pane(page).getByLabel('From', { exact: true }).fill('?y');
  await pane(page).getByLabel('Predicate', { exact: true }).fill('ex:of');
  await pane(page).getByLabel('To', { exact: true }).fill('?person');
  await pane(page).getByRole('button', { name: 'Add', exact: true }).click();
  await expect.poll(() => textOf(page)).toContain('?y ex:of ?person .');
  const rows = pane(page).locator('li');
  await rows.filter({ hasText: 'edge ex:name' }).getByRole('button', { name: 'Optional' }).click();
  await expect.poll(() => textOf(page)).toContain('OPTIONAL { ?person ex:name ?name . }');
  await rows
    .filter({ hasText: 'edge ex:born' })
    .getByRole('button', { name: /^Delete/ })
    .click();
  await expect.poll(() => textOf(page)).not.toContain('ex:born');
  const now = await textOf(page);
  expect(now).toContain('BIND(YEAR(NOW())   AS ?y) # year');
  expect(now).toContain('{ SELECT ?person WHERE { ?person ex:p   ?q } }   # sub');
  // the new triple binds ?y, so it went after the BIND that binds it
  expect(now.indexOf('?y ex:of ?person')).toBeGreaterThan(now.indexOf('BIND('));
});

test('A7 and A8: typing keeps every node in place, and a rename keeps its position', async ({
  page,
}) => {
  await openPane(page, A3);
  const c = canvas(page);
  await expect.poll(() => settled(c)).toBeGreaterThan(0);
  const before = await positions(c);
  expect(Object.keys(before)).toHaveLength(3);
  // type a new triple at the end of the group
  const n = await settled(c);
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+End');
  for (let i = 0; i < 2; i++) await page.keyboard.press('ArrowUp');
  await page.keyboard.press('End');
  await page.keyboard.press('Enter');
  await page.keyboard.type('?person ex:age ?age .');
  await redrawn(c, n);
  await expect.poll(async () => Object.keys(await positions(c))).toContain('v:age');
  const after = await positions(c);
  for (const [k, [x, y]] of Object.entries(before)) {
    expect(Math.abs(after[k][0] - x), k).toBeLessThanOrEqual(1);
    expect(Math.abs(after[k][1] - y), k).toBeLessThanOrEqual(1);
  }
  // ?age sits next to ?person
  const [px, py] = after['v:person'];
  const [ax, ay] = after['v:age'];
  expect(Math.hypot(ax - px, ay - py)).toBeLessThan(260);

  // rename ?org to ?company in the editor
  const m = await settled(c);
  const renamed = (await textOf(page)).replaceAll('?org', '?company');
  await setText(page, renamed);
  await redrawn(c, m);
  await expect.poll(async () => Object.keys(await positions(c))).toContain('v:company');
  const moved = await positions(c);
  expect(moved['v:company'][0]).toBeCloseTo(after['v:org'][0], 0);
  expect(moved['v:company'][1]).toBeCloseTo(after['v:org'][1], 0);
  // the same query opened again draws the same picture
  await page.reload();
  await expect(pane(page)).toHaveAttribute('data-state', 'ok');
  await expect.poll(() => settled(canvas(page))).toBeGreaterThan(0);
  const again = await positions(canvas(page));
  expect(again['v:company'][0]).toBeCloseTo(moved['v:company'][0], 0);
});

test('A11: the keyboard reaches every element and edits with announcements', async ({ page }) => {
  await page.goto('/ui/query');
  await setText(page, A3);
  // Alt+P from the editor opens the pane and focuses it
  await page.keyboard.press('Alt+P');
  await expect(canvas(page)).toBeFocused();
  const focus = pane(page).getByTestId('pattern-focus');
  await expect(focus).toHaveText('variable ?person, ex:Person');
  await page.keyboard.press('Tab');
  await expect(focus).toHaveText('edge ex:worksFor from ?person to ?org');
  await page.keyboard.press('Tab');
  await page.keyboard.press('Tab');
  await expect(focus).toHaveText('edge ex:locatedIn from ?org to ex:lyon');
  await page.keyboard.press('Shift+Tab');
  await expect(focus).toHaveText('variable ?org, ex:Org');
  await page.keyboard.press('Tab');
  // O toggles optional on the focused edge; the live region announces it
  await page.keyboard.press('o');
  await expect.poll(() => textOf(page)).toContain('OPTIONAL { ?org ex:locatedIn ex:lyon . }');
  await expect(pane(page).getByTestId('pattern-live')).toContainText('3 nodes, 2 edges');
  // Enter opens the focused element's panel; Escape closes it, then returns to the editor
  await expect.poll(async () => (await graph(canvas(page))).hulls).toBe(1);
  await canvas(page).focus();
  await page.keyboard.press('Enter');
  await expect(pane(page).getByRole('dialog', { name: 'Pattern element' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(pane(page).getByRole('dialog', { name: 'Pattern element' })).toHaveCount(0);
  // E adds an edge from a form
  await page.keyboard.press('e');
  await expect(pane(page).getByLabel('From', { exact: true })).toBeFocused();
  await page.keyboard.type('?org');
  await page.keyboard.press('Tab');
  await page.keyboard.type('ex:name');
  await page.keyboard.press('Tab');
  await page.keyboard.type('"Acme"');
  await page.keyboard.press('Enter');
  await expect.poll(() => textOf(page)).toContain('?org ex:name "Acme" .');
  await expect(pane(page).getByTestId('pattern-live')).toContainText('Added ?org ex:name "Acme"');
  // Delete removes the focused edge
  await canvas(page).focus();
  await page.keyboard.press('Tab');
  await expect(focus).toHaveText('edge ex:worksFor from ?person to ?org');
  await page.keyboard.press('Delete');
  await expect.poll(() => textOf(page)).not.toContain('ex:worksFor');
  await page.keyboard.press('Escape');
  await expect(editor(page)).toBeFocused();
});

test('error states: broken text, updates and disconnected parts', async ({ page }) => {
  await openPane(page, A3);
  // text that does not parse keeps the last pattern, dimmed, and refuses edits
  await setText(page, A3.replace('ex:lyon .', 'ex:lyon . ?x'));
  await expect(canvas(page)).toHaveAttribute('data-dim', 'true');
  await expect(pane(page)).toContainText('does not parse at line 6');
  await expect(pane(page).getByRole('button', { name: 'Edge', exact: true })).toHaveCount(0);
  await setText(page, 'INSERT DATA { <http://x/a> <http://x/b> 1 }');
  await expect(pane(page)).toContainText('Updates are not drawn. The builder edits queries.');
  await setText(page, 'SELECT * { ?a <http://x/p> ?b . ?c <http://x/q> ?d }');
  await expect(pane(page)).toContainText('These parts are not connected');
  await setText(page, 'SELECT * WHERE { }');
  await expect(pane(page)).toContainText('Add a node to start');
});

test('the Pattern tab draws the query that ran, read-only, and Edit opens it in the pane', async ({
  page,
}) => {
  await page.goto('/ui/query');
  await setText(page, 'SELECT ?s ?o WHERE { ?s <http://example.org/p> ?o } LIMIT 5');
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await results.getByRole('tab', { name: /Pattern/ }).click();
  const ran = results.getByRole('region', {
    name: 'Pattern of the query that ran',
  });
  await expect(ran).toHaveAttribute('data-state', 'ok');
  await expect(ran.getByText('read-only')).toBeVisible();
  await expect
    .poll(async () => (await graph(ran.locator('.pcanvas'))).nodes.map((n) => n[0]))
    .toEqual(['?s', '?o']);
  // typing on does not change the result's pattern
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type(' OFFSET 1');
  await expect(ran.getByLabel('Projection and modifiers')).not.toContainText('OFFSET');
  await results.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(pane(page)).toHaveAttribute('data-state', 'ok');
  await expect
    .poll(() => textOf(page))
    .toBe('SELECT ?s ?o WHERE { ?s <http://example.org/p> ?o } LIMIT 5');
});

// the regressions of the first review (target/audits/u08-phase1/plan.md)

test('an open panel keeps its triple when typing moves the text', async ({ page }) => {
  await openPane(
    page,
    'PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?a .\n  ?s ex:q ?b .\n}',
  );
  const c = canvas(page);
  await expect.poll(() => settled(c)).toBeGreaterThan(0);
  await c.focus();
  for (let i = 0; i < 3; i++) await page.keyboard.press('Tab');
  await expect(pane(page).getByTestId('pattern-focus')).toHaveText('edge ex:q from ?s to ?b');
  await page.keyboard.press('Enter');
  const panel = pane(page).getByRole('dialog', { name: 'Pattern element' });
  await expect(panel).toContainText('?s ex:q ?b');
  // a triple typed before it in the editor
  await editor(page).locator('.cm-line').nth(1).click();
  await page.keyboard.press('End');
  await page.keyboard.press('Enter');
  await page.keyboard.type('?s ex:z ?z .');
  await expect(panel).toContainText('?s ex:q ?b');
  await panel.getByRole('button', { name: /Delete/ }).click();
  await expect.poll(() => textOf(page)).not.toContain('ex:q');
  const now = await textOf(page);
  expect(now).toContain('?s ex:p ?a .');
  expect(now).toContain('?s ex:z ?z .');
});

test('a panel closes when typing makes its edge ambiguous', async ({ page }) => {
  const text =
    'SELECT * {\n  ?s <http://p> ?o .\n  ?s <http://p> ?o .\n  OPTIONAL { ?s <http://p> ?o }\n}';
  await openPane(page, text);
  const c = canvas(page);
  await expect.poll(() => settled(c)).toBeGreaterThan(0);
  await c.focus();
  // ?s, the first edge, ?o, the second edge
  for (let i = 0; i < 3; i++) await page.keyboard.press('Tab');
  await page.keyboard.press('Enter');
  const panel = pane(page).getByRole('dialog', { name: 'Pattern element' });
  await expect(panel).toBeVisible();
  // the first copy's predicate changes: the selected id would now name the OPTIONAL copy
  await editor(page).locator('.cm-line').nth(1).click();
  await page.keyboard.press('End');
  for (let i = 0; i < 6; i++) await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('Backspace');
  await page.keyboard.type('q');
  await expect.poll(() => textOf(page)).toContain('?s <http://q> ?o .');
  await expect(panel).toHaveCount(0);
  expect(await textOf(page)).toContain('OPTIONAL { ?s <http://p> ?o }');
});

test('each editor tab keeps its own positions', async ({ page }) => {
  await openPane(page, A3);
  const c = canvas(page);
  await expect.poll(() => settled(c)).toBeGreaterThan(0);
  const tabs = page.locator('.qtab-btn');
  // move ?person in the first tab
  await c.focus();
  for (let i = 0; i < 5; i++) await page.keyboard.press('Shift+ArrowRight');
  await expect.poll(async () => (await positions(c))['v:person']?.[0]).toBeDefined();
  const first = (await positions(c))['v:person'];
  // the same query in a second tab, ?person moved the other way
  await page.getByRole('button', { name: 'New query tab' }).click();
  await setText(page, A3);
  await expect(pane(page)).toHaveAttribute('data-state', 'ok');
  await expect.poll(() => settled(canvas(page))).toBeGreaterThan(0);
  // an empty tab draws an empty canvas first, so wait for the query's nodes
  await expect.poll(async () => (await positions(canvas(page)))['v:person']?.[0]).toBeDefined();
  await canvas(page).focus();
  for (let i = 0; i < 10; i++) await page.keyboard.press('Shift+ArrowLeft');
  const second = (await positions(canvas(page)))['v:person'];
  expect(Math.abs(second[0] - first[0])).toBeGreaterThan(100);
  // back and forth: each tab draws its own picture
  await tabs.nth(0).click();
  await expect.poll(() => settled(canvas(page))).toBeGreaterThan(0);
  await expect
    .poll(async () => (await positions(canvas(page)))['v:person']?.[0])
    .toBeCloseTo(first[0], 0);
  await tabs.nth(1).click();
  await expect.poll(() => settled(canvas(page))).toBeGreaterThan(0);
  await expect
    .poll(async () => (await positions(canvas(page)))['v:person']?.[0])
    .toBeCloseTo(second[0], 0);
});

test('undo, redo and Escape work in the list and from the panel', async ({ page }) => {
  await openPane(page, A3);
  await pane(page).getByRole('button', { name: 'List' }).click();
  const optional = pane(page)
    .locator('li')
    .filter({ hasText: 'edge ex:locatedIn' })
    .getByRole('button', { name: 'Optional' });
  await optional.click();
  await expect.poll(() => textOf(page)).toContain('OPTIONAL {');
  // the list button still has focus: Mod+Z there undoes the edit
  await pane(page).locator('li button').first().focus();
  await page.keyboard.press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe(A3);
  await page.keyboard.press('ControlOrMeta+Shift+Z');
  await expect.poll(() => textOf(page)).toContain('OPTIONAL {');
  await page.keyboard.press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe(A3);
  // Escape from a button of the panel closes it
  await pane(page).getByRole('button', { name: 'Canvas' }).click();
  await canvas(page).focus();
  await page.keyboard.press('Tab');
  await page.keyboard.press('Enter');
  const panel = pane(page).getByRole('dialog', { name: 'Pattern element' });
  await panel.getByRole('button', { name: 'Optional' }).focus();
  await page.keyboard.press('Escape');
  await expect(panel).toHaveCount(0);
  // typing in the add form keeps the field's own undo
  await pane(page).getByRole('button', { name: 'Edge', exact: true }).click();
  await pane(page).getByLabel('From', { exact: true }).fill('?x');
  await pane(page).getByLabel('From', { exact: true }).press('ControlOrMeta+Z');
  await expect.poll(() => textOf(page)).toBe(A3);
});

test('a prefix nobody declares is refused, and DESCRIBE draws its locked terms', async ({
  page,
}) => {
  await openPane(page, 'SELECT * { ?s <http://x/p> ?o }');
  await pane(page).getByRole('button', { name: 'Edge', exact: true }).click();
  await pane(page).getByLabel('From', { exact: true }).fill('?o');
  await pane(page).getByLabel('Predicate', { exact: true }).fill('missing:p');
  await pane(page).getByLabel('To', { exact: true }).fill('?z');
  await pane(page).getByRole('button', { name: 'Add', exact: true }).click();
  await expect(pane(page).getByRole('alert')).toHaveText('The prefix "missing:" is not declared');
  expect(await textOf(page)).toBe('SELECT * { ?s <http://x/p> ?o }');
  await setText(page, 'DESCRIBE <http://x/a>');
  await expect
    .poll(async () => (await graph(canvas(page))).locked)
    .toEqual(['DESCRIBE <http://x/a>']);
  await expect(pane(page)).not.toContainText('Add a node to start');
});

test.describe('at 390 px', () => {
  test.use({
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
  });

  test('the editor and the pane are two tabs, edits work with taps, and nothing scrolls sideways', async ({
    page,
  }) => {
    await page.goto('/ui/query');
    await setText(page, A3);
    await page.getByRole('button', { name: 'Pattern', exact: true }).tap();
    const tabs = page.getByRole('tablist', { name: 'Query views' });
    await expect(tabs.getByRole('tab', { name: 'Pattern' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(pane(page)).toHaveAttribute('data-state', 'ok');
    await expect(editor(page)).toBeHidden();
    await expectNoSidewaysScroll(page, 'the Pattern tab');
    await pane(page).getByRole('button', { name: 'List' }).tap();
    await pane(page)
      .locator('li')
      .filter({ hasText: 'edge ex:locatedIn' })
      .getByRole('button', { name: 'Optional' })
      .tap();
    await tabs.getByRole('tab', { name: 'Text' }).tap();
    await expect(editor(page)).toBeVisible();
    await expect.poll(() => textOf(page)).toContain('OPTIONAL { ?org ex:locatedIn ex:lyon . }');
    await expectNoSidewaysScroll(page, 'the Text tab');
    // buttons keep 44 px touch targets
    await tabs.getByRole('tab', { name: 'Pattern' }).tap();
    const box = await pane(page).getByRole('button', { name: 'Canvas' }).boundingBox();
    expect(box!.height).toBeGreaterThanOrEqual(44);
    await pane(page).getByRole('button', { name: 'Canvas' }).tap();
    await expect.poll(async () => (await graph(canvas(page))).hulls).toBe(1);
    await expectNoSidewaysScroll(page, 'the canvas');
  });
});
