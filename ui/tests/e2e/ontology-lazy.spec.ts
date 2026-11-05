// The Workbench reads what its views show (ROADMAP item 725), against a real server. An
// ontology of a few thousand classes opens on the summary and the first level of the tree:
// it must not read every label, every subclass axiom or the entity lists. A selected
// entity reads its own frame and the names that frame draws, the Individuals tab reads its
// list when it opens, and the rows in the page stay a screenful however long the level is.

import type { Page, Request } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'onto-lazy-e2e';
const NS = 'http://example.org/lazy#';
const ROOTS = 3;
const KIDS = 1000;

function turtle(): string {
  const out = [
    '@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .',
    '@prefix owl: <http://www.w3.org/2002/07/owl#> .',
    `@prefix l: <${NS}> .`,
    '<http://example.org/lazy> a owl:Ontology ; rdfs:label "Lazy ontology"@en .',
    'l:partOf a owl:ObjectProperty ; rdfs:label "part of"@en .',
  ];
  for (let r = 0; r < ROOTS; r++) {
    out.push(`l:R${r} a owl:Class ; rdfs:label "root ${r}"@en .`);
    for (let k = 0; k < KIDS; k++) {
      out.push(
        `l:R${r}_${k} a owl:Class ; rdfs:subClassOf l:R${r} ; rdfs:label "class ${r}-${String(k).padStart(4, '0')}"@en .`,
      );
    }
  }
  for (let i = 0; i < 240; i++) {
    out.push(`l:ind${i} a owl:NamedIndividual, l:R0 ; rdfs:label "individual ${i}"@en .`);
  }
  out.push('l:untyped a owl:NamedIndividual . l:ind239 a l:R1 . [] a l:R0 .');
  return out.join('\n');
}

/** What the page asked the server for, by what it asked. */
type Seen = { path: string; query: string; body: string };
function watch(page: Page) {
  const seen: Seen[] = [];
  const onRequest = (r: Request) => {
    const u = new URL(r.url());
    if (!u.pathname.startsWith(`/$/ontology/${DS}`) && !u.pathname.startsWith(`/${DS}/`)) return;
    seen.push({ path: u.pathname, query: u.search, body: r.postData() ?? '' });
  };
  page.on('request', onRequest);
  return {
    seen,
    mark: () => seen.length,
    since: (from: number) => seen.slice(from),
  };
}

const route = (s: Seen, name: string) => s.path === `/$/ontology/${DS}/${name}`;
/** A SPARQL request that reads the labels of every subject of the graph. */
const wholeLabelQuery = (s: Seen) =>
  s.path.endsWith('/sparql') && /\?s \?p \?l/.test(s.body) && !/VALUES \?s|\?s a \?t/.test(s.body);

const tree = (page: Page) => page.getByRole('tree', { name: 'Classes (asserted)' });

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: turtle(),
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
});

open('opening the editor reads the summary and one level of the tree', async ({ page }) => {
  const w = watch(page);
  await page.goto(`/ui/workbench/ontology?ds=${DS}`);
  await expect(tree(page).getByRole('button', { name: 'root 0', exact: true })).toBeVisible({
    timeout: 60_000,
  });
  await expect(page.getByLabel('Entity kind')).toContainText('Classes');
  // the summary, the roots of the class tree, and nothing that reads the whole ontology
  expect(
    w.seen.filter((s) => route(s, 'entities')),
    'entity lists',
  ).toHaveLength(0);
  expect(
    w.seen.filter((s) => route(s, 'axioms')),
    'axiom listings',
  ).toHaveLength(0);
  expect(w.seen.filter(wholeLabelQuery), 'labels of the whole graph').toHaveLength(0);
  expect(w.seen.filter((s) => route(s, 'hierarchy')).length).toBeLessThanOrEqual(2);
  expect(w.seen.filter((s) => s.path === `/$/ontology/${DS}`).length).toBeLessThanOrEqual(2);
});

