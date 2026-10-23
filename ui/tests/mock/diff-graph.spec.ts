// The diff graph, the time-lapse and the overview through time (docs/specs/U01-time-slider.md,
// Phase 4: acceptance examples A11 to A14 and A16). The dataset `chrono4` has the commits of
// §7 and a fourth that adds 600 people of class ex:Person who work for ex:acme, in a graph
// that a reader who is not an admin cannot read.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, phone } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 120_000 });

const EX = 'http://example.org/';
const PRIVATE = 'http://example.org/private';

const update = (request: APIRequestContext, ds: string, sparql: string, message: string) =>
  request
    .post(`/${ds}/update`, {
      headers: { 'Content-Type': 'application/sparql-update', 'Sparkles-Commit-Message': message },
      data: `PREFIX ex: <${EX}> ${sparql}`,
    })
    .then((r) => expect(r.ok(), `update: ${message}`).toBe(true));

async function dataset(request: APIRequestContext, name: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  if (r.status() === 409) return false;
  expect(r.ok()).toBe(true);
  return true;
}

test.beforeAll(async ({ request }) => {
  if (!(await dataset(request, 'chrono4'))) return;
  await update(
    request,
    'chrono4',
    'INSERT DATA { ex:alice ex:name "Alice" . ex:alice ex:knows ex:bob . ex:acme a ex:Org . ex:alice a ex:Person }',
    'Load Alice',
  );
  await update(request, 'chrono4', 'INSERT DATA { ex:bob ex:name "Bob" }', 'Add Bob');
  await update(
    request,
    'chrono4',
    'DELETE DATA { ex:alice ex:knows ex:bob } ; INSERT DATA { ex:alice ex:knows ex:carol }',
    'Alice knows Carol now',
  );
  const people = Array.from(
    { length: 600 },
    (_, i) => `ex:p${i} a ex:Person ; ex:worksFor ex:acme .`,
  ).join(' ');
  await update(request, 'chrono4', `INSERT DATA { GRAPH <${PRIVATE}> { ${people} } }`, 'Hire 600');
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'chrono4'));
});

type Drawn = { nodes: [string, string][]; edges: [string, string][] };

const view = (page: Page) => page.locator('.dg');
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await view(page).locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const labels = async (page: Page) => (await drawn(page)).nodes.map(([l]) => l).join(' ');
const diffUrl = (to: number, from: number, extra = '') =>
  `/ui/explore?ds=chrono4&diff=commit%3A${to}&diffFrom=commit%3A${from}${extra}`;

test('A11: Show on graph in the History panel draws the commit and nothing else', async ({
  page,
}) => {
  await page.goto('/ui/datasets/chrono4');
  await page.getByRole('button', { name: 'Show commit 3 on the graph' }).click();
  await expect(page).toHaveURL(/\/explore\?.*diff=commit%3A3/);
  await expect.poll(async () => (await drawn(page)).edges.length).toBe(2);
  const d = await drawn(page);
  expect(d.edges.map(([, c]) => c).sort()).toEqual(['added', 'removed']);
  const change = (re: RegExp) => d.nodes.find(([l]) => re.test(l))?.[1];
  expect(change(/carol/)).toBe('added');
  expect(change(/bob/)).toBe('removed');
  expect(d.nodes).toHaveLength(3);
  await expect(view(page)).toContainText('commit 2 → 3');
  await expect(view(page).getByLabel('1 added, 1 removed')).toBeVisible();

  // keys: R hides removals, A additions, Escape leaves the diff
  await page.keyboard.press('r');
  await expect.poll(async () => (await drawn(page)).edges.map(([, c]) => c)).toEqual(['added']);
  await page.keyboard.press('r');
  await page.keyboard.press('a');
  await expect.poll(async () => (await drawn(page)).edges.map(([, c]) => c)).toEqual(['removed']);
  await page.keyboard.press('a');
  // ▶ keeps the width: 2 → 3 becomes 3 → 4
  await page.keyboard.press('ArrowRight');
  await expect(view(page)).toContainText('commit 3 → 4');
  expect(new URL(page.url()).searchParams.get('diff')).toBe('commit:4');
  await page.keyboard.press('ArrowLeft');
  await expect(view(page)).toContainText('commit 2 → 3');
  await page.keyboard.press('Escape');
  await expect(page.locator('.dg')).toHaveCount(0);
  expect(new URL(page.url()).searchParams.get('diff')).toBeNull();
});

