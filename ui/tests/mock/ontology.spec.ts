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
  await page.goto('/ui/ontology?ds=uni');
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
  await page.getByRole('tab', { name: /Usage/ }).click();
  await expect(page.getByRole('region', { name: 'ObjectPropertyRange' })).toContainText(
    'uni:advisor',
  );
  await page.getByRole('tab', { name: 'Frame' }).click();

  // the property trees
  await page.getByRole('tab', { name: /Object properties/ }).click();
  await expand(page, 'member of');
  await expand(page, 'works for');
  await pick(page, 'Object properties (asserted)', 'head of');
  await expect(page.getByRole('checkbox', { name: 'Functional', exact: true })).toBeChecked();
  await expect(frame.locator('[data-section="superProperties"]')).toContainText('works for');
  await page.getByRole('tab', { name: /Data properties/ }).click();
  await pick(page, 'Data properties (asserted)', 'credits');
  await expect(frame.locator('[data-section="ranges"]')).toContainText('xsd:integer');
  await page.getByRole('tab', { name: /Individuals/ }).click();
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
  await page.goto('/ui/ontology?ds=uni&tab=Class&inferred=1');
  const inferred = page.getByRole('tree', { name: 'Classes (inferred)' });
  await expect(inferred).toBeVisible({ timeout: 30_000 });
  // Teacher is defined as a person who teaches a course, so Professor falls under it
  await inferred.getByRole('button', { name: 'Expand Teacher' }).click();
  await expect(inferred.getByRole('button', { name: 'Professor', exact: true })).toBeVisible();
  await expect(
    page
      .getByRole('tree', { name: 'Classes (asserted)' })
      .getByRole('button', { name: 'Expand Teacher' }),
  ).toHaveCount(0);
});

test('edits on a session branch are previewed, committed, undone and redone', async ({
  page,
  request,
}) => {
  await page.goto('/ui/ontology?ds=uni');
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
  await expect(d).toContainText('Declaration: class teaching assistant');
  await expect(d).toContainText('teaching assistant SubClassOf Employee');
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
    'teaching assistant SubClassOf Employee',
  );
  await expect(preview(page).locator('.axioms.ins')).toContainText('[1 annotation]');
  await commitPreview(page);
  await expect(sub.getByRole('button', { name: /Annotations of SubClassOf/ })).toContainText('1');

  // a superclass from a functional-style expression
  await frame
    .getByRole('combobox', { name: 'Add to SubClass Of' })
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
  await page.goto('/ui/ontology?ds=uni');
  await expect(page.getByRole('button', { name: 'Agent', exact: true })).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.locator('.live')).toContainText('Live');
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
  await page.getByRole('combobox', { name: 'Dataset' }).selectOption('foaf');
  await expect(page).toHaveURL(/ds=foaf/);
  await page.getByRole('combobox', { name: 'Dataset' }).selectOption('uni');
  // Course stays open from before
  await expect(page.getByRole('button', { name: 'Collapse Course' })).toBeVisible();
  await expect(page.locator('.live')).toContainText('Live');
  const again = await request.post('/$/ontology/uni/axioms', {
    data: { add: [{ type: 'SubClassOf', subClass: `${UNI}Workshop`, superClass: `${UNI}Course` }] },
  });
  expect(again.ok()).toBe(true);
  await expect(page.getByRole('status').filter({ hasText: 'by another writer' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'uni:Workshop' })).toBeVisible();
});
