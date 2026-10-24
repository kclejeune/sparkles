// The time slider against the mock backend (docs/specs/U01-time-slider.md, acceptance
// examples A1 to A6, A8 and A10 for Phase 1). The dataset `chrono` has three commits:
// 1 loads ex:alice ex:name "Alice" and ex:alice ex:knows ex:bob, 2 adds ex:bob ex:name "Bob",
// 3 replaces ex:alice ex:knows ex:bob with ex:alice ex:knows ex:carol. A snapshot s2 pins
// commit 2. The mock's seeded `foaf` history has no readable states, which is the gone case.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const QUERY = `PREFIX ex: <http://example.org/>
SELECT ?s ?o WHERE { ?s ex:knows ?o }`;

const update = (request: APIRequestContext, sparql: string, message: string) =>
  request
    .post('/chrono/update', {
      headers: { 'Content-Type': 'application/sparql-update', 'Sparkles-Commit-Message': message },
      data: `PREFIX ex: <http://example.org/> ${sparql}`,
    })
    .then((r) => expect(r.ok(), `update: ${message}`).toBe(true));

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: 'chrono', dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  await update(
    request,
    'INSERT DATA { ex:alice ex:name "Alice" . ex:alice ex:knows ex:bob }',
    'Load Alice',
  );
  await update(request, 'INSERT DATA { ex:bob ex:name "Bob" }', 'Add Bob');
  await update(
    request,
    'DELETE DATA { ex:alice ex:knows ex:bob } ; INSERT DATA { ex:alice ex:knows ex:carol }',
    'Alice knows Carol now',
  );
  const s = await request.post('/$/snapshots/chrono', { data: { name: 's2', at: 'commit:2' } });
  expect(s.ok()).toBe(true);
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript(
    ([query]) => {
      localStorage.setItem('sparkles.dataset', 'chrono');
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
const run = (page: Page) => page.getByRole('button', { name: /^Run/ }).click();

async function openSlider(page: Page) {
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toContainText('/carol');
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await expect(slider(page)).toBeVisible();
}

test('A1: stepping back with the keyboard reads the earlier commits', async ({ page }) => {
  await openSlider(page);
  await expect(right(page)).toHaveAttribute('aria-valuenow', '3');
  await right(page).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(grid(page)).toContainText('/bob');
  await page.keyboard.press('ArrowLeft');
  await expect(right(page)).toHaveAttribute('aria-valuenow', '1');
  await expect(grid(page)).not.toContainText('/carol');
  await expect(grid(page)).toContainText('/bob');
  await expect(grid(page).getByRole('row')).toHaveCount(2);
  await expect(slider(page)).toContainText('commit 1');
  // the At field is the same state
  await expect(page.locator('input[list="at-options"]')).toHaveValue('commit:1');
  // End returns to the head
  await page.keyboard.press('End');
  await expect(grid(page)).toContainText('/carol');
  await expect(page.locator('input[list="at-options"]')).toHaveValue('');
});

test('A2 and A3: compare marks the rows and a link restores them', async ({ page }) => {
  await openSlider(page);
  await right(page).focus();
  await page.keyboard.press('c');
  await expect(left(page)).toBeVisible();
  await expect(right(page)).toHaveAttribute('aria-valuenow', '3');
  // the left handle starts one commit back, commit 2; ex:alice knows ex:bob still held then
  await expect(left(page)).toHaveAttribute('aria-valuenow', '2');
  const added = grid(page).getByRole('row').filter({ hasText: '/carol' });
  await expect(added.getByRole('gridcell', { name: 'added' })).toHaveText('+');
  const removed = grid(page).getByRole('row').filter({ hasText: '/bob' });
  await expect(removed.getByRole('gridcell', { name: 'removed' })).toHaveText('−');
  await expect(slider(page).getByRole('status')).toHaveText('Commit 3: 1 row added, 1 removed');
  // the chips filter the rows
  const chips = page.getByRole('group', { name: 'Changes between the two commits' });
  await chips.getByRole('button', { name: /^added/ }).click();
  await expect(grid(page)).toContainText('/carol');
  await expect(grid(page)).not.toContainText('/bob');
  await chips.getByRole('button', { name: /^removed/ }).click();
  await expect(grid(page)).toContainText('/bob');
  await expect(grid(page)).not.toContainText('/carol');
  await chips.getByRole('button', { name: /^all/ }).click();
  // the address names the state
  const url = new URL(page.url());
  expect(url.searchParams.get('from')).toBe('commit:2');
  expect(url.searchParams.get('ds')).toBe('chrono');
  // reloading the link restores compare mode and the same table
  await page.reload();
  await expect(left(page)).toHaveAttribute('aria-valuenow', '2');
  await expect(right(page)).toHaveAttribute('aria-valuenow', '3');
  await expect(
    grid(page)
      .getByRole('row')
      .filter({ hasText: '/carol' })
      .getByRole('gridcell', { name: 'added' }),
  ).toBeVisible();
  await expect(
    grid(page)
      .getByRole('row')
      .filter({ hasText: '/bob' })
      .getByRole('gridcell', { name: 'removed' }),
  ).toBeVisible();
});

test('A4: S moves to the next named snapshot and the caption names it', async ({ page }) => {
  await openSlider(page);
  await right(page).focus();
  await page.keyboard.press('Home');
  await expect(right(page)).toHaveAttribute('aria-valuenow', '0');
  await page.keyboard.press('ArrowRight');
  await expect(right(page)).toHaveAttribute('aria-valuenow', '1');
  await page.keyboard.press('s');
  await expect(right(page)).toHaveAttribute('aria-valuenow', '2');
  await expect(slider(page)).toContainText('snapshot:s2');
  await page.keyboard.press('Shift+S');
  // there is no earlier snapshot, so the handle stays
  await expect(right(page)).toHaveAttribute('aria-valuenow', '2');
});

test('A10: every handle names its commit', async ({ page }) => {
  await openSlider(page);
  await right(page).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(right(page)).toHaveAttribute(
    'aria-valuetext',
    /^commit 2, .+Add Bob|^commit 2, .+SPARQL update/,
  );
  await expect(right(page)).toHaveAttribute('aria-valuemin', '0');
  await expect(right(page)).toHaveAttribute('aria-valuemax', '3');
});

test('A8: Play visits the commits in order, one read at a time, and stops at the head', async ({
  page,
}) => {
  await openSlider(page);
  const seen: string[] = [];
  let inFlight = 0;
  let overlap = false;
  page.on('request', (req) => {
    if (req.method() === 'POST' && new URL(req.url()).pathname === '/chrono/sparql') {
      inFlight++;
      if (inFlight > 1) overlap = true;
      seen.push(new URL(req.url()).searchParams.get('at') ?? 'head');
    }
  });
  page.on('requestfinished', (req) => {
    if (req.method() === 'POST' && new URL(req.url()).pathname === '/chrono/sparql') inFlight--;
  });
  await right(page).focus();
  await page.keyboard.press('Home');
  await expect(right(page)).toHaveAttribute('aria-valuenow', '0');
  seen.length = 0;
  await slider(page).getByRole('combobox', { name: 'Play speed' }).selectOption('4');
  await slider(page).getByRole('button', { name: 'Play' }).click();
  await expect(right(page)).toHaveAttribute('aria-valuenow', '3', { timeout: 15_000 });
  await expect(slider(page).getByRole('button', { name: 'Play' })).toBeVisible();
  expect(seen).toEqual(['commit:1', 'commit:2', 'head']);
  expect(overlap).toBe(false);
});

test('A6: a commit the server can no longer read keeps the view and hatches the track', async ({
  page,
  request,
}) => {
  // the mock's demo history of a dataset made for this test: its commits before the head
  // are gone. The seeded `foaf` dataset cannot serve, because a backup restore in another
  // spec replaces its history with one that starts after commit 5.
  const made = await request.post('/$/datasets', {
    data: { dbName: 'pruned', dbType: 'persistent', seedHistory: true },
  });
  expect(made.ok() || made.status() === 409).toBe(true);
  await page.addInitScript(() => {
    localStorage.setItem('sparkles.dataset', 'pruned');
    localStorage.setItem(
      'sparkles.queryTabs',
      JSON.stringify({
        tabs: [{ id: 't1', title: 'Query 1', query: 'SELECT ?s WHERE { ?s a ?t } LIMIT 3' }],
        active: 't1',
      }),
    );
  });
  await page.goto('/ui/query?at=commit%3A5');
  // the link reads commit 5, which the mock no longer holds: a toast names what is readable
  await expect(page.getByText('Commit 5 is no longer available')).toBeVisible();
  // the head of the dataset is the only commit that can be read
  const readable = page.getByText(/Available commits: \d+\./);
  await expect(readable).toBeVisible();
  const head = /Available commits: (\d+)\./.exec((await readable.textContent()) ?? '')?.[1];
  await expect(right(page)).toHaveAttribute('aria-valuenow', head ?? '');
  await expect(page.locator('input[list="at-options"]')).toHaveValue('');
  // the track marks the stretch as gone
  await expect(slider(page).getByText('gone')).toBeVisible();
});

test('A5: an entity history strip has a tick for each commit that changed the resource', async ({
  page,
}) => {
  await page.goto(`/ui/explore?ds=chrono&iri=${encodeURIComponent('http://example.org/alice')}`);
  const panel = page.getByRole('complementary', { name: 'Resource details' });
  await expect(panel.getByRole('table').first()).toContainText('carol');
  await panel.getByRole('button', { name: 'History', exact: true }).click();
  const bar = panel.getByRole('region', { name: 'Resource history time slider' });
  await expect(bar).toBeVisible();
  const handle = bar.getByRole('slider', { name: /^(Commit|Compared to)$/ });
  await expect(handle).toHaveAttribute('aria-valuemin', '1');
  await expect(handle).toHaveAttribute('aria-valuemax', '3');
  // commit 2 did not touch ex:alice, so the handle goes from commit 3 to commit 1
  await handle.focus();
  await page.keyboard.press('ArrowLeft');
  await expect(handle).toHaveAttribute('aria-valuenow', '1');
  await expect(panel.getByRole('table').first()).toContainText('bob');
  await expect(panel.getByRole('table').first()).not.toContainText('carol');
  // comparing lists the values added and removed, with the commit that changed them
  await page.keyboard.press('End');
  await page.keyboard.press('c');
  await expect(bar.getByRole('slider', { name: 'Compared from' })).toHaveAttribute(
    'aria-valuenow',
    '1',
  );
  const net = panel.getByRole('region', { name: 'Changes between the two commits' });
  await expect(net.locator('.net.add')).toContainText('carol');
  await expect(net.locator('.net.add')).toContainText('commit 3');
  await expect(net.locator('.net.add')).toContainText('Alice knows Carol now');
  await expect(net.locator('.net.remove')).toContainText('bob');
  await expect(net.locator('.net')).toHaveCount(2);
});

test('U2: a refused read in the middle of a scrub keeps the last result on screen', async ({
  page,
}) => {
  await openSlider(page);
  const reads: string[] = [];
  await page.route('**/chrono/sparql*', async (route) => {
    const at = new URL(route.request().url()).searchParams.get('at') ?? 'head';
    reads.push(at);
    if (at === 'commit:2') {
      // slow enough for the next move to supersede it
      await new Promise((r) => setTimeout(r, 1500));
      return route.continue().catch(() => {});
    }
    if (at === 'commit:1') {
      return route.fulfill({
        status: 410,
        contentType: 'application/json',
        body: JSON.stringify({
          error: 'gone',
          message: 'commit 1 is no longer reconstructable',
          code: 'history-gone',
          commit: 1,
          reconstructable: [{ from: 2, to: 3 }],
        }),
      });
    }
    return route.continue();
  });
  await right(page).focus();
  await page.keyboard.press('ArrowLeft');
  await expect.poll(() => reads.includes('commit:2')).toBe(true);
  await page.keyboard.press('ArrowLeft');
  await expect(page.getByText('Commit 1 is no longer available')).toBeVisible();
  // the head's rows were put back without reading the head again
  await expect(right(page)).toHaveAttribute('aria-valuenow', '3');
  await expect(grid(page)).toContainText('/carol');
  await page.waitForTimeout(800);
  expect(reads).toEqual(['commit:2', 'commit:1']);
  await expect(grid(page)).toContainText('/carol');
});

test('U2: Play stops on a read that fails and leaves the handle on that commit', async ({
  page,
}) => {
  await openSlider(page);
  const reads: string[] = [];
  await page.route('**/chrono/sparql*', (route) => {
    const at = new URL(route.request().url()).searchParams.get('at') ?? 'head';
    reads.push(at);
    if (at !== 'commit:2') return route.continue();
    return route.fulfill({
      status: 408,
      contentType: 'application/json',
      body: JSON.stringify({ error: 'timeout', message: 'the query timed out' }),
    });
  });
  await right(page).focus();
  await page.keyboard.press('Home');
  await expect(right(page)).toHaveAttribute('aria-valuenow', '0');
  reads.length = 0;
  await slider(page).getByRole('combobox', { name: 'Play speed' }).selectOption('4');
  await slider(page).getByRole('button', { name: 'Play' }).click();
  await expect(page.locator('.error-box')).toBeVisible();
  await expect(slider(page).getByRole('button', { name: 'Play' })).toBeVisible();
  // nothing steps on and overwrites the error
  await page.waitForTimeout(1500);
  expect(reads).toEqual(['commit:1', 'commit:2']);
  await expect(right(page)).toHaveAttribute('aria-valuenow', '2');
  await expect(page.locator('.error-box')).toBeVisible();
});

test('the bar closes with Escape and the view reads the head again', async ({ page }) => {
  await openSlider(page);
  await right(page).focus();
  await page.keyboard.press('ArrowLeft');
  await expect(grid(page)).toContainText('/bob');
  await page.keyboard.press('Escape');
  await expect(slider(page)).toBeHidden();
  await expect(grid(page)).toContainText('/carol');
  await expect(page.locator('input[list="at-options"]')).toHaveValue('');
  expect(new URL(page.url()).searchParams.get('at')).toBeNull();
});

test.describe('Memory', () => {
  const ANA = 'http://example.org/ana';
  const note = (request: APIRequestContext, sparql: string) =>
    request
      .post('/notes/update', {
        headers: { 'Content-Type': 'application/sparql-update' },
        data: `PREFIX ex: <http://example.org/> ${sparql}`,
      })
      .then((r) => expect(r.ok()).toBe(true));

  test.beforeAll(async ({ request }) => {
    const r = await request.post('/$/datasets', {
      data: { dbName: 'notes', dbType: 'persistent' },
    });
    if (r.status() === 409) return;
    expect(r.ok()).toBe(true);
    await note(request, 'INSERT DATA { ex:ana ex:memberOf ex:platform }');
    await note(
      request,
      'DELETE DATA { ex:ana ex:memberOf ex:platform } ; INSERT DATA { ex:ana ex:memberOf ex:payments . ex:ana ex:email "ana@example.org" }',
    );
  });

  test('a recall that a later move supersedes is aborted', async ({ page, request }) => {
    await page.goto(`/ui/explore?ds=notes&iri=${encodeURIComponent(ANA)}&view=memory`);
    const memory = page.getByLabel('Memory', { exact: true });
    await expect(memory.getByRole('region', { name: /Facts about/ })).toContainText('payments');
    await memory.getByRole('button', { name: /^As of/ }).click();
    const bar = page.getByRole('region', { name: 'Memory time slider' });
    const handle = bar.getByRole('slider', { name: /^(Commit|Compared to)$/ });
    // hold the read of the earlier state, so that the move back to the head arrives while it runs
    await page.route('**/notes/recall', (route) =>
      route.continue({ url: `${route.request().url()}?mockDelay=3000` }).catch(() => {}),
    );
    await handle.focus();
    await page.keyboard.press('ArrowLeft');
    await page.waitForTimeout(700);
    await page.keyboard.press('End');
    await expect
      .poll(async () => {
        const aborted = (await (await request.get('/$/mock/aborted')).json()) as string[];
        return aborted.filter((r) => r.includes('/notes/recall')).length;
      })
      .toBeGreaterThan(0);
  });

  test('the as-of slider reads the facts at a commit and compare lists what changed', async ({
    page,
  }) => {
    await page.goto(`/ui/explore?ds=notes&iri=${encodeURIComponent(ANA)}&view=memory`);
    const memory = page.getByLabel('Memory', { exact: true });
    const facts = memory.getByRole('region', { name: /Facts about/ });
    await expect(facts).toContainText('payments');
    await memory.getByRole('button', { name: /^As of/ }).click();
    const bar = page.getByRole('region', { name: 'Memory time slider' });
    const handle = bar.getByRole('slider', { name: /^(Commit|Compared to)$/ });
    await handle.focus();
    await page.keyboard.press('ArrowLeft');
    await expect(facts).toContainText('platform');
    await expect(facts).not.toContainText('payments');
    await expect(memory).toContainText('Read at commit 1');
    // comparing marks the facts that were added and superseded
    await page.keyboard.press('End');
    await expect(facts).toContainText('payments');
    await page.keyboard.press('c');
    const changes = memory.getByRole('region', { name: 'Changed facts' });
    await expect(changes.getByText('added').first()).toBeVisible();
    await expect(changes.locator('.change.added')).toHaveCount(2);
    await expect(changes.locator('.change.superseded')).toContainText('platform');
  });
});
