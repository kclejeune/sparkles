// The ontology browser against a real server without auth: a small OBO-style ontology
// (tests/browse-fixture.ts) loaded into a dataset of its own, read through the hierarchy
// and term routes. The tests check the sections the server's answers fill, which the mock
// cannot: definitions with their sources from axiom annotations, the lineage of a class
// with two parents, mentions through restrictions, a deprecated class and the downloads.

import { expect, open } from './fixtures';
import { BROWSE_DATASET as DS, BROWSE_TURTLE, CELLO } from '../browse-fixture';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const term = (n: number, extra = '') =>
  `/ui/browse?ds=${DS}&iri=${encodeURIComponent(CELLO(n))}${extra}`;

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: BROWSE_TURTLE,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
});

open('the landing page reads the header, the counts and the roots', async ({ page }) => {
  await page.goto(`/ui/browse?ds=${DS}`);
  await expect(
    page.getByRole('heading', { level: 1, name: 'Cell organelle ontology' }),
  ).toBeVisible();
  await expect(page.locator('.lede')).toContainText('parts of a eukaryotic cell');
  const top = page.getByRole('region', { name: 'Top classes' });
  await expect(top.getByRole('link', { name: 'cellular component' })).toBeVisible();
  await expect(top).toContainText('A location in or around a cell');
  const contents = page.getByRole('region', { name: 'Contents' });
  await expect(contents).toContainText('of them deprecated');

  await page.getByRole('searchbox').fill('powerhouse');
  const hit = page.locator('.hits').getByRole('link', { name: 'mitochondrion' });
  await expect(hit).toBeVisible();
  await expect(page.locator('.hits')).toContainText('synonym');
  await hit.click();
  await expect(page.getByRole('heading', { level: 1, name: 'mitochondrion' })).toBeVisible();
});

open(
  'a class page shows its definition, synonyms, axioms and cross-references',
  async ({ page }) => {
    await page.goto(term(5));
    await expect(page.getByRole('heading', { level: 1, name: 'mitochondrion' })).toBeVisible();
    await expect(page.locator('.lede')).toContainText('self-replicating organelle');
    await expect(page.locator('.lede')).toContainText('CELLO:curators');

    const synonyms = page.getByRole('region', { name: 'Synonyms' });
    await expect(synonyms).toContainText('Exact');
    await expect(synonyms).toContainText('mitochondria');
    await expect(synonyms).toContainText('CELLO:plural');
    await expect(synonyms).toContainText('powerhouse of the cell');

    const logic = page.getByRole('region', { name: 'Axioms' });
    await expect(logic.getByRole('link', { name: 'membrane-bounded organelle' })).toBeVisible();
    await expect(logic.locator('[data-section="sub"]')).toContainText('part of some cytoplasm');

    // a CURIE of the ontology's own namespace in a comment links to its page
    await expect(page.getByRole('link', { name: 'CELLO:0000011' })).toHaveAttribute(
      'href',
      new RegExp(encodeURIComponent(CELLO(11))),
    );
    const xrefs = page.getByRole('region', { name: 'Cross-references' });
    await expect(xrefs.getByRole('link', { name: /Mitochondrion/ })).toHaveAttribute(
      'href',
      'https://en.wikipedia.org/wiki/Mitochondrion',
    );
    const facts = page.getByRole('complementary', { name: 'About this entity' });
    await expect(facts).toContainText('CELLO:0000005');
    await expect(facts).toContainText('Cell organelle ontology');
    await expect(facts).toContainText('2 parents, 1 subclass');
  },
);

open('the lineage tree follows one path and names the other parent', async ({ page }) => {
  await page.goto(term(5));
  const tree = page.getByRole('complementary', { name: 'Hierarchy' });
  const spine = tree.getByRole('list', { name: 'Lineage' });
  await expect(spine.getByRole('link', { name: 'cellular component' })).toBeVisible();
  await expect(spine.getByRole('link', { name: /^organelle/ })).toBeVisible();
  // the path takes the first parent by label, and the other one is named beside it
  await expect(spine.getByRole('link', { name: /^intracellular organelle/ })).toBeVisible();
  await expect(tree.locator('.alsop')).toContainText('Also under membrane-bounded organelle');
  await expect(tree.getByRole('link', { name: 'mitochondrial nucleoid' })).toBeVisible();

  await page.goto(term(9));
  await tree
    .getByRole('button', { name: /3 other subclasses of membrane-bounded organelle/ })
    .click();
  await expect(tree.getByRole('link', { name: /^mitochondrion/ })).toBeVisible();
  await expect(tree.getByRole('link', { name: /^vacuole/ })).toBeVisible();
  await expect(tree.getByRole('link', { name: /^mitosome/ })).toBeVisible();
});

