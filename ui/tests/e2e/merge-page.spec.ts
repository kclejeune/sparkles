// The merge page and the History graph against the server without auth, on persistent
// datasets of their own. The target's write guard refuses a merge and the page shows the
// guard's report, a choice per subject resolves conflicts, and the graph draws every
// branch with its forks and merges and shows a commit's changes.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

async function update(request: APIRequestContext, target: string, sparql: string) {
  const r = await request.post(`/${target}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: sparql,
  });
  expect(r.ok(), `update on ${target}: ${await r.text()}`).toBe(true);
}

/** A persistent dataset with `<urn:a>` and `<urn:b>`, and a branch `dev` of it. */
async function dataset(request: APIRequestContext, name: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  await update(
    request,
    name,
    'INSERT DATA { <urn:a> <urn:height> 1 ; <urn:age> 1 . <urn:b> <urn:height> 1 }',
  );
  const b = await request.post(`/$/branches/${name}`, { data: { name: 'dev' } });
  expect(b.status(), await b.text()).toBe(201);
}

const swap = (s: string, p: string, from: number, to: number) =>
  `DELETE DATA { <urn:${s}> <urn:${p}> ${from} } ; INSERT DATA { <urn:${s}> <urn:${p}> ${to} }`;

const mainHead = async (request: APIRequestContext, ds: string) =>
  (await (await request.get(`/$/branches/${ds}/main`)).json()).head as number;

const ask = async (request: APIRequestContext, ds: string, q: string) =>
  (
    await (
      await request.get(`/${ds}/sparql?query=${encodeURIComponent(q)}`, {
        headers: { Accept: 'application/sparql-results+json' },
      })
    ).json()
  ).boolean as boolean;

const conflicts = (page: Page) => page.getByRole('region', { name: 'Conflicts' });

open(
  'a choice per subject resolves the conflicts, and the merge goes through',
  async ({ page, request }) => {
    await dataset(request, 'hedges');
    await update(request, 'hedges@dev', swap('a', 'height', 1, 3));
    await update(request, 'hedges@dev', swap('a', 'age', 1, 3));
    await update(request, 'hedges', swap('a', 'height', 1, 2));
    await update(request, 'hedges', swap('a', 'age', 1, 2));
    await page.goto('/ui/datasets/hedges/merge?source=dev&target=main');
    await expect(conflicts(page)).toContainText('Default graph · 2 conflicts');
    await expect(page.getByRole('region', { name: 'Summary' })).toContainText('main@1');
    await page.getByRole('combobox', { name: 'Take for <urn:a>' }).selectOption('theirs');
    await expect(conflicts(page)).toContainText('Every conflict has a choice');
    const lines = page.getByRole('region', { name: 'Changes' }).getByLabel('Changes of the merge');
    await expect(lines).toContainText('+ <urn:a> <urn:age> "3"^^');
    const before = await mainHead(request, 'hedges');
    await page.getByRole('button', { name: 'Merge into main' }).click();
    await expect(page.getByText('Merged dev into main')).toBeVisible({ timeout: 15_000 });
    expect(await mainHead(request, 'hedges')).toBe(before + 1);
    expect(await ask(request, 'hedges', 'ASK { <urn:a> <urn:height> 3 ; <urn:age> 3 }')).toBe(true);
  },
);

open(
  "the target's guard refuses a merge, and the page shows its report",
  async ({ page, request }) => {
    await dataset(request, 'thicket');
    await update(request, 'thicket@dev', 'INSERT DATA { <urn:oak> a <urn:Tree> }');
    // main gets its guard after dev started, so dev has none
    const shapes = `@prefix sh: <http://www.w3.org/ns/shacl#> .
<urn:TreeShape> a sh:NodeShape ; sh:targetClass <urn:Tree> ;
  sh:property [ sh:path <urn:species> ; sh:minCount 1 ] .`;
    const g = await request.put('/$/validation/thicket', {
      data: { mode: 'reject', shapes: { inline: shapes } },
    });
    expect(g.ok(), await g.text()).toBe(true);
    const before = await mainHead(request, 'thicket');
    await page.goto('/ui/datasets/thicket/merge?source=dev&target=main');
    await expect(page.getByRole('region', { name: 'Changes' }).getByRole('alert')).toContainText(
      "main's validation would refuse this result",
    );
    await page.getByRole('button', { name: 'Merge into main' }).click();
    const refused = page.getByRole('region', { name: 'Merge' }).getByRole('alert');
    await expect(refused).toContainText("main's validation refused the merge");
    await expect(refused).toContainText('1 blocking result');
    const results = refused.getByRole('table', { name: 'Validation results' });
    await expect(results).toContainText('<urn:oak>');
    await expect(results).toContainText('<urn:species>');
    expect(await mainHead(request, 'thicket')).toBe(before);
  },
);

open('the History graph draws the branches, their forks and merges', async ({ page, request }) => {
  await dataset(request, 'woods');
  for (let i = 0; i < 6; i++)
    await update(request, 'woods@dev', `INSERT DATA { <urn:d${i}> <urn:p> ${i} }`);
  const fix = await request.post('/$/branches/woods', { data: { name: 'fix', from: 'dev' } });
  expect(fix.status(), await fix.text()).toBe(201);
  await update(request, 'woods@fix', 'INSERT DATA { <urn:f> <urn:p> 1 }');
  await update(request, 'woods', 'INSERT DATA { <urn:m> <urn:p> 1 }');
  const m = await request.post('/$/merge/woods', { data: { source: 'dev' } });
  expect(m.ok(), await m.text()).toBe(true);

  await page.goto('/ui/datasets/woods');
  const panel = page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'History', exact: true }),
  });
  await panel.getByRole('button', { name: 'Graph' }).click();
  await expect(panel).toContainText('commits of 3 branches');
  await expect(panel.locator('svg.lines path.edge.merge')).toHaveCount(1);
  await expect(panel.locator('svg.lines path.edge.fork')).toHaveCount(2);
  await expect(panel.locator('.headlabel', { hasText: 'fix' })).toBeVisible();
  const seg = panel.getByRole('button', { name: /commits dev/ });
  await expect(seg).toContainText('5 commits');
  await seg.click();
  await expect(panel.getByRole('button', { name: /^Commit \d+ on dev$/ })).toHaveCount(6);

  // fix's commit: its changes against the dev commit it started from
  await panel.getByRole('button', { name: /^Commit \d+ on fix$/ }).click();
  const detail = panel.getByRole('region', { name: /^Commit \d+ on fix$/ });
  await expect(detail.getByLabel('Changes')).toContainText('+ <urn:f> <urn:p> "1"^^');
  await expect(detail.getByRole('link', { name: /^Query at commit/ })).toBeVisible();
});
