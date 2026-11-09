// Bulk edits of the ontology editor (ROADMAP item 626) against the mock's `uni` dataset:
// find and replace in labels, setting and removing an annotation on a list of entities and
// moving classes under a new parent, each one previewed commit that undo takes back whole;
// the refusals of a cycle and of an edit above the bound; a reader and a protected branch,
// which list changes but get no preview; and the phone layout. Each test works on its own
// branch.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';

async function newBranch(request: APIRequestContext, name: string) {
  const made = await request.post('/$/branches/uni', { data: { name } });
  expect(made.status()).toBe(201);
}

async function openBulk(page: Page, branch: string) {
  await page.goto(
    `/ui/workbench/ontology?${new URLSearchParams({ ds: 'uni', branch, tab: 'bulk' })}`,
  );
  await expect(page.getByRole('group', { name: 'Bulk edit' })).toBeVisible({ timeout: 30_000 });
}

/** The functional-style text of the branch's axioms of `type`. */
async function axioms(request: APIRequestContext, branch: string, type: string) {
  const r = await request.get(`/$/ontology/uni@${branch}/axioms?type=${type}&limit=10000`);
  expect(r.ok(), await r.text()).toBe(true);
  return ((await r.json()).axioms as { ofn: string }[]).map((a) => a.ofn);
}

async function messages(request: APIRequestContext, branch: string): Promise<string[]> {
  const r = await request.get(`/$/commits/uni?branch=${branch}&limit=20`);
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).commits.map((c: { message?: string }) => c.message ?? '');
}

const preview = (page: Page) => page.getByRole('dialog');
async function commitPreview(page: Page) {
  const d = preview(page);
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden();
}

const changes = (page: Page) => page.getByRole('region', { name: 'Changes' });
const list = (page: Page) => page.getByRole('list', { name: 'Entities in the list' });