test('A11 from the commit graph, and the list of changes', async ({ page }) => {
  await page.goto('/ui/datasets/chrono4');
  const panel = page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'History', exact: true }),
  });
  const toggle = panel.getByRole('button', { name: 'Graph', exact: true });
  await expect(toggle).toHaveAttribute('aria-pressed', /true|false/, { timeout: 30_000 });
  if ((await toggle.getAttribute('aria-pressed')) !== 'true') await toggle.click();
  // the run of older commits opens into its commits
  await panel.getByRole('button', { name: /commits main/ }).click();
  await panel
    .getByRole('button', { name: /^Commit 3 on main$/ })
    .first()
    .click();
  await panel.getByRole('link', { name: 'Show on graph' }).click();
  await expect(page).toHaveURL(/diff=commit%3A3/);
  await expect.poll(async () => (await drawn(page)).edges.length).toBe(2);
  await page.keyboard.press('l');
  const list = view(page).getByRole('region', { name: 'Changes' });
  await expect(list.locator('tbody tr')).toHaveCount(2);
  await expect(list).toContainText('−');
  await expect(list).toContainText('carol');
});

test('A12: a large diff draws 500 changes and groups by class', async ({ page }) => {
  await page.goto(diffUrl(4, 3));
  await expect(view(page)).toContainText('Showing 500 of 1,200 changes.');
  const answer = page.waitForResponse((r) => r.url().includes('group=class'));
  await view(page).getByRole('button', { name: 'Group by class' }).click();
  const g = await (await answer).json();
  const works = g.groups.find((x: { predicate: string }) => x.predicate === `${EX}worksFor`);
  expect(works).toMatchObject({
    subjectClass: `${EX}Person`,
    object: { class: `${EX}Org` },
    added: 600,
  });
  await expect.poll(() => labels(page)).toMatch(/Person \+600/);
  const d = await drawn(page);
  expect(d.edges).toContainEqual([expect.stringMatching(/worksFor \+600$/), 'added']);
  expect(d.edges).toHaveLength(1);
  expect(new URL(page.url()).searchParams.get('group')).toBe('class');
  // G goes back to the changes
  await page.keyboard.press('g');
  await expect(view(page)).toContainText('Showing 500 of 1,200 changes.');
});

test('A13: the time-lapse makes one request per step and groups the large one', async ({
  page,
}) => {
  const diffs: string[] = [];
  let open = 0;
  let most = 0;
  page.on('request', (r) => {
    if (!/\/chrono4\/diff\?/.test(r.url())) return;
    diffs.push(r.url());
    open++;
    most = Math.max(most, open);
  });
  page.on('requestfinished', (r) => {
    if (/\/chrono4\/diff\?/.test(r.url())) open--;
  });
  await page.goto(diffUrl(4, 0));
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(0);
  diffs.length = 0;
  most = 0;
  await view(page).getByRole('combobox', { name: 'Steps a second' }).selectOption('2');
  const seen: string[] = [];
  const steps = view(page).getByText(/^step \d\/4$/);
  await view(page).getByRole('button', { name: 'Play the time-lapse' }).click();
  await expect(steps).toHaveText('step 2/4');
  seen.push(await labels(page));
  await expect(view(page)).toContainText('grouped by class from here on');
  await expect(steps).toHaveText('step 4/4');
  await expect(view(page).getByRole('button', { name: 'Play the time-lapse' })).toBeVisible();
  // bob was drawn by step 2
  expect(seen[0]).toMatch(/bob/);
  // four steps, four diff requests, never two at once
  expect(diffs).toHaveLength(4);
  expect(most).toBe(1);
  expect(diffs.at(-1)).toContain('group=class');
  await expect.poll(() => labels(page)).toMatch(/Person \+600/);
  // the live region announces the step
  await expect(view(page).locator('[aria-live="polite"]')).toHaveText(
    'commit 4, 1,200 added, 0 removed',
  );
});

