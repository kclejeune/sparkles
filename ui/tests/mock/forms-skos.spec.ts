// SKOS-shaped editing forms against the mock (mock/forms.mjs): a concept's SKOS-XL label as
// a nested child edited and added in one save, a collection's members chosen among the
// options of `sh:or`, and an ordered collection's `skos:memberList` reordered with the list
// editor (docs/specs/U02-shape-driven-forms/SPEC.md §2.3.1, Phase 3).

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 60_000 });

const EX = 'http://example.org/thesaurus/';
const SKOS = 'http://www.w3.org/2004/02/skos/core#';
const XL = 'http://www.w3.org/2008/05/skos-xl#';
const DS = 'formskos';

const PREFIXES = `PREFIX ex: <${EX}> PREFIX skos: <${SKOS}> PREFIX skosxl: <${XL}>`;

const update = (request: APIRequestContext, sparql: string) =>
  request
    .post(`/${DS}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `${PREFIXES} ${sparql}`,
    })
    .then((r) => expect(r.ok()).toBe(true));

const ask = async (request: APIRequestContext, pattern: string) => {
  const r = await request.get(
    `/${DS}/sparql?query=${encodeURIComponent(`${PREFIXES} ASK { ${pattern} }`)}`,
    { headers: { Accept: 'application/sparql-results+json' } },
  );
  expect(r.ok()).toBe(true);
  return ((await r.json()) as { boolean: boolean }).boolean;
};

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  await update(
    request,
    `INSERT DATA {
      ex:animals a skos:Concept ; skos:prefLabel "Animals"@en , "Animaux"@fr ;
        skosxl:prefLabel [ skosxl:literalForm "Animals"@en ] .
      ex:mammals a skos:Concept ; skos:prefLabel "Mammals"@en ; skos:broader ex:animals .
      ex:birds a skos:Concept ; skos:prefLabel "Birds"@en ; skos:broader ex:animals .
      ex:pets a skos:Collection ; skos:prefLabel "Pets"@en ; skos:member ex:mammals ;
        skos:memberList ( ex:mammals ex:birds ) .
    }`,
  );
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript((ds) => localStorage.setItem('sparkles.dataset', ds), DS);
});

const openEdit = async (page: Page, local: string, heading: RegExp) => {
  await page.goto(`/ui/datasets/${DS}/edit?iri=${encodeURIComponent(EX + local)}`);
  const form = page.getByRole('region', { name: heading });
  await expect(form.locator('.ff').first()).toBeVisible({ timeout: 30_000 });
  return form;
};

test('a SKOS-XL label is a nested form, edited and added in one save', async ({
  page,
  request,
}) => {
  const form = await openEdit(page, 'animals', /^Edit Anima/);
  const labels = form.locator('[data-field$="skos-xl#prefLabel>"]');
  // the stored label child shows its own field
  const forms = labels.getByLabel('Literal form', { exact: true });
  await expect(forms).toHaveCount(1);
  await expect(forms).toHaveValue('Animals');
  await expect(labels.getByLabel('Language of Literal form')).toHaveValue('en');
  await forms.fill('All animals');
  // a new child takes the template's fields
  await labels.getByRole('button', { name: '+ Add XL labels' }).click();
  await expect(labels.getByText('New', { exact: true })).toBeVisible();
  const second = labels.getByLabel('Literal form', { exact: true }).nth(1);
  await second.fill('Animaux');
  await labels.getByLabel('Language of Literal form').nth(1).fill('fr');
  await expect(labels.getByLabel('Language of Literal form').nth(1)).toHaveValue('fr');
  // the new child, its value and the edited label
  await expect(form.getByText(/^0 problems · 3 changes/)).toBeVisible();
  await form.getByRole('button', { name: 'Preview' }).click();
  const preview = form.getByRole('region', { name: 'Preview' });
  await expect(preview).toContainText('The save would commit.');
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
  expect(
    await ask(request, 'ex:animals skosxl:prefLabel/skosxl:literalForm "All animals"@en'),
  ).toBe(true);
  expect(await ask(request, 'ex:animals skosxl:prefLabel/skosxl:literalForm "Animaux"@fr')).toBe(
    true,
  );
  expect(await ask(request, 'ex:animals skosxl:prefLabel/skosxl:literalForm "Animals"@en')).toBe(
    false,
  );
  // removing a child deletes its quads with the edge
  const reloaded = labels.getByLabel('Literal form', { exact: true });
  await expect(reloaded).toHaveCount(2);
  await labels.getByRole('button', { name: 'Remove XL labels 1' }).click();
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved commit \d+ to main/).last()).toBeVisible();
  await expect(labels.getByLabel('Literal form', { exact: true })).toHaveCount(1);
  expect(
    (await ask(request, '?l skosxl:literalForm "All animals"@en')) ||
      (await ask(request, '?l skosxl:literalForm "Animaux"@fr')),
  ).toBe(true);
  expect(
    (await ask(request, '?l skosxl:literalForm "All animals"@en')) &&
      (await ask(request, '?l skosxl:literalForm "Animaux"@fr')),
  ).toBe(false);
});

test('a collection offers member options and reorders its member list', async ({
  page,
  request,
}) => {
  const form = await openEdit(page, 'pets', /^Edit .*pets/);
  const members = form.locator('[data-field$="core#member>"]');
  // the stored member is a concept, so its option is Concept
  await expect(members.getByLabel('Kind of Members')).toHaveValue('0');
  await members.getByLabel('Kind of Members').selectOption({ label: 'Collection' });
  await expect(form.getByText('must be one of: Concept, Collection')).toHaveCount(0);
  // the ordered members move with the list editor
  const list = form.getByRole('list', { name: 'Ordered members members' });
  await expect(list.getByRole('listitem')).toHaveCount(2);
  await list.getByRole('button', { name: 'Move Ordered members member 2 up' }).click();
  await members.getByLabel('Kind of Members').selectOption({ label: 'Concept' });
  await members.getByRole('combobox', { name: 'Members', exact: true }).fill(`<${EX}mammals>`);
  await page.getByRole('option', { name: /Use/ }).click();
  await expect(form.getByText(/^0 problems · 1 change/)).toBeVisible();
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
  expect(await ask(request, 'ex:pets skos:memberList (ex:birds ex:mammals)')).toBe(true);
  expect(await ask(request, 'ex:pets skos:memberList (ex:mammals ex:birds)')).toBe(false);
});

test('the nested and list editors fit a phone', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openEdit(page, 'pets', /^Edit .*pets/);
  await expectNoSidewaysScroll(page, 'the member list');
  await openEdit(page, 'animals', /^Edit Anima/);
  await expectNoSidewaysScroll(page, 'the nested labels');
});
