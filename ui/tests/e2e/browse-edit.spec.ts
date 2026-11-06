// Edit mode on the ontology browser's pages (ROADMAP item 630) against a real server: the
// OBO-style ontology of tests/browse-fixture.ts in a dataset of its own on the server
// without auth, edited through the axioms route with the term route's entity tag, and on
// the server with auth a reader who sees no edit affordance and a writer whose grant
// leaves out the ontology's graph, whose write the server refuses. The checks the mock
// cannot make: sources kept as axiom annotations, the server's Manchester parser and
// entity search, a stale tag after another writer's commit, and the grants.

import type { Page } from '@playwright/test';
import { anonymous, expect, open, test } from './fixtures';
import { BROWSE_TURTLE, CELLO } from '../browse-fixture';
import { BROWSE_EDIT_AUTH, PASSWORD, READER } from './data';

open.describe.configure({ mode: 'serial', timeout: 120_000 });
open.use({ actionTimeout: 20_000 });

const DS = 'browse-edit-e2e';
const OIO = 'http://www.geneontology.org/formats/oboInOwl#';
const termUrl = (ds: string, n: number, extra = '') =>
  `/ui/browse?ds=${ds}&iri=${encodeURIComponent(CELLO(n))}${extra}`;
const bar = (page: Page) => page.getByRole('toolbar', { name: 'Edit mode' });
const preview = (page: Page) => page.getByRole('dialog');

async function commit(page: Page) {
  const d = preview(page);
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden();
}

async function openEdit(page: Page, ds: string, n: number) {
  await page.goto(termUrl(ds, n, '&edit=1'));
  await expect(page.locator('h1.term-title')).toBeVisible();
}

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  const loaded = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: BROWSE_TURTLE,
  });
  expect(loaded.ok(), await loaded.text()).toBe(true);
});

open('a definition and a synonym keep their sources through an edit', async ({ page, request }) => {
  await openEdit(page, DS, 5);
  await expect(bar(page)).toContainText('Each change is previewed, then committed to main');
  const def = page.getByRole('textbox', { name: 'Definition', exact: true });
  await expect(def).toHaveValue(/self-replicating organelle/);
  await expect(page.getByRole('group', { name: 'Sources of the definition' })).toContainText(
    'CELLO:curators',
  );
  await def.fill('A self-replicating organelle of eukaryotic cells that makes most of their ATP.');
  await page
    .getByRole('group', { name: /^Definition/ })
    .getByRole('button', { name: 'Preview change' })
    .click();
  await expect(preview(page)).toContainText('Edit the definition of mitochondrion');
  await commit(page);
  await expect(def).toHaveValue(/makes most of their ATP/);

  // the synonym "mitochondria" becomes narrow and keeps CELLO:plural
  const syn = page.getByRole('region', { name: /^Synonyms/ });
  await syn.getByRole('button', { name: 'Edit the synonym mitochondria' }).click();
  await syn.getByRole('combobox', { name: 'Kind', exact: true }).selectOption({ label: 'Narrow' });
  await syn.getByRole('button', { name: 'Preview change' }).click();
  await commit(page);
  await expect(syn.locator('li', { hasText: 'mitochondria' })).toContainText('Narrow');
  await expect(syn.locator('li', { hasText: 'mitochondria' })).toContainText('CELLO:plural');

  // the server's answer holds both annotations as axiom annotations
  const t = await (
    await request.get(`/$/ontology/${DS}/term?iri=${encodeURIComponent(CELLO(5))}&graph=default`)
  ).json();
  const st = (p: string) =>
    t.statements.find((s: { property: string }) => s.property === p) as {
      value: { '@value': string };
      annotations: { property: string; value: { '@value': string } }[];
    };
  expect(st('http://purl.obolibrary.org/obo/IAO_0000115').value['@value']).toContain('ATP');
  expect(
    st('http://purl.obolibrary.org/obo/IAO_0000115').annotations.map((a) => a.value['@value']),
  ).toEqual(['CELLO:curators']);
  expect(st(OIO + 'hasNarrowSynonym').annotations.map((a) => a.value['@value'])).toEqual([
    'CELLO:plural',
  ]);
  expect(st(OIO + 'hasExactSynonym')).toBeUndefined();

  // undo takes the synonym's change back
  await bar(page).getByRole('button', { name: 'Undo' }).click();
  await commit(page);
  await expect(syn.locator('li', { hasText: 'mitochondria' })).toContainText('Exact');
});

