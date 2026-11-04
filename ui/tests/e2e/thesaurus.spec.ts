// The thesaurus editor against a real server without auth: a small thesaurus loaded into a
// dataset of its own, read as a lazy concept tree and searched in two languages, then
// edited on a session branch. Each edit is previewed as a dry run of a SPARQL update with
// If-Match and committed. The test checks the built-in SKOS shapes against the thesaurus,
// that main keeps none of the edits, that undo takes a commit back, that the notes form
// reads the built-in shapes and that the unrelated triples stay as they were.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'thes-e2e';
const TH = 'http://example.org/thesaurus/';
const SKOS = 'http://www.w3.org/2004/02/skos/core#';

const TURTLE = `
@prefix skos: <${SKOS}> . @prefix skosxl: <http://www.w3.org/2008/05/skos-xl#> .
@prefix th: <${TH}> .
th:animals a skos:ConceptScheme ; skos:prefLabel "Animals"@en ; skos:hasTopConcept th:vertebrates .
th:vertebrates a skos:Concept ; skos:inScheme th:animals ; skos:topConceptOf th:animals ;
  skos:prefLabel "Vertebrates"@en , "Vertébrés"@fr ; skos:narrower th:mammals , th:birds .
th:mammals a skos:Concept ; skos:inScheme th:animals ; skos:broader th:vertebrates ;
  skos:prefLabel "Mammals"@en , "Mammifères"@fr ; skos:narrower th:bats .
th:birds a skos:Concept ; skos:inScheme th:animals ; skos:broader th:vertebrates ;
  skos:prefLabel "Birds"@en ; skos:altLabel "Birds"@en .
th:bats a skos:Concept ; skos:inScheme th:animals ; skos:broader th:mammals ;
  skos:prefLabel "Bats"@en ; skosxl:altLabel th:bats-xl .
th:bats-xl a skosxl:Label ; skosxl:literalForm "Chiroptera"@la .
th:flight a skos:Concept ; skos:inScheme th:animals ; skos:prefLabel "Flying animals"@en .
<urn:unrelated:s> <urn:unrelated:p> "kept as it is" .
`;

async function count(request: APIRequestContext, target: string, pattern: string) {
  const r = await request.post(`/${target}/query`, {
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: `SELECT (COUNT(*) AS ?n) WHERE { ${pattern} }`,
  });
  expect(r.ok(), await r.text()).toBe(true);
  return Number((await r.json()).results.bindings[0].n.value);
}

const tree = (page: Page) => page.getByRole('tree', { name: 'Concepts' });
const node = (page: Page, name: string) =>
  tree(page).getByRole('button', { name, exact: true }).first();
const preview = (page: Page) => page.getByRole('dialog');
const labels = (page: Page) => page.getByRole('region', { name: 'Labels' });

async function commitPreview(page: Page) {
  const d = preview(page);
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden();
}

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: TURTLE,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
});

open('the tree, the search and the checks read the thesaurus', async ({ page, request }) => {
  const shapes = await request.get('/$/skos/shapes');
  expect(shapes.ok()).toBe(true);
  expect(await shapes.text()).toContain('urn:x-sparkles:skos:');

  await page.goto(`/ui/workbench/thesaurus?ds=${DS}`);
  await expect(node(page, 'Vertebrates')).toBeVisible({ timeout: 30_000 });
  await expect(tree(page).getByRole('treeitem', { name: 'Flying animals' })).toContainText(
    'not a top concept',
  );
  await tree(page).getByRole('button', { name: 'Expand Vertebrates' }).click();
  await tree(page).getByRole('button', { name: 'Expand Mammals' }).click();
  await expect(node(page, 'Bats')).toBeVisible();

  await page.getByRole('searchbox', { name: 'Search labels' }).fill('mammif');
  const hit = page.getByRole('region', { name: 'Search results' }).getByRole('button').first();
  await expect(hit).toContainText('“Mammifères”@fr');
  await page.getByRole('searchbox', { name: 'Search labels' }).fill('chiro');
  await expect(hit).toContainText('Bats');
  await hit.click();
  await expect(labels(page).locator('tr[data-lang="la"] .badge')).toHaveText('XL');

  // Birds is both preferred and alternative: S13, from the server's shapes
  await page.getByRole('tab', { name: 'Validation' }).click();
  const results = page.getByRole('list', { name: 'Check results' });
  await expect(results).toContainText('S13');
  await expect(results.getByRole('button', { name: 'Birds' }).first()).toBeVisible();
});

