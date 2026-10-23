// Lenses on the graph canvas against a real server (docs/specs/U10-canvas-lenses.md,
// acceptance examples A1 and A2). The data is the ontology of §9 with
// `ex:alice ex:manages ex:team7`: one dataset materializes RDFS, the other answers it with
// RDFS on read. In both, Explore draws `ex:alice rdf:type ex:Employee` dashed and
// `ex:alice ex:manages ex:team7` solid.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

const EX = 'http://example.org/';
const DATA = `PREFIX ex: <${EX}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA {
  ex:Manager rdfs:subClassOf ex:Employee .
  ex:manages rdfs:domain ex:Manager .
  ex:alice ex:manages ex:team7 .
}`;

async function create(request: APIRequestContext, name: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const u = await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: DATA,
  });
  expect(u.ok(), await u.text()).toBe(true);
}

open.beforeAll(async ({ request }) => {
  await create(request, 'lenses-materialized');
  const t = await request.post('/$/reason/lenses-materialized', { data: { profile: 'rdfs' } });
  expect(t.ok(), await t.text()).toBe(true);
  const task = await t.json();
  await expect
    .poll(async () => (await (await request.get(`/$/tasks/${task.id}`)).json()).status, {
      timeout: 30_000,
    })
    .toBe('done');
  await create(request, 'lenses-on-read');
  const r = await request.put('/$/rdfs/lenses-on-read', { data: { graph: 'default' } });
  expect(r.ok(), await r.text()).toBe(true);
});

type Drawn = { nodes: [string, string, object?][]; edges: [string, string, string?][] };
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};

for (const [name, title] of [
  ['lenses-materialized', 'A1: materialized RDFS'],
  ['lenses-on-read', 'A2: RDFS on read'],
] as const) {
  open(`${title} draws the inferred type dashed and the asserted edge solid`, async ({ page }) => {
    await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), name);
    await page.goto(`/ui/explore?ds=${name}&iri=${encodeURIComponent(`${EX}alice`)}`);
    await expect.poll(async () => (await drawn(page)).nodes.map(([l]) => l)).toContain('Employee');
    await expect
      .poll(async () => (await drawn(page)).edges.some(([, , i]) => i === 'inferred'), {
        timeout: 15_000,
      })
      .toBe(true);
    const d = await drawn(page);
    const types = d.edges.filter(([l]) => /type/.test(l));
    expect(types.length).toBe(2);
    for (const t of types) expect(t[2]).toBe('inferred');
    expect(d.edges.find(([l]) => /manages/.test(l))?.[2]).toBe('');
    await expect(page.getByTestId('lens-legend')).toContainText(
      'dashed edges were inferred by the reasoner',
    );
  });
}
