// Bulk edits of the ontology editor (ROADMAP item 626) against a real server. On the server
// without auth, an ontology in a named graph of a persistent dataset gets a find and replace
// in its labels and a move of two classes, one with an annotated SubClassOf, on a branch.
// Each is one commit, main and the other graph stay as they were, and undo restores the
// annotated axiom's reification. An edit above the bound is refused, and a protected branch
// offers no preview and refuses the write. Under auth, a reader lists changes without a
// preview and is refused the write.

import type { APIRequestContext, Page } from '@playwright/test';
import { BULK_AUTH, PASSWORD, READER } from './data';
import { anonymous, expect, open, server } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'onto-bulk-e2e';
const BRANCH = 'bulk';
const GRAPH = 'http://example.org/uni';
const OTHER = 'http://example.org/other';
const UNI = 'http://example.org/uni#';
const RDFS = 'http://www.w3.org/2000/01/rdf-schema#';
const OWL = 'http://www.w3.org/2002/07/owl#';

const TURTLE = `
@prefix rdfs: <${RDFS}> .
@prefix owl: <${OWL}> .
@prefix uni: <${UNI}> .

<${GRAPH}> a owl:Ontology ; owl:imports <${OTHER}> .
uni:Agent a owl:Class ; rdfs:label "Agent"@en .
uni:Person a owl:Class ; rdfs:subClassOf uni:Agent ; rdfs:label "Person"@en .
uni:Student a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Student"@en , "Étudiant"@fr .
uni:GraduateStudent a owl:Class ; rdfs:subClassOf uni:Student ; rdfs:label "Graduate student"@en .
[] a owl:Axiom ; owl:annotatedSource uni:GraduateStudent ; owl:annotatedProperty rdfs:label ;
  owl:annotatedTarget "Graduate student"@en ; rdfs:comment "from the handbook"@en .
uni:Employee a owl:Class ; rdfs:subClassOf uni:Person ; rdfs:label "Employee"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ; rdfs:label "Professor"@en .
[] a owl:Axiom ; owl:annotatedSource uni:GraduateStudent ; owl:annotatedProperty rdfs:subClassOf ;
  owl:annotatedTarget uni:Student ; rdfs:comment "Every graduate student is a student."@en .
`;

async function count(
  request: APIRequestContext,
  target: string,
  pattern: string,
  headers: Record<string, string> = {},
) {
  const r = await request.post(`/${target}/query`, {
    headers: {
      ...headers,
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: `SELECT (COUNT(*) AS ?n) WHERE { ${pattern} }`,
  });
  expect(r.ok(), await r.text()).toBe(true);
  return Number((await r.json()).results.bindings[0].n.value);
}

const inGraph = (p: string) => `GRAPH <${GRAPH}> { ${p} }`;

async function openBulk(page: Page, ds: string, branch: string | null) {
  const p = new URLSearchParams({ ds, graph: GRAPH, tab: 'bulk' });
  if (branch) p.set('branch', branch);
  await page.goto(`/ui/workbench/ontology?${p}`);
  await expect(page.getByRole('group', { name: 'Bulk edit' })).toBeVisible({ timeout: 30_000 });
}

const changes = (page: Page) => page.getByRole('region', { name: 'Changes' });
const preview = (page: Page) => page.getByRole('dialog');
async function commitPreview(page: Page) {
  const d = preview(page);
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit', {
    timeout: 30_000,
  });
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden({ timeout: 30_000 });
}

async function findReplace(page: Page, find: string, replace: string) {
  const form = page.getByRole('form', { name: 'Find and replace' });
  await form.getByRole('textbox', { name: 'Find' }).fill(find);
  await form.getByRole('textbox', { name: 'Replace with' }).fill(replace);
  await form.getByRole('button', { name: 'Find' }).click();
}

async function pasteEntities(page: Page, text: string) {
  await page.getByRole('textbox', { name: 'Paste IRIs or prefixed names' }).fill(text);
  await page.getByRole('button', { name: 'Add pasted' }).click();
}

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  for (const [g, data] of [
    [GRAPH, TURTLE],
    // the imported graph already holds the label the replacement writes, which the
    // edit must still add to the edited graph
    [OTHER, `<${OTHER}> a <${OWL}Ontology> . <${UNI}Student> <${RDFS}label> "learner"@en .`],
  ]) {
    const loaded = await request.post(`/${DS}/data?graph=${encodeURIComponent(g)}`, {
      headers: { 'Content-Type': 'text/turtle' },
      data,
    });
    expect(loaded.ok(), await loaded.text()).toBe(true);
  }
  const b = await request.post(`/$/branches/${DS}`, { data: { name: BRANCH } });
  expect(b.ok(), await b.text()).toBe(true);
});