open(
  'a definition that two axioms annotate is replaced whole, with both sources',
  async ({ page, request }) => {
    const c = CELLO(99);
    const DEF = 'http://purl.obolibrary.org/obo/IAO_0000115';
    const reif = (src: string) =>
      `[] a owl:Axiom ; owl:annotatedSource <${c}> ; owl:annotatedProperty <${DEF}> ; ` +
      `owl:annotatedTarget "A twice-sourced organelle."@en ; <${OIO}hasDbXref> "${src}" .`;
    const seed = await request.post(`/${DS}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data:
        `PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ` +
        `INSERT DATA { <${c}> a owl:Class ; rdfs:label "twice-sourced organelle"@en ; ` +
        `<${DEF}> "A twice-sourced organelle."@en . ${reif('CELLO:one')} ${reif('CELLO:two')} }`,
    });
    expect(seed.ok(), await seed.text()).toBe(true);
    await openEdit(page, DS, 99);
    const def = page.getByRole('textbox', { name: 'Definition', exact: true });
    await expect(def).toHaveValue('A twice-sourced organelle.');
    await def.fill('An organelle with two sources.');
    await page
      .getByRole('group', { name: /^Definition/ })
      .getByRole('button', { name: 'Preview change' })
      .click();
    await commit(page);
    const t = await (
      await request.get(`/$/ontology/${DS}/term?iri=${encodeURIComponent(c)}&graph=default`)
    ).json();
    const defs = (
      t.statements as {
        property: string;
        value: { '@value': string };
        annotations: { value: { '@value': string } }[];
      }[]
    ).filter((s) => s.property === DEF);
    expect(defs.map((d) => d.value['@value'])).toEqual(['An organelle with two sources.']);
    expect(defs[0].annotations.map((a) => a.value['@value']).sort()).toEqual([
      'CELLO:one',
      'CELLO:two',
    ]);
    // no reification of the old text is left behind
    const left = await request.post(`/${DS}/query`, {
      headers: {
        'Content-Type': 'application/sparql-query',
        Accept: 'application/sparql-results+json',
      },
      data: `ASK { ?a <http://www.w3.org/2002/07/owl#annotatedTarget> "A twice-sourced organelle."@en }`,
    });
    expect((await left.json()).boolean).toBe(false);
  },
);