open('a long level draws a screenful of rows and pages on request', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const w = watch(page);
  await page.goto(`/ui/workbench/ontology?ds=${DS}`);
  await tree(page).getByRole('button', { name: 'Expand root 0' }).click();
  await expect(tree(page).getByRole('treeitem', { name: 'class 0-0000' })).toBeVisible({
    timeout: 60_000,
  });
  // 1,000 children are read and fewer than 80 rows are in the page
  expect(await tree(page).getByRole('treeitem').count()).toBeLessThan(80);
  // the level is one page, which the tree reads without another request while it scrolls
  const before = w.mark();
  await tree(page).focus();
  await page.keyboard.press('PageDown');
  await page.keyboard.press('PageDown');
  await page.keyboard.press('End');
  expect(w.since(before).filter((s) => route(s, 'hierarchy'))).toHaveLength(0);
  expect(await tree(page).getByRole('treeitem').count()).toBeLessThan(80);
});

open('a selected class reads its frame and the names the frame draws', async ({ page }) => {
  const w = watch(page);
  await page.goto(`/ui/workbench/ontology?ds=${DS}&iri=${encodeURIComponent(`${NS}R1_7`)}`);
  await expect(page.getByRole('heading', { name: 'class 1-0007' })).toBeVisible({
    timeout: 60_000,
  });
  // the frame's own axioms, listed about the class, and not the ontology's subclass axioms
  const axioms = w.seen.filter((s) => route(s, 'axioms'));
  expect(axioms.length).toBeGreaterThan(0);
  for (const a of axioms) expect(a.query).toContain('about=');
  expect(w.seen.filter(wholeLabelQuery), 'labels of the whole graph').toHaveLength(0);
});

open('the Individuals tab opens buckets and pages only the expanded type', async ({ page }) => {
  const w = watch(page);
  await page.goto(`/ui/workbench/ontology?ds=${DS}`);
  await expect(tree(page).getByRole('button', { name: 'root 0', exact: true })).toBeVisible({
    timeout: 60_000,
  });
  const before = w.mark();
  await page.getByLabel('Entity kind').selectOption('NamedIndividual');
  const individuals = page.getByRole('tree', { name: 'Individuals (asserted)' });
  await expect(individuals.getByRole('button', { name: 'root 0', exact: true })).toBeVisible({
    timeout: 60_000,
  });
  expect(w.since(before).filter((s) => route(s, 'individuals'))).toHaveLength(1);
  expect(w.since(before).filter((s) => route(s, 'entities') || route(s, 'axioms'))).toHaveLength(0);
  await individuals.getByRole('button', { name: 'Expand root 0', exact: true }).click();
  await expect(
    individuals.getByRole('button', { name: 'individual 0', exact: true }),
  ).toBeVisible();
  expect(await individuals.getByRole('treeitem').count()).toBeLessThan(80);
  await individuals.focus();
  await page.keyboard.press('End');
  await individuals.getByRole('button', { name: 'Show more', exact: true }).click();
  await individuals.focus();
  await page.keyboard.press('End');
  await expect(
    individuals.getByRole('treeitem', { name: 'Show more, 200 of 240 shown' }),
  ).toBeAttached();
  const later = w.since(before);
  expect(later.filter((s) => route(s, 'individuals')).length).toBe(3);
  expect(later.filter((s) => route(s, 'entities') || route(s, 'axioms'))).toHaveLength(0);
  expect(
    later
      .filter((s) => route(s, 'individuals') && s.query.includes('type='))
      .every((s) => s.query.includes('at=')),
  ).toBe(true);
  // the individuals' labels come in one query for the kind, not one for the graph
  expect(later.filter(wholeLabelQuery)).toHaveLength(0);
});

open('an individual beyond the first page reveals both asserted memberships', async ({ page }) => {
  const w = watch(page);
  await page.goto(
    `/ui/workbench/ontology?ds=${DS}&kind=NamedIndividual&iri=${encodeURIComponent(NS + 'ind239')}`,
  );
  const individuals = page.getByRole('tree', { name: 'Individuals (asserted)' });
  await expect(
    individuals.getByRole('button', { name: 'individual 239', exact: true }).first(),
  ).toBeVisible({ timeout: 60_000 });
  await expect(page.getByRole('heading', { name: 'individual 239', exact: true })).toBeVisible();
  const reads = w.seen.filter((s) => route(s, 'individuals'));
  expect(reads.some((s) => s.query.includes('iri='))).toBe(true);
  expect(reads.every((s) => !s.query.includes('after='))).toBe(true);
  expect(reads.filter((s) => s.query.includes('type=')).length).toBe(2);
});
