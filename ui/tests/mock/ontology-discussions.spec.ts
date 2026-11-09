// Discussions on entities (ROADMAP item 618) against the mock's `uni` dataset: a thread on
// an entity and one on an axiom, with a mention, a reply, a resolve and a reopen, each one
// commit of the branch in the discussion graph; the ontology's Discussions tab; a reader and
// a protected branch, which see the threads and get no composer; and the layout at phone
// width. Each test works on its own branch.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';
const DISCUSSIONS = 'urn:x-sparkles:discussions:default';
const OA = 'http://www.w3.org/ns/oa#';

/** A thread on Professor by `bo`, written as the editor writes one. */
async function seedThread(request: APIRequestContext, branch: string, text: string) {
  const r = await request.post(`/uni@${branch}/update`, {
    headers: {
      'Content-Type': 'application/sparql-update',
      'Sparkles-Commit-Message': 'Comment on Professor',
    },
    data: `PREFIX oa: <${OA}>
PREFIX dcterms: <http://purl.org/dc/terms/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA { GRAPH <${DISCUSSIONS}> {
  <urn:uuid:seed> a oa:Annotation ; oa:motivatedBy oa:commenting ; oa:hasBody <urn:uuid:seed#body> ;
    oa:hasTarget <${UNI}Professor> ; dcterms:creator <urn:x-sparkles:user:bo> ;
    dcterms:created "2026-11-01T10:00:00Z"^^xsd:dateTime ; <urn:x-sparkles:discussion#resolved> false .
  <urn:uuid:seed#body> a oa:TextualBody ; rdf:value ${JSON.stringify(text)} .
} }`,
  });
  expect(r.ok(), await r.text()).toBe(true);
}

async function newBranch(request: APIRequestContext, name: string) {
  const made = await request.post('/$/branches/uni', { data: { name } });
  expect(made.status()).toBe(201);
}

async function openDiscussion(page: Page, branch: string, iri = `${UNI}Professor`) {
  await page.goto(
    `/ui/workbench/ontology?${new URLSearchParams({ ds: 'uni', branch, iri, view: 'discussion' })}`,
  );
  await expect(page.getByRole('button', { name: 'Discussion', exact: true })).toHaveAttribute(
    'aria-pressed',
    'true',
    { timeout: 30_000 },
  );
}

const threadsOf = (page: Page) => page.getByRole('list', { name: 'Threads on the entity' });

/** The commit messages of the branch, newest first. */
async function messages(request: APIRequestContext, branch: string): Promise<string[]> {
  const r = await request.get(`/$/commits/uni?branch=${branch}&limit=20`);
  expect(r.ok(), await r.text()).toBe(true);
  const j = await r.json();
  return j.commits.map((c: { message?: string }) => c.message ?? '');
}

const whoami = (name: string, level: string) => ({
  authEnabled: true,
  principal: { kind: 'token', name },
  method: 'bearer',
  server: [],
  datasets: { uni: level },
  canMintTokens: false,
  logout: false,
});

