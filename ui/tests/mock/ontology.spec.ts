// The ontology editor against the mock's `uni` dataset (mock/ontology.mjs): the entity
// trees, the frame, the usage view, the header and the inferred class tree, then edits on
// a session branch, each previewed and committed, with undo and redo, annotations with
// language tags and on axioms, rename and delete, and the refresh after another writer's
// commit. The tests share the mock's state and run in order.

import { expect, test, type Page } from '@playwright/test';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';
const item = (page: Page, name: string) =>
  page.getByRole('treeitem', { name: new RegExp(`^${name}`) }).first();
const node = (page: Page, name: string) =>
  page
    .locator('.node')
    .filter({ has: page.getByRole('button', { name, exact: true }) })
    .first();
const pick = async (page: Page, tree: string, name: string) =>
  page.getByRole('tree', { name: tree }).getByRole('button', { name, exact: true }).first().click();
const expand = async (page: Page, name: string) =>
  page
    .getByRole('button', { name: `Expand ${name}` })
    .first()
    .click();
const preview = (page: Page) => page.getByRole('dialog');

async function commitPreview(page: Page) {
  const d = preview(page);
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden();
}

test('the trees, the frame, the usage and the header', async ({ page }) => {
  await page.goto('/ui/workbench/ontology?ds=uni');
  const classes = page.getByRole('tree', { name: 'Classes (asserted)' });
  await expect(classes.getByRole('button', { name: 'Agent', exact: true })).toBeVisible({
    timeout: 30_000,
  });
  await expand(page, 'Agent');
  await expand(page, 'Person');
  await expand(page, 'Employee');
  await pick(page, 'Classes (asserted)', 'Professor');
  await expect(page).toHaveURL(/iri=http%3A%2F%2Fexample.org%2Funi%23Professor/);
  const frame = page.locator('.frame');
  const sub = frame.locator('[data-section="sub"]');
  await expect(sub).toContainText('Employee');
  await expect(sub).toContainText('teaches some Course');
  await expect(frame).toContainText('Professeur');
  // the usage view lists every axiom that mentions the class
  await page.getByRole('button', { name: /^Usage/ }).click();
  await expect(page.getByRole('region', { name: 'ObjectPropertyRange' })).toContainText(
    'uni:advisor',
  );
  await page.getByRole('button', { name: 'Frame', exact: true }).click();

  // the property trees
  await page.getByRole('combobox', { name: 'Entity kind' }).selectOption('ObjectProperty');
  await expand(page, 'member of');
  await expand(page, 'works for');
  await pick(page, 'Object properties (asserted)', 'head of');
  await expect(page.getByRole('checkbox', { name: 'Functional', exact: true })).toBeChecked();
  await expect(frame.locator('[data-section="superProperties"]')).toContainText('works for');
  await page.getByRole('combobox', { name: 'Entity kind' }).selectOption('DataProperty');
  await pick(page, 'Data properties (asserted)', 'credits');
  await expect(frame.locator('[data-section="ranges"]')).toContainText('xsd:integer');
  await page.getByRole('combobox', { name: 'Entity kind' }).selectOption('NamedIndividual');
  await expect(node(page, 'Ada Lovelace')).toContainText('Professor');
  await pick(page, 'Individuals (asserted)', 'Ada Lovelace');
  await expect(frame.locator('[data-section="objectAssertions"]')).toContainText('Logic 101');

  // the header
  await page.getByRole('tab', { name: 'Header' }).click();
  await expect(page.getByRole('heading', { name: 'Ontology header' })).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'Version IRI' })).toHaveValue(
    'http://example.org/uni/1.2',
  );
  await expect(page.getByRole('list', { name: 'Annotations of the ontology' })).toContainText(
    'Ontologie universitaire',
  );
});

