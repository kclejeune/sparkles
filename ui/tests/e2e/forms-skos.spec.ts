// SKOS-shaped editing forms against a real server (docs/specs/U02-shape-driven-forms/
// SPEC.md §2.3.1 and Phase 3): a concept's SKOS-XL labels as nested blank-node children,
// edited, added and removed in atomic saves under a `reject` guard, and an ordered
// collection whose `skos:memberList` the list editor reorders.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

const EX = 'http://example.org/thesaurus/';
const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const XL = 'http://www.w3.org/2008/05/skos-xl#';
const PREFIXES = `PREFIX ex: <${EX}> PREFIX skos: <${SKOS}> PREFIX skosxl: <${XL}>`;

const SHAPES = `@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix ex: <${EX}> . @prefix skos: <${SKOS}> . @prefix skosxl: <${XL}> .
ex:ConceptShape a sh:NodeShape ; sh:targetClass skos:Concept ;
  sh:property [ sh:path skos:prefLabel ; sh:name "Preferred label" ; sh:datatype rdf:langString ;
                sh:uniqueLang true ; sh:minCount 1 ; sh:order 1 ] ;
  sh:property [ sh:path skos:broader ; sh:name "Broader" ; sh:class skos:Concept ; sh:order 2 ] ;
  sh:property [ sh:path skosxl:prefLabel ; sh:name "XL labels" ; sh:node ex:XLabel ;
                sh:nodeKind sh:BlankNode ; sh:order 3 ] .
ex:XLabel sh:property [ sh:path skosxl:literalForm ; sh:name "Literal form" ;
                        sh:datatype rdf:langString ; sh:minCount 1 ; sh:maxCount 1 ] .
ex:CollectionShape a sh:NodeShape ; sh:targetClass skos:Collection ;
  sh:property [ sh:path skos:prefLabel ; sh:name "Label" ; sh:datatype rdf:langString ; sh:order 1 ] ;
  sh:property [ sh:path skos:member ; sh:name "Members" ; sh:order 2 ;
                sh:or ( [ sh:class skos:Concept ; sh:name "Concept" ]
                        [ sh:class skos:Collection ; sh:name "Collection" ] ) ] ;
  sh:property [ sh:path skos:memberList ; sh:name "Ordered members" ; sh:maxCount 1 ; sh:order 3 ;
                sh:memberShape [ sh:class skos:Concept ] ] .`;

const DATA = `${PREFIXES} INSERT DATA {
  ex:animals a skos:Concept ; skos:prefLabel "Animals"@en , "Animaux"@fr ;
    skosxl:prefLabel [ skosxl:literalForm "Animals"@en ] .
  ex:mammals a skos:Concept ; skos:prefLabel "Mammals"@en ; skos:broader ex:animals .
  ex:birds a skos:Concept ; skos:prefLabel "Birds"@en ; skos:broader ex:animals .
  ex:pets a skos:Collection ; skos:prefLabel "Pets"@en ; skos:member ex:mammals ;
    skos:memberList ( ex:mammals ex:birds ) .
}`;

async function thesaurus(request: APIRequestContext, name: string) {
  const made = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  expect(made.ok(), await made.text()).toBe(true);
  const r = await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: DATA,
  });
  expect(r.ok(), await r.text()).toBe(true);
  const g = await request.put(`/$/validation/${name}`, {
    data: { mode: 'reject', shapes: { inline: SHAPES } },
  });
  expect(g.ok(), await g.text()).toBe(true);
}

async function openForm(page: Page, ds: string, local: string) {
  await page.goto(`/ui/datasets/${ds}/edit?${new URLSearchParams({ iri: EX + local })}`);
  const form = page.getByRole('region', { name: /^Edit / });
  await expect(form.locator('.ff').first()).toBeVisible({ timeout: 30_000 });
  return form;
}

const ask = async (request: APIRequestContext, ds: string, pattern: string) => {
  const q = `${PREFIXES} ASK { ${pattern} }`;
  const r = await request.get(`/${ds}/sparql?query=${encodeURIComponent(q)}`, {
    headers: { Accept: 'application/sparql-results+json' },
  });
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).boolean as boolean;
};

open('SKOS-XL labels are nested forms saved atomically', async ({ page, request }) => {
  const ds = 'forms-skos-xl';
  await thesaurus(request, ds);
  const form = await openForm(page, ds, 'animals');
  const labels = form.locator(`[data-field="<${XL}prefLabel>"]`);
  const forms = labels.getByLabel('Literal form', { exact: true });
  await expect(forms).toHaveCount(1);
  await expect(forms).toHaveValue('Animals');
  await forms.fill('All animals');
  await labels.getByRole('button', { name: '+ Add XL labels' }).click();
  await labels.getByLabel('Literal form', { exact: true }).nth(1).fill('Tiere');
  await labels.getByLabel('Language of Literal form').nth(1).fill('de');
  await form.getByRole('button', { name: 'Preview' }).click();
  const preview = form.getByRole('region', { name: 'Preview' });
  await expect(preview).toContainText('The save would commit.');
  await form.getByLabel('Message', { exact: true }).fill('XL labels');
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
  const label = (v: string) => `ex:animals skosxl:prefLabel/skosxl:literalForm ${v}`;
  expect(await ask(request, ds, label('"All animals"@en'))).toBe(true);
  expect(await ask(request, ds, label('"Tiere"@de'))).toBe(true);
  expect(await ask(request, ds, label('"Animals"@en'))).toBe(false);
  // the unrelated labels stay
  expect(await ask(request, ds, 'ex:animals skos:prefLabel "Animaux"@fr')).toBe(true);

  // a new label without its literal form breaks sh:minCount, which the browser shows
  await labels.getByRole('button', { name: '+ Add XL labels' }).click();
  await expect(labels.getByText('is required').last()).toBeVisible();
  await labels.getByRole('button', { name: 'Remove XL labels 3' }).click();

  // removing a label deletes the child with its literal form
  await expect(labels.getByLabel('Literal form', { exact: true })).toHaveCount(2);
  await labels.getByRole('button', { name: 'Remove XL labels 1' }).click();
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText(/Saved commit \d+ to main/).last()).toBeVisible();
  await expect(labels.getByLabel('Literal form', { exact: true })).toHaveCount(1);
  const left =
    Number(await ask(request, ds, '?l skosxl:literalForm "All animals"@en')) +
    Number(await ask(request, ds, '?l skosxl:literalForm "Tiere"@de'));
  expect(left).toBe(1);
});

open('a collection reorders its member list and checks its options', async ({ page, request }) => {
  const ds = 'forms-skos-list';
  await thesaurus(request, ds);
  const form = await openForm(page, ds, 'pets');
  const members = form.locator(`[data-field="<${SKOS}member>"]`);
  await expect(members.getByLabel('Kind of Members')).toHaveValue('0');
  const list = form.getByRole('list', { name: 'Ordered members members' });
  await expect(list.getByRole('listitem')).toHaveCount(2);
  await list.getByRole('button', { name: 'Move Ordered members member 2 up' }).click();
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
  expect(await ask(request, ds, 'ex:pets skos:memberList (ex:birds ex:mammals)')).toBe(true);
  // the stored list has no leftover cells
  expect(
    await ask(
      request,
      ds,
      '?c <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ?m FILTER NOT EXISTS { ?x <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest>* ?c . ex:pets skos:memberList ?x }',
    ),
  ).toBe(false);
});