open(
  'an annotated SKOS definition without SKOS declarations keeps its source',
  async ({ page, request }) => {
    const c = 'http://example.org/e2e-thesaurus/fish';
    const SKOS = 'http://www.w3.org/2004/02/skos/core#';
    const seed = await request.post(`/${DS}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data:
        `PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX skos: <${SKOS}> ` +
        `INSERT DATA { <${c}> a skos:Concept ; skos:prefLabel "fish"@en ; skos:definition "A cold animal." . ` +
        `[] a owl:Axiom ; owl:annotatedSource <${c}> ; owl:annotatedProperty skos:definition ; ` +
        `owl:annotatedTarget "A cold animal." ; <${OIO}hasDbXref> "ISBN:1" . }`,
    });
    expect(seed.ok(), await seed.text()).toBe(true);
    await page.goto(`/ui/browse?ds=${DS}&iri=${encodeURIComponent(c)}&edit=1`);
    const def = page.getByRole('textbox', { name: 'Definition', exact: true });
    await expect(def).toHaveValue('A cold animal.');
    await def.fill('A cold-blooded animal of the water.');
    await page
      .getByRole('group', { name: /^Definition/ })
      .getByRole('button', { name: 'Preview change' })
      .click();
    await commit(page);
    const ask = async (q: string) =>
      (
        await (
          await request.post(`/${DS}/query`, {
            headers: {
              'Content-Type': 'application/sparql-query',
              Accept: 'application/sparql-results+json',
            },
            data: `PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX skos: <${SKOS}> ASK { ${q} }`,
          })
        ).json()
      ).boolean;
    expect(await ask(`<${c}> skos:definition "A cold animal."`)).toBe(false);
    expect(await ask(`?a owl:annotatedTarget "A cold animal."`)).toBe(false);
    expect(
      await ask(
        `<${c}> skos:definition "A cold-blooded animal of the water." . ?a owl:annotatedSource <${c}> ; owl:annotatedTarget "A cold-blooded animal of the water." ; <${OIO}hasDbXref> "ISBN:1"`,
      ),
    ).toBe(true);
  },
);

open(
  'a parent through the entity search and an axiom through the Manchester parser',
  async ({ page }) => {
    await openEdit(page, DS, 9);
    const tree = page.getByRole('complementary', { name: 'Hierarchy', exact: true });
    await expect(
      tree.getByRole('button', { name: 'Remove the parent membrane-bounded organelle' }),
    ).toBeVisible();
    await tree.getByRole('combobox', { name: 'Add a parent class' }).fill('intracellular');
    await tree
      .getByRole('option', { name: /intracellular organelle/ })
      .first()
      .click();
    await expect(preview(page).locator('.axioms.ins')).toContainText('SubClassOf');
    await commit(page);
    await expect(
      tree.getByRole('button', { name: 'Remove the parent intracellular organelle' }),
    ).toBeVisible();
    await expect(tree).toContainText('Also under');

    const logic = page.getByRole('region', { name: 'Axioms', exact: true });
    await page.locator('#browse-add-axiom .cm-content').click();
    await page.keyboard.type("'part of' some cytoplasm");
    await page
      .getByRole('form', { name: 'Add an axiom' })
      .getByRole('button', { name: 'Add' })
      .click();
    await commit(page);
    await expect(logic.locator('[data-section="sub"]')).toContainText('part of some cytoplasm');

    // a name the ontology does not know is underlined with the parser's message
    await page.locator('#browse-add-axiom .cm-content').click();
    await page.keyboard.type("'part of' some golgi");
    await page
      .getByRole('form', { name: 'Add an axiom' })
      .getByRole('button', { name: 'Add' })
      .click();
    await expect(page.getByRole('form', { name: 'Add an axiom' }).getByRole('alert')).toBeVisible();
    await expect(preview(page)).toBeHidden();

    await tree.getByRole('button', { name: 'Remove the parent intracellular organelle' }).click();
    await commit(page);
    await expect(
      tree.getByRole('button', { name: 'Remove the parent intracellular organelle' }),
    ).toHaveCount(0);
  },
);

open(
  'another writer’s commit makes the preview stale until the page reads again',
  async ({ page, request }) => {
    await openEdit(page, DS, 10);
    await page
      .getByRole('textbox', { name: 'New synonym', exact: true })
      .fill('tonoplast compartment');
    // another writer commits after the page read the term
    const other = await request.post(`/${DS}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `INSERT DATA { <${CELLO(10)}> <http://www.w3.org/2000/01/rdf-schema#comment> "Seen in plants." }`,
    });
    expect(other.ok()).toBe(true);
    await page
      .getByRole('form', { name: 'Add a synonym' })
      .getByRole('button', { name: 'Add' })
      .click();
    const d = preview(page);
    await expect(d.locator('[data-outcome]')).toHaveAttribute(
      'data-outcome',
      'precondition-failed',
    );
    await d.getByRole('button', { name: 'Reload' }).click();
    await expect(page.getByRole('region', { name: /^Annotations/ })).toContainText(
      'Seen in plants.',
    );
    // the form kept its text, and the edit now goes through
    await page
      .getByRole('form', { name: 'Add a synonym' })
      .getByRole('button', { name: 'Add' })
      .click();
    await commit(page);
    await expect(page.getByRole('region', { name: /^Synonyms/ })).toContainText(
      'tonoplast compartment',
    );
  },
);