test('the inferred class tree sits beside the asserted one', async ({ page }) => {
  await page.goto('/ui/workbench/ontology?ds=uni&inferred=1');
  const inferred = page.getByRole('tree', { name: 'Classes (inferred)' });
  await expect(inferred).toBeVisible({ timeout: 30_000 });
  // the two trees share the pane in equal columns, side by side
  const widths = await page
    .locator('.tree-col')
    .evaluateAll((cols) => cols.map((c) => c.getBoundingClientRect()));
  expect(widths).toHaveLength(2);
  expect(Math.abs(widths[0].width - widths[1].width)).toBeLessThan(2);
  expect(widths[1].left).toBeGreaterThan(widths[0].left + 100);
  // Teacher is defined as a person who teaches a course, so Professor falls under it
  await inferred.getByRole('button', { name: 'Expand Teacher' }).click();
  await expect(inferred.getByRole('button', { name: 'Professor', exact: true })).toBeVisible();
  await expect(
    page
      .getByRole('tree', { name: 'Classes (asserted)' })
      .getByRole('button', { name: 'Expand Teacher' }),
  ).toHaveCount(0);
});

test('a filtered tree is read again when the branch changes', async ({ page, request }) => {
  const made = await request.post('/$/branches/uni', { data: { name: 'dr-filter' } });
  expect(made.status()).toBe(201);
  try {
    const up = await request.post('/uni@dr-filter/update', {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA { <http://example.org/uni#Zebra> a owl:Class ; rdfs:label "Zebra"@en }`,
    });
    expect(up.ok()).toBe(true);
    await page.goto('/ui/workbench/ontology?ds=uni');
    const tree = page.getByRole('tree', { name: 'Classes (asserted)' });
    await expect(tree.getByRole('button', { name: 'Agent', exact: true })).toBeVisible({
      timeout: 30_000,
    });
    await page.getByRole('searchbox', { name: /^Filter/ }).fill('Zebra');
    await expect(page.getByText('Nothing matches')).toBeVisible({ timeout: 10_000 });
    // the same filter on the branch that has the class finds it
    await page.getByRole('combobox', { name: 'Branch', exact: true }).selectOption('dr-filter');
    await expect(tree.getByRole('treeitem', { name: /Zebra/ })).toBeVisible({ timeout: 10_000 });
  } finally {
    await request.delete('/$/branches/uni/dr-filter?force=true');
  }
});

test('the history of the default graph leaves out what a commit changed in a named graph', async ({
  page,
  request,
}) => {
  const made = await request.post('/$/branches/uni', { data: { name: 'dr-hist' } });
  expect(made.status()).toBe(201);
  try {
    const up = await request.post('/uni@dr-hist/update', {
      headers: {
        'Content-Type': 'application/sparql-update',
        'Sparkles-Commit-Message': 'Add to two graphs',
      },
      data: `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA {
  <http://example.org/uni#Newt> rdfs:comment "in the default graph" .
  GRAPH <http://example.org/graphs/other> { <http://example.org/uni#Newt> rdfs:comment "in a named graph" }
}`,
    });
    expect(up.ok()).toBe(true);
    await page.goto('/ui/workbench/ontology?ds=uni&branch=dr-hist&tab=history');
    const commits = page.getByRole('list', { name: 'Changes' });
    await commits.getByRole('button', { name: /Add to two graphs/ }).click();
    const changes = page.locator('.changeset').first();
    await expect(changes).toContainText('in the default graph', { timeout: 10_000 });
    await expect(changes).not.toContainText('in a named graph');
    // a change set that reaches a named graph says which graph each change is in
    await page.goto(
      '/ui/workbench/ontology?ds=uni&branch=dr-hist&tab=history&graph=http%3A%2F%2Fexample.org%2Fgraphs%2Fother',
    );
    await page
      .getByRole('list', { name: 'Changes' })
      .getByRole('button', { name: /Add to two graphs/ })
      .click();
    const named = page.locator('.changeset').first();
    await expect(named).toContainText('in a named graph', { timeout: 10_000 });
    await expect(named).toContainText('in http://example.org/graphs/other');
    await expect(named).not.toContainText('in the default graph');
    // the viewer's choice of full IRIs reaches the names a change set falls back to
    await expect(named).toContainText('rdfs:comment');
    await page.evaluate(() => localStorage.setItem('sparkles.iris', 'full'));
    await page.reload();
    await page
      .getByRole('list', { name: 'Changes' })
      .getByRole('button', { name: /Add to two graphs/ })
      .click();
    await expect(page.locator('.changeset').first()).toContainText(
      'http://www.w3.org/2000/01/rdf-schema#comment',
      { timeout: 10_000 },
    );
  } finally {
    await request.delete('/$/branches/uni/dr-hist?force=true');
  }
});

test('edits on a session branch are previewed, committed, undone and redone', async ({
  page,
  request,
}) => {
  await page.goto('/ui/workbench/ontology?ds=uni');
  await expect(page.getByRole('button', { name: 'Agent', exact: true })).toBeVisible({
    timeout: 30_000,
  });
  await page.getByRole('button', { name: 'Start session' }).click();
  await expect(page).toHaveURL(/branch=onto\.anonymous\.\d{8}/);
  const branch = new URL(page.url()).searchParams.get('branch')!;
  await expect(page.getByRole('combobox', { name: 'Branch' })).toHaveValue(branch);

  // a new class under Employee, previewed first
  await expand(page, 'Agent');
  await expand(page, 'Person');
  await pick(page, 'Classes (asserted)', 'Employee');
  await page.getByRole('button', { name: 'New', exact: true }).click();
  const create = page.getByRole('dialog', { name: 'New class' });
  await create.getByRole('textbox', { name: 'Label', exact: true }).fill('teaching assistant');
  await expect(create.getByRole('textbox', { name: /IRI/ })).toHaveValue(`${UNI}TeachingAssistant`);
  await create.getByRole('button', { name: 'Preview' }).click();
  const d = preview(page);
  await expect(d).toContainText("Declaration: class 'teaching assistant'");
  await expect(d).toContainText("'teaching assistant' SubClassOf Employee");
  await commitPreview(page);
  await expect(page).toHaveURL(/TeachingAssistant/);
  await expect(node(page, 'teaching assistant')).toBeVisible();
  // main has none of it
  const main = await (
    await request.get(`/$/ontology/uni/entity?iri=${encodeURIComponent(UNI + 'TeachingAssistant')}`)
  ).status();
  expect(main).toBe(404);

  // undo takes the commit back, redo makes it again
  await page.getByRole('button', { name: /^Undo/ }).click();
  await expect(preview(page)).toContainText('Undo: Create class teaching assistant');
  await commitPreview(page);
  await expect(
    page
      .getByRole('tree', { name: 'Classes (asserted)' })
      .getByRole('button', { name: 'teaching assistant' }),
  ).toHaveCount(0);
  await page.getByRole('button', { name: 'Redo' }).click();
  await commitPreview(page);
  await expect(node(page, 'teaching assistant')).toBeVisible();

  // an annotation with a language tag
  await pick(page, 'Classes (asserted)', 'teaching assistant');
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

  // an IRI value edits as the IRI's prefixed name, not as the label it shows
  await frame.getByRole('button', { name: 'Add annotation' }).first().click();
  await form.getByRole('combobox', { name: 'Property' }).fill('rdfs:seeAlso');
  await form.getByRole('combobox', { name: 'Value is' }).selectOption('an IRI');
  await form.getByRole('textbox', { name: 'Value' }).fill('uni:Professor');
  await form.getByRole('button', { name: 'Preview' }).click();
  await commitPreview(page);
  await frame.getByRole('button', { name: 'Edit rdfs:seeAlso Professor' }).click();
  await expect(form.getByRole('textbox', { name: 'Value' })).toHaveValue('uni:Professor');
  await form.getByRole('button', { name: 'Cancel' }).click();

  // an annotation on the subclass axiom
  const sub = frame.locator('[data-section="sub"]');
  await sub.getByRole('button', { name: /Annotations of SubClassOf/ }).click();
  await sub.getByRole('button', { name: 'Add annotation' }).click();
  const axForm = sub.getByRole('form');
  await axForm.getByRole('combobox', { name: 'Property' }).fill('rdfs:comment');
  await axForm.getByRole('textbox', { name: 'Value' }).fill('assists a professor');
  await axForm.getByRole('textbox', { name: 'Language' }).fill('en');
  await axForm.getByRole('button', { name: 'Preview' }).click();
  await expect(preview(page).locator('.axioms.del')).toContainText(
    "'teaching assistant' SubClassOf Employee",
  );
  await expect(preview(page).locator('.axioms.ins')).toContainText('[1 annotation]');
  await commitPreview(page);
  await expect(sub.getByRole('button', { name: /Annotations of SubClassOf/ })).toContainText('1');

  // a superclass from a functional-style expression
  await frame
    .getByRole('textbox', { name: 'Add to SubClass Of' })
    .fill('ObjectSomeValuesFrom(uni:teaches uni:Course)');
  await sub.getByRole('button', { name: 'Add', exact: true }).click();
  await commitPreview(page);
  await expect(sub).toContainText('teaches some Course');

  // rename, then delete
  await page.getByRole('button', { name: 'Rename' }).click();
  const rename = page.getByRole('dialog', { name: /Rename/ });
  await rename.getByRole('textbox', { name: 'New IRI' }).fill(`${UNI}Assistant`);
  await rename.getByRole('button', { name: 'Preview' }).click();
  await commitPreview(page);
  await expect(page).toHaveURL(/uni%23Assistant/);
  await expect(page.locator('.entity-head')).toContainText(`${UNI}Assistant`);
  await page.getByRole('button', { name: 'Delete' }).click();
  await expect(preview(page)).toContainText('Removes the entity');
  await commitPreview(page);
  await expect(page.getByRole('heading', { name: 'Ontology header' })).toBeVisible();
  await expect(
    page
      .getByRole('tree', { name: 'Classes (asserted)' })
      .getByRole('button', { name: 'teaching assistant' }),
  ).toHaveCount(0);
  // the undo stack holds every edit of the session
  await expect(page.getByRole('button', { name: /^Undo/ })).toHaveText('Undo (7)');
});

test('a stale edit is refused and another writer’s commit refreshes the views', async ({
  page,
  request,
}) => {
  await page.goto('/ui/workbench/ontology?ds=uni');
  await expect(page.getByRole('button', { name: 'Agent', exact: true })).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.getByText('Following changes')).toBeVisible();
  await page.getByRole('button', { name: 'New', exact: true }).click();
  const create = page.getByRole('dialog', { name: 'New class' });
  await create.getByRole('textbox', { name: 'Label', exact: true }).fill('Lab');
  await create.getByRole('button', { name: 'Preview' }).click();
  await expect(preview(page).locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  // another writer commits while the preview is open
  const r = await request.post('/$/ontology/uni/axioms', {
    data: { add: [{ type: 'SubClassOf', subClass: `${UNI}Seminar`, superClass: `${UNI}Course` }] },
  });
  expect(r.ok()).toBe(true);
  await expect(page.getByRole('status').filter({ hasText: 'by another writer' })).toBeVisible();
  await preview(page).getByRole('button', { name: 'Commit' }).click();
  await expect(preview(page)).toBeHidden();
  await expect(
    page
      .locator('[role="status"], .toast')
      .filter({ hasText: /Another commit landed|by another writer/ })
      .first(),
  ).toBeVisible();
  await expect(
    page
      .getByRole('tree', { name: 'Classes (asserted)' })
      .getByRole('button', { name: 'Lab', exact: true }),
  ).toHaveCount(0);
  await page.getByRole('button', { name: 'Expand Course' }).click();
  await expect(page.getByRole('button', { name: 'uni:Seminar' })).toBeVisible();

  // after a visit to another dataset, the feed follows uni again from the commit it shows
  // the top bar's switcher reopens the page on the dataset it chooses
  const choose = async (ds: string) => {
    await page.locator('.ds-button').click();
    await page
      .getByRole('listbox', { name: 'Datasets' })
      .getByRole('option', { name: new RegExp(`^${ds} `) })
      .click();
  };
  await choose('foaf');
  await expect(page).toHaveURL(/ds=foaf/);
  await choose('uni');
  // Course stays open from before
  await expect(page.getByRole('button', { name: 'Collapse Course' })).toBeVisible();
  await expect(page.getByText('Following changes')).toBeVisible();
  const again = await request.post('/$/ontology/uni/axioms', {
    data: { add: [{ type: 'SubClassOf', subClass: `${UNI}Workshop`, superClass: `${UNI}Course` }] },
  });
  expect(again.ok()).toBe(true);
  await expect(page.getByRole('status').filter({ hasText: 'by another writer' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'uni:Workshop' })).toBeVisible();

  // an undo previewed on uni closes when the page moves to another dataset, so its commit
  // cannot revert a commit of the same number there
  await page.getByRole('button', { name: 'New', exact: true }).click();
  await create.getByRole('textbox', { name: 'Label', exact: true }).fill('Lab');
  await create.getByRole('button', { name: 'Preview' }).click();
  await commitPreview(page);
  await page.getByRole('button', { name: /^Undo/ }).click();
  await expect(preview(page)).toContainText('Reverts commit');
  // back past the new class's page, then to foaf
  await page.goBack();
  await expect(page).toHaveURL(/ds=uni$/);
  await page.goBack();
  await expect(page).toHaveURL(/ds=foaf/);
  await expect(preview(page)).toBeHidden();
  await page.goForward();
  await expect(page).toHaveURL(/ds=uni$/);
  await expect(page.getByRole('button', { name: /^Undo/ })).toHaveText('Undo (1)');
});

test('the mock revert keeps blank nodes and refuses a cell a later commit changed', async ({
  request,
}) => {
  /** The members of the answers this test reads: writes, reverts and listings. */
  type Answer = {
    commit: { seq: number } | null;
    changes: { inserted: number; deleted: number };
    upToDate?: boolean;
    axioms: { ofn: string }[];
  };
  const json = async (r: Awaited<ReturnType<typeof request.get>>): Promise<Answer> => {
    expect(r.ok(), await r.text()).toBe(true);
    return (await r.json()) as Answer;
  };
  const headOf = async () =>
    ((await (await request.get('/$/ontology/uni')).json()) as { commit: number }).commit;
  const about = async (iri: string) => {
    const r = await request.get(
      `/$/ontology/uni/axioms?about=${encodeURIComponent(iri)}&limit=100`,
    );
    // an entity the ontology no longer uses answers 404
    if (r.status() === 404) return '';
    return (await json(r)).axioms.map((a) => a.ofn).join('\n');
  };
  const update = async (sparql: string) =>
    request.post('/uni/update?receipt=true', {
      headers: { 'Content-Type': 'application/sparql-update', Accept: 'application/json' },
      data: sparql,
    });

  // a restriction, reverted and then restored by reverting the revert
  const added = await json(
    await request.post('/$/ontology/uni/axioms?receipt=true', {
      data: { add: ['SubClassOf(uni:Tutorial ObjectSomeValuesFrom(uni:teaches uni:Course))'] },
    }),
  );
  const seq = added.commit!.seq;
  expect(await about(`${UNI}Tutorial`)).toContain('ObjectSomeValuesFrom');
  const undone = await json(await request.post(`/$/revert/uni?commit=${seq}`, { data: {} }));
  expect(undone.commit?.seq).toBe(seq + 1);
  expect(undone.changes.deleted).toBeGreaterThan(0);
  expect(await about(`${UNI}Tutorial`)).not.toContain('ObjectSomeValuesFrom');
  const redone = await json(await request.post(`/$/revert/uni?commit=${seq + 1}`, { data: {} }));
  expect(redone.changes.inserted).toBe(undone.changes.deleted);
  expect(await about(`${UNI}Tutorial`)).toContain('ObjectSomeValuesFrom');
  // the head holds the changes no longer, so reverting the revert again changes nothing
  const spent = await json(
    await request.post(`/$/revert/uni?commit=${seq + 1}`, { data: { dryRun: true } }),
  );
  expect(spent.upToDate).toBe(true);
  expect(spent.commit).toBeNull();

  // A to B, then B to C by another writer: reverting the first conflicts and writes nothing
  const comment = (v: string) =>
    `<${UNI}Seminar> <http://www.w3.org/2000/01/rdf-schema#comment> "${v}"`;
  expect((await update(`INSERT DATA { ${comment('A')} }`)).ok()).toBe(true);
  const ab = await json(
    await update(`DELETE DATA { ${comment('A')} } ; INSERT DATA { ${comment('B')} }`),
  );
  expect(
    (await update(`DELETE DATA { ${comment('B')} } ; INSERT DATA { ${comment('C')} }`)).ok(),
  ).toBe(true);
  const head = await headOf();
  const refused = await request.post(`/$/revert/uni?commit=${ab.commit!.seq}`, { data: {} });
  expect(refused.status()).toBe(409);
  expect((await refused.json()).code).toBe('merge-conflict');
  expect(await headOf()).toBe(head);

  // quads in a blank-node graph beside a literal that looks like a blank node and a triple
  // term: undo removes them and redo puts them back in the same graph
  const nq = [
    '<urn:s> <urn:p> "reference _:example" _:g .',
    '<urn:s> <urn:q> <<( <urn:a> <urn:b> <urn:c> )>> _:g .',
  ].join('\n');
  const upload = await json(
    await request.post('/uni/upload?receipt=true', {
      headers: { Accept: 'application/x-sparkles+json' },
      multipart: {
        file: { name: 'odd.nq', mimeType: 'application/n-quads', buffer: Buffer.from(nq) },
      },
    }),
  );
  const up = upload.commit!.seq;
  const off = await json(await request.post(`/$/revert/uni?commit=${up}`, { data: {} }));
  expect(off.changes.deleted).toBe(2);
  const on = await json(await request.post(`/$/revert/uni?commit=${up + 1}`, { data: {} }));
  expect(on.changes.inserted).toBe(2);
  const offAgain = await json(await request.post(`/$/revert/uni?commit=${up + 2}`, { data: {} }));
  expect(offAgain.changes.deleted).toBe(2);

  // keep and cd, then cd to ab, then ab to ba by another writer: reverting cd to ab
  // conflicts on the cell that holds two values
  expect((await update(`INSERT DATA { ${comment('keep')} . ${comment('cd')} }`)).ok()).toBe(true);
  const cdab = await json(
    await update(`DELETE DATA { ${comment('cd')} } ; INSERT DATA { ${comment('ab')} }`),
  );
  expect(
    (await update(`DELETE DATA { ${comment('ab')} } ; INSERT DATA { ${comment('ba')} }`)).ok(),
  ).toBe(true);
  const before = await headOf();
  const clash = await request.post(`/$/revert/uni?commit=${cdab.commit!.seq}`, { data: {} });
  expect(clash.status()).toBe(409);
  expect(await headOf()).toBe(before);

  // a seeded commit has no recorded lines, and the mock diffs its kept states instead
  const seeded = (await (await request.get('/$/commits/uni?limit=100')).json()) as {
    commits: { seq: number; kind: string }[];
  };
  // the oldest upload, the seeded load of the ontology (the list is newest first)
  const load = seeded.commits.filter((c) => c.kind === 'upload').at(-1)!;
  const dry = await request.post(`/$/revert/uni?commit=${load.seq}`, { data: { dryRun: true } });
  // earlier edits changed cells of that load, so the revert may conflict, but it is
  // computed rather than refused as outside the kept history
  expect([200, 409], await dry.text()).toContain(dry.status());

  // the first commit after a seeded history, whose parent the mock kept no state for
  const foaf = await request.post('/foaf/update?receipt=true', {
    headers: { 'Content-Type': 'application/sparql-update', Accept: 'application/json' },
    data: 'INSERT DATA { <urn:revert:s> <urn:revert:p> 1 }',
  });
  const first = await json(foaf);
  const back = await json(
    await request.post(`/$/revert/foaf?commit=${first.commit!.seq}`, { data: {} }),
  );
  expect(back.changes.deleted).toBe(1);
});
