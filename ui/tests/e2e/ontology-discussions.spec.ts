// Discussions on entities (ROADMAP item 618) against a real server. On the server without
// auth, an ontology in a named graph of a persistent dataset gets a thread on Professor and
// one on an axiom, a reply, a resolve and a reopen on a branch. Each is a commit, the
// annotations land in the graph's discussion graph as Web Annotation RDF, the frame leaves
// them out and the branch's export carries them. A protected branch shows the threads with
// no composer and refuses a comment. On the server with auth, the writer's comments name
// them, and a reader sees the threads with no composer and is refused a comment.

import type { APIRequestContext, Page } from '@playwright/test';
import { DISCUSSIONS_AUTH, PASSWORD, READER } from './data';
import { anonymous, expect, open, server, test } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'onto-discussions-e2e';
const BRANCH = 'talk';
const GRAPH = 'http://example.org/uni';
const DISCUSSIONS = `${GRAPH}/discussions`;
const UNI = 'http://example.org/uni#';
const OA = 'http://www.w3.org/ns/oa#';

const TURTLE = `
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix uni: <${UNI}> .

<${GRAPH}> a owl:Ontology .
uni:Employee a owl:Class ; rdfs:label "Employee"@en .
uni:Course a owl:Class ; rdfs:label "Course"@en .
uni:Professor a owl:Class ; rdfs:subClassOf uni:Employee ; rdfs:label "Professor"@en .
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

async function openDiscussion(page: Page, ds: string, branch: string | null) {
  const p = new URLSearchParams({ ds, graph: GRAPH, iri: `${UNI}Professor`, view: 'discussion' });
  if (branch) p.set('branch', branch);
  await page.goto(`/ui/workbench/ontology?${p}`);
  await expect(page.getByRole('button', { name: 'Discussion', exact: true })).toHaveAttribute(
    'aria-pressed',
    'true',
    { timeout: 30_000 },
  );
}

const threadsOf = (page: Page) => page.getByRole('list', { name: 'Threads on the entity' });

async function comment(page: Page, text: string) {
  const compose = page.getByRole('form', { name: 'New thread' });
  await compose.getByRole('textbox', { name: 'Comment' }).fill(text);
  await compose.getByRole('button', { name: 'Comment' }).click();
  await expect(threadsOf(page)).toContainText(text);
}

const COMMENTS = `GRAPH <${DISCUSSIONS}> { ?a <${OA}motivatedBy> <${OA}commenting> }`;
const UPDATE = (graph: string) =>
  `INSERT DATA { GRAPH <${graph}> { <urn:uuid:refused> a <${OA}Annotation> } }`;

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data?graph=${encodeURIComponent(GRAPH)}`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: TURTLE,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
  const b = await request.post(`/$/branches/${DS}`, { data: { name: BRANCH } });
  expect(b.ok(), await b.text()).toBe(true);
});

open(
  'threads, a reply, a resolve and a reopen are commits in the discussion graph',
  async ({ page, request }) => {
    await openDiscussion(page, DS, BRANCH);
    await comment(page, 'Should @ana review this class?');
    const compose = page.getByRole('form', { name: 'New thread' });
    await compose.getByRole('combobox', { name: 'About' }).selectOption({ index: 1 });
    await comment(page, 'Is Employee the right parent?');
    const threads = threadsOf(page);
    await expect(threads.locator('.axiom')).toContainText('SubClassOf');
    await expect(threads.locator('.mention')).toHaveText('@ana');

    const first = threads.getByRole('listitem').filter({ hasText: '@ana' });
    await first.getByRole('textbox', { name: /Reply/ }).fill('Ana agrees.');
    await first.getByRole('button', { name: 'Reply' }).click();
    await expect(first.getByRole('list', { name: 'Replies' })).toContainText('Ana agrees.');
    await first.getByRole('button', { name: 'Resolve' }).click();
    await expect(threads.getByRole('article')).toHaveCount(1);
    await page.getByRole('button', { name: /^Resolved 1$/ }).click();
    await threads.getByRole('button', { name: 'Reopen' }).click();
    await expect(page.getByText('No resolved thread.')).toBeVisible();

    // the annotations sit in the discussion graph of the branch only, never as rdfs:comment
    expect(await count(request, `${DS}@${BRANCH}`, COMMENTS)).toBe(2);
    expect(await count(request, DS, COMMENTS)).toBe(0);
    expect(
      await count(
        request,
        `${DS}@${BRANCH}`,
        `GRAPH ?g { ?s <http://www.w3.org/2000/01/rdf-schema#comment> ?o }`,
      ),
    ).toBe(0);
    expect(
      await count(
        request,
        `${DS}@${BRANCH}`,
        `GRAPH <${DISCUSSIONS}> { ?t <urn:x-sparkles:discussion#resolved> false }`,
      ),
    ).toBe(2);
    const log = await request.get(`/$/commits/${DS}?branch=${BRANCH}&limit=10`);
    expect(log.ok(), await log.text()).toBe(true);
    const messages = (await log.json()).commits.map((c: { message?: string }) => c.message);
    expect(messages.slice(0, 5)).toEqual([
      'Reopen a discussion on Professor',
      'Resolve a discussion on Professor',
      'Reply on Professor',
      'Comment on Professor',
      'Comment on Professor',
    ]);
    // an export of the branch carries the discussion graph like any other
    const dump = await request.get(`/${DS}@${BRANCH}/data`, {
      headers: { Accept: 'application/n-quads' },
    });
    expect(dump.ok(), await dump.text()).toBe(true);
    expect(await dump.text()).toContain(`<${DISCUSSIONS}>`);

    // the frame and the graph list leave the discussion graph out
    await page.getByRole('button', { name: 'Frame', exact: true }).click();
    await expect(page.getByText('Is Employee the right parent?')).toHaveCount(0);
    await expect(page.locator('option', { hasText: DISCUSSIONS })).toHaveCount(0);

    // the ontology's tab lists both threads and opens their entity
    await page.goto(
      `/ui/workbench/ontology?${new URLSearchParams({ ds: DS, branch: BRANCH, graph: GRAPH, tab: 'discussions' })}`,
    );
    const all = page.getByRole('list', { name: 'Threads in the ontology' });
    await expect(all.getByRole('article')).toHaveCount(2, { timeout: 30_000 });
    await all.getByRole('button', { name: 'Professor' }).first().click();
    await expect(page).toHaveURL(/view=discussion/);
  },
);

