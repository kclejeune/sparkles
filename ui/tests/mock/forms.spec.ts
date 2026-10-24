// Editing forms generated from shapes (docs/specs/U02-shape-driven-forms.md) against the
// mock (mock/forms.mjs): the layout, the browser checks, the server check, saves to main
// and to a session branch, conflicts, the error summary and the empty state. The dataset
// `formsw` holds a few people and companies of the `world` vocabulary, whose classes the
// mock has form layouts for.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 60_000 });

const W = 'http://example.org/world/';
const DS = 'formsw';

const update = (request: APIRequestContext, sparql: string) =>
  request
    .post(`/${DS}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <${W}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ${sparql}`,
    })
    .then((r) => expect(r.ok()).toBe(true));

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  await update(
    request,
    `INSERT DATA {
      ex:dubai a ex:City ; rdfs:label "Dubai"@en .
      ex:lima a ex:City ; rdfs:label "Lima"@en .
      ex:meridian a ex:Company ; rdfs:label "Meridian Logistics"@en ; ex:headquarters ex:dubai .
      ex:andes a ex:Company ; rdfs:label "Andes Cargo"@en ; ex:headquarters ex:lima .
      ex:amira a ex:Person ; rdfs:label "Amira Haddad"@en ; ex:jobTitle "Chief executive"@en ;
        ex:livesIn ex:dubai ; ex:worksFor ex:meridian ; ex:age "47"^^xsd:integer ;
        ex:joined "2009-03-01"^^xsd:date ; ex:nickname "Ami" .
      ex:omar a ex:Person ; rdfs:label "Omar Rahman"@en ; ex:worksFor ex:meridian ; ex:knows ex:amira .
      ex:valentina a ex:Person ; rdfs:label "Valentina Ruiz"@en ; ex:worksFor ex:andes .
      ex:sofia a ex:Person ; rdfs:label "Sofía Martínez"@en ; ex:worksFor ex:andes .
      ex:route1 rdfs:label "Dubai to Lima" .
      GRAPH <urn:x-mock:readonly> { ex:andes ex:hasOffice ex:dubai }
    }`,
  );
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript((ds) => localStorage.setItem('sparkles.dataset', ds), DS);
});

const openEdit = async (page: Page, who: string) => {
  await page.goto(`/ui/datasets/${DS}/edit?iri=${encodeURIComponent(W + who)}`);
  const form = page.getByRole('region', { name: /^Edit / });
  await expect(form.getByRole('group', { name: 'Identity' })).toBeVisible({ timeout: 30_000 });
  return form;
};

test('A1 and A7: Explore offers Edit, and the form follows the shapes', async ({ page }) => {
  await page.goto(`/ui/explore?ds=${DS}&iri=${encodeURIComponent(`${W}amira`)}`);
  const edit = page.getByRole('button', { name: 'Edit', exact: true });
  await expect(edit).toBeVisible({ timeout: 30_000 });
  await edit.click();
  const form = page.getByRole('region', { name: /^Edit Amira Haddad/ });
  await expect(form).toBeVisible();
  // groups in their order, fields in theirs, required ones marked in words
  const legends = await form.locator('fieldset.group > legend').allTextContents();
  expect(legends.map((l) => l.trim())).toEqual([
    'Identity',
    'Work',
    'Network',
    'Other properties not in the shapes',
  ]);
  const identity = form.getByRole('group', { name: 'Identity' });
  await expect(identity.locator('.ff-label')).toHaveText(['Name', 'Email', 'Age', 'Lives in']);
  await expect(identity.locator('.ff').first()).toContainText('required');
  // Status is a radio group of its three values
  const status = form.getByRole('radiogroup', { name: 'Status' });
  await expect(status.getByRole('radio')).toHaveCount(3);
  // the value outside the shapes is listed read-only
  const other = form.getByRole('group', { name: /Other properties/ });
  await expect(other).toContainText('Ami');
  await expect(other).toContainText('read-only');
  // the inputs have their values and names
  await expect(form.getByLabel('Age', { exact: true })).toHaveValue('47');
  await expect(form.getByLabel('Joined', { exact: true })).toHaveValue('2009-03-01');
  // Cancel goes back to the properties
  await form.getByRole('button', { name: 'Cancel' }).click();
  await expect(page.getByRole('heading', { name: /^Properties/ })).toBeVisible();
});

test('A2: the browser checks as the person types, without a request', async ({ page }) => {
  const form = await openEdit(page, 'omar');
  let checks = 0;
  page.on('request', (r) => {
    if (r.url().includes('/$/forms/') && r.method() === 'POST') checks++;
  });
  const email = form.getByLabel('Email', { exact: true });
  await email.fill('omar@example');
  const problem = form.getByText(/must match/);
  await expect(problem).toBeVisible();
  await expect(email).toHaveAttribute('aria-invalid', 'true');
  const describedBy = (await email.getAttribute('aria-describedby')) ?? '';
  expect(describedBy).toMatch(/problems/);
  await expect(form.getByText(/^1 problem · 1 change/)).toBeVisible();
  await form.getByLabel('Age', { exact: true }).fill('12');
  await expect(form.getByText('must be at least 18')).toBeVisible();
  await expect(form.getByText(/^2 problems · 2 changes/)).toBeVisible();
  expect(checks).toBe(0);
});

