// The plan drawn on the pattern against a real server (spec U04 §11.6): A14 puts each
// operator's estimate on the edge of the pattern it reads, equal to the tree's, A15 marks
// a real misestimate on its edge and in the tree, and A17 shows actual rows only to a
// reader whose view hides the estimates.

import type { Locator, Page } from '@playwright/test';
import { anonymous, expect, open, test } from './fixtures';
import { PASSWORD, PATTERN_DATASET, READER } from './data';

const editor = (page: Page) => page.locator('.cm-content');
const results = (page: Page) => page.getByRole('region', { name: 'Results' });
const ran = (page: Page) =>
  results(page).getByRole('region', { name: 'Pattern of the query that ran' });

async function setText(page: Page, text: string) {
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

type Overlay = { edges: [string, string, boolean, number][] };
const overlayOf = async (c: Locator): Promise<Overlay | null> => {
  const raw = await c.getAttribute('data-overlay');
  return raw ? JSON.parse(raw) : null;
};

/** Run the query and open the result's Pattern tab with the plan on it. */
async function runWithPlan(page: Page, text: string, ds = PATTERN_DATASET) {
  await page.addInitScript(() => localStorage.setItem('sparkles.planOnPattern', 'true'));
  // `at` makes the page take its dataset from the address
  await page.goto(`/ui/query?ds=${ds}&at=`);
  await expect(page.getByRole('button', { name: new RegExp(ds) })).toBeVisible();
  await setText(page, text);
  await page.getByRole('button', { name: /^Run\b/ }).click();
  await results(page)
    .getByRole('tab', { name: /Pattern/ })
    .click();
  await expect(ran(page)).toHaveAttribute('data-state', 'ok');
  const c = ran(page).locator('.pcanvas');
  await expect.poll(async () => (await overlayOf(c))?.edges.length ?? 0).toBeGreaterThan(0);
  return c;
}

test('A14: each edge shows the estimate of the operator that reads it, as the tree does', async ({
  page,
}) => {
  const c = await runWithPlan(
    page,
    `PREFIX ex: <http://example.org/>
SELECT * WHERE {
  ?person ex:worksFor ?org .
  ?person ex:name ?n .
  ?org ex:locatedIn ?city .
}`,
  );
  await expect.poll(async () => (await overlayOf(c))!.edges.length).toBe(3);
  const edges = (await overlayOf(c))!.edges;
  const tree = results(page).locator('.result-pattern-tree');
  await expect(tree.locator('.tr').first()).toBeVisible();
  const treeEst = await tree
    .locator('.tr')
    .evaluateAll((rows) =>
      rows.map((r) => (r.querySelector('.num')?.textContent ?? '').replace(/[^\d]/g, '')),
    );
  for (const [label, text] of edges) {
    const est = /est ([\d,]+)/.exec(text)?.[1]?.replace(/,/g, '');
    expect(est, `${label}: ${text}`).toBeDefined();
    expect(treeEst, `${label}: ${text}`).toContain(est);
    expect(text).toMatch(/rows \d/);
  }
});

open(
  'A15: a misestimate is marked on its edge and on its operator in the tree',
  async ({ page, request }) => {
    // a hundred and twenty ex:follows triples and no loop: the planner expects a tenth of
    // them for ?a ex:follows ?a, and none match
    const ds = 'plan-pattern-e2e';
    const made = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'mem' } });
    expect(made.ok() || made.status() === 409, await made.text()).toBe(true);
    const triples = Array.from(
      { length: 120 },
      (_, i) => `ex:f${i} ex:follows ex:f${i + 1} .`,
    ).join('\n');
    const insert = await request.post(`/${ds}/update`, {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <http://example.org/>\nINSERT DATA {\n${triples}\n}`,
    });
    expect(insert.ok(), await insert.text()).toBe(true);
    const c = await runWithPlan(
      page,
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE { ?a ex:follows ?a }',
      ds,
    );
    const edge = (await overlayOf(c))!.edges.find((e) => e[0] === 'ex:follows')!;
    expect(edge[1]).toMatch(/^est 12 · rows 0 ⚠ ÷12$/);
    expect(edge[2]).toBe(true);
    const tree = results(page).locator('.result-pattern-tree');
    await expect(tree.locator('.tr').filter({ hasText: 'follows' }).locator('.mis')).toBeVisible();
  },
);

anonymous(
  'A17: a reader whose view hides estimates sees actual rows only, without marks',
  async ({ page }) => {
    const login = await page.request.post('/$/auth/login', {
      data: { user: READER, password: PASSWORD },
    });
    expect(login.ok()).toBe(true);
    const c = await runWithPlan(
      page,
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE { ?p ex:name ?n }',
    );
    const edge = (await overlayOf(c))!.edges.find((e) => e[0] === 'ex:name')!;
    expect(edge[1]).toMatch(/^rows \d+$/);
    expect(edge[2]).toBe(false);
    await expect(ran(page).getByTestId('plan-bar')).toContainText(
      'Row estimates are hidden because your access is restricted.',
    );
  },
);
