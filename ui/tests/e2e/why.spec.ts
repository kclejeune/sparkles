// Why is this inferred? against a real server (docs/specs/U10-canvas-lenses.md, acceptance
// examples A7, A9 and A10). The data is the ontology of §9 with `ex:alice ex:manages
// ex:team7`, the fact in graph ex:hr and the ontology in graph ex:ontology, materialized
// with RDFS over both graphs. A7 explains `ex:alice rdf:type ex:Employee` over HTTP, A9
// asks the same as a caller who cannot read ex:ontology, and A10 opens the tree from the
// dashed edge in Explore and draws its premises.

import type { APIRequestContext, Page } from '@playwright/test';
import { anonymous, expect, open, server } from './fixtures';

const EX = 'http://example.org/';
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const TRIG = `@prefix ex: <${EX}> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:hr { ex:alice ex:manages ex:team7 . }
ex:ontology { ex:Manager rdfs:subClassOf ex:Employee . ex:manages rdfs:domain ex:Manager . }
`;

const WHY = {
  triple: {
    s: { type: 'uri', value: `${EX}alice` },
    p: { type: 'uri', value: RDF_TYPE },
    o: { type: 'uri', value: `${EX}Employee` },
  },
  mode: 'why',
};

async function setUp(request: APIRequestContext, name: string, headers: Record<string, string>) {
  const r = await request.post('/$/datasets', {
    headers,
    data: { dbName: name, dbType: 'persistent' },
  });
  expect(r.ok() || r.status() === 409, await r.text()).toBe(true);
  if (r.status() === 409) return;
  const l = await request.post(`/${name}/data`, {
    headers: { ...headers, 'Content-Type': 'application/trig' },
    data: TRIG,
  });
  expect(l.ok(), await l.text()).toBe(true);
  const t = await request.post(`/$/reason/${name}`, {
    headers,
    data: { profile: 'rdfs', dataGraphs: [`${EX}hr`], ontologyGraphs: [`${EX}ontology`] },
  });
  expect(t.ok(), await t.text()).toBe(true);
  const task = await t.json();
  await expect
    .poll(
      async () => (await (await request.get(`/$/tasks/${task.id}`, { headers })).json()).state,
      { timeout: 30_000 },
    )
    .toBe('done');
}

anonymous(
  'A7 and A9: the tree names its rules and graphs, and a hidden ontology answers 404',
  async ({ request }) => {
    const admin = { Authorization: `Bearer ${server.token}` };
    await setUp(request, 'why-limited', admin);
    const r = await request.post('/$/reason/why-limited/explain', { headers: admin, data: WHY });
    expect(r.status(), await r.text()).toBe(200);
    const j = await r.json();
    expect(j.source).toBe('materialized');
    const root = j.trees[0];
    expect(root.rule.name).toBe('rdfs9');
    const manager = root.premises.find(
      (p: { triple: { o: { value: string } } }) => p.triple.o.value === `${EX}Manager`,
    );
    expect(manager.rule.name).toBe('rdfs2');
    const graphs = manager.premises.map((p: { kind: string; graph: string }) => [p.kind, p.graph]);
    expect(graphs).toEqual(
      expect.arrayContaining([
        ['asserted', `${EX}hr`],
        ['asserted', `${EX}ontology`],
      ]),
    );
    // A9: the limited caller reads ex:hr and the inferences, not ex:ontology
    const limited = { Authorization: `Bearer ${server.limitedToken}` };
    const hidden = await request.post('/$/reason/why-limited/explain', {
      headers: limited,
      data: WHY,
    });
    expect(hidden.status(), await hidden.text()).toBe(404);
    expect((await hidden.json()).code).toBe('no-visible-derivation');
    const absent = await request.post('/$/reason/why-limited/explain', {
      headers: admin,
      data: { ...WHY, triple: { ...WHY.triple, s: { type: 'uri', value: `${EX}bob` } } },
    });
    expect(absent.status()).toBe(404);
    expect((await absent.json()).code).toBe('no-visible-derivation');
  },
);

type Drawn = { nodes: [string, string, object?][]; edges: [string, string, string?][] };
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};

async function edgeOn(page: Page, from: string, to: string) {
  return await page
    .locator('.wrap canvas')
    .first()
    .evaluate(
      (canvas, [from, to]) => {
        type Node = { data(k: string): unknown };
        type Edge = {
          source(): Node;
          target(): Node;
          renderedMidpoint(): { x: number; y: number };
        };
        let el: HTMLElement | null = canvas.parentElement;
        while (el && !('_cyreg' in el)) el = el.parentElement;
        const cy = (el as unknown as { _cyreg: { cy: { edges(): Edge[] } } })._cyreg.cy;
        const e = [...cy.edges()].find(
          (x) => x.source().data('label') === from && x.target().data('label') === to,
        )!;
        const p = e.renderedMidpoint();
        const box = el!.getBoundingClientRect();
        return { x: box.left + p.x, y: box.top + p.y };
      },
      [from, to],
    );
}

open(
  'A10: the dashed edge opens its tree, and Show on graph draws the premises',
  async ({ page, request }) => {
    await setUp(request, 'why-open', {});
    await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), 'why-open');
    await page.goto(`/ui/explore?ds=why-open&iri=${encodeURIComponent(`${EX}alice`)}`);
    await expect.poll(async () => (await drawn(page)).nodes.map(([l]) => l)).toContain('Employee');
    await expect
      .poll(async () => (await drawn(page)).edges.some(([, , i]) => i === 'inferred'))
      .toBe(true);
    const at = await edgeOn(page, 'alice', 'Employee');
    await page.mouse.click(at.x, at.y);
    const panel = page.getByTestId('why-panel');
    const tree = panel.getByRole('tree', { name: 'Derivation' });
    await expect(tree.locator('> li')).toHaveAttribute('data-rule', 'rdfs9');
    await expect(tree.locator('li[data-rule="rdfs2"]')).toHaveCount(1);
    await expect(tree).toContainText(/asserted · graph \S*hr/);
    await expect(tree).toContainText(/asserted · graph \S*ontology/);
    await panel.getByRole('button', { name: 'Show on graph' }).click();
    // alice's manages triple and the two ontology triples are drawn and highlighted
    await expect
      .poll(async () => (await drawn(page)).edges.some(([l]) => /domain/.test(l)))
      .toBe(true);
    await expect
      .poll(async () => (await drawn(page)).edges.some(([l]) => /subClassOf/.test(l)))
      .toBe(true);
    await expect
      .poll(async () => (await drawn(page)).edges.some(([l]) => /manages/.test(l)))
      .toBe(true);
    await expect
      .poll(async () => Number(await page.locator('.wrap').first().getAttribute('data-whymarked')))
      .toBeGreaterThanOrEqual(10);
  },
);
