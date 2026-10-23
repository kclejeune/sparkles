// Lenses on the graph canvas (docs/specs/U10-canvas-lenses.md, Phase 1): acceptance example
// A1 (inferred edges drawn dashed), the size and colour lenses with their legend and list
// columns, the keyboard and the 390 px layout of A12, and the empty states of §2.7. The
// dataset `lenses1` holds the ontology of §9, materialized with RDFS by the mock's reasoner,
// and `lenses0` holds the same data without reasoning.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, phone } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';
const DATA = `PREFIX ex: <${EX}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA {
  ex:Manager rdfs:subClassOf ex:Employee .
  ex:manages rdfs:domain ex:Manager .
  ex:alice ex:manages ex:team7 .
  ex:alice ex:knows ex:bob , ex:carol , ex:dave .
  ex:bob ex:knows ex:carol .
  ex:bob a ex:Person . ex:carol a ex:Person . ex:dave a ex:Robot .
  ex:alice ex:age 41 . ex:bob ex:age 29 . ex:carol ex:age 35 .
  GRAPH ex:hr { ex:team7 ex:size 7 . ex:dave ex:knows ex:bob . }
}`;

async function create(request: APIRequestContext, name: string, reason: boolean) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  const u = await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: DATA,
  });
  expect(u.ok()).toBe(true);
  if (!reason) return;
  const t = await request.post(`/$/reason/${name}`, { form: { profile: 'rdfs' } });
  expect(t.ok()).toBe(true);
  await expect
    .poll(async () => (await (await request.get(`/$/reason/${name}`)).json()).profile, {
      timeout: 20_000,
    })
    .toBe('rdfs');
}

test.beforeAll(async ({ request }) => {
  await create(request, 'lenses1', true);
  await create(request, 'lenses0', false);
});

type Look = { size?: number; colour?: number; badge?: number };
type Drawn = { nodes: [string, string, Look?][]; edges: [string, string, string?][] };

const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const edge = async (page: Page, re: RegExp) => (await drawn(page)).edges.find(([l]) => re.test(l));
const node = async (page: Page, re: RegExp) => (await drawn(page)).nodes.find(([l]) => re.test(l));