test('A14: the overview compares commit 3 with commit 4', async ({ page }) => {
  await page.goto('/ui/explore?ds=chrono4');
  await expect(page.getByRole('navigation', { name: 'Overview levels' })).toBeVisible();
  await page.keyboard.press('Shift+H');
  const slider = page.getByRole('region', { name: 'Overview history time slider' });
  const right = slider.getByRole('slider', { name: /^(Commit|Compared to)$/ });
  await expect(right).toHaveAttribute('aria-valuenow', '4');
  const grouped = page.waitForResponse((r) => r.url().includes('group=class'));
  await slider.getByRole('button', { name: 'Compare' }).click();
  await expect(slider.getByRole('slider', { name: 'Compared from' })).toHaveAttribute(
    'aria-valuenow',
    '3',
  );
  const g = await (await grouped).json();
  expect(g.from.commit.seq).toBe(3);
  // the list view names the change of each class
  await page.keyboard.press('l');
  await expect(page.getByRole('row', { name: /Person.*\+600/ })).toBeVisible();
  await page.keyboard.press('l');
  await expect(slider.locator('[role="status"]')).toHaveText(
    'Commit 4: 1,200 added, 0 removed since commit 3',
  );
});

test("the Explore graph plays a time-lapse from each step's diff of the drawn resources", async ({
  page,
}) => {
  const diffs: string[] = [];
  page.on('request', (r) => {
    if (/\/chrono4\/diff\?/.test(r.url())) diffs.push(decodeURIComponent(r.url()));
  });
  await page.goto(`/ui/explore?ds=chrono4&iri=${encodeURIComponent(`${EX}alice`)}`);
  const graph = page.locator('.cy-host .wrap[data-graph]');
  await expect.poll(async () => (await graph.getAttribute('data-graph')) ?? '').toMatch(/carol/);
  await page.getByRole('button', { name: 'Graph history' }).click();
  const slider = page.getByRole('region', { name: 'Graph history time slider' });
  await slider.getByRole('combobox', { name: 'Play mode' }).selectOption('lapse');
  await slider.getByRole('combobox', { name: 'Play speed' }).selectOption('4');
  await slider.getByRole('button', { name: 'Play', exact: true }).click();
  const right = slider.getByRole('slider', { name: 'Commit' });
  await expect(right).toHaveAttribute('aria-valuenow', '4', { timeout: 60_000 });
  // the steps after the first read the diff of the drawn resources, not the graph again
  const steps = diffs.filter((u) => u.includes('term=') && u.includes('from=commit:'));
  expect(steps.length).toBeGreaterThanOrEqual(2);
  await expect.poll(async () => (await graph.getAttribute('data-graph')) ?? '').toMatch(/carol/);
});

test('the overview plays a time-lapse from grouped diffs with its positions held', async ({
  page,
}) => {
  const grouped: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('group=class')) grouped.push(r.url());
  });
  await page.goto('/ui/explore?ds=chrono4');
  await expect(page.getByRole('navigation', { name: 'Overview levels' })).toBeVisible();
  await page.keyboard.press('Shift+H');
  const slider = page.getByRole('region', { name: 'Overview history time slider' });
  await slider.getByRole('combobox', { name: 'Play mode' }).selectOption('lapse');
  await slider.getByRole('combobox', { name: 'Play speed' }).selectOption('4');
  await slider.getByRole('button', { name: 'Play', exact: true }).click();
  const right = slider.getByRole('slider', { name: 'Commit' });
  await expect(right).toHaveAttribute('aria-valuenow', '4', { timeout: 60_000 });
  await expect(slider.locator('[role="status"]')).toHaveText('Commit 4: 1,200 added, 0 removed');
  expect(grouped.length).toBeGreaterThanOrEqual(3);
  await page.keyboard.press('l');
  await expect(page.getByRole('row', { name: /Person.*\+600/ })).toBeVisible();
  await page.keyboard.press('l');
  // Shift+H closes it and the overview reads the head again
  await page.keyboard.press('Shift+H');
  await expect(slider).toHaveCount(0);
});

test('the list follows the time-lapse, and moving the range starts it again', async ({ page }) => {
  await page.goto(diffUrl(3, 0));
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(0);
  await view(page).getByRole('combobox', { name: 'Steps a second' }).selectOption('10');
  await view(page).getByRole('button', { name: 'Play the time-lapse' }).click();
  await expect(view(page).getByText(/^step \d\/3$/)).toHaveText('step 3/3');
  await expect(view(page).getByRole('button', { name: 'Play the time-lapse' })).toBeVisible();
  await expect(view(page)).toContainText('commit 0 → 3');
  // the list shows the last step's changes, not the range's
  await page.keyboard.press('l');
  const list = view(page).getByRole('region', { name: 'Changes' });
  await expect(list.locator('tbody tr')).toHaveCount(2);
  await expect(list).toContainText('carol');
  await page.keyboard.press('l');
  // what earlier steps drew stays: Alice's name from commit 1
  expect((await drawn(page)).nodes.map(([l]) => l)).toContain('Alice');
  // a new range plays from its own first commit
  await page.keyboard.press('ArrowRight');
  await expect(view(page)).toContainText('commit 1 → 4');
  const asked: string[] = [];
  page.on('request', (r) => {
    if (/\/chrono4\/diff\?/.test(r.url())) asked.push(decodeURIComponent(r.url()));
  });
  await view(page).getByRole('button', { name: 'Play the time-lapse' }).click();
  await expect(view(page).getByText(/^step \d\/3$/)).toHaveText('step 3/3', { timeout: 30_000 });
  expect(asked[0]).toContain('from=commit:1');
});

