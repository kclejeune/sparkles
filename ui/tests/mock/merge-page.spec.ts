// The merge page and the History panel's graph against the mock. The merge page lists the
// conflicts of a merge grouped by graph and subject, takes a choice per row or per
// subject, lists the changes those choices make, and merges with the heads it showed.
// A branch that moved meanwhile makes it read the branches again, and a refusal of the
// target's write guard shows the guard's report. The graph draws every branch with its
// forks and merges, collapses long runs and shows a commit's changes.
// The tests work on datasets of their own, `orchard` and `forest`.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

test.describe.configure({ timeout: 90_000 });

const update = (request: APIRequestContext, target: string, sparql: string) =>
  request
    .post(`/${target}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: sparql,
    })
    .then((r) => expect(r.ok(), `update on ${target}`).toBe(true));

const swap = (s: string, p: string, from: number, to: number) =>
  `DELETE DATA { <urn:${s}> <urn:${p}> ${from} } ; INSERT DATA { <urn:${s}> <urn:${p}> ${to} }`;

const SHAPES = `@prefix sh: <http://www.w3.org/ns/shacl#> .
<urn:TreeShape> a sh:NodeShape ; sh:targetClass <urn:Tree> ;
  sh:property [ sh:path <urn:species> ; sh:minCount 1 ] .`;

/** A persistent dataset `name` with `<urn:a>` and `<urn:b>`, and a branch `dev` of it. */
async function dataset(request: APIRequestContext, name: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  expect(r.ok()).toBe(true);
  await update(
    request,
    name,
    'INSERT DATA { <urn:a> <urn:height> 1 ; <urn:age> 1 . <urn:b> <urn:height> 1 }',
  );
  const b = await request.post(`/$/branches/${name}`, { data: { name: 'dev' } });
  expect(b.status()).toBe(201);
}

const mainHead = async (request: APIRequestContext, ds: string) =>
  (await (await request.get(`/$/branches/${ds}/main`)).json()).head as number;

const conflicts = (page: Page) => page.getByRole('region', { name: 'Conflicts' });
const changes = (page: Page) => page.getByRole('region', { name: 'Changes' });

test('the merge page resolves conflicts per row and per subject, then merges', async ({
  page,
  request,
}) => {
  await dataset(request, 'orchard');
  await update(request, 'orchard@dev', swap('a', 'height', 1, 3));
  await update(request, 'orchard@dev', swap('a', 'age', 1, 3));
  await update(request, 'orchard@dev', swap('b', 'height', 1, 3));
  await update(request, 'orchard', swap('a', 'height', 1, 2));
  await update(request, 'orchard', swap('a', 'age', 1, 2));
  await update(request, 'orchard', swap('b', 'height', 1, 2));

  await page.goto('/ui/datasets/orchard/merge?source=dev&target=main');
  const summary = page.getByRole('region', { name: 'Summary' });
  await expect(summary).toContainText('dev@', { timeout: 30_000 });
  await expect(summary).toContainText('main@');
  await expect(summary).toContainText('Merge base');
  await expect(conflicts(page)).toContainText('Default graph · 3 conflicts');
  await expect(page.getByRole('button', { name: 'Merge into main' })).toBeDisabled();
  await expect(conflicts(page)).toContainText('3 conflicts still open');

  // a subject's rows at once, and one row against its group
  await page.getByRole('combobox', { name: 'Take for <urn:a>' }).selectOption('theirs');
  await expect(conflicts(page)).toContainText('1 conflict still open');
  await page
    .getByRole('radiogroup', { name: 'Take for <urn:b> <urn:height>' })
    .getByRole('radio', { name: 'Both' })
    .click();
  await expect(conflicts(page)).toContainText('Every conflict has a choice');

  // the changes those choices make
  const lines = changes(page).getByLabel('Changes of the merge');
  await expect(lines).toContainText('− <urn:a> <urn:height> "2"^^');
  await expect(lines).toContainText('+ <urn:a> <urn:height> "3"^^');
  await expect(lines).toContainText('+ <urn:b> <urn:height> "3"^^');
  await expect(lines).not.toContainText('− <urn:b> <urn:height>');

  const before = await mainHead(request, 'orchard');
  await page.getByRole('button', { name: 'Merge into main' }).click();
  await expect(page.getByText('Merged dev into main')).toBeVisible();
  await expect(page).toHaveURL(/\/ui\/datasets\/orchard$/);
  expect(await mainHead(request, 'orchard')).toBe(before + 1);
  const ask = async (q: string) =>
    (
      await (
        await request.get(`/orchard/sparql?query=${encodeURIComponent(q)}`, {
          headers: { Accept: 'application/sparql-results+json' },
        })
      ).json()
    ).boolean;
  expect(await ask('ASK { <urn:a> <urn:height> 3 ; <urn:age> 3 }')).toBe(true);
  expect(await ask('ASK { <urn:b> <urn:height> 2 , 3 }')).toBe(true);
});

test('a branch that moved makes the page read it again', async ({ page, request }) => {
  await dataset(request, 'grove');
  await update(request, 'grove@dev', swap('a', 'height', 1, 3));
  await update(request, 'grove', swap('a', 'height', 1, 2));
  await page.goto('/ui/datasets/grove/merge?source=dev&target=main');
  await page
    .getByRole('radiogroup', { name: 'Take for <urn:a> <urn:height>' })
    .getByRole('radio', { name: 'Ours' })
    .click();
  await expect(conflicts(page)).toContainText('Every conflict has a choice');
  const before = await mainHead(request, 'grove');
  await update(request, 'grove', 'INSERT DATA { <urn:c> <urn:height> 7 }');
  await page.getByRole('button', { name: 'Merge into main' }).click();
  await expect(page.getByRole('status').first()).toContainText('changed since the page read them');
  await expect(page.getByRole('region', { name: 'Summary' })).toContainText(`main@${before + 1}`);
  expect(await mainHead(request, 'grove')).toBe(before + 1);
  // the choice still holds for the conflict, so the merge goes through now
  await page.getByRole('button', { name: 'Merge into main' }).click();
  await expect(page.getByText('Merged dev into main')).toBeVisible();
});

test("the target's guard refuses a merge, and the page shows its report", async ({
  page,
  request,
}) => {
  await dataset(request, 'copse');
  await update(request, 'copse@dev', 'INSERT DATA { <urn:oak> a <urn:Tree> }');
  const g = await request.put('/$/validation/copse', {
    data: { mode: 'reject', shapes: { inline: SHAPES } },
  });
  expect(g.ok()).toBe(true);
  const before = await mainHead(request, 'copse');
  await page.goto('/ui/datasets/copse/merge?source=dev&target=main');
  // the dry run warns before the merge
  await expect(changes(page).getByRole('alert')).toContainText(
    "main's validation would refuse this result",
  );
  await page.getByRole('button', { name: 'Merge into main' }).click();
  const refused = page.getByRole('region', { name: 'Merge' }).getByRole('alert');
  await expect(refused).toContainText("main's validation refused the merge");
  await expect(refused).toContainText('1 blocking result');
  const results = refused.getByRole('table', { name: 'Validation results' });
  await expect(results).toContainText('<urn:oak>');
  await expect(results).toContainText('<urn:species>');
  await expect(results).toContainText('<urn:TreeShape>');
  expect(await mainHead(request, 'copse')).toBe(before);
  await expect(page).toHaveURL(/\/merge\?/);
});

test('the History graph draws every branch with its forks and merges', async ({
  page,
  request,
}) => {
  await dataset(request, 'forest');
  for (let i = 0; i < 6; i++)
    await update(request, 'forest@dev', `INSERT DATA { <urn:d${i}> <urn:p> ${i} }`);
  await request.post('/$/branches/forest', { data: { name: 'fix', from: 'dev' } });
  await update(request, 'forest@fix', 'INSERT DATA { <urn:f> <urn:p> 1 }');
  await update(request, 'forest', 'INSERT DATA { <urn:m> <urn:p> 1 }');
  const m = await request.post('/$/merge/forest', { data: { source: 'dev' } });
  expect(m.ok()).toBe(true);

  await page.goto('/ui/datasets/forest');
  const panel = page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'History', exact: true }),
  });
  const toggle = panel.getByRole('button', { name: 'Graph' });
  await expect(toggle).toHaveAttribute('aria-pressed', 'false', { timeout: 30_000 });
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await expect(panel).toContainText('commits of 3 branches');
  // the merge commit, and the heads named by their branches
  const merge = panel.getByRole('button', { name: /^Commit \d+ on main$/ }).first();
  await expect(merge).toContainText('merge');
  await expect(panel.locator('.headlabel', { hasText: 'fix' })).toBeVisible();
  await expect(panel.locator('svg.lines path.edge.merge')).toHaveCount(1);
  await expect(panel.locator('svg.lines path.edge.fork')).toHaveCount(2);
  // dev's run of commits is one segment, which expands
  const seg = panel.getByRole('button', { name: /commits dev/ });
  await expect(seg).toContainText('5 commits');
  await seg.click();
  await expect(panel.getByRole('button', { name: /^Commit \d+ on dev$/ })).toHaveCount(6);
  await panel.getByRole('button', { name: 'Collapse the run of dev' }).click();
  await expect(panel.getByRole('button', { name: /^Commit \d+ on dev$/ })).toHaveCount(1);

  // a commit's changes, and a link to query at it
  await panel.getByRole('button', { name: /^Commit \d+ on fix$/ }).click();
  const detail = panel.getByRole('region', { name: /^Commit \d+ on fix$/ });
  await expect(detail.getByLabel('Changes')).toContainText('+ <urn:f> <urn:p> "1"^^');
  await detail.getByRole('link', { name: /^Query at commit/ }).click();
  await expect(page).toHaveURL(/\/ui\/query$/);
  await expect(page.getByRole('combobox', { name: 'Branch' })).toHaveValue('fix');

  // the choice of view is kept
  await page.goto('/ui/datasets/forest');
  await expect(panel.getByRole('button', { name: 'Graph' })).toHaveAttribute(
    'aria-pressed',
    'true',
    { timeout: 30_000 },
  );
});
