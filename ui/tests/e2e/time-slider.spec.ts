// The time slider against a real server (docs/specs/U01-time-slider.md, acceptance examples
// A1, A2, A5, A7 and A9, and a scrub of the History view as a smoke test). The dataset has three
// updates: the first loads ex:alice ex:name "Alice" and ex:alice ex:knows ex:bob, the second
// adds ex:bob ex:name "Bob", and the third replaces ex:alice ex:knows ex:bob with
// ex:alice ex:knows ex:carol. The tests read commit numbers from the receipts, because the
// numbering of the first commit is the server's business.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

const EX = 'http://example.org/';
const QUERY = `SELECT ?s ?o WHERE { ?s <${EX}knows> ?o }`;

/** One update with a message; the number of the commit it made. */
async function update(request: APIRequestContext, sparql: string, message: string) {
  const r = await request.post('/chronology/update?receipt=true', {
    headers: {
      'Content-Type': 'application/sparql-update',
      Accept: 'application/json',
      'Sparkles-Commit-Message': message,
    },
    data: sparql,
  });
  expect(r.ok(), `${message}: ${await r.text()}`).toBe(true);
  const body = await r.json();
  expect(body.committed, message).toBe(true);
  return body.commit.seq as number;
}

let first = 0;
let second = 0;
let third = 0;

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', {
    data: { dbName: 'chronology', dbType: 'persistent' },
  });
  expect(r.ok(), await r.text()).toBe(true);
  first = await update(
    request,
    `INSERT DATA { <${EX}alice> <${EX}name> "Alice" . <${EX}alice> <${EX}knows> <${EX}bob> }`,
    'Load Alice',
  );
  second = await update(request, `INSERT DATA { <${EX}bob> <${EX}name> "Bob" }`, 'Add Bob');
  third = await update(
    request,
    `DELETE DATA { <${EX}alice> <${EX}knows> <${EX}bob> } ; INSERT DATA { <${EX}alice> <${EX}knows> <${EX}carol> }`,
    'Alice knows Carol now',
  );
  expect([second, third]).toEqual([first + 1, first + 2]);
});

open.beforeEach(async ({ page }) => {
  await page.addInitScript(
    ([query]) => {
      localStorage.setItem('sparkles.dataset', 'chronology');
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Query 1', query }], active: 't1' }),
      );
    },
    [QUERY],
  );
});

const slider = (page: Page) => page.getByRole('region', { name: 'History time slider' });
const right = (page: Page) => slider(page).getByRole('slider', { name: /^(Commit|Compared to)$/ });
const left = (page: Page) => slider(page).getByRole('slider', { name: 'Compared from' });
const grid = (page: Page) => page.getByRole('grid');
const atField = (page: Page) => page.locator('input[list="at-options"]');

async function openSlider(page: Page) {
  await page.goto('/ui/query');
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(grid(page)).toContainText('/carol');
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await expect(right(page)).toHaveAttribute('aria-valuenow', String(third));
}

open('A1: stepping back reads the earlier commits of the query', async ({ page }) => {
  await openSlider(page);
  await right(page).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(right(page)).toHaveAttribute('aria-valuenow', String(second));
  await page.keyboard.press('ArrowLeft');
  await expect(right(page)).toHaveAttribute('aria-valuenow', String(first));
  await expect(atField(page)).toHaveValue(`commit:${first}`);
  await expect(grid(page)).toContainText('/bob');
  await expect(grid(page)).not.toContainText('/carol');
  await expect(grid(page).getByRole('row')).toHaveCount(2);
  await expect(slider(page)).toContainText(`commit ${first}`);
  // the result says which state it read
  await expect(page.getByText(`at commit ${first}`, { exact: false }).first()).toBeVisible();
  await page.keyboard.press('End');
  await expect(grid(page)).toContainText('/carol');
  await expect(atField(page)).toHaveValue('');
});

open('A2: compare marks the rows added and removed since the left commit', async ({ page }) => {
  await openSlider(page);
  await right(page).focus();
  await page.keyboard.press('c');
  await expect(left(page)).toHaveAttribute('aria-valuenow', String(second));
  const added = grid(page).getByRole('row').filter({ hasText: '/carol' });
  await expect(added.getByRole('gridcell', { name: 'added' })).toHaveText('+');
  const removed = grid(page).getByRole('row').filter({ hasText: '/bob' });
  await expect(removed.getByRole('gridcell', { name: 'removed' })).toHaveText('−');
  await expect(slider(page).getByRole('status')).toHaveText(
    `Commit ${third}: 1 row added, 1 removed`,
  );
  const chips = page.getByRole('group', { name: 'Changes between the two commits' });
  await chips.getByRole('button', { name: /^added/ }).click();
  await expect(grid(page)).toContainText('/carol');
  await expect(grid(page)).not.toContainText('/bob');
  // the link restores the comparison
  await chips.getByRole('button', { name: /^all/ }).click();
  const url = new URL(page.url());
  expect(url.searchParams.get('from')).toBe(`commit:${second}`);
  await page.reload();
  await expect(left(page)).toHaveAttribute('aria-valuenow', String(second));
  await expect(
    grid(page)
      .getByRole('row')
      .filter({ hasText: '/carol' })
      .getByRole('gridcell', { name: 'added' }),
  ).toBeVisible();
});