open('what links here groups the restrictions that hold the class', async ({ page }) => {
  await page.goto(term(5));
  const usage = page.getByRole('region', { name: /What links here/ });
  const partOf = usage.locator('.grp', { hasText: 'part of some mitochondrion' });
  await expect(partOf.getByRole('link', { name: 'mitochondrial envelope' })).toBeVisible();
  await expect(partOf.getByRole('link', { name: 'mitochondrial matrix' })).toBeVisible();
  await expect(usage.getByRole('heading', { name: /Replaces/ })).toBeVisible();
});

open('a deprecated class names its replacement', async ({ page }) => {
  await page.goto(term(12));
  await expect(page.getByRole('note')).toContainText('deprecated');
  await page.getByRole('note').getByRole('link', { name: 'mitochondrion' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'mitochondrion' })).toBeVisible();
});

open('a property page shows its characteristics and inverse', async ({ page }) => {
  await page.goto(
    `/ui/browse?ds=${DS}&iri=${encodeURIComponent('http://purl.obolibrary.org/obo/BFO_0000050')}`,
  );
  await expect(page.getByRole('heading', { level: 1, name: 'part of' })).toBeVisible();
  const logic = page.getByRole('region', { name: 'Axioms' });
  await expect(logic).toContainText('Transitive');
});

open('the downloads answer each format as an attachment', async ({ page }) => {
  await page.goto(term(5));
  await expect(page.locator('[data-format]')).toHaveCount(4);
  for (const f of ['ttl', 'rdfxml', 'ofn', 'jsonld']) {
    const href = await page.locator(`[data-format="${f}"]`).getAttribute('href');
    const res = await page.request.get(href!);
    expect(res.status(), f).toBe(200);
    expect(res.headers()['content-disposition'], f).toContain('attachment');
    // Turtle and functional-style syntax may write the IRI with the data's obo: prefix
    expect(await res.text(), f).toContain('CELLO_0000005');
  }
});

open('the graph view draws the neighbourhood', async ({ page }) => {
  await page.goto(term(5, '&view=graph'));
  await expect(page.locator('.canvas canvas').first()).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole('link', { name: 'Open in Explore' }).first()).toBeVisible();
});

open('a cycle in the hierarchy stops at the class it returns to', async ({ page, request }) => {
  const ds = 'browse-e2e-cycle';
  const ex = 'http://example.org/cycle#';
  expect((await request.post('/$/datasets', { data: { dbName: ds, dbType: 'mem' } })).ok()).toBe(
    true,
  );
  const loaded = await request.post(`/${ds}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: `@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix : <${ex}> .
:Root a owl:Class ; rdfs:label "root" .
:A a owl:Class ; rdfs:label "alpha" ; rdfs:subClassOf :Root , :B .
:B a owl:Class ; rdfs:label "beta" ; rdfs:subClassOf :A .`,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
  await page.goto(`/ui/browse?ds=${ds}&iri=${encodeURIComponent(ex + 'Root')}`);
  const tree = page.getByRole('complementary', { name: 'Hierarchy' });
  await tree.getByRole('button', { name: 'Open alpha' }).click();
  await tree.getByRole('button', { name: 'Open beta' }).click();
  // alpha comes again under beta, marked as a cycle, with nothing further to open
  await expect(tree.locator('.cyc')).toHaveCount(1);
  await expect(tree.getByRole('button', { name: 'Open alpha' })).toHaveCount(0);
  await expect(tree.getByRole('link', { name: /^alpha/ })).toHaveCount(2);
});

open('Show more pages instances and users past the first ones', async ({ page, request }) => {
  const ds = 'browse-e2e-more';
  const ex = 'http://example.org/more#';
  expect((await request.post('/$/datasets', { data: { dbName: ds, dbType: 'mem' } })).ok()).toBe(
    true,
  );
  const many = Array.from(
    { length: 75 },
    (_, n) => `:i${String(n).padStart(3, '0')} a :K ; :p :o .`,
  );
  const loaded = await request.post(`/${ds}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: `@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix : <${ex}> .
:K a owl:Class ; rdfs:label "kind" .
:p a owl:ObjectProperty ; rdfs:label "points at" .
${many.join('\n')}`,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
  for (const [local, heading] of [
    ['K', 'Instances'],
    ['p', 'Used by'],
  ]) {
    await page.goto(`/ui/browse?ds=${ds}&iri=${encodeURIComponent(ex + local)}`);
    const usage = page.getByRole('region', { name: /What links here/ });
    const grp = usage.locator('.grp', { has: page.getByRole('heading', { name: heading }) });
    await expect(grp.getByRole('link')).toHaveCount(20);
    await grp.getByRole('button', { name: /Show more/ }).click();
    // the next page starts again in IRI order and skips the subjects already shown
    await expect(grp.getByRole('link')).toHaveCount(50);
    await grp.getByRole('button', { name: /Show more/ }).click();
    await expect(grp.getByRole('link')).toHaveCount(75);
    await expect(grp.getByRole('button', { name: /Show more/ })).toHaveCount(0);
  }
});