open.afterAll(async ({ request }) => {
  await request.delete(`/$/datasets/${DS}`);
});

open('find and replace in labels is one commit on the branch', async ({ page, request }) => {
  const target = `${DS}@${BRANCH}`;
  await openBulk(page, DS, BRANCH);
  await findReplace(page, 'student', 'learner');
  const c = changes(page);
  await expect(c).toContainText('Replace "student" with "learner" in 2 annotations', {
    timeout: 30_000,
  });
  await c.getByRole('button', { name: 'Preview the edit' }).click();
  await commitPreview(page);
  await expect
    .poll(() => count(request, target, inGraph(`?s <${RDFS}label> "learner"@en`)))
    .toBe(1);
  expect(await count(request, target, inGraph(`?s <${RDFS}label> "Graduate learner"@en`))).toBe(1);
  // the annotated label keeps its annotation on the new value
  expect(
    await count(
      request,
      target,
      inGraph(
        `?x <${OWL}annotatedTarget> "Graduate learner"@en ; <${RDFS}comment> "from the handbook"@en`,
      ),
    ),
  ).toBe(1);
  // the French label, the other graph and main are untouched
  expect(await count(request, target, inGraph(`?s <${RDFS}label> "Étudiant"@fr`))).toBe(1);
  expect(await count(request, target, `GRAPH <${OTHER}> { ?s <${RDFS}label> "learner"@en }`)).toBe(
    1,
  );
  expect(await count(request, DS, inGraph(`?s <${RDFS}label> "Student"@en`))).toBe(1);
  const log = await request.get(`/$/commits/${DS}?branch=${BRANCH}&limit=1`);
  expect((await log.json()).commits[0].message).toBe(
    'Replace "student" with "learner" in 2 annotations',
  );
});

open('a move drops an annotated parent, and undo restores it', async ({ page, request }) => {
  const target = `${DS}@${BRANCH}`;
  const reified = inGraph(
    `?x <${OWL}annotatedSource> <${UNI}GraduateStudent> ; <${OWL}annotatedProperty> <${RDFS}subClassOf>`,
  );
  const before = await count(request, target, inGraph('?s ?p ?o'));
  expect(await count(request, target, reified)).toBe(1);
  await openBulk(page, DS, BRANCH);
  await page
    .getByRole('group', { name: 'Bulk edit' })
    .getByRole('button', { name: 'Move classes' })
    .click();
  await pasteEntities(page, `<${UNI}GraduateStudent> <${UNI}Professor>`);
  const form = page.getByRole('form', { name: 'Move classes' });
  await form.getByRole('combobox', { name: 'New parent class' }).fill(`<${UNI}Agent>`);
  await page.getByRole('listbox').getByRole('option').first().click();
  await form.getByRole('button', { name: 'List the changes' }).click();
  await expect(changes(page)).toContainText('Move 2 classes under', { timeout: 30_000 });
  await changes(page).getByRole('button', { name: 'Preview the edit' }).click();
  await commitPreview(page);
  await expect
    .poll(() => count(request, target, inGraph(`?c <${RDFS}subClassOf> <${UNI}Agent>`)))
    .toBe(3);
  expect(await count(request, target, reified)).toBe(0);
  expect(await count(request, target, inGraph(`<${UNI}Professor> <${RDFS}subClassOf> ?p`))).toBe(1);

  await page.getByRole('button', { name: /^Undo/ }).click();
  await commitPreview(page);
  await expect.poll(() => count(request, target, reified)).toBe(1);
  expect(await count(request, target, inGraph('?s ?p ?o'))).toBe(before);
});