open(
  'A5: the history of an entity has a tick for each commit that changed it',
  async ({ page }) => {
    await page.goto(`/ui/explore?ds=chronology&iri=${encodeURIComponent(`${EX}alice`)}`);
    const panel = page.getByRole('complementary', { name: 'Resource details' });
    await expect(panel.getByRole('table').first()).toContainText('carol');
    await panel.getByRole('button', { name: 'History', exact: true }).click();
    const bar = panel.getByRole('region', { name: 'Resource history time slider' });
    const handle = bar.getByRole('slider', { name: /^(Commit|Compared to)$/ });
    // the second commit did not touch ex:alice
    await expect(handle).toHaveAttribute('aria-valuemin', String(first));
    await expect(handle).toHaveAttribute('aria-valuemax', String(third));
    await handle.focus();
    await page.keyboard.press('ArrowLeft');
    await expect(handle).toHaveAttribute('aria-valuenow', String(first));
    await expect(panel.getByRole('table').first()).toContainText('bob');
    await page.keyboard.press('End');
    await page.keyboard.press('c');
    const net = panel.getByRole('region', { name: 'Changes between the two commits' });
    await expect(net.locator('.net.add')).toContainText('carol');
    await expect(net.locator('.net.add')).toContainText(`commit ${third}`);
    await expect(net.locator('.net.add')).toContainText('Alice knows Carol now');
    await expect(net.locator('.net.remove')).toContainText('bob');
  },
);

open('the History panel scrubs a query to an earlier commit', async ({ page }) => {
  await openSlider(page);
  await right(page).focus();
  await page.keyboard.press('Home');
  // the oldest readable commit is before the data was loaded
  await expect(page.getByText(`${EX}carol`)).toHaveCount(0);
  await expect(atField(page)).not.toHaveValue('');
  await page.keyboard.press('End');
  await expect(grid(page)).toContainText('/carol');
});

// --- the Explore graph and the schema tab (Phase 2) --------------------------------------------

type Drawn = { nodes: [string, string][]; edges: [string, string][] };

/** What the graph draws, from the text the view keeps for the tests. */
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const labels = async (page: Page) => (await drawn(page)).nodes.map(([l]) => l).join(' ');
const changeOf = async (page: Page, re: RegExp) =>
  (await drawn(page)).nodes.find(([l]) => re.test(l))?.[1];
const graphBar = (page: Page) => page.getByRole('region', { name: 'Graph history time slider' });
const graphHandle = (page: Page) =>
  graphBar(page).getByRole('slider', { name: /^(Commit|Compared to)$/ });

open(
  'A7: the graph is read at a commit, and compare marks added and removed edges',
  async ({ page }) => {
    await page.goto(`/ui/explore?ds=chronology&iri=${encodeURIComponent(`${EX}alice`)}`);
    await expect.poll(() => labels(page)).toMatch(/carol/);
    await page.getByRole('button', { name: 'Graph history' }).click();
    await expect(graphHandle(page)).toHaveAttribute('aria-valuenow', String(third));
    await graphHandle(page).focus();
    await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('ArrowLeft');
    await expect(graphHandle(page)).toHaveAttribute('aria-valuenow', String(first));
    await expect.poll(() => labels(page)).toMatch(/bob/);
    expect(await labels(page)).not.toMatch(/carol/);
    // compare the second commit with the head
    await page.keyboard.press('End');
    await expect.poll(() => labels(page)).toMatch(/carol/);
    await page.keyboard.press('c');
    await expect(graphBar(page).getByRole('slider', { name: 'Compared from' })).toHaveAttribute(
      'aria-valuenow',
      String(second),
    );
    await expect.poll(() => changeOf(page, /carol/)).toBe('added');
    await expect.poll(() => changeOf(page, /bob/)).toBe('removed');
    await expect(graphBar(page).getByRole('status')).toHaveText(
      `Commit ${third}: 1 edge added, 1 removed`,
    );
  },
);

