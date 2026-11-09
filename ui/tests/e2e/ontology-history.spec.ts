// The entity frame's History view (ROADMAP item 620) against a real server: an ontology in
// a persistent dataset, edited on a branch through the ontology write route and SPARQL, then
// the History view of Professor lists those commits, opens the axioms each changed and
// reverts the removal of a restriction, which restores its blank-node structure. An entity
// that no commit on the branch changed has an empty history, and a reader gets no revert.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'onto-history-e2e';
const BRANCH = 'history';
const UNI = 'http://example.org/uni#';

const TURTLE = `
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix uni: <${UNI}> .

<http://example.org/uni> a owl:Ontology .
uni:Employee a owl:Class ; rdfs:label "Employee"@en .
uni:Course a owl:Class ; rdfs:label "Course"@en .
uni:Department a owl:Class ; rdfs:label "Department"@en .
uni:teaches a owl:ObjectProperty ; rdfs:label "teaches"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ;
  rdfs:subClassOf [ a owl:Restriction ; owl:onProperty uni:teaches ; owl:someValuesFrom uni:Course ] ;
  rdfs:label "Professor"@en .
`;

const RESTRICTION = `?r <http://www.w3.org/2002/07/owl#someValuesFrom> <${UNI}Course> .
  <${UNI}Professor> <http://www.w3.org/2000/01/rdf-schema#subClassOf> ?r`;

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

const historyOf = (page: Page) =>
  page.getByRole('list', { name: 'Commits that changed the entity' });

async function openHistory(page: Page, iri: string) {
  await page.goto(
    `/ui/workbench/ontology?${new URLSearchParams({ ds: DS, branch: BRANCH, iri, view: 'history' })}`,
  );
  await expect(page.getByRole('button', { name: 'History', exact: true })).toHaveAttribute(
    'aria-pressed',
    'true',
    { timeout: 30_000 },
  );
}

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: TURTLE,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
  const b = await request.post(`/$/branches/${DS}`, { data: { name: BRANCH } });
  expect(b.ok(), await b.text()).toBe(true);
  // the restriction goes through the axiom model, the comment through SPARQL
  const edit = await request.post(`/$/ontology/${DS}/axioms?branch=${BRANCH}`, {
    headers: { 'Sparkles-Commit-Message': 'Drop the teaching restriction' },
    data: {
      remove: [
        'SubClassOf(<' +
          UNI +
          'Professor> ObjectSomeValuesFrom(<' +
          UNI +
          'teaches> <' +
          UNI +
          'Course>))',
      ],
    },
  });
  expect(edit.ok(), await edit.text()).toBe(true);
  const up = await request.post(`/${DS}@${BRANCH}/update`, {
    headers: {
      'Content-Type': 'application/sparql-update',
      'Sparkles-Commit-Message': 'Comment on Professor',
    },
    data: `INSERT DATA { <${UNI}Professor> <http://www.w3.org/2000/01/rdf-schema#comment> "Teaches courses."@en }`,
  });
  expect(up.ok(), await up.text()).toBe(true);
  expect(await count(request, `${DS}@${BRANCH}`, RESTRICTION)).toBe(0);
});

open('lists the commits that changed Professor and reverts one', async ({ page, request }) => {
  await openHistory(page, `${UNI}Professor`);
  const commits = historyOf(page);
  await expect(commits.getByRole('button', { name: /Comment on Professor/ })).toBeVisible();
  await expect(commits.getByRole('listitem').first()).toContainText('Comment on Professor');
  const drop = commits.getByRole('button', { name: /Drop the teaching restriction/ });
  await drop.click();
  const changed = page.getByRole('list', { name: /Axioms commit \d+ changed/ });
  await expect(changed.locator('li.removed')).toContainText('ObjectSomeValuesFrom');
  await expect(changed.locator('li.added')).toHaveCount(0);

  await page.getByRole('button', { name: /^Revert commit \d+$/ }).click();
  const d = page.getByRole('dialog');
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden();
  await expect.poll(() => count(request, `${DS}@${BRANCH}`, RESTRICTION)).toBe(1);
  // main never had the edits
  expect(await count(request, DS, RESTRICTION)).toBe(1);
  await expect(commits.getByRole('listitem')).toHaveCount(3, { timeout: 15_000 });
  await commits.getByRole('listitem').first().getByRole('button').first().click();
  await expect(page.locator('li.added').first()).toContainText('ObjectSomeValuesFrom');
});

open('an entity no commit on the branch changed has an empty history', async ({ page }) => {
  await openHistory(page, `${UNI}Department`);
  await expect(
    page.getByText('No recorded commit changed this entity in this graph.'),
  ).toBeVisible();
});

open('a reader sees the history without a revert', async ({ page }) => {
  await page.route('**/$/whoami', (route) =>
    route.fulfill({
      json: {
        authEnabled: true,
        principal: { kind: 'token', name: 'reader' },
        method: 'bearer',
        server: [],
        datasets: { [DS]: 'read' },
        canMintTokens: false,
        logout: false,
      },
    }),
  );
  await openHistory(page, `${UNI}Professor`);
  await historyOf(page)
    .getByRole('button', { name: /Comment on Professor/ })
    .click();
  await expect(page.getByRole('list', { name: /Axioms commit \d+ changed/ })).toContainText(
    'rdfs:comment',
  );
  await expect(page.getByRole('button', { name: /^Revert commit \d+$/ })).toHaveCount(0);
});
