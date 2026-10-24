// Algorithm lenses against a real server (docs/specs/U10-canvas-lenses.md, Phase 2):
// acceptance examples A3 (drawn-graph PageRank, which agrees with the same SERVICE query),
// A4 (Leiden colours stay with their communities across seeds) and A5 (a kept projection
// run answers other nodes from the cache, which /metrics counts). The data is three groups
// of five people, densely linked inside each group by `ex:knows`, who all know `ex:hub`.

import type { Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

const EX = 'http://example.org/';
const DS = 'algo-lenses';
const people = Array.from({ length: 15 }, (_, i) => `${EX}p${i + 1}`);

function data(): string {
  const t: string[] = [];
  for (let g = 0; g < 3; g++) {
    const ps = people.slice(g * 5, g * 5 + 5);
    for (const a of ps) {
      t.push(`<${a}> ex:knows ex:hub .`);
      for (const b of ps) if (a !== b) t.push(`<${a}> ex:knows <${b}> .`);
    }
  }
  return `PREFIX ex: <${EX}>\nINSERT DATA {\n${t.join('\n')}\n}`;
}

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const u = await request.post(`/${DS}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: data(),
  });
  expect(u.ok(), await u.text()).toBe(true);
});

type Look = { size?: number; colour?: number };
type Drawn = { nodes: [string, string, Look?][]; edges: [string, string, string?][] };
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const look = async (page: Page, label: string) =>
  (await drawn(page)).nodes.find(([l]) => l === label)?.[2];

async function openHub(page: Page) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), DS);
  await page.goto(`/ui/explore?ds=${DS}&iri=${encodeURIComponent(`${EX}hub`)}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(10);
}

const menu = (page: Page) => page.getByRole('dialog', { name: 'Lenses' });

open('A3: drawn-graph PageRank makes the hub the largest node', async ({ page, request }) => {
  await openHub(page);
  await page.keyboard.press('Shift+L');
  await menu(page).getByRole('combobox', { name: 'Size' }).selectOption('pageRank');
  await expect.poll(async () => (await look(page, 'hub'))?.size, { timeout: 15_000 }).toBe(4);
  await expect(page.getByTestId('lens-status-size')).toContainText(/iteration/);
  // the same SERVICE algo:pageRank query, run as a person would in the editor
  const drawnIris = (await drawn(page)).nodes.map(([l]) =>
    l === 'hub' ? `${EX}hub` : `${EX}${l}`,
  );
  const vals = drawnIris.map((i) => `<${i}>`).join(' ');
  const q = `PREFIX algo: <urn:x-sparkles:algo#>
SELECT ?node ?score WHERE { SERVICE algo:pageRank {
  [] algo:start ?s ; algo:end ?o ; algo:node ?node ; algo:score ?score .
  VALUES ?s { ${vals} } VALUES ?o { ${vals} } ?s ?p ?o . } } ORDER BY DESC(?score)`;
  const r = await request.post(`/${DS}/sparql`, {
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: q,
  });
  expect(r.ok(), await r.text()).toBe(true);
  const rows = (await r.json()).results.bindings;
  expect(rows[0].node.value).toBe(`${EX}hub`);
  // the list column shows the same top score
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'List', exact: true }).click();
  const top = Number(rows[0].score.value);
  const row = page.getByRole('table', { name: 'Drawn nodes' }).getByRole('row').nth(1);
  await expect(row).toContainText('hub');
  await expect(row).toContainText(top.toLocaleString('en-US', { maximumSignificantDigits: 4 }));
});

open(
  'A4: Leiden colours three groups and keeps the colours with another seed',
  async ({ page }) => {
    await openHub(page);
    await page.keyboard.press('Shift+L');
    await menu(page).getByRole('combobox', { name: 'Colour' }).selectOption('leiden');
    const groups = async () =>
      Promise.all(
        ['p1', 'p2', 'p6', 'p7', 'p11', 'p12'].map(async (p) => (await look(page, p))?.colour),
      );
    await expect.poll(async () => (await groups())[0], { timeout: 15_000 }).toBeDefined();
    const first = await groups();
    expect(first[1]).toBe(first[0]);
    expect(first[3]).toBe(first[2]);
    expect(first[5]).toBe(first[4]);
    expect(new Set([first[0], first[2], first[4]]).size).toBe(3);
    const seed = page.getByTestId('lens-options-colour').getByRole('spinbutton', { name: 'seed' });
    await seed.fill('43');
    await seed.press('Enter');
    await expect.poll(groups, { timeout: 15_000 }).toEqual(first);
  },
);

open('A5: a kept projection run answers other nodes from the cache', async ({ request }) => {
  const body = (nodes: string[]) => ({
    algorithm: 'pageRank',
    projection: { predicates: [`${EX}knows`] },
    params: { dampingFactor: 0.8 },
    nodes,
    keep: true,
  });
  const metric = async () => {
    const m = await (await request.get('/$/metrics')).text();
    return Number(/^sparkles_algo_view_cache_hits_total (\d+)/m.exec(m)?.[1] ?? 0);
  };
  const r1 = await request.post(`/$/algo/${DS}/run`, { data: body([`${EX}hub`]) });
  expect(r1.ok(), await r1.text()).toBe(true);
  const a = await r1.json();
  expect(a.run.cached).toBe(false);
  expect(a.results.bindings[0].node.value).toBe(`${EX}hub`);
  const hits = await metric();
  const r2 = await request.post(`/$/algo/${DS}/run`, { data: body([`${EX}p1`, `${EX}p2`]) });
  const b = await r2.json();
  expect(b.run.cached).toBe(true);
  expect(b.run.commit).toBe(a.run.commit);
  expect(b.results.bindings.map((x: { node: { value: string } }) => x.node.value)).toEqual([
    `${EX}p1`,
    `${EX}p2`,
  ]);
  expect(await metric()).toBeGreaterThan(hits);
  // a background run of another request is a task that needs only read, then kept
  const r3 = await request.post(`/$/algo/${DS}/run`, {
    data: { ...body([`${EX}hub`]), algorithm: 'wcc', params: {}, async: true },
  });
  expect(r3.status(), await r3.text()).toBe(202);
  const t = await r3.json();
  expect(t.kind).toBe('algo-view');
  await expect
    .poll(async () => (await (await request.get(`/$/tasks/${t.id}`)).json()).state)
    .toBe('done');
  const r4 = await request.post(`/$/algo/${DS}/run`, {
    data: { ...body([`${EX}hub`]), algorithm: 'wcc', params: {} },
  });
  const c = await r4.json();
  expect(c.run.cached).toBe(true);
  expect(c.results.bindings[0].communitySize.value).toBe('16');
});