async function pasteEntities(page: Page, text: string) {
  await page.getByRole('textbox', { name: 'Paste IRIs or prefixed names' }).fill(text);
  await page.getByRole('button', { name: 'Add pasted' }).click();
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

test('find and replace in labels is one commit that undo takes back', async ({ page, request }) => {
  const branch = 'bulk-replace';
  await newBranch(request, branch);
  // every write of a bulk edit reads and writes the graph without its imports, as it planned
  const writes: unknown[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().includes('/axioms')) writes.push(r.postDataJSON());
  });
  try {
    await openBulk(page, branch);
    const form = page.getByRole('form', { name: 'Find and replace' });
    await form.getByRole('textbox', { name: 'Find' }).fill('student');
    await form.getByRole('textbox', { name: 'Replace with' }).fill('learner');
    await form.getByRole('combobox', { name: 'Language' }).selectOption('tag');
    await form.getByRole('textbox', { name: 'Tag' }).fill('en');
    await form.getByRole('button', { name: 'Find' }).click();
    const c = changes(page);
    await expect(c).toContainText('Replace "student" with "learner" in 2 annotations');
    await expect(c.locator('tbody tr')).toHaveCount(2);
    await expect(c).toContainText('"Graduate learner"@en');
    // the French label is outside the language filter
    await expect(c).not.toContainText('Étudiant');

    await c.getByRole('button', { name: 'Preview the edit' }).click();
    const d = preview(page);
    await expect(d.locator('.axioms.del li')).toHaveCount(2);
    await expect(d.locator('.axioms.ins li')).toHaveCount(2);
    await commitPreview(page);
    const after = await axioms(request, branch, 'AnnotationAssertion');
    expect(after).toContain(`AnnotationAssertion(rdfs:label uni:Student "learner"@en)`);
    expect(after.some((a) => a.includes('"Student"@en'))).toBe(false);
    expect((await messages(request, branch))[0]).toBe(
      'Replace "student" with "learner" in 2 annotations',
    );
    expect(writes.length).toBeGreaterThan(0);
    for (const w of writes) expect(w).toMatchObject({ imports: false });
    // a new head clears the listed changes, which belonged to the old one
    await expect(c).toHaveCount(0);

    // the whole bulk edit is one entry of the undo stack
    await page.getByRole('button', { name: /^Undo \(1\)$/ }).click();
    await commitPreview(page);
    const back = await axioms(request, branch, 'AnnotationAssertion');
    expect(back).toContain(`AnnotationAssertion(rdfs:label uni:Student "Student"@en)`);
    expect(back.some((a) => a.includes('learner'))).toBe(false);
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a regular expression keeps the annotations of an annotated value', async ({
  page,
  request,
}) => {
  const branch = 'bulk-regex';
  await newBranch(request, branch);
  try {
    // the label of GraduateStudent carries an axiom annotation
    const seeded = await request.post(`/uni@${branch}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA { [] a owl:Axiom ; owl:annotatedSource <${UNI}GraduateStudent> ;
  owl:annotatedProperty rdfs:label ; owl:annotatedTarget "Graduate student"@en ;
  rdfs:comment "from the handbook"@en . }`,
    });
    expect(seeded.ok(), await seeded.text()).toBe(true);
    await openBulk(page, branch);
    const form = page.getByRole('form', { name: 'Find and replace' });
    await form.getByRole('textbox', { name: 'Find' }).fill('^(\\w+) student$');
    await form.getByRole('textbox', { name: 'Replace with' }).fill('$1 learner');
    await form.getByRole('checkbox', { name: 'Regular expression' }).check();
    await form.getByRole('button', { name: 'Find' }).click();
    await expect(changes(page).locator('tbody tr')).toHaveCount(1);
    await expect(changes(page)).toContainText('"Graduate learner"@en');
    await changes(page).getByRole('button', { name: 'Preview the edit' }).click();
    await expect(preview(page).locator('.axioms.ins')).toContainText('[1 annotation]');
    await commitPreview(page);
    const r = await request.get(
      `/$/ontology/uni@${branch}/axioms?type=AnnotationAssertion&about=${encodeURIComponent(`${UNI}GraduateStudent`)}&limit=100`,
    );
    const label = ((await r.json()).axioms as { axiom: Record<string, unknown> }[]).find(
      (a) => (a.axiom.value as { '@value'?: string })?.['@value'] === 'Graduate learner',
    );
    expect(label?.axiom.annotations).toEqual([
      {
        property: 'http://www.w3.org/2000/01/rdf-schema#comment',
        value: { '@value': 'from the handbook', '@language': 'en' },
      },
    ]);
    // a pattern that matches empty text between characters is refused
    await form.getByRole('textbox', { name: 'Find' }).fill('(?=e)');
    await form.getByRole('button', { name: 'Find' }).click();
    await expect(changes(page).getByRole('alert')).toContainText('matches empty text inside');
    // a pattern that is not valid is refused before any read
    await form.getByRole('textbox', { name: 'Find' }).fill('(');
    await form.getByRole('button', { name: 'Find' }).click();
    await expect(changes(page).getByRole('alert')).toContainText('not valid');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('sets and removes an annotation on the entities of the list', async ({ page, request }) => {
  const branch = 'bulk-set';
  await newBranch(request, branch);
  try {
    await openBulk(page, branch);
    await pasteEntities(page, 'uni:Student\nuni:Employee, nonsense');
    await expect(list(page).getByRole('listitem')).toHaveCount(2);
    await expect(page.getByText('Not an IRI or a prefixed name: nonsense.')).toBeVisible();

    await page
      .getByRole('group', { name: 'Bulk edit' })
      .getByRole('button', { name: 'Set annotation' })
      .click();
    const form = page.getByRole('form', { name: 'Set an annotation' });
    await form.getByRole('textbox', { name: 'Annotation property' }).fill('skos:editorialNote');
    await form.getByRole('textbox', { name: 'Value' }).fill('Reviewed');
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page)).toContainText('Set skos:editorialNote on 2 entities');
    await changes(page).getByRole('button', { name: 'Preview the edit' }).click();
    await commitPreview(page);
    const set = await axioms(request, branch, 'AnnotationAssertion');
    for (const e of ['Student', 'Employee'])
      expect(set).toContain(`AnnotationAssertion(skos:editorialNote uni:${e} "Reviewed"@en)`);

    // a second run finds nothing left to set
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page).getByRole('alert')).toContainText('would change nothing');
    await expect(changes(page)).toContainText('2 entities already hold the value.');

    await page
      .getByRole('group', { name: 'Bulk edit' })
      .getByRole('button', { name: 'Remove annotation' })
      .click();
    const rm = page.getByRole('form', { name: 'Remove an annotation' });
    await rm.getByRole('textbox', { name: 'Annotation property' }).fill('skos:editorialNote');
    await rm.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page)).toContainText('Remove skos:editorialNote from 2 entities');
    await changes(page).getByRole('button', { name: 'Preview the edit' }).click();
    await commitPreview(page);
    const removed = await axioms(request, branch, 'AnnotationAssertion');
    expect(removed.some((a) => a.includes('"Reviewed"'))).toBe(false);
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('moves classes under a new parent and refuses a cycle', async ({ page, request }) => {
  const branch = 'bulk-move';
  await newBranch(request, branch);
  try {
    await openBulk(page, branch);
    await page
      .getByRole('group', { name: 'Bulk edit' })
      .getByRole('button', { name: 'Move classes' })
      .click();
    // Person and its subtree, then a parent below Person: a cycle
    await page.getByRole('combobox', { name: 'Add a class and its subclasses' }).fill('uni:Person');
    await page.getByRole('option').first().click();
    await page.getByRole('button', { name: 'Add subtree' }).click();
    await expect(list(page).getByRole('listitem')).toHaveCount(5);
    const form = page.getByRole('form', { name: 'Move classes' });
    await form.getByRole('combobox', { name: 'New parent class' }).fill('uni:Course');
    await page.getByRole('option').first().click();
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page)).toContainText('Move 5 classes under Course');
    await page.getByRole('button', { name: 'Clear the list' }).click();
    await pasteEntities(page, 'uni:Person');
    await form.getByRole('combobox', { name: 'New parent class' }).fill('uni:Student');
    await page.getByRole('option').first().click();
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page).getByRole('alert')).toContainText(
      'Student is below Person, so the move would make a cycle.',
    );
    await expect(changes(page).getByRole('button', { name: 'Preview the edit' })).toHaveCount(0);

    // two classes, one with an annotated SubClassOf, move under Agent
    await page.getByRole('button', { name: 'Clear the list' }).click();
    await pasteEntities(page, 'uni:GraduateStudent uni:Professor');
    await form.getByRole('combobox', { name: 'New parent class' }).fill('uni:Agent');
    await page.getByRole('option').first().click();
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page)).toContainText('Move 2 classes under Agent');
    await changes(page).getByRole('button', { name: 'Preview the edit' }).click();
    const d = preview(page);
    await expect(d.locator('.axioms.del')).toContainText('[1 annotation]');
    await expect(d.locator('.axioms.ins li')).toHaveCount(2);
    await commitPreview(page);
    const sub = await axioms(request, branch, 'SubClassOf');
    expect(sub).toContain('SubClassOf(uni:GraduateStudent uni:Agent)');
    expect(sub).toContain('SubClassOf(uni:Professor uni:Agent)');
    expect(sub.some((a) => a.includes('SubClassOf(uni:Professor uni:Employee)'))).toBe(false);
    // the restriction stays
    expect(sub.some((a) => a.startsWith('SubClassOf(uni:Professor ObjectSomeValuesFrom'))).toBe(
      true,
    );
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('switching operation while the graph is read drops the pending changes', async ({
  page,
  request,
}) => {
  const branch = 'bulk-race';
  await newBranch(request, branch);
  try {
    let release!: () => void;
    const held = new Promise<void>((done) => (release = done));
    await page.route('**/axioms?**', async (route) => {
      if (route.request().method() === 'GET') await held;
      await route.continue().catch(() => {});
    });
    await openBulk(page, branch);
    await pasteEntities(page, 'uni:Student');
    const ops = page.getByRole('group', { name: 'Bulk edit' });
    await ops.getByRole('button', { name: 'Set annotation' }).click();
    const form = page.getByRole('form', { name: 'Set an annotation' });
    await form.getByRole('textbox', { name: 'Value' }).fill('Reviewed');
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(form.getByRole('button', { name: 'Reading…' })).toBeVisible();
    await ops.getByRole('button', { name: 'Move classes' }).click();
    release();
    await page.waitForTimeout(1000);
    await expect(changes(page)).toHaveCount(0);
    await expect(
      page
        .getByRole('form', { name: 'Move classes' })
        .getByRole('button', { name: 'List the changes' }),
    ).toBeEnabled();
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('an edit above the bound is refused with the reason', async ({ page, request }) => {
  const branch = 'bulk-bound';
  await newBranch(request, branch);
  try {
    // 1,001 labels that one find matches
    const triples = Array.from(
      { length: 1001 },
      (_, i) => `<${UNI}Extra${i}> <http://www.w3.org/2000/01/rdf-schema#label> "extra ${i}"@en .`,
    ).join('\n');
    const r = await request.post(`/uni@${branch}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `INSERT DATA { ${triples} }`,
    });
    expect(r.ok(), await r.text()).toBe(true);
    await openBulk(page, branch);
    const form = page.getByRole('form', { name: 'Find and replace' });
    await form.getByRole('textbox', { name: 'Find' }).fill('extra');
    await form.getByRole('textbox', { name: 'Replace with' }).fill('more');
    await form.getByRole('button', { name: 'Find' }).click();
    await expect(changes(page).getByRole('alert')).toContainText(
      'The edit would change 1,001 annotations, more than the 1,000 that one bulk edit may change.',
    );
    await expect(changes(page).getByRole('button', { name: 'Preview the edit' })).toHaveCount(0);
    // the list refuses more entities than one edit may change
    const many = Array.from({ length: 1001 }, (_, i) => `uni:Extra${i}`).join('\n');
    await pasteEntities(page, many);
    await expect(
      page.getByRole('alert').filter({ hasText: 'The list holds at most 1,000' }),
    ).toBeVisible();
    await expect(list(page)).toHaveCount(0);
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a reader lists changes without a preview, and the tab fits a phone', async ({
  page,
  request,
}) => {
  const branch = 'bulk-reader';
  await newBranch(request, branch);
  try {
    await page.route('**/$/whoami', (route) => route.fulfill({ json: whoami('reader', 'read') }));
    await page.setViewportSize({ width: 360, height: 760 });
    await openBulk(page, branch);
    await expect(page.getByText('Bulk edits are read-only here.')).toBeVisible();
    const form = page.getByRole('form', { name: 'Find and replace' });
    await form.getByRole('textbox', { name: 'Find' }).fill('Professor');
    await form.getByRole('textbox', { name: 'Replace with' }).fill('Lecturer');
    await form.getByRole('button', { name: 'Find' }).click();
    await expect(changes(page).locator('tbody tr')).toHaveCount(1);
    await expect(changes(page).getByRole('button', { name: 'Preview the edit' })).toHaveCount(0);
    await expectNoSidewaysScroll(page, 'the bulk edits tab at phone width');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a protected branch offers no preview and refuses the edit', async ({ page, request }) => {
  const branch = 'bulk-protected';
  await newBranch(request, branch);
  try {
    const p = await request.patch(`/$/branches/uni/${branch}`, { data: { protected: true } });
    expect(p.ok(), await p.text()).toBe(true);
    await openBulk(page, branch);
    await expect(page.getByText('Bulk edits are read-only here.')).toBeVisible();
    await pasteEntities(page, 'uni:Student');
    await page
      .getByRole('group', { name: 'Bulk edit' })
      .getByRole('button', { name: 'Set annotation' })
      .click();
    const form = page.getByRole('form', { name: 'Set an annotation' });
    await form.getByRole('textbox', { name: 'Value' }).fill('Reviewed');
    await form.getByRole('button', { name: 'List the changes' }).click();
    await expect(changes(page)).toContainText('Set rdfs:comment on 1 entity');
    await expect(changes(page).getByRole('button', { name: 'Preview the edit' })).toHaveCount(0);
    // the edit the list describes is a write the protected branch refuses
    const r = await request.post(`/$/ontology/uni@${branch}/axioms`, {
      data: {
        add: [
          {
            type: 'AnnotationAssertion',
            property: 'http://www.w3.org/2000/01/rdf-schema#comment',
            subject: `${UNI}Student`,
            value: { '@value': 'Reviewed', '@language': 'en' },
          },
        ],
        declare: false,
      },
    });
    expect(r.status()).toBe(403);
  } finally {
    await request.patch(`/$/branches/uni/${branch}`, { data: { protected: false } });
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});