async function open(page: Page, ds: string, iri = `${EX}alice`) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto(`/ui/explore?ds=${ds}&iri=${encodeURIComponent(iri)}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(3);
}

const menu = (page: Page) => page.getByRole('dialog', { name: 'Lenses' });

test('A1: inferred edges are drawn dashed with an i marker, asserted ones solid', async ({
  page,
}) => {
  await open(page, 'lenses1');
  // on by default where the dataset has reasoning
  await expect.poll(() => edge(page, /type/)).toBeTruthy();
  await expect
    .poll(async () => (await drawn(page)).edges.some(([, , i]) => i === 'inferred'))
    .toBe(true);
  const types = (await drawn(page)).edges.filter(([l]) => /rdf:type|type$/.test(l));
  expect(types.length).toBeGreaterThan(0);
  for (const t of types) expect(t[2], t[0]).toBe('inferred');
  expect((await edge(page, /manages/))?.[2]).toBe('');
  expect((await edge(page, /knows/))?.[2]).toBe('');
  // the legend says what dashes mean
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText('dashed edges were inferred by the reasoner');
  // a click on the entry selects the ends of the inferred edges
  await legend.getByRole('button', { name: /^inferred edges/ }).click();
  await expect(legend.getByRole('button', { name: /^inferred edges/ })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  // the lens can be turned off
  await page.keyboard.press('Shift+4');
  await expect
    .poll(async () => (await drawn(page)).edges.some(([, , i]) => i === 'inferred'))
    .toBe(false);
  await expect(legend).toHaveCount(0);
  await page.keyboard.press('Shift+4');
  await expect
    .poll(async () => (await drawn(page)).edges.some(([, , i]) => i === 'inferred'))
    .toBe(true);
});

test('size and colour lenses with their legend, list columns and announcement', async ({
  page,
}) => {
  await open(page, 'lenses1');
  await page.keyboard.press('Shift+L');
  await expect(menu(page)).toBeVisible();
  await expect(menu(page).getByRole('combobox', { name: 'Size' })).toBeFocused();
  await menu(page).getByRole('combobox', { name: 'Size' }).selectOption('degree');
  await expect(
    page.getByText(/Size by degree in the drawn graph on \d+ nodes, top node alice/),
  ).toBeAttached();
  // alice is the focus and has every drawn edge, so she is the largest
  await expect.poll(async () => (await node(page, /^alice$/))?.[2]?.size).toBe(4);
  await menu(page).getByRole('combobox', { name: 'Colour' }).selectOption('class');
  await expect(page.getByTestId('lens-status-colour')).toContainText(/categor/);
  // classes are the asserted ones: bob and carol are people, dave a robot
  const bob = (await node(page, /^bob$/))?.[2]?.colour;
  expect(bob).toBeDefined();
  expect((await node(page, /^carol$/))?.[2]?.colour).toBe(bob);
  expect((await node(page, /^dave$/))?.[2]?.colour).not.toBe(bob);
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText('size is degree in the drawn graph, larger is higher');
  await expect(legend).toContainText(/colour 1 is ex:Person|colour 1 is Person/);
  await page.keyboard.press('Escape');
  await expect(menu(page)).toHaveCount(0);
  // the list view has a column per active lens
  await page.getByRole('button', { name: 'List', exact: true }).click();
  const table = page.getByRole('table', { name: 'Drawn nodes' });
  await expect(
    table.getByRole('columnheader', { name: 'degree in the drawn graph' }),
  ).toBeVisible();
  await expect(table.getByRole('columnheader', { name: 'class', exact: true })).toBeVisible();
  await expect(table.getByRole('columnheader', { name: 'inferred edges' })).toBeVisible();
  const first = table.locator('tbody tr').first();
  await expect(first).toContainText('alice');
  await expect(table.locator('tbody tr', { hasText: 'bob' }).first()).toContainText('Person');
  // a legend entry marks its rows
  await legend.getByRole('button', { name: /Person/ }).click();
  await expect(table.locator('tbody tr.marked')).toHaveCount(2);
});

test('a numeric property sizes nodes and those without a value count as "no value"', async ({
  page,
}) => {
  await open(page, 'lenses1');
  await page.getByRole('button', { name: /^Lenses/ }).click();
  await menu(page).getByRole('combobox', { name: 'Size' }).selectOption('property');
  const prop = menu(page).getByRole('combobox', { name: 'Size property' });
  await expect(prop.locator('option', { hasText: 'age' })).toHaveCount(1);
  await prop.selectOption(`${EX}age`);
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText(/size is (ex:)?age, larger is higher/);
  await expect(legend).toContainText('no value');
  // bob has an age of 29 and dave has none
  await expect.poll(async () => (await node(page, /^dave$/))?.[2]?.size).toBe(-1);
  expect((await node(page, /^bob$/))?.[2]?.size).toBeGreaterThanOrEqual(0);
});

test('graph colours name the named graph and the default graph', async ({ page }) => {
  await open(page, 'lenses1');
  await page.getByRole('button', { name: /^Lenses/ }).click();
  await menu(page).getByRole('combobox', { name: 'Colour' }).selectOption('graph');
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText('colour is graph');
  await expect(legend).toContainText(/several graphs|the default graph/);
});

test('without reasoning the inferred-edge lens is absent (§2.7)', async ({ page }) => {
  await open(page, 'lenses0');
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const edges = menu(page).getByRole('combobox', { name: 'Edges' });
  await expect(edges.locator('option')).toHaveText(['none']);
  await expect(menu(page)).toContainText('The dataset has no reasoning');
  expect((await drawn(page)).edges.every(([, , i]) => !i)).toBe(true);
});

test('A12: at 390 px the menu is a bottom sheet and the legend a strip, without scrolling sideways', async ({
  browser,
}) => {
  const ctx = await browser.newContext({
    ...phone,
    viewport: { width: 390, height: 844 },
    baseURL: test.info().project.use.baseURL,
  });
  const page = await ctx.newPage();
  await open(page, 'lenses1');
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toBeVisible();
  // collapsed to one line of the active encodings until tapped
  await expect(legend.getByRole('button', { expanded: false })).toContainText('dashed edges');
  await legend.getByRole('button', { expanded: false }).tap();
  await expect(legend).toContainText('Computed for this picture');
  await page.getByRole('button', { name: /^Lenses/ }).tap();
  const sheet = menu(page);
  await expect(sheet).toBeVisible();
  const box = await sheet.boundingBox();
  const vw = page.viewportSize()!.width;
  const vh = page.viewportSize()!.height;
  expect(box!.x).toBe(0);
  expect(Math.round(box!.width)).toBe(vw);
  expect(Math.round(box!.y + box!.height)).toBe(vh);
  await sheet.getByRole('combobox', { name: 'Size' }).selectOption('degree');
  await expect.poll(async () => (await node(page, /^alice$/))?.[2]?.size).toBe(4);
  await expectNoSidewaysScroll(page, 'Explore with the lens sheet open');
  await ctx.close();
});

async function runQuery(page: Page, ds: string, query: string) {
  await page.addInitScript(
    ([d, q]) => {
      localStorage.setItem('sparkles.dataset', d);
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Lenses', query: q }], active: 't1' }),
      );
    },
    [ds, query],
  );
  await page.goto(`/ui/query?ds=${ds}`);
  await page.getByRole('button', { name: /^Run/ }).click();
  await page.getByRole('tab', { name: 'Graph' }).click();
}

test("the query page's graph: a constructed triple that is not in the data is not inferred", async ({
  page,
}) => {
  await runQuery(
    page,
    'lenses1',
    `PREFIX ex: <${EX}>
CONSTRUCT { ?s ?p ?o . ex:alice ex:invented ex:bob } WHERE { ex:alice ?p ?o BIND(ex:alice AS ?s) }`,
  );
  await expect
    .poll(async () => (await drawn(page)).edges.some(([, , i]) => i === 'inferred'))
    .toBe(true);
  expect((await edge(page, /invented/))?.[2]).toBe('');
  expect((await edge(page, /manages/))?.[2]).toBe('');
  await expect(page.getByTestId('lens-legend')).toContainText('inferred edges');
});

test('the WebGL view draws the lenses and the list has their columns', async ({ page }) => {
  await runQuery(page, 'lenses1', 'CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }');
  await page.getByRole('combobox', { name: 'Renderer' }).selectOption('webgl');
  await expect(page.locator('.lg-host canvas').first()).toBeVisible();
  await page.getByRole('button', { name: /^Lenses/ }).click();
  await menu(page).getByRole('combobox', { name: 'Size' }).selectOption('degree');
  await menu(page).getByRole('combobox', { name: 'Colour' }).selectOption('graph');
  await expect(page.getByTestId('lens-status-colour')).toContainText(/categor/);
  await page.keyboard.press('Escape');
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText('size is degree in the drawn graph');
  await expect(legend).toContainText(/hr|several graphs/);
  await page.getByRole('button', { name: 'List', exact: true }).click();
  const table = page.getByRole('table', { name: 'Drawn nodes' });
  await expect(table.getByRole('columnheader', { name: 'graph', exact: true })).toBeVisible();
  await expect(table.locator('tbody tr').first()).toContainText('alice');
  // the dashed inferred edges keep their direction: each has an arrowhead
  await page.getByRole('button', { name: 'List', exact: true }).click();
  const overlay = page.locator('.lg-overlay');
  await expect
    .poll(async () => Number(await overlay.getAttribute('data-inferred')))
    .toBeGreaterThan(0);
  expect(await overlay.getAttribute('data-arrows')).toBe(
    await overlay.getAttribute('data-inferred'),
  );
});