open(
  'a protected branch shows its threads with no composer and refuses a comment',
  async ({ page, request }) => {
    const p = await request.patch(`/$/branches/${DS}/${BRANCH}`, { data: { protected: true } });
    expect(p.ok(), await p.text()).toBe(true);
    try {
      await openDiscussion(page, DS, BRANCH);
      await expect(threadsOf(page)).toContainText('Should @ana review this class?');
      await expect(page.getByRole('form', { name: 'New thread' })).toHaveCount(0);
      await expect(threadsOf(page).getByRole('button', { name: 'Resolve' })).toHaveCount(0);
      const r = await request.post(`/${DS}@${BRANCH}/update`, {
        headers: { 'Content-Type': 'application/sparql-update' },
        data: UPDATE(DISCUSSIONS),
      });
      expect(r.status()).toBe(403);
    } finally {
      await request.patch(`/$/branches/${DS}/${BRANCH}`, { data: { protected: false } });
    }
  },
);

const admin = () => ({ Authorization: `Bearer ${server.token}` });

anonymous.describe('under auth', () => {
  anonymous.describe.configure({ mode: 'serial', timeout: 120_000 });

  anonymous.beforeAll(async ({ request }) => {
    const r = await request.post('/$/datasets', {
      headers: admin(),
      data: { dbName: DISCUSSIONS_AUTH, dbType: 'mem' },
    });
    expect(r.ok(), await r.text()).toBe(true);
    const loaded = await request.post(
      `/${DISCUSSIONS_AUTH}/data?graph=${encodeURIComponent(GRAPH)}`,
      { headers: { ...admin(), 'Content-Type': 'text/turtle' }, data: TURTLE },
    );
    expect(loaded.ok(), await loaded.text()).toBe(true);
  });

  test("a writer's comment names them", async ({ page }) => {
    await openDiscussion(page, DISCUSSIONS_AUTH, null);
    await comment(page, 'Written by the signed-in writer.');
    await expect(threadsOf(page).getByRole('article', { name: /^Thread by alice$/ })).toBeVisible();
  });

  anonymous(
    'a reader sees the threads with no composer and is refused a comment',
    async ({ page }) => {
      const login = await page.request.post('/$/auth/login', {
        data: { user: READER, password: PASSWORD },
      });
      expect(login.ok()).toBe(true);
      await openDiscussion(page, DISCUSSIONS_AUTH, null);
      await expect(threadsOf(page)).toContainText('Written by the signed-in writer.');
      await expect(page.getByRole('form', { name: 'New thread' })).toHaveCount(0);
      await expect(threadsOf(page).getByRole('button', { name: 'Resolve' })).toHaveCount(0);
      const r = await page.request.post(`/${DISCUSSIONS_AUTH}/update`, {
        headers: { 'Content-Type': 'application/sparql-update' },
        data: UPDATE(DISCUSSIONS),
      });
      expect([401, 403]).toContain(r.status());
    },
  );
});