test('A3: Preview shows the server check under its field', async ({ page }) => {
  const form = await openEdit(page, 'valentina');
  const works = form.getByRole('combobox', { name: 'Works for' });
  // clear the employer and pick a city instead, which the browser cannot check
  await form.getByRole('button', { name: 'Clear Works for' }).click();
  await works.fill(`<${W}lima>`);
  await page.getByRole('option', { name: /Use/ }).click();
  await form.getByRole('button', { name: 'Preview' }).click();
  const preview = form.getByRole('region', { name: 'Preview' });
  await expect(preview).toContainText('The save would commit.');
  await expect(preview.getByLabel('Changes the save would make')).toContainText('worksFor');
  await expect(form.locator('[data-field$="worksFor>"]')).toContainText(
    'Value does not have class',
  );
});

test('A4: saves go to a session branch while main is protected', async ({ page, request }) => {
  const p = await request.patch(`/$/branches/${DS}/main`, { data: { protected: true } });
  expect(p.ok()).toBe(true);
  try {
    const form = await openEdit(page, 'sofia');
    await expect(form.getByRole('note').filter({ hasText: /main is protected/ })).toContainText(
      'edits.anonymous.',
    );
    await form.getByLabel('Email', { exact: true }).fill('sofia@andes.example');
    await form.getByLabel('Message', { exact: true }).fill('add email');
    await form.getByRole('button', { name: 'Save' }).click();
    await expect(page.getByText(/Saved commit \d+ to branch edits\.anonymous\./)).toBeVisible();
    // the form now edits the branch and links to its review
    const banner = form.getByRole('status').filter({ hasText: 'Editing on branch' });
    await expect(banner).toContainText('1 commit');
    const review = banner.getByRole('link', { name: 'Review and merge' });
    await expect(review).toHaveAttribute('href', /\/merge\?source=edits\.anonymous\./);
    await expect(form.getByLabel('Email', { exact: true })).toHaveValue('sofia@andes.example');
    await expect(page).toHaveURL(/branch=edits\.anonymous\./);
    // main is unchanged
    const r = await request.get(
      `/${DS}/sparql?query=${encodeURIComponent(`ASK { <${W}sofia> <${W}email> ?e }`)}`,
      { headers: { Accept: 'application/sparql-results+json' } },
    );
    expect((await r.json()).boolean).toBe(false);
  } finally {
    await request.patch(`/$/branches/${DS}/main`, { data: { protected: false } });
  }
});

test('A5: a stale form gets the conflict view and resolves it', async ({ page, request }) => {
  const form = await openEdit(page, 'amira');
  // another writer changes the age after the form loaded
  await update(
    request,
    'DELETE DATA { ex:amira ex:age "47"^^xsd:integer } ; INSERT DATA { ex:amira ex:age "48"^^xsd:integer }',
  );
  await form.getByLabel('Age', { exact: true }).fill('50');
  await form.getByRole('button', { name: 'Save' }).click();
  const conflicts = form.getByRole('region', { name: 'Conflicting changes' });
  await expect(conflicts).toContainText('You loaded');
  await expect(conflicts).toContainText('47');
  await expect(conflicts).toContainText('48');
  await expect(conflicts).toContainText('50');
  const summary = form.getByRole('alert').filter({ hasText: /problem/ });
  await expect(summary).toBeFocused();
  await conflicts.getByRole('button', { name: 'Keep mine' }).click();
  await expect(conflicts).toHaveCount(0);
  await form.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
  await expect(form.getByLabel('Age', { exact: true })).toHaveValue('50');
});

test('A9: the error summary links each problem to its field', async ({ page }) => {
  const form = await openEdit(page, 'omar');
  const email = form.getByLabel('Email', { exact: true });
  await email.fill('broken');
  await form.getByRole('button', { name: 'Save' }).click();
  // a guard in warn mode or none: the save asks first, and the summary lists the problem
  const summary = form.getByRole('alert').filter({ hasText: /There is 1 problem/ });
  await expect(summary).toBeFocused();
  await expect(form.getByRole('button', { name: 'Save with problems' })).toBeVisible();
  await page.keyboard.press('Tab');
  const link = summary.getByRole('link', { name: /Email: must match/ });
  await expect(link).toBeFocused();
  // the input's accessible description carries its problem
  await expect(email).toHaveAccessibleDescription(/must match/);
  await page.keyboard.press('Enter');
  await expect(email).toBeFocused();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.type('omar@meridian.example');
  await expect(form.getByText(/^0 problems · 1 change/)).toBeVisible();
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
});

