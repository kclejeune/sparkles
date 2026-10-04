// Branches and merges against the mock: the branch selector keeps the branch in the URL
// and every request of the dataset page names it, the Branches panel creates, protects
// and deletes branches, the Merge button merges a branch without conflicts and opens the
// merge page for one with conflicts, and History marks the merge commit.
// The tests work on a dataset of their own, `ledger`, which has a branch `dev` one commit
// ahead of main.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

test.describe.configure({ timeout: 90_000 });

const update = (request: APIRequestContext, target: string, sparql: string) =>
  request
    .post(`/${target}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: sparql,
    })
    .then((r) => expect(r.ok(), `update on ${target}`).toBe(true));

const panel = (page: Page) => page.getByRole('region', { name: 'Branches' });
const row = (page: Page, name: string) =>
  panel(page)
    .getByRole('row')
    .filter({ has: page.getByRole('cell').first().getByText(name, { exact: true }) });
const history = (page: Page) =>
  page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'History', exact: true }),
  });

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: 'ledger', dbType: 'persistent' } });
  // a worker that starts again after a failed test finds it made
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  await update(request, 'ledger', 'INSERT DATA { <urn:acct:1> <urn:balance> 100 }');
  const b = await request.post('/$/branches/ledger', { data: { name: 'dev', note: 'audit' } });
  expect(b.status()).toBe(201);
  await update(request, 'ledger@dev', 'INSERT DATA { <urn:acct:1> <urn:audited> true }');
});

test('the branch selector keeps the branch in the URL and in every request', async ({ page }) => {
  const asked: string[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    asked.push(u.pathname + u.search);
  });
  await page.goto('/ui/datasets/ledger');
  const select = page.getByRole('combobox', { name: 'Branch' });
  // the first page load compiles the UI in vite dev
  await expect(select).toHaveValue('main', { timeout: 30_000 });
  await expect(select.getByRole('option')).toHaveText(['main · commit 1', 'dev · commit 2']);

  await select.selectOption('dev');
  await expect(page).toHaveURL(/\/ui\/datasets\/ledger\?branch=dev$/);
  await expect(page.locator('.meta')).toContainText('/ledger@dev/sparql');
  await expect(page.locator('.meta')).toContainText('commit 2');
  // the commits dev shares with main name the branch that made them
  const commits = history(page).getByRole('row');
  await expect(commits.nth(1)).toContainText('SPARQL update');
  await expect(commits.nth(1).getByRole('link', { name: 'main' })).toHaveCount(0);
  await expect(commits.nth(2).getByRole('link', { name: 'main' })).toBeVisible();
  expect(asked).toContain('/$/stats/ledger?branch=dev');
  expect(asked).toContain('/$/commits/ledger?limit=20&branch=dev');
  expect(asked).toContain('/$/snapshots/ledger?branch=dev');

  // a link opens the same branch
  await page.reload();
  await expect(page.getByRole('combobox', { name: 'Branch' })).toHaveValue('dev');
  await expect(row(page, 'dev')).toHaveAttribute('aria-current', 'true');

  await page.getByRole('combobox', { name: 'Branch' }).selectOption('main');
  await expect(page).toHaveURL(/\/ui\/datasets\/ledger$/);
  await expect(page.locator('.meta')).toContainText('/ledger/sparql');
});

test('an in-memory dataset shows no branches', async ({ page }) => {
  await page.goto('/ui/datasets/scratch');
  await expect(page.getByRole('heading', { name: 'History' })).toBeVisible();
  await expect(page.getByRole('combobox', { name: 'Branch' })).toHaveCount(0);
  await expect(panel(page)).toHaveCount(0);
});

test('the Branches panel creates, protects and deletes branches', async ({ page, request }) => {
  await page.goto('/ui/datasets/ledger');
  await expect(row(page, 'main')).toBeVisible();

  await panel(page).getByRole('button', { name: 'New branch' }).click();
  const form = panel(page).getByRole('form', { name: 'New branch' });
  await form.getByLabel('Name').fill('main');
  await expect(form).toContainText('A branch named main exists.');
  await form.getByLabel('Name').fill('draft');
  await form.getByLabel('Note').fill('trying things');
  await form.getByRole('button', { name: 'Create' }).click();
  await expect(page.getByText('Created branch draft')).toBeVisible();
  await expect(row(page, 'draft')).toContainText('trying things');
  await expect(row(page, 'draft')).toContainText('main@1');
  await expect(row(page, 'draft')).toContainText('up to date');

  // protect, then unprotect
  const protect = row(page, 'draft').getByRole('button', { name: 'Protect draft' });
  await protect.click();
  await expect(protect).toHaveAttribute('aria-pressed', 'true');
  await expect(row(page, 'draft')).toContainText('protected');
  await protect.click();
  await expect(protect).toHaveAttribute('aria-pressed', 'false');
  await expect(row(page, 'draft')).not.toContainText('protected');

  // a branch with a commit main lacks asks for a forced delete
  await update(request, 'ledger@draft', 'INSERT DATA { <urn:acct:2> <urn:balance> 5 }');
  await panel(page).getByRole('button', { name: 'Reload branches' }).click();
  await expect(row(page, 'draft')).toContainText('1 ahead, 0 behind');
  await row(page, 'draft').getByRole('button', { name: 'Delete branch draft' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('draft has 1 commit that main does not have.');
  await dialog.getByRole('button', { name: 'Delete anyway' }).click();
  await expect(dialog).toBeHidden();
  await expect(row(page, 'draft')).toHaveCount(0);

  // the server's answer counts when the list is behind
  await request.post('/$/branches/ledger', { data: { name: 'late' } });
  await panel(page).getByRole('button', { name: 'Reload branches' }).click();
  await row(page, 'late').getByRole('button', { name: 'Delete branch late' }).click();
  await update(request, 'ledger@late', 'INSERT DATA { <urn:acct:3> <urn:balance> 7 }');
  await dialog.getByRole('button', { name: 'Delete late' }).click();
  await expect(dialog).toContainText('late has 1 commit that main does not have');
  await dialog.getByRole('button', { name: 'Delete anyway' }).click();
  await expect(dialog).toBeHidden();
  await expect(row(page, 'late')).toHaveCount(0);
});

test('an update on a branch through the query page, merged with the Merge button', async ({
  page,
  request,
}) => {
  await request.post('/$/branches/ledger', { data: { name: 'feature' } });
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'ledger'));
  await page.goto('/ui/query');
  const branch = page.getByRole('combobox', { name: 'Branch' });
  await branch.selectOption({ label: 'feature · commit 1' });
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText('INSERT DATA { <urn:acct:9> <urn:balance> 42 }');
  await page.getByRole('button', { name: 'Run update' }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results).toContainText('on the branch feature');

  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText('SELECT ?b WHERE { <urn:acct:9> <urn:balance> ?b }');
  await page.getByRole('button', { name: /^Run\b/ }).click();
  await expect(results.getByRole('grid')).toContainText('42');
  await expect(results.locator('.badge.commit')).toHaveText('feature · at commit 2');

  // merge it into main
  await page.goto('/ui/datasets/ledger');
  await row(page, 'feature').getByRole('button', { name: 'Merge feature' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('combobox', { name: 'Target branch' })).toHaveValue('main');
  await expect(dialog).toContainText('+1 −0');
  await expect(dialog).toContainText('fast-forward');
  await dialog.getByRole('button', { name: 'Merge into main' }).click();
  await expect(page.getByText('Merged feature into main')).toBeVisible();
  await expect(dialog).toBeHidden();

  // History marks the merge commit and links to the merged one
  const top = history(page).getByRole('row').nth(1);
  await expect(top).toContainText('merge');
  await expect(top.getByRole('link', { name: 'from feature@2' })).toHaveAttribute(
    'href',
    '/ui/datasets/ledger?branch=feature',
  );

  // a second merge has nothing to do
  await row(page, 'feature').getByRole('button', { name: 'Merge feature' }).click();
  await expect(dialog).toContainText('main already has every change of feature');
  await expect(dialog.getByRole('button', { name: 'Merge into main' })).toHaveCount(0);
  await dialog.getByRole('button', { name: 'Close', exact: true }).last().click();
});

test('a merge with conflicts opens the merge page, and merges nothing on its own', async ({
  page,
  request,
}) => {
  await request.post('/$/branches/ledger', { data: { name: 'clash' } });
  await update(
    request,
    'ledger@clash',
    'DELETE DATA { <urn:acct:1> <urn:balance> 100 } ; INSERT DATA { <urn:acct:1> <urn:balance> 150 }',
  );
  await update(
    request,
    'ledger',
    'DELETE DATA { <urn:acct:1> <urn:balance> 100 } ; INSERT DATA { <urn:acct:1> <urn:balance> 90 }',
  );
  const mainHead = async () => (await (await request.get('/$/branches/ledger/main')).json()).head;
  const head = await mainHead();
  await page.goto('/ui/datasets/ledger');
  await row(page, 'clash').getByRole('button', { name: 'Merge clash' }).click();
  await expect(page).toHaveURL(/\/ui\/datasets\/ledger\/merge\?source=clash&target=main$/);
  const conflicts = page.getByRole('region', { name: 'Conflicts' });
  await expect(conflicts).toContainText('<urn:acct:1>');
  await expect(conflicts).toContainText('"100"^^');
  await expect(conflicts).toContainText('"90"^^');
  await expect(conflicts).toContainText('"150"^^');
  await expect(page.getByRole('button', { name: 'Merge into main' })).toBeDisabled();
  const origin = new URL(page.url()).origin;
  await page.getByText('The same from the command line').click();
  await expect(page.locator('details.cli')).toContainText(
    `sparkles merge --server ${origin} --dataset ledger clash --into main --on-conflict theirs`,
  );
  expect(await mainHead()).toBe(head);
  await page.getByRole('link', { name: 'Cancel' }).click();
  await expect(page).toHaveURL(/\/ui\/datasets\/ledger$/);
  await expect(history(page).getByRole('row').nth(1)).not.toContainText('merge');
});
