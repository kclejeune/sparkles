// The ontology editor against a real server without auth: an ontology loaded into a
// persistent dataset of its own, browsed in the trees and the frame, then edited on a
// session branch. Each edit is previewed as a dry run and committed through the
// `/$/ontology` write routes. The test checks that main keeps none of the edits, that
// undo takes a commit back and that the inferred class tree comes from the EL classifier.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'onto-e2e';
const UNI = 'http://example.org/uni#';

const TURTLE = `
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix uni: <${UNI}> .

<http://example.org/uni> a owl:Ontology ; rdfs:label "University ontology"@en .
uni:Agent a owl:Class ; rdfs:label "Agent"@en .
uni:Person a owl:Class ; rdfs:subClassOf uni:Agent ; rdfs:label "Person"@en , "Personne"@fr .
uni:Employee a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Employee"@en .
uni:Course a owl:Class ; rdfs:label "Course"@en .
uni:teaches a owl:ObjectProperty ; rdfs:label "teaches"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ;
  rdfs:label "Professor"@en .
uni:Teacher a owl:Class ; rdfs:label "Teacher"@en ;
  owl:equivalentClass [ owl:intersectionOf ( uni:Person
    [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ) ] .
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

const tree = (page: Page) => page.getByRole('tree', { name: 'Classes (asserted)' });
const expand = (page: Page, name: string) =>
  tree(page)
    .getByRole('button', { name: `Expand ${name}` })
    .click();
const preview = (page: Page) => page.getByRole('dialog');

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

open(
  'kept class frames read without computing and pin their real backend pages',
  async ({ page, request }, info) => {
    const ds = `${DS}-kept`;
    const created = await request.post('/$/datasets', {
      data: { dbName: ds, dbType: 'persistent' },
    });
    expect(created.ok(), await created.text()).toBe(true);
    const loaded = await request.post(`/${ds}/data`, {
      headers: { 'Content-Type': 'text/turtle' },
      data: TURTLE,
    });
    expect(loaded.ok(), await loaded.text()).toBe(true);
    const posts: string[] = [];
    const pages: {
      url: string;
      commit: number;
      resultId: string;
      part: string;
      rows: number;
      decodedBytes: number;
    }[] = [];
    const settled: Promise<void>[] = [];
    page.on('request', (r) => {
      if (r.method() === 'POST' && /\/classify(?:\?|$)/.test(r.url())) posts.push(r.url());
    });
    page.on('response', (r) => {
      if (!/\/classification\?/.test(r.url())) return;
      settled.push(
        (async () => {
          const body = await r.body();
          const value = JSON.parse(body.toString());
          if (value.status === 'available')
            pages.push({
              url: r.url(),
              commit: value.commit,
              resultId: value.result.resultId,
              part: new URL(r.url()).searchParams.get('part') ?? 'status',
              rows: value.page?.items.length ?? 0,
              decodedBytes: body.length,
            });
        })(),
      );
    });
    const region = page.getByRole('region', { name: 'Inferred class axioms', exact: true });
    await page.goto(`/ui/browse?ds=${ds}&iri=${encodeURIComponent(UNI + 'Professor')}`);
    await expect(region.getByRole('button', { name: 'Classify', exact: true })).toBeVisible();
    expect(posts).toEqual([]);
    const projected = page.waitForResponse(
      (r) => r.request().method() === 'POST' && /\/classify(?:\?|$)/.test(r.url()),
    );
    await region.getByRole('button', { name: 'Classify', exact: true }).click();
    const response = await projected;
    expect(response.ok(), await response.text()).toBe(true);
    const frame = (await response.json()).frame;
    expect(frame.retention.kept).toBe(true);
    await expect(region.getByRole('link', { name: 'Teacher', exact: true })).toBeVisible();
    expect(posts).toHaveLength(1);

    // A fresh editor with inferred=1 shares the completed read but never POSTs.
    await page.goto(
      `/ui/workbench/ontology?ds=${ds}&inferred=1&iri=${encodeURIComponent(UNI + 'Professor')}`,
    );
    await expect(region.getByRole('button', { name: 'Teacher', exact: true })).toBeVisible();
    expect(posts).toHaveLength(1);
    await Promise.all(settled);
    // The frame still reads its same three bounded parts. The adjacent hierarchy
    // now also reads kept roots/children and bounded ancestor paths.
    const framePages = pages.filter((p) => ['equivalents', 'parents', 'status'].includes(p.part));
    expect(framePages.filter((p) => p.part === 'equivalents')).toHaveLength(1);
    expect(framePages.filter((p) => p.part === 'status')).toHaveLength(1);
    expect(framePages.some((p) => p.part === 'parents')).toBe(true);
    for (const p of pages) {
      expect(p.commit).toBe(frame.result.classifiedCommit);
      expect(p.resultId).toBe(frame.result.resultId);
      expect(p.rows).toBeLessThanOrEqual(100);
      expect(new URL(p.url).searchParams.get('at')).toBe(`commit:${p.commit}`);
      if (!['status', 'roots'].includes(p.part))
        expect(new URL(p.url).searchParams.get('resultId')).toBe(p.resultId);
    }
    await info.attach('kept-frame-display.json', {
      body: JSON.stringify(
        { pages, explicitPosts: posts.length, domElements: await region.locator('*').count() },
        null,
        2,
      ),
      contentType: 'application/json',
    });
  },
);

open('the trees, the frame and the inferred tree read the ontology', async ({ page }) => {
  await page.goto(`/ui/workbench/ontology?ds=${DS}&inferred=1`);
  await expect(tree(page).getByRole('button', { name: 'Agent', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Classify tree', exact: true }).click();
  await expand(page, 'Agent');
  await expand(page, 'Person');
  await expand(page, 'Employee');
  await tree(page).getByRole('button', { name: 'Professor', exact: true }).click();
  const sub = page.locator('.frame [data-section="sub"]');
  await expect(sub).toContainText('Employee');
  await expect(sub).toContainText('teaches some Course');

  // the classifier puts Teacher under Person and Professor under Teacher as well as under
  // Employee, and the selection opens both of Professor's paths
  const inferred = page.getByRole('tree', { name: 'Classes (inferred)' });
  const teacher = inferred.getByRole('treeitem', { name: 'Teacher', exact: true });
  // the rows are flat, so a child is the row after its parent, one level down
  const level = Number(await teacher.getAttribute('aria-level'));
  const below = teacher.locator('xpath=following-sibling::li[1]');
  await expect(below).toHaveAttribute('aria-level', String(level + 1));
  await expect(below.getByRole('button', { name: 'Professor', exact: true })).toBeVisible();
  await expect(inferred.getByRole('button', { name: 'Professor', exact: true })).toHaveCount(2);
  await expect(tree(page).getByRole('button', { name: 'Expand Teacher' })).toHaveCount(0);
});

open(
  'edits on a session branch leave main and unrelated triples alone',
  async ({ page, request }) => {
    const before = await count(request, DS, '?s ?p ?o');
    await page.goto(`/ui/workbench/ontology?ds=${DS}`);
    await expect(tree(page).getByRole('button', { name: 'Agent', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Start session' }).click();
    await expect(page).toHaveURL(/branch=onto\./);
    const branch = new URL(page.url()).searchParams.get('branch')!;

    // a new class under Employee
    await expand(page, 'Agent');
    await expand(page, 'Person');
    await tree(page).getByRole('button', { name: 'Employee', exact: true }).click();
    await page.getByRole('button', { name: 'New', exact: true }).click();
    const create = page.getByRole('dialog', { name: 'New class' });
    await create.getByRole('textbox', { name: 'Label', exact: true }).fill('teaching assistant');
    await expect(create.getByRole('textbox', { name: /IRI/ })).toHaveValue(
      `${UNI}TeachingAssistant`,
    );
    await create.getByRole('button', { name: 'Preview' }).click();
    await expect(preview(page)).toContainText("'teaching assistant' SubClassOf Employee");
    await commitPreview(page);
    await expect(page).toHaveURL(/TeachingAssistant/);
    const ta = `<${UNI}TeachingAssistant> ?p ?o`;
    expect(await count(request, `${DS}@${branch}`, ta)).toBeGreaterThan(0);

    // a French label, previewed and committed
    const frame = page.locator('.frame');
    await frame.getByRole('button', { name: 'Add annotation' }).first().click();
    const form = frame.getByRole('form', { name: /Annotation of teaching assistant/ });
    await form.getByRole('combobox', { name: 'Property' }).fill('rdfs:label');
    await form.getByRole('textbox', { name: 'Value' }).fill('assistant d’enseignement');
    await form.getByRole('textbox', { name: 'Language' }).fill('fr');
    await form.getByRole('button', { name: 'Preview' }).click();
    await expect(preview(page)).toContainText('"assistant d’enseignement"@fr');
    await commitPreview(page);
    await expect(frame).toContainText('assistant d’enseignement');

    // undo takes the label back
    await page.getByRole('button', { name: /^Undo/ }).click();
    await expect(preview(page)).toContainText('Undo:');
    await commitPreview(page);
    await expect(frame).not.toContainText('assistant d’enseignement');

    // delete removes the class with its axioms
    await page.getByRole('button', { name: 'Delete' }).click();
    await expect(preview(page)).toContainText('Removes the entity');
    await commitPreview(page);
    await expect(tree(page).getByRole('button', { name: 'teaching assistant' })).toHaveCount(0);
    expect(await count(request, `${DS}@${branch}`, ta)).toBe(0);

    // main kept none of it, and the branch kept every other triple
    expect(await count(request, DS, ta)).toBe(0);
    expect(await count(request, DS, '?s ?p ?o')).toBe(before);
    expect(await count(request, `${DS}@${branch}`, '?s ?p ?o')).toBe(before);
    expect(
      await count(
        request,
        `${DS}@${branch}`,
        '<urn:unrelated:s> <urn:unrelated:p> "kept as it is"',
      ),
    ).toBe(1);
  },
);