open(
  'edits on a session branch leave main and unrelated triples alone',
  async ({ page, request }) => {
    const before = await count(request, DS, '?s ?p ?o');
    await page.goto(`/ui/workbench/thesaurus?ds=${DS}&iri=${encodeURIComponent(TH + 'bats')}`);
    await expect(page.getByRole('heading', { name: 'Bats' })).toBeVisible({ timeout: 30_000 });
    await page.getByRole('button', { name: 'Start session' }).click();
    await expect(page).toHaveURL(/branch=skos\./);
    const branch = new URL(page.url()).searchParams.get('branch')!;
    const on = `${DS}@${branch}`;
    await expect(page.getByRole('heading', { name: 'Bats' })).toBeVisible();

    // a French preferred label, whose commit message is not ASCII
    const add = page.getByRole('form', { name: 'Add a label' });
    await add.getByRole('combobox', { name: 'Kind' }).selectOption('pref');
    await add.getByRole('textbox', { name: 'Label' }).fill('Chauves-souris');
    await add.getByRole('combobox', { name: 'Language' }).fill('fr');
    await add.getByRole('button', { name: 'Add' }).click();
    await commitPreview(page);
    await expect(labels(page).locator('tr[data-lang="fr"]')).toContainText('Chauves-souris');

    // the XL label made plain, then undone
    await labels(page).getByRole('button', { name: 'Make Chiroptera a plain label' }).click();
    await commitPreview(page);
    await expect(labels(page).locator('tr[data-lang="la"] .badge')).toHaveCount(0);
    expect(await count(request, on, `<${TH}bats-xl> ?p ?o`)).toBe(0);
    await page.getByRole('button', { name: /^Undo/ }).click();
    await expect(preview(page)).toContainText('Undo:');
    await commitPreview(page);
    await expect(labels(page).locator('tr[data-lang="la"] .badge')).toHaveText('XL');
    expect(await count(request, on, `<${TH}bats-xl> ?p ?o`)).toBe(2);

    // Bats moved under Flying animals with both directions of the hierarchy
    await expect(tree(page).getByRole('button', { name: 'Collapse Mammals' })).toBeVisible();
    await node(page, 'Bats').dragTo(node(page, 'Flying animals'));
    await expect(preview(page)).toContainText('Move Bats under Flying animals');
    await commitPreview(page);
    expect(await count(request, on, `<${TH}bats> <${SKOS}broader> <${TH}flight>`)).toBe(1);
    expect(await count(request, on, `<${TH}flight> <${SKOS}narrower> <${TH}bats>`)).toBe(1);
    expect(await count(request, on, `<${TH}bats> <${SKOS}broader> <${TH}mammals>`)).toBe(0);
    expect(await count(request, on, `<${TH}mammals> <${SKOS}narrower> <${TH}bats>`)).toBe(0);

    // a related link, written in both directions
    await page.getByRole('button', { name: 'Relations', exact: true }).click();
    await page.getByRole('button', { name: 'Add related', exact: true }).click();
    const picker = page.getByRole('group', { name: 'Related picker' });
    await picker.getByRole('searchbox').fill('bird');
    await picker.getByRole('button', { name: /Birds/ }).click();
    await commitPreview(page);
    expect(await count(request, on, `<${TH}birds> <${SKOS}related> <${TH}bats>`)).toBe(1);

    // a definition through the notes form of the built-in shapes
    await page.getByRole('button', { name: 'Notes', exact: true }).click();
    await expect(page.getByRole('region', { name: 'Details' })).toContainText(
      'built-in SKOS shapes',
    );
    await expect(page.getByRole('region', { name: 'Details' })).toContainText('Definition');
    // the server checks the change against the built-in shapes before the save
    const notes = page.getByRole('region', { name: 'Details' });
    await notes.getByRole('textbox', { name: 'Definition', exact: true }).fill('Mammals that fly.');
    await expect(notes.getByText(/^0 problems · 1 change/)).toBeVisible();
    await notes.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(page.getByText(/Saved commit \d+ to branch skos\./)).toBeVisible();
    expect(await count(request, on, `<${TH}bats> <${SKOS}definition> "Mammals that fly."`)).toBe(1);

    // a new concept under Bats
    await page.getByRole('button', { name: 'Concept', exact: true }).click();
    const create = page.getByRole('dialog', { name: 'New concept' });
    await create.getByRole('textbox', { name: 'Preferred label' }).fill('Fruit bats');
    await create.getByRole('button', { name: 'Preview' }).click();
    await commitPreview(page);
    await expect(page.getByRole('heading', { name: 'Fruit bats' })).toBeVisible();
    expect(await count(request, on, `?c <${SKOS}broader> <${TH}bats>`)).toBe(1);

    // main kept none of it, and the branch kept the unrelated triple
    expect(await count(request, DS, '?s ?p ?o')).toBe(before);
    expect(await count(request, DS, `<${TH}bats> <${SKOS}broader> <${TH}mammals>`)).toBe(1);
    expect(await count(request, on, '<urn:unrelated:s> <urn:unrelated:p> "kept as it is"')).toBe(1);
  },
);

open(
  'the forms of a named graph lock while the stored default graph holds a copy',
  async ({ page, request }) => {
    const ds = 'thes-graphs-e2e';
    const r = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'mem' } });
    expect(r.ok(), await r.text()).toBe(true);
    const up = (u: string) =>
      request.post(`/${ds}/update`, {
        headers: { 'Content-Type': 'application/sparql-update' },
        data: u,
      });
    const note = `<${TH}owls> <${SKOS}note> "Night birds"@en`;
    expect(
      (
        await up(`INSERT DATA { GRAPH <urn:g> { <${TH}owls> a <${SKOS}Concept> ;
        <${SKOS}prefLabel> "Owls"@en . ${note} } ${note} }`)
      ).ok(),
    ).toBe(true);
    const url = `/ui/workbench/thesaurus?ds=${ds}&graph=${encodeURIComponent('urn:g')}&iri=${encodeURIComponent(
      TH + 'owls',
    )}&view=notes`;
    const details = page.getByRole('region', { name: 'Details' });
    await page.goto(url);
    await expect(details.getByRole('status')).toContainText("a form's save could change them", {
      timeout: 30_000,
    });
    // without the copy the dataset keeps urn:g alone, and the page lifts its lock
    expect((await up(`DELETE DATA { ${note} }`)).ok()).toBe(true);
    await page.goto(url);
    await expect(details).toContainText('built-in SKOS shapes', { timeout: 30_000 });
    await expect(details.getByText("a form's save could change them")).toHaveCount(0);
  },
);