test('starts a thread with a mention, replies, resolves and reopens it, each a commit', async ({
  page,
  request,
}) => {
  const branch = 'disc-write';
  await newBranch(request, branch);
  try {
    await openDiscussion(page, branch);
    await expect(page.getByText('No open thread on this entity.')).toBeVisible();
    const compose = page.getByRole('form', { name: 'New thread' });
    await compose.getByRole('textbox', { name: 'Comment' }).fill('Should @ana review this?');
    await compose.getByRole('button', { name: 'Comment' }).click();
    const threads = threadsOf(page);
    await expect(threads.getByRole('listitem')).toHaveCount(1);
    await expect(threads.locator('.mention')).toHaveText('@ana');
    await expect(threads).toContainText('local');

    // a thread on one of the entity's axioms shows that axiom
    const about = compose.getByRole('combobox', { name: 'About' });
    const axiom = (await about.locator('option').nth(1).textContent())!.trim();
    await about.selectOption({ index: 1 });
    await compose.getByRole('textbox', { name: 'Comment' }).fill('Too broad.');
    await compose.getByRole('button', { name: 'Comment' }).click();
    await expect(threads.getByRole('article')).toHaveCount(2);
    await expect(threads.locator('.axiom')).toHaveText(axiom);

    const first = threads.getByRole('listitem').filter({ hasText: '@ana' });
    await first.getByRole('textbox', { name: /Reply/ }).fill('Agreed.');
    await first.getByRole('button', { name: 'Reply' }).click();
    await expect(first.getByRole('list', { name: 'Replies' })).toContainText('Agreed.');

    await first.getByRole('button', { name: 'Resolve' }).click();
    await expect(threads.getByRole('article')).toHaveCount(1);
    await expect(page.getByRole('button', { name: /^Resolved 1$/ })).toBeVisible();
    await page.getByRole('button', { name: /^Resolved 1$/ }).click();
    await expect(threads).toContainText('Resolved by local');
    await threads.getByRole('button', { name: 'Reopen' }).click();
    await expect(page.getByText('No resolved thread.')).toBeVisible();
    await page.getByRole('button', { name: /^Open 2$/ }).click();
    await expect(threads.getByRole('article')).toHaveCount(2);

    // every comment, reply, resolve and reopen is an ordinary commit of the branch
    expect((await messages(request, branch)).slice(0, 5)).toEqual([
      'Reopen a discussion on Professor',
      'Resolve a discussion on Professor',
      'Reply on Professor',
      'Comment on Professor',
      'Comment on Professor',
    ]);
    // the frame shows the ontology's axioms and none of the discussion graph
    await page.getByRole('button', { name: 'Frame', exact: true }).click();
    await expect(page.locator('.panel-body').last()).not.toContainText('urn:uuid');
    await expect(page.locator('.panel-body').last()).not.toContainText('Too broad');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test("another entity starts with an empty composer, not the last entity's draft", async ({
  page,
  request,
}) => {
  const branch = 'disc-scope';
  await newBranch(request, branch);
  try {
    await openDiscussion(page, branch);
    const compose = page.getByRole('form', { name: 'New thread' });
    await compose.getByRole('textbox', { name: 'Comment' }).fill('About Professor only.');
    await compose.getByRole('combobox', { name: 'About' }).selectOption({ index: 1 });
    // the breadcrumb keeps the Discussion view while it moves to Employee
    await page.locator('.crumbs').getByRole('button', { name: 'Employee', exact: true }).click();
    await expect(page).toHaveURL(/Employee/);
    await expect(compose.getByRole('textbox', { name: 'Comment' })).toHaveValue('');
    const about = compose.getByRole('combobox', { name: 'About' });
    if (await about.count()) await expect(about).toHaveValue('');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a write in flight locks the composer and keeps a later draft for another entity', async ({
  page,
  request,
}) => {
  const branch = 'disc-flight';
  await newBranch(request, branch);
  try {
    let release!: () => void;
    const held = new Promise<void>((done) => (release = done));
    await page.route(`**/uni@${branch}/update**`, async (route) => {
      await held;
      await route.continue().catch(() => {});
    });
    await openDiscussion(page, branch);
    const compose = page.getByRole('form', { name: 'New thread' });
    const text = compose.getByRole('textbox', { name: 'Comment' });
    await text.fill('Sent from Professor.');
    await compose.getByRole('button', { name: 'Comment' }).click();
    await expect(text).toBeDisabled();
    await page.locator('.crumbs').getByRole('button', { name: 'Employee', exact: true }).click();
    await expect(page).toHaveURL(/Employee/);
    await text.fill('A draft for Employee.');
    release();
    // the commit lands, and Professor's completion leaves Employee's draft alone
    await expect
      .poll(async () => (await messages(request, branch))[0], { timeout: 15_000 })
      .toBe('Comment on Professor');
    await page.waitForTimeout(500);
    await expect(text).toHaveValue('A draft for Employee.');
    await expect(text).toBeEnabled();
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test("the Discussions tab lists the ontology's threads and opens their entity", async ({
  page,
  request,
}) => {
  const branch = 'disc-list';
  await newBranch(request, branch);
  try {
    await seedThread(request, branch, 'Rename to Lecturer?');
    await page.goto(
      `/ui/workbench/ontology?${new URLSearchParams({ ds: 'uni', branch, tab: 'discussions' })}`,
    );
    const list = page.getByRole('list', { name: 'Threads in the ontology' });
    await expect(list).toContainText('Rename to Lecturer?', { timeout: 30_000 });
    await expect(list).toContainText('bo');
    await list.getByRole('button', { name: 'Professor' }).click();
    await expect(page).toHaveURL(/view=discussion/);
    await expect(threadsOf(page)).toContainText('Rename to Lecturer?');
    // the discussion graph is not offered as an ontology graph
    await expect(page.locator('option', { hasText: DISCUSSIONS })).toHaveCount(0);
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a reader sees the threads without a composer, and they fit a phone', async ({
  page,
  request,
}) => {
  const branch = 'disc-reader';
  await newBranch(request, branch);
  try {
    await seedThread(request, branch, 'A question for @ana about this class.');
    await page.route('**/$/whoami', (route) => route.fulfill({ json: whoami('reader', 'read') }));
    await page.setViewportSize({ width: 360, height: 760 });
    await openDiscussion(page, branch);
    await expect(threadsOf(page)).toContainText('A question for @ana');
    await expect(page.getByRole('form', { name: 'New thread' })).toHaveCount(0);
    await expect(threadsOf(page).getByRole('button', { name: 'Resolve' })).toHaveCount(0);
    await expect(threadsOf(page).getByRole('textbox')).toHaveCount(0);
    await expectNoSidewaysScroll(page, 'the discussion view at phone width');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a protected branch shows its threads, offers no composer and refuses a comment', async ({
  page,
  request,
}) => {
  const branch = 'disc-protected';
  await newBranch(request, branch);
  try {
    await seedThread(request, branch, 'Written before the branch was protected.');
    const p = await request.patch(`/$/branches/uni/${branch}`, { data: { protected: true } });
    expect(p.ok(), await p.text()).toBe(true);
    await openDiscussion(page, branch);
    await expect(threadsOf(page)).toContainText('Written before the branch was protected.');
    await expect(page.getByRole('form', { name: 'New thread' })).toHaveCount(0);
    await expect(threadsOf(page).getByRole('button', { name: 'Resolve' })).toHaveCount(0);
    // a comment is a write like any other, which the protected branch refuses
    const r = await request.post(`/uni@${branch}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `INSERT DATA { GRAPH <${DISCUSSIONS}> { <urn:uuid:x> a <${OA}Annotation> } }`,
    });
    expect(r.status()).toBe(403);
  } finally {
    await request.patch(`/$/branches/uni/${branch}`, { data: { protected: false } });
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});
