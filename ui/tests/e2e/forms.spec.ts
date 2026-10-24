// Editing forms generated from SHACL shapes against a real server
// (docs/specs/U02-shape-driven-forms.md §7): the acceptance examples A1 to A5, A7 and A9
// with the spec's shapes installed as a `reject` guard. A6, the fields a caller's grants
// hide or protect, needs users with protections and is covered by the server's router
// tests (crates/sparkles-server/src/http/router_tests/auth/forms.rs).

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

const EX = 'http://example.org/';

const SHAPES = `@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <${EX}> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:name "Name" ; sh:minCount 1 ; sh:datatype xsd:string ; sh:order 1 ] ;
  sh:property [ sh:path ex:email ; sh:name "Email" ; sh:maxCount 1 ;
                sh:pattern "^[^@]+@[^@]+\\\\.[a-z]+$" ; sh:order 2 ] ;
  sh:property [ sh:path ex:worksFor ; sh:name "Works for" ; sh:class ex:Org ; sh:minCount 1 ; sh:order 3 ] ;
  sh:property [ sh:path ex:role ; sh:name "Role" ; sh:in ( "Engineer" "Manager" "Other" ) ; sh:order 4 ] .`;

const DATA = `PREFIX ex: <${EX}> INSERT DATA {
  ex:acme a ex:Org ; ex:name "Acme Corp" .
  ex:alice a ex:Person ; ex:name "Alice Lima" ; ex:email "alice@acme.example" ;
    ex:worksFor ex:acme ; ex:nickname "Al" .
  ex:bob a ex:Person ; ex:name "Bob Stone" ; ex:worksFor ex:acme .
}`;

/** A dataset with the data and the guard, one per test so that tests run in parallel. */
async function people(request: APIRequestContext, name: string) {
  const made = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  expect(made.ok(), await made.text()).toBe(true);
  const r = await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: DATA,
  });
  expect(r.ok(), await r.text()).toBe(true);
  const g = await request.put(`/$/validation/${name}`, {
    data: { mode: 'reject', shapes: { inline: SHAPES } },
  });
  expect(g.ok(), await g.text()).toBe(true);
}

async function openForm(page: Page, ds: string, who = 'alice', branch?: string) {
  const q = new URLSearchParams({ iri: EX + who });
  if (branch) q.set('branch', branch);
  await page.goto(`/ui/datasets/${ds}/edit?${q}`);
  const form = page.getByRole('region', { name: /^Edit / });
  await expect(form.getByText('Name', { exact: true })).toBeVisible({ timeout: 30_000 });
  return form;
}

const ask = async (request: APIRequestContext, target: string, query: string) => {
  const r = await request.get(`/${target}/sparql?query=${encodeURIComponent(query)}`, {
    headers: { Accept: 'application/sparql-results+json' },
  });
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).boolean as boolean;
};

open(
  'A1 and A7: Explore offers Edit and lays the form out by the shapes',
  async ({ page, request }) => {
    const ds = 'forms-a1';
    await people(request, ds);
    await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
    await page.goto(`/ui/explore?ds=${ds}&iri=${encodeURIComponent(`${EX}alice`)}`);
    const edit = page.getByRole('button', { name: 'Edit', exact: true });
    await expect(edit).toBeVisible({ timeout: 30_000 });
    await edit.click();
    const form = page.getByRole('region', { name: /^Edit / });
    await expect(form.locator('.ff-label')).toHaveText(['Name', 'Email', 'Works for', 'Role']);
    const fields = form.locator('.ff');
    await expect(fields.nth(0)).toContainText('required');
    await expect(fields.nth(1)).not.toContainText('required');
    await expect(fields.nth(2)).toContainText('required');
    await expect(form.getByRole('radiogroup', { name: 'Role' }).getByRole('radio')).toHaveCount(3);
    await expect(form.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue(
      'Alice Lima',
    );
    // A7: the nickname no shape covers is listed, read-only
    const other = form.getByRole('group', { name: /Other properties/ });
    await expect(other).toContainText('nickname');
    await expect(other).toContainText('Al');
    await expect(other).toContainText('read-only');
  },
);

open('A2: the pattern problem shows at once, without a request', async ({ page, request }) => {
  const ds = 'forms-a2';
  await people(request, ds);
  const form = await openForm(page, ds);
  let posts = 0;
  page.on('request', (r) => {
    if (r.url().includes('/$/forms/') && r.method() === 'POST') posts++;
  });
  await form.getByLabel('Email', { exact: true }).fill('alice@example');
  await expect(form.getByText(/must match/)).toBeVisible();
  await expect(form.getByText(/^1 problem · 1 change/)).toBeVisible();
  expect(posts).toBe(0);
});

open('A3: Preview shows the sh:class result under Works for', async ({ page, request }) => {
  const ds = 'forms-a3';
  await people(request, ds);
  const form = await openForm(page, ds);
  await form.getByRole('button', { name: 'Clear Works for' }).click();
  await form.getByRole('combobox', { name: 'Works for' }).fill(`<${EX}bob>`);
  await page.getByRole('option', { name: /Use/ }).click();
  await form.getByRole('button', { name: 'Preview' }).click();
  const preview = form.getByRole('region', { name: 'Preview' });
  await expect(preview).toContainText('The write-time guard would reject the save.');
  const works = form.locator(`[data-field="<${EX}worksFor>"]`);
  await expect(works).toContainText('server check');
  // the guard rejects it, so Save does not write
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(form.getByRole('alert').filter({ hasText: /would reject/ })).toBeVisible();
  expect(await ask(request, ds, `ASK { <${EX}alice> <${EX}worksFor> <${EX}bob> }`)).toBe(false);
});

open('A4: with main protected, a save creates the session branch', async ({ page, request }) => {
  const ds = 'forms-a4';
  await people(request, ds);
  const p = await request.patch(`/$/branches/${ds}/main`, { data: { protected: true } });
  expect(p.ok(), await p.text()).toBe(true);
  const form = await openForm(page, ds);
  await expect(form.getByRole('note').filter({ hasText: /main is protected/ })).toBeVisible();
  await form.getByLabel('Email', { exact: true }).fill('alice@lima.example');
  await form.getByLabel('Message', { exact: true }).fill('fix email');
  await form.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText(/Saved commit \d+ to branch edits\./)).toBeVisible({
    timeout: 15_000,
  });
  const banner = form.getByRole('status').filter({ hasText: 'Editing on branch' });
  const review = banner.getByRole('link', { name: 'Review and merge' });
  await expect(review).toHaveAttribute('href', /\/merge\?source=edits\.[^.]+\.\d{8}$/);
  const branch = decodeURIComponent((await review.getAttribute('href'))!.split('source=')[1]);
  // one commit with the message and an author on the branch, and main unchanged
  expect(
    await ask(request, `${ds}@${branch}`, `ASK { <${EX}alice> <${EX}email> "alice@lima.example" }`),
  ).toBe(true);
  expect(await ask(request, ds, `ASK { <${EX}alice> <${EX}email> "alice@acme.example" }`)).toBe(
    true,
  );
  const commits = await (await request.get(`/$/commits/${ds}?branch=${branch}&limit=1`)).json();
  expect(commits.commits[0].message).toBe('fix email');
  await review.click();
  await expect(page).toHaveURL(/\/merge\?source=edits\./);
});