open(
  'sets an annotation with an undeclared property on listed entities',
  async ({ page, request }) => {
    const target = `${DS}@${BRANCH}`;
    const note = 'http://www.w3.org/2004/02/skos/core#editorialNote';
    await openBulk(page, DS, BRANCH);
    await pasteEntities(page, `<${UNI}Student> <${UNI}Employee>`);
    await page
      .getByRole('group', { name: 'Bulk edit' })
      .getByRole('button', { name: 'Set annotation' })
      .click();
    const form = page.getByRole('form', { name: 'Set an annotation' });
    await form.getByRole('combobox', { name: 'Annotation property' }).fill(`<${note}>`);
    await form.getByRole('textbox', { name: 'Value' }).fill('Reviewed');
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page)).toContainText('on 2 entities', { timeout: 30_000 });
    await changes(page).getByRole('button', { name: 'Preview the edit' }).click();
    await commitPreview(page);
    await expect.poll(() => count(request, target, inGraph(`?s <${note}> "Reviewed"@en`))).toBe(2);
    // the property is declared with the edit, and the subjects are not redeclared
    expect(await count(request, target, inGraph(`<${note}> a <${OWL}AnnotationProperty>`))).toBe(1);
  },
);

open('an edit above the bound is refused', async ({ page, request }) => {
  const target = `${DS}@${BRANCH}`;
  const triples = Array.from(
    { length: 1001 },
    (_, i) => `<${UNI}Extra${i}> <${RDFS}label> "extra ${i}"@en .`,
  ).join('\n');
  const r = await request.post(`/${target}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: `INSERT DATA { GRAPH <${GRAPH}> { ${triples} } }`,
  });
  expect(r.ok(), await r.text()).toBe(true);
  await openBulk(page, DS, BRANCH);
  await findReplace(page, 'extra', 'more');
  await expect(changes(page).getByRole('alert')).toContainText(
    'The edit would change 1,001 annotations, more than the 1,000',
    { timeout: 30_000 },
  );
  await expect(changes(page).getByRole('button', { name: 'Preview the edit' })).toHaveCount(0);
});

open('a protected branch offers no preview and refuses the write', async ({ page, request }) => {
  const p = await request.patch(`/$/branches/${DS}/${BRANCH}`, { data: { protected: true } });
  expect(p.ok(), await p.text()).toBe(true);
  try {
    await openBulk(page, DS, BRANCH);
    await expect(page.getByText('Bulk edits are read-only here.')).toBeVisible();
    await findReplace(page, 'Professor', 'Lecturer');
    await expect(changes(page).locator('tbody tr')).toHaveCount(1, { timeout: 30_000 });
    await expect(changes(page).getByRole('button', { name: 'Preview the edit' })).toHaveCount(0);
    const w = await request.post(`/$/ontology/${DS}/axioms?branch=${BRANCH}`, {
      data: { add: [`AnnotationAssertion(<${RDFS}comment> <${UNI}Professor> "x")`] },
    });
    expect(w.status()).toBe(403);
  } finally {
    await request.patch(`/$/branches/${DS}/${BRANCH}`, { data: { protected: false } });
  }
});

const admin = () => ({ Authorization: `Bearer ${server.token}` });

anonymous.describe('under auth', () => {
  anonymous.describe.configure({ mode: 'serial', timeout: 120_000 });

  anonymous.beforeAll(async ({ request }) => {
    const r = await request.post('/$/datasets', {
      headers: admin(),
      data: { dbName: BULK_AUTH, dbType: 'mem' },
    });
    expect(r.ok(), await r.text()).toBe(true);
    const loaded = await request.post(`/${BULK_AUTH}/data?graph=${encodeURIComponent(GRAPH)}`, {
      headers: { ...admin(), 'Content-Type': 'text/turtle' },
      data: TURTLE,
    });
    expect(loaded.ok(), await loaded.text()).toBe(true);
  });

  anonymous(
    'a reader lists changes without a preview and is refused the write',
    async ({ page, request }) => {
      const login = await page.request.post('/$/auth/login', {
        data: { user: READER, password: PASSWORD },
      });
      expect(login.ok()).toBe(true);
      await openBulk(page, BULK_AUTH, null);
      await expect(page.getByText('Bulk edits are read-only here.')).toBeVisible();
      await findReplace(page, 'student', 'learner');
      await expect(changes(page).locator('tbody tr')).toHaveCount(2, { timeout: 30_000 });
      await expect(changes(page).getByRole('button', { name: 'Preview the edit' })).toHaveCount(0);
      const w = await page.request.post(`/$/ontology/${BULK_AUTH}/axioms`, {
        data: {
          graph: GRAPH,
          add: [`AnnotationAssertion(<${RDFS}comment> <${UNI}Professor> "x")`],
        },
      });
      expect([401, 403]).toContain(w.status());
      expect(await count(request, BULK_AUTH, inGraph(`?s <${RDFS}comment> "x"`), admin())).toBe(0);
    },
  );
});
