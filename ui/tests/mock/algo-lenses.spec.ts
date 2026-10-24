// Algorithm lenses on the graph canvas (docs/specs/U10-canvas-lenses.md, Phase 2):
// acceptance examples A3 (drawn-graph PageRank) and A4 (Leiden colours that stay stable
// across seeds), projection scope with its background run, progress and Cancel, the GraphRAG
// community lens with its links to the reports, and the 390 px layout of A12. The dataset
// `algo2` holds three groups of five people, densely linked inside each group by
// `ex:knows`, who all know `ex:hub`.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, phone } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';

function data(): string {
  const t: string[] = [];
  for (let g = 0; g < 3; g++) {
    const ps = Array.from({ length: 5 }, (_, i) => `ex:p${g * 5 + i + 1}`);
    for (const a of ps) {
      t.push(`${a} ex:knows ex:hub .`);
      for (const b of ps) if (a !== b) t.push(`${a} ex:knows ${b} .`);
    }
  }
  return `PREFIX ex: <${EX}>\nINSERT DATA {\n${t.join('\n')}\n}`;
}

async function create(request: APIRequestContext, name: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  const u = await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: data(),
  });
  expect(u.ok()).toBe(true);
}

test.beforeAll(async ({ request }) => {
  await create(request, 'algo2');
});

type Look = { size?: number; colour?: number; badge?: number };
type Drawn = { nodes: [string, string, Look?][]; edges: [string, string, string?][] };

const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const look = async (page: Page, label: RegExp) =>
  (await drawn(page)).nodes.find(([l]) => label.test(l))?.[2];