open(
  'A5: a stale form gets the conflict view, and another field still saves',
  async ({ browser, request }) => {
    const ds = 'forms-a5';
    await people(request, ds);
    const one = await (
      await browser.newContext({ baseURL: process.env.SPARKLES_E2E_OPEN_URL })
    ).newPage();
    const two = await (
      await browser.newContext({ baseURL: process.env.SPARKLES_E2E_OPEN_URL })
    ).newPage();
    const f1 = await openForm(one, ds);
    const f2 = await openForm(two, ds);
    await f1.getByLabel('Email', { exact: true }).fill('alice@one.example');
    await f1.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(one.getByText(/Saved commit \d+ to main/)).toBeVisible({ timeout: 15_000 });
    await f2.getByLabel('Email', { exact: true }).fill('alice@two.example');
    await f2.getByRole('button', { name: 'Save', exact: true }).click();
    const conflicts = f2.getByRole('region', { name: 'Conflicting changes' });
    await expect(conflicts).toContainText('alice@acme.example');
    await expect(conflicts).toContainText('alice@one.example');
    await expect(conflicts).toContainText('alice@two.example');
    await conflicts.getByRole('button', { name: 'Take the stored values' }).click();
    await expect(f2.getByLabel('Email', { exact: true })).toHaveValue('alice@one.example');
    await f2.getByRole('radio', { name: 'Manager' }).check();
    await f2.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(two.getByText(/Saved commit \d+ to main/)).toBeVisible({ timeout: 15_000 });
    expect(
      await ask(
        request,
        ds,
        `ASK { <${EX}alice> <${EX}email> "alice@one.example" ; <${EX}role> "Manager" }`,
      ),
    ).toBe(true);
    await one.context().close();
    await two.context().close();
  },
);

open(
  'A9: the keyboard alone fixes a problem from the summary and saves',
  async ({ page, request }) => {
    const ds = 'forms-a9';
    await people(request, ds);
    const form = await openForm(page, ds);
    const email = form.getByLabel('Email', { exact: true });
    await email.focus();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.type('alice@example');
    // Enter submits the form: the save checks first, and the guard would reject it
    await page.keyboard.press('Enter');
    const summary = form.getByRole('alert').filter({ hasText: /problem/ });
    await expect(summary).toBeFocused({ timeout: 15_000 });
    await page.keyboard.press('Tab');
    await expect(summary.getByRole('link', { name: /^Email: must match/ })).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(email).toBeFocused();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.type('alice@example.org');
    await page.keyboard.press('Enter');
    await expect(page.getByText(/Saved commit \d+ to main/)).toBeVisible({ timeout: 15_000 });
    expect(await ask(request, ds, `ASK { <${EX}alice> <${EX}email> "alice@example.org" }`)).toBe(
      true,
    );
    // every input of the form has an accessible name
    const unnamed = await form.locator('input, select, textarea').evaluateAll(
      (els) =>
        els.filter((e) => {
          const el = e as HTMLInputElement;
          if (el.type === 'hidden') return false;
          const byLabel = el.labels && el.labels.length > 0;
          return !byLabel && !el.getAttribute('aria-label') && !el.getAttribute('aria-labelledby');
        }).length,
    );
    expect(unnamed).toBe(0);
  },
);