open(
  'drafts survive another writer changing the definition and synonym being edited',
  async ({ page, request }) => {
    const DEF = 'http://purl.obolibrary.org/obo/IAO_0000115';
    const update = (sparql: string) =>
      request.post(`/${DS}/update`, {
        headers: { 'Content-Type': 'application/sparql-update' },
        data: sparql,
      });
    await openEdit(page, DS, 5);
    const def = page.getByRole('textbox', { name: 'Definition', exact: true });
    await def.fill('My draft of the definition.');
    const syn = page.getByRole('region', { name: /^Synonyms/ });
    await syn.getByRole('button', { name: 'Edit the synonym powerhouse of the cell' }).click();
    await syn.getByRole('textbox', { name: 'Synonym', exact: true }).fill('cell powerhouse');

    // another writer replaces the definition and removes the synonym
    const cur = await (
      await request.get(`/$/ontology/${DS}/term?iri=${encodeURIComponent(CELLO(5))}&graph=default`)
    ).json();
    const text = cur.statements.find((s: { property: string }) => s.property === DEF).value[
      '@value'
    ];
    expect(
      (
        await update(
          `DELETE DATA { <${CELLO(5)}> <${DEF}> ${JSON.stringify(text)} . <${CELLO(5)}> <${OIO}hasRelatedSynonym> "powerhouse of the cell" . } ;\n` +
            `INSERT DATA { <${CELLO(5)}> <${DEF}> "Another writer's definition." . }`,
        )
      ).ok(),
    ).toBe(true);

    // the page follows the commit; the definition draft stands as a definition of its own
    const defs = page.getByRole('textbox', { name: 'Definition', exact: true });
    await expect(defs.first()).toHaveValue("Another writer's definition.", { timeout: 20_000 });
    await expect(
      page.getByText('Another commit changed this definition while you typed'),
    ).toBeVisible();
    await expect(page.getByRole('textbox', { name: 'Definition', exact: true }).last()).toHaveValue(
      'My draft of the definition.',
    );
    // the synonym's draft stays open above the list and adds the synonym when previewed
    await expect(syn.getByRole('status')).toContainText('Another commit changed the synonym');
    await expect(syn.getByRole('textbox', { name: 'Synonym', exact: true })).toHaveValue(
      'cell powerhouse',
    );
    await syn.getByRole('button', { name: 'Preview change' }).click();
    await expect(preview(page)).toContainText('Add the synonym cell powerhouse');
    await commit(page);
    await expect(syn.locator('li', { hasText: 'cell powerhouse' })).toContainText('Related');
  },
);

open(
  'a protected main asks for a session branch, where the edits go',
  async ({ page, request }) => {
    expect(
      (await request.patch(`/$/branches/${DS}/main`, { data: { protected: true } })).ok(),
    ).toBe(true);
    try {
      await openEdit(page, DS, 6);
      await expect(bar(page)).toContainText('main is protected');
      await expect(page.getByRole('textbox', { name: 'Definition', exact: true })).toBeDisabled();
      await bar(page).getByRole('button', { name: 'Start session' }).click();
      await expect(page).toHaveURL(/branch=onto\.[^&]+\.\d{8}/);
      const branch = new URL(page.url()).searchParams.get('branch')!;
      await page
        .getByRole('textbox', { name: 'New synonym', exact: true })
        .fill('cytosol and organelles');
      await page
        .getByRole('form', { name: 'Add a synonym' })
        .getByRole('button', { name: 'Add' })
        .click();
      await commit(page);
      await expect(page.getByRole('region', { name: /^Synonyms/ })).toContainText(
        'cytosol and organelles',
      );
      // main kept none of it
      const onMain = await (
        await request.get(
          `/$/ontology/${DS}/term?iri=${encodeURIComponent(CELLO(6))}&graph=default`,
        )
      ).json();
      expect(JSON.stringify(onMain.statements)).not.toContain('cytosol and organelles');
      const onBranch = await (
        await request.get(
          `/$/ontology/${DS}/term?iri=${encodeURIComponent(CELLO(6))}&graph=default&branch=${encodeURIComponent(branch)}`,
        )
      ).json();
      expect(onBranch.statements, JSON.stringify(onBranch)).toBeDefined();
      expect(JSON.stringify(onBranch.statements)).toContain('cytosol and organelles');
    } finally {
      await request.patch(`/$/branches/${DS}/main`, { data: { protected: false } });
    }
  },
);

anonymous('a reader sees no edit affordance', async ({ page }) => {
  const login = await page.request.post('/$/auth/login', {
    data: { user: READER, password: PASSWORD },
  });
  expect(login.ok()).toBe(true);
  await page.goto(termUrl(BROWSE_EDIT_AUTH, 5, '&edit=1'));
  await expect(page.locator('h1.term-title')).toHaveText('mitochondrion');
  await expect(page.getByRole('region', { name: 'Axioms', exact: true })).toBeVisible();
  await expect(page).not.toHaveURL(/edit=1/);
  await expect(page.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0);
  await expect(bar(page)).toHaveCount(0);
});

test('a write the grants leave out is refused with a clear message', async ({ page }) => {
  await openEdit(page, BROWSE_EDIT_AUTH, 5);
  await expect(bar(page)).toBeVisible();
  await page.getByRole('textbox', { name: 'New synonym', exact: true }).fill('mito');
  await page
    .getByRole('form', { name: 'Add a synonym' })
    .getByRole('button', { name: 'Add' })
    .click();
  await expect(preview(page).getByRole('alert')).toContainText(
    `You can read ${BROWSE_EDIT_AUTH} but not write to it`,
  );
  await expect(preview(page).getByRole('button', { name: 'Commit' })).toBeDisabled();
});