test('a resource no shape describes says so', async ({ page }) => {
  await page.goto(`/ui/datasets/${DS}/edit?iri=${encodeURIComponent(`${W}route1`)}`);
  await expect(page.getByText('No shape describes this resource.')).toBeVisible({
    timeout: 30_000,
  });
  // and Explore offers no Edit
  await page.goto(`/ui/explore?ds=${DS}&iri=${encodeURIComponent(`${W}route1`)}`);
  await expect(page.getByRole('button', { name: 'Describe' })).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0);
});

test('the edit page fits a phone', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openEdit(page, 'amira');
  await expectNoSidewaysScroll(page, 'the edit page');
});

test('A8: a DASH editor makes Notes a text area', async ({ page }) => {
  const form = await openEdit(page, 'amira');
  const notes = form.getByLabel('Notes', { exact: true });
  await expect(notes).toBeVisible();
  expect(await notes.evaluate((e) => e.tagName)).toBe('TEXTAREA');
});

test('the entity picker offers instances of the field class only', async ({ page }) => {
  const form = await openEdit(page, 'omar');
  await form.getByRole('button', { name: 'Clear Works for' }).click();
  await form.getByRole('combobox', { name: 'Works for' }).fill('an');
  // Andes Cargo is a company; nothing else whose label holds "an" (Dubai, Omar Rahman…) is
  const list = page.getByRole('listbox', { name: 'Works for choices' });
  await expect(list.getByRole('option', { name: /Andes Cargo/ })).toBeVisible();
  await expect(list.getByRole('option', { name: /Rahman/ })).toHaveCount(0);
  await list.getByRole('option', { name: /Andes Cargo/ }).click();
  await expect(form.getByText(/^0 problems · 1 change/)).toBeVisible();
});

test('New instance in the schema tab makes a resource with a minted IRI', async ({ page }) => {
  await page.goto(`/ui/explore?ds=${DS}&tab=schema`);
  await page
    .getByRole('treeitem', { name: /Person/ })
    .first()
    .click();
  const add = page.getByRole('link', { name: 'New instance' });
  await expect(add).toBeVisible({ timeout: 30_000 });
  await add.click();
  const form = page.getByRole('region', { name: /^New ex:Person|^New http/ });
  await expect(form.getByRole('group', { name: 'Identity' })).toBeVisible({ timeout: 30_000 });
  await form.getByRole('textbox', { name: 'Name', exact: true }).fill('Lucía Moreno');
  await form.getByRole('combobox', { name: 'Works for' }).fill(`<${W}andes>`);
  await page.getByRole('option', { name: /Use/ }).click();
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page).toHaveURL(/iri=urn%3Auuid%3A/);
  const edit = page.getByRole('region', { name: /^Edit / });
  await expect(edit.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue(
    'Lucía Moreno',
  );
  await expect(edit).toContainText('Person');
});

const openCompany = async (page: Page, which: string) => {
  await page.goto(`/ui/datasets/${DS}/edit?iri=${encodeURIComponent(W + which)}`);
  const form = page.getByRole('region', { name: /^Edit / });
  await expect(form.locator(`[data-field="<${W}sector>"]`)).toBeVisible({ timeout: 30_000 });
  return form;
};

test('an empty scalar list of more than six values is a select that sets the value', async ({
  page,
}) => {
  const form = await openCompany(page, 'meridian');
  const sector = form.getByLabel('Sector', { exact: true });
  await expect(sector).toHaveJSProperty('tagName', 'SELECT');
  await sector.selectOption({ label: 'Sea' });
  await expect(form.getByText(/^0 problems · 1 change/)).toBeVisible();
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
  await expect(form.getByLabel('Sector', { exact: true })).toHaveValue(/Sea/);
});

test('a value in a graph the person cannot write is shown read-only', async ({ page }) => {
  const form = await openCompany(page, 'andes');
  const offices = form.locator(`[data-field="<${W}hasOffice>"]`);
  await expect(offices.getByText('in a graph you cannot write')).toBeVisible();
  await expect(offices.getByRole('button', { name: /^Remove/ })).toHaveCount(0);
  await expect(offices.getByRole('button', { name: /Add Offices/ })).toBeEnabled();
});

test('the fields wait while a save is in flight', async ({ page }) => {
  let release = () => {};
  const held = new Promise<void>((r) => (release = r));
  await page.route(`**/$/forms/${DS}/save*`, async (route) => {
    await held;
    await route.continue();
  });
  const form = await openEdit(page, 'valentina');
  const email = form.getByLabel('Email', { exact: true });
  await email.fill('valentina@andes.example');
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(email).toBeDisabled();
  await expect(form.getByLabel('Age', { exact: true })).toBeDisabled();
  release();
  await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible();
  await expect(email).toBeEnabled();
  await expect(email).toHaveValue('valentina@andes.example');
});