open(
  'A9: the schema is read at a commit and compare shows the counts that changed',
  async ({ page }) => {
    const nameRow = () => page.getByRole('row').filter({ hasText: 'example.org/name' }).first();
    await page.goto('/ui/explore?ds=chronology&tab=schema');
    await page.getByRole('button', { name: 'Schema history' }).click();
    await page.getByRole('tab', { name: /^Properties/ }).click();
    const bar = page.getByRole('region', { name: 'Schema history time slider' });
    const handle = bar.getByRole('slider', { name: /^(Commit|Compared to)$/ });
    await expect(handle).toHaveAttribute('aria-valuenow', String(third));
    await expect(nameRow().getByRole('cell').nth(1)).toContainText('2');
    await handle.focus();
    await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('ArrowLeft');
    await expect(handle).toHaveAttribute('aria-valuenow', String(first));
    await expect(nameRow().getByRole('cell').nth(1)).toHaveText('1');
    // compare the first commit with the head
    await page.goto(`/ui/explore?ds=chronology&tab=schema&from=commit:${first}`);
    await page.getByRole('tab', { name: /^Properties/ }).click();
    await expect(nameRow().getByRole('cell').nth(1)).toContainText('+1');
  },
);

/** An update against any dataset or branch path. */
async function updateAt(request: APIRequestContext, path: string, sparql: string) {
  const r = await request.post(`/${path}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: sparql,
  });
  expect(r.ok(), `${path}: ${await r.text()}`).toBe(true);
}

open('compare reads the head of another branch', async ({ page, request }) => {
  const made = await request.post('/$/datasets', {
    data: { dbName: 'forked', dbType: 'persistent' },
  });
  expect(made.ok(), await made.text()).toBe(true);
  await updateAt(request, 'forked', `INSERT DATA { <${EX}root> <${EX}link> <${EX}base> }`);
  const b = await request.post('/$/branches/forked', { data: { name: 'dev' } });
  expect(b.status()).toBe(201);
  await updateAt(request, 'forked', `INSERT DATA { <${EX}root> <${EX}link> <${EX}m> }`);
  await updateAt(request, 'forked@dev', `INSERT DATA { <${EX}root> <${EX}link> <${EX}d> }`);
  await page.addInitScript(
    ([query]) => {
      localStorage.setItem('sparkles.dataset', 'forked');
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Query 1', query }], active: 't1' }),
      );
    },
    [`SELECT ?o WHERE { <${EX}root> <${EX}link> ?o } ORDER BY ?o`],
  );
  await page.goto('/ui/query');
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(grid(page)).toContainText('/m');
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await right(page).focus();
  await page.keyboard.press('c');
  await slider(page).getByRole('combobox', { name: 'Compare with' }).selectOption('dev');
  await expect(
    grid(page).getByRole('row').filter({ hasText: '/m' }).getByRole('gridcell', { name: 'added' }),
  ).toHaveText('+');
  await expect(
    grid(page)
      .getByRole('row')
      .filter({ hasText: '/d' })
      .getByRole('gridcell', { name: 'removed' }),
  ).toHaveText('−');
  // the Explore graph of the same resource compares the same two heads
  await page.goto(`/ui/explore?ds=forked&iri=${encodeURIComponent(`${EX}root`)}&against=dev`);
  await expect.poll(() => changeOf(page, /\bm\b/)).toBe('added');
  await expect.poll(() => changeOf(page, /\bd\b/)).toBe('removed');
});

open('Compare on the server lists the rows the read cut', async ({ page, request }) => {
  const made = await request.post('/$/datasets', {
    data: { dbName: 'bulky', dbType: 'persistent' },
  });
  expect(made.ok(), await made.text()).toBe(true);
  const rows = Array.from({ length: 1100 }, (_, i) => `<${EX}n${i}> <${EX}v> ${i} .`).join(' ');
  await updateAt(request, 'bulky', `INSERT DATA { ${rows} }`);
  await updateAt(
    request,
    'bulky',
    `INSERT DATA { <${EX}x1> <${EX}v> 5000 . <${EX}x2> <${EX}v> 5001 . <${EX}x3> <${EX}v> 5002 }`,
  );
  await page.addInitScript(
    ([query]) => {
      localStorage.setItem('sparkles.dataset', 'bulky');
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({
          tabs: [{ id: 't1', title: 'Query 1', query }],
          active: 't1',
          limit: 1000,
        }),
      );
    },
    [`SELECT ?s ?v WHERE { ?s <${EX}v> ?v } ORDER BY ?v`],
  );
  await page.goto('/ui/query');
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(grid(page)).toBeVisible();
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await right(page).focus();
  await page.keyboard.press('c');
  await page.getByRole('button', { name: 'Compare on the server' }).click();
  await expect(grid(page).getByRole('row')).toHaveCount(4);
  await expect(
    grid(page)
      .getByRole('row')
      .filter({ hasText: '5000' })
      .getByRole('gridcell', { name: 'added' }),
  ).toHaveText('+');
  await expect(page.getByText('Compared on the server')).toBeVisible();
});