async function open(page: Page, ds: string, iri: string, nodes = 10) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto(`/ui/explore?ds=${ds}&iri=${encodeURIComponent(iri)}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(nodes);
}

const menu = (page: Page) => page.getByRole('dialog', { name: 'Lenses' });

test('A3: PageRank over the drawn graph makes the hub the largest node', async ({ page }) => {
  await open(page, 'algo2', `${EX}hub`);
  await page.keyboard.press('Shift+L');
  await menu(page).getByRole('combobox', { name: 'Size' }).selectOption('pageRank');
  const opts = page.getByTestId('lens-options-size');
  await expect(opts.getByRole('combobox', { name: 'scope' })).toHaveValue('drawn');
  await expect(opts.getByRole('spinbutton', { name: 'damping' })).toHaveValue('0.85');
  await expect.poll(async () => (await look(page, /^hub$/))?.size, { timeout: 15_000 }).toBe(4);
  for (const p of ['p1', 'p7', 'p13'])
    expect((await look(page, new RegExp(`^${p}$`)))?.size ?? 0).toBeLessThan(4);
  // the run's summary and the announcement
  await expect(page.getByTestId('lens-status-size')).toContainText(/iteration/);
  await expect(page.getByText(/Size by PageRank on \d+ nodes, top node hub/)).toBeAttached();
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText('size is PageRank in the drawn graph, larger is higher');
  // a changed parameter runs again
  await opts.getByRole('spinbutton', { name: 'damping' }).fill('0.5');
  await opts.getByRole('spinbutton', { name: 'damping' }).press('Enter');
  await expect.poll(async () => (await look(page, /^hub$/))?.size).toBe(4);
  // the list column shows the scores
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'List', exact: true }).click();
  const table = page.getByRole('table', { name: 'Drawn nodes' });
  await expect(
    table.getByRole('columnheader', { name: 'PageRank in the drawn graph' }),
  ).toBeVisible();
  await expect(table.getByRole('row').nth(1)).toContainText('hub');
});

test('A4: Leiden colours three groups and keeps their colours with another seed', async ({
  page,
}) => {
  await open(page, 'algo2', `${EX}hub`);
  await page.keyboard.press('Shift+L');
  await menu(page).getByRole('combobox', { name: 'Colour' }).selectOption('leiden');
  await expect(page.getByTestId('lens-status-colour')).toContainText(/communit/, {
    timeout: 15_000,
  });
  const colours = async () => {
    const out: Record<string, number | undefined> = {};
    for (const p of ['p1', 'p2', 'p6', 'p7', 'p11', 'p12'])
      out[p] = (await look(page, new RegExp(`^${p}$`)))?.colour;
    return out;
  };
  await expect.poll(async () => (await colours()).p1).toBeDefined();
  const first = await colours();
  // a group shares a colour, and the three groups differ
  expect(first.p2).toBe(first.p1);
  expect(first.p7).toBe(first.p6);
  expect(first.p12).toBe(first.p11);
  expect(new Set([first.p1, first.p6, first.p11]).size).toBe(3);
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText(/community \d+, \d+ nodes/);
  const opts = page.getByTestId('lens-options-colour');
  await opts.getByRole('spinbutton', { name: 'seed' }).fill('43');
  await opts.getByRole('spinbutton', { name: 'seed' }).press('Enter');
  await expect(page.getByTestId('lens-status-colour')).toContainText(/communit/);
  await expect.poll(colours).toEqual(first);
});

test('projection scope runs in the background with progress and Cancel, then from the kept run', async ({
  page,
}) => {
  await open(page, 'algo2', `${EX}hub`);
  await page.keyboard.press('Shift+L');
  await menu(page).getByRole('combobox', { name: 'Size' }).selectOption('kCore');
  const opts = page.getByTestId('lens-options-size');
  await opts.getByRole('combobox', { name: 'scope' }).selectOption('projection');
  await expect(opts.getByRole('combobox', { name: 'projection' })).toHaveValue('drawn');
  // the run is a task: its progress shows, and Cancel stops it
  await expect(opts.getByRole('progressbar', { name: 'Size run progress' })).toBeVisible();
  await opts.getByRole('button', { name: 'Cancel' }).click();
  await expect(page.getByTestId('lens-status-size')).toContainText('Cancelled.');
  await expect(opts.getByRole('progressbar')).toHaveCount(0);
  // another projection starts a new run, which finishes and is read from the kept result
  await opts.getByRole('combobox', { name: 'projection' }).selectOption(`pred:${EX}knows`);
  await expect(page.getByTestId('lens-status-size')).toContainText("server's kept run", {
    timeout: 15_000,
  });
  await expect.poll(async () => (await look(page, /^p1$/))?.size).toBeDefined();
  await expect(page.getByTestId('lens-legend')).toContainText('k-core in the projection');
});

test('GraphRAG communities colour the nodes and link to their reports', async ({ page }) => {
  await open(page, 'org', 'http://example.org/resource/ana', 1);
  await page.keyboard.press('Shift+L');
  const colour = menu(page).getByRole('combobox', { name: 'Colour' });
  await expect(colour.locator('option', { hasText: 'GraphRAG community' })).toHaveCount(1);
  await colour.selectOption('graphrag');
  const legend = page.getByTestId('lens-legend');
  await expect(legend).toContainText(/Engineering, \d+ drawn of 4/, { timeout: 15_000 });
  // another level splits it into the two teams
  await page
    .getByTestId('lens-options-colour')
    .getByRole('combobox', { name: 'level' })
    .selectOption('1');
  await expect(legend).toContainText(/The payments team, \d+ drawn of 2/);
  const report = legend.getByRole('link', { name: 'report' }).first();
  await expect(report).toHaveAttribute('href', /\/datasets\/org\?tab=communities&community=/);
});

test('A12: at 390 px the algorithm options fit the bottom sheet without scrolling sideways', async ({
  browser,
}) => {
  const ctx = await browser.newContext({ ...phone, viewport: { width: 390, height: 844 } });
  const page = await ctx.newPage();
  await open(page, 'algo2', `${EX}hub`);
  await page.getByRole('button', { name: /^Lenses/ }).tap();
  const sheet = menu(page);
  await sheet.getByRole('combobox', { name: 'Colour' }).selectOption('louvain');
  await expect(page.getByTestId('lens-options-colour')).toBeVisible();
  await expect(page.getByTestId('lens-status-colour')).toContainText(/communit/, {
    timeout: 15_000,
  });
  await expectNoSidewaysScroll(page);
  await ctx.close();
});
