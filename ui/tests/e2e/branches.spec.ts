// Branches and merges against the server without auth, on persistent datasets of their own
// (in-memory datasets have no branches): a branch made in the Branches panel, an update on
// it through the query page's Branch field, the Merge button, and the merge commit in
// History. A merge with conflicts shows the server's report and merges nothing.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

async function dataset(request: APIRequestContext, name: string, data: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  await update(request, name, `INSERT DATA { ${data} }`);
}

async function update(request: APIRequestContext, target: string, sparql: string) {
  const r = await request.post(`/${target}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: sparql,
  });
  expect(r.ok(), `update on ${target}: ${await r.text()}`).toBe(true);
}

const panel = (page: Page) => page.getByRole('region', { name: 'Branches' });
const row = (page: Page, name: string) =>
  panel(page)
    .getByRole('row')
    .filter({ has: page.getByRole('cell').first().getByText(name, { exact: true }) });
const history = (page: Page) =>
  page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'History', exact: true }),
  });

open(
  'a branch made in the panel, changed in the query page and merged',
  async ({ page, request }) => {
    await dataset(request, 'branchy', '<urn:acct:1> <urn:balance> 100');
    await page.goto('/ui/datasets/branchy');
    await expect(row(page, 'main')).toBeVisible();
    await panel(page).getByRole('button', { name: 'New branch' }).click();
    const form = panel(page).getByRole('form', { name: 'New branch' });
    await form.getByLabel('Name').fill('feature');
    await form.getByLabel('Note').fill('a new account');
    await form.getByRole('button', { name: 'Create' }).click();
    await expect(page.getByText('Created branch feature')).toBeVisible();
    await expect(row(page, 'feature')).toContainText('a new account');
    await expect(row(page, 'feature')).toContainText('main@');

    // the query page writes to and reads from the branch
    await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'branchy'));
    await page.goto('/ui/query');
    await page.getByRole('combobox', { name: 'Branch' }).selectOption('feature');
    const editor = page.locator('.cm-content');
    await editor.click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.insertText('INSERT DATA { <urn:acct:2> <urn:balance> 42 }');
    await page.getByRole('button', { name: 'Run update' }).click();
    const results = page.getByRole('region', { name: 'Results' });
    await expect(results).toContainText('on the branch feature');
    await editor.click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.insertText('SELECT ?b WHERE { <urn:acct:2> <urn:balance> ?b }');
    await page.getByRole('button', { name: /^Run\b/ }).click();
    await expect(results.getByRole('grid')).toContainText('42');
    await expect(results.locator('.badge.commit')).toHaveText(/^feature · at commit \d+$/);
    // main does not have it
    const onMain = await request.get(
      `/branchy/sparql?query=${encodeURIComponent('ASK { <urn:acct:2> ?p ?o }')}`,
      { headers: { Accept: 'application/sparql-results+json' } },
    );
    expect((await onMain.json()).boolean).toBe(false);

    // merge it into main with the Merge button
    await page.goto('/ui/datasets/branchy');
    await expect(row(page, 'feature')).toContainText('1 ahead, 0 behind');
    await row(page, 'feature').getByRole('button', { name: 'Merge feature' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog.getByRole('combobox', { name: 'Target branch' })).toHaveValue('main');
    await expect(dialog).toContainText('+1 −0');
    await dialog.getByRole('button', { name: 'Merge into main' }).click();
    await expect(page.getByText('Merged feature into main')).toBeVisible();
    await expect(dialog).toBeHidden();

    // History marks the merge commit and links to the merged one
    const top = history(page).getByRole('row').nth(1);
    await expect(top).toContainText('merge');
    await expect(top.getByRole('link', { name: /^from feature@\d+$/ })).toHaveAttribute(
      'href',
      '/ui/datasets/branchy?branch=feature',
    );
    const merged = await request.get(
      `/branchy/sparql?query=${encodeURIComponent('ASK { <urn:acct:2> ?p ?o }')}`,
      { headers: { Accept: 'application/sparql-results+json' } },
    );
    expect((await merged.json()).boolean).toBe(true);

    // the branch's own page lists the commits it shares with main
    await row(page, 'feature').getByRole('link', { name: 'feature' }).click();
    await expect(page).toHaveURL(/\/ui\/datasets\/branchy\?branch=feature$/);
    await expect(page.locator('.meta')).toContainText('/branchy@feature/sparql');
    await expect(history(page).getByRole('link', { name: 'main' }).first()).toBeVisible();
  },
);

open('a merge with conflicts shows the report and merges nothing', async ({ page, request }) => {
  await dataset(request, 'clashy', '<urn:acct:1> <urn:balance> 100');
  const created = await request.post('/$/branches/clashy', { data: { name: 'clash' } });
  expect(created.status(), await created.text()).toBe(201);
  const change = (to: number) =>
    `DELETE DATA { <urn:acct:1> <urn:balance> 100 } ; INSERT DATA { <urn:acct:1> <urn:balance> ${to} }`;
  await update(request, 'clashy@clash', change(150));
  await update(request, 'clashy', change(90));
  const head = async () => (await (await request.get('/$/branches/clashy/main')).json()).head;
  const before = await head();

  await page.goto('/ui/datasets/clashy');
  await row(page, 'clash').getByRole('button', { name: 'Merge clash' }).click();
  const dialog = page.getByRole('dialog');
  const report = dialog.getByRole('alert');
  await expect(report).toContainText('1 conflict.');
  const cell = report.getByRole('row').nth(1);
  await expect(cell).toContainText('<urn:acct:1> <urn:balance>');
  await expect(cell).toContainText('"100"^^xsd:integer');
  await expect(cell).toContainText('"90"^^xsd:integer');
  await expect(cell).toContainText('"150"^^xsd:integer');
  await expect(report).toContainText('--dataset clashy clash --into main --on-conflict theirs');
  await expect(dialog.getByRole('button', { name: 'Merge into main' })).toHaveCount(0);
  await dialog.getByRole('button', { name: 'Close', exact: true }).last().click();
  expect(await head()).toBe(before);

  // deleting the branch asks for a forced delete
  await row(page, 'clash').getByRole('button', { name: 'Delete branch clash' }).click();
  await expect(dialog).toContainText('clash has 1 commit that main does not have.');
  await dialog.getByRole('button', { name: 'Delete anyway' }).click();
  await expect(dialog).toBeHidden();
  await expect(row(page, 'clash')).toHaveCount(0);
});
