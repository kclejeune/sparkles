// Watches on entities (ROADMAP item 619) against the mock's `uni` dataset: watching an
// entity and its subtree from the ontology editor's entity frame, a commit below it that
// the Watches page lists with a link to the member's History view, the top bar's unread
// count, marking read, stopping and removing, and the Watches page at phone width. The
// test works on its own branch and starts with no watches.

import { expect, test, type APIRequestContext } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';

async function newBranch(request: APIRequestContext, name: string) {
  const made = await request.post('/$/branches/uni', { data: { name } });
  expect(made.status()).toBe(201);
}

test.beforeEach(async ({ request }) => {
  expect((await request.delete('/$/watches')).status()).toBe(204);
});

test('watches a subtree from the entity frame and lists the commits that change it', async ({
  page,
  request,
}) => {
  const branch = 'watch-subtree';
  await newBranch(request, branch);
  await page.goto(
    `/ui/workbench/ontology?${new URLSearchParams({ ds: 'uni', branch, iri: `${UNI}Professor` })}`,
  );
  const button = page.getByRole('button', { name: 'Watch', exact: true });
  await expect(button).toBeVisible({ timeout: 30_000 });
  await expect(button).toHaveAttribute('aria-pressed', 'false');
  await button.click();
  const dialog = page.getByRole('dialog', { name: 'Watch this entity' });
  await dialog.getByLabel('Entity and subtree').check();
  await dialog.getByLabel('Levels below it').fill('0');
  await expect(dialog.getByText('A whole number from 1 to 1000')).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Watch', exact: true })).toBeDisabled();
  await dialog.getByLabel('Levels below it').fill('2');
  await expect(dialog).toContainText('mailed to bob@example.org');
  await dialog.getByRole('button', { name: 'Watch', exact: true }).click();
  await expect(dialog).toBeHidden();
  const watching = page.getByRole('button', { name: 'Watching', exact: true });
  await expect(watching).toHaveAttribute('aria-pressed', 'true');

  // a new class below Professor, then a label on it
  for (const u of [
    `INSERT DATA { <${UNI}Emeritus> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <${UNI}Professor> }`,
    `INSERT DATA { <${UNI}Emeritus> <http://www.w3.org/2000/01/rdf-schema#label> "Emeritus" }`,
  ]) {
    const r = await request.post(`/uni@${branch}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: u,
    });
    expect(r.ok(), await r.text()).toBe(true);
  }

  await page.goto('/ui/watches');
  const top = page.getByRole('link', { name: 'Watches, 2 unread' });
  await expect(top).toBeVisible({ timeout: 70_000 });
  const row = page.getByRole('row').filter({ hasText: `${UNI}Professor` });
  await expect(row).toContainText('Subtree, 2 levels');
  await expect(row).toContainText(`uni@${branch}`);
  await row.getByRole('button', { name: `${UNI}Professor` }).click();
  const commits = page.getByRole('list', { name: 'Matched commits' });
  await expect(commits.getByRole('listitem')).toHaveCount(2);
  const member = commits.getByRole('link', { name: `${UNI}Emeritus` }).first();
  await expect(member).toHaveAttribute('href', /view=history/);
  await expect(member).toHaveAttribute('href', new RegExp(`branch=${branch}`));

  await row.getByRole('button', { name: 'Mark read' }).click();
  await expect(row.getByRole('button', { name: 'Mark read' })).toBeDisabled();
  await expect(page.getByRole('link', { name: 'Watches', exact: true })).toBeVisible();
  await expect(commits.locator('.badge.unread')).toHaveCount(0);

  await row.getByRole('button', { name: 'Remove' }).click();
  await expect(page.getByText('You watch no entities.')).toBeVisible();
});

test('stops watching from the entity frame', async ({ page, request }) => {
  const branch = 'watch-stop';
  await newBranch(request, branch);
  await page.goto(
    `/ui/workbench/ontology?${new URLSearchParams({ ds: 'uni', branch, iri: `${UNI}Student` })}`,
  );
  await page.getByRole('button', { name: 'Watch', exact: true }).click({ timeout: 30_000 });
  const dialog = page.getByRole('dialog', { name: 'Watch this entity' });
  await dialog.getByRole('button', { name: 'Watch', exact: true }).click();
  await page.getByRole('button', { name: 'Watching', exact: true }).click();
  const current = dialog.getByRole('list', { name: 'Your watches on this entity' });
  await expect(current).toContainText('Entity');
  await current.getByRole('button', { name: 'Stop watching' }).click();
  await expect(current).toBeHidden();
  await dialog.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByRole('button', { name: 'Watch', exact: true })).toHaveAttribute(
    'aria-pressed',
    'false',
  );
});

test('a slow answer for one watch never shows under another', async ({ page, request }) => {
  const branch = 'watch-race';
  await newBranch(request, branch);
  const ids: string[] = [];
  for (const entity of [`${UNI}Professor`, `${UNI}Student`]) {
    const r = await request.post('/$/watches', { data: { dataset: 'uni', branch, entity } });
    ids.push((await r.json()).id);
  }
  for (const entity of ['Professor', 'Student']) {
    const r = await request.post(`/uni@${branch}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `INSERT DATA { <${UNI}${entity}> <http://www.w3.org/2000/01/rdf-schema#comment> "c" }`,
    });
    expect(r.ok(), await r.text()).toBe(true);
  }
  // the first watch's matches answer only after the second's
  await page.route(`**/$/watches/${ids[0]}/matches*`, async (route) => {
    await new Promise((r) => setTimeout(r, 1500));
    await route.continue();
  });
  await page.goto('/ui/watches');
  await page.getByRole('button', { name: `${UNI}Professor` }).click();
  await page.getByRole('button', { name: `${UNI}Student` }).click();
  const commits = page.getByRole('list', { name: 'Matched commits' });
  await expect(commits.getByRole('link', { name: `${UNI}Student` })).toBeVisible();
  await page.waitForTimeout(2000);
  await expect(commits.getByRole('link', { name: `${UNI}Professor` })).toHaveCount(0);
  await expect(commits.getByRole('listitem')).toHaveCount(1);
});

test('the Watches page fits a phone', async ({ page, request }) => {
  await request.post('/$/watches', {
    data: { dataset: 'uni', entity: `${UNI}Professor`, scope: 'subtree' },
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/ui/watches');
  await expect(page.getByRole('heading', { name: 'Watches' })).toBeVisible();
  await expectNoSidewaysScroll(page, 'the Watches page');
});