test('the overview reads the profiles again when a time-lapse step fails', async ({ page }) => {
  let grouped = 0;
  let failedAt = -1;
  await page.route(/group=class/, async (route) => {
    // the second step fails as a grouping past the budget does
    if (++grouped === 2) {
      failedAt = profiles.length;
      return route.fulfill({
        status: 507,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'memory budget exceeded', budget: 'memory' }),
      });
    }
    return route.continue();
  });
  const profiles: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/profiles')) profiles.push(decodeURIComponent(r.url()));
  });
  await page.goto('/ui/explore?ds=chrono4');
  await expect(page.getByRole('navigation', { name: 'Overview levels' })).toBeVisible();
  await page.keyboard.press('Shift+H');
  const slider = page.getByRole('region', { name: 'Overview history time slider' });
  await slider.getByRole('combobox', { name: 'Play mode' }).selectOption('lapse');
  await slider.getByRole('combobox', { name: 'Play speed' }).selectOption('4');
  await slider.getByRole('button', { name: 'Play', exact: true }).click();
  await expect(slider.getByRole('button', { name: 'Play', exact: true })).toBeVisible({
    timeout: 30_000,
  });
  // after the failure, the state the last good step drew is read exactly
  expect(failedAt).toBeGreaterThanOrEqual(0);
  await expect.poll(() => profiles.slice(failedAt).some((u) => /at=commit:\d/.test(u))).toBe(true);
});

test('a write receipt on the query page shows the write on the graph', async ({ page }) => {
  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText(`INSERT DATA { <${EX}dave> <${EX}knows> <${EX}alice> }`);
  await page.getByRole('button', { name: 'Run update' }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await results.getByRole('link', { name: 'Show on graph' }).click();
  await expect(page).toHaveURL(/diff=commit%3A5&diffFrom=commit%3A4/);
  await expect.poll(async () => (await drawn(page)).edges.map(([, c]) => c)).toEqual(['added']);
  await expect.poll(() => labels(page)).toMatch(/dave/);
});

test('A16: a reader who cannot read the graph of commit 4 sees no changes', async ({
  page,
  context,
}) => {
  await context.addCookies([
    { name: 'mock-principal', value: 'reader', domain: '127.0.0.1', path: '/' },
  ]);
  await page.goto(diffUrl(4, 3));
  await expect(view(page)).toContainText('No changes you can read between these commits');
  await context.clearCookies();
});

test('a grouped diff past the budget falls back to the first changes', async ({
  page,
  request,
}) => {
  if (await dataset(request, 'chrono507'))
    await update(request, 'chrono507', 'INSERT DATA { ex:a ex:p ex:b }', 'One');
  await page.goto('/ui/explore?ds=chrono507&diff=commit%3A1&diffFrom=commit%3A0&group=class');
  await expect(view(page)).toContainText('Grouping needs more than the server allows');
  await expect.poll(async () => (await drawn(page)).edges.length).toBe(1);
});

test.describe('at 390 px', () => {
  test.use({ ...phone, viewport: { width: 390, height: 844 } });

  test('the options fold into a sheet and the changes open as a tab', async ({ page }) => {
    await page.goto(diffUrl(3, 2));
    await expect.poll(async () => (await drawn(page)).edges.length).toBe(2);
    const tools = view(page).getByRole('toolbar', { name: 'Diff options' });
    await expect(tools).toBeHidden();
    await view(page).getByRole('button', { name: 'Options' }).click();
    await expect(tools).toBeVisible();
    await view(page).getByRole('button', { name: 'Options' }).click();
    await view(page).getByRole('tab', { name: 'Changes' }).click();
    await expect(view(page).getByRole('region', { name: 'Changes' })).toBeVisible();
    await expect(view(page).getByRole('group', { name: 'Diff steps' })).toBeVisible();
    await expectNoSidewaysScroll(page, 'the diff graph');
  });
});
