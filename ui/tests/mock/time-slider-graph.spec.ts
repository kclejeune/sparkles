// The time slider over the Explore graph and the schema tab, and over a history too long
// to list (docs/specs/U01-time-slider.md, Phase 2: acceptance examples A7 and A9, and the
// bucketed timeline). The dataset `chrono3` has the commits of the Phase 1 spec: 1 loads
// ex:alice ex:name "Alice" and ex:alice ex:knows ex:bob, 2 adds ex:bob ex:name "Bob", and
// 3 replaces ex:alice ex:knows ex:bob with ex:alice ex:knows ex:carol.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';

const update = (request: APIRequestContext, sparql: string, message: string) =>
  request
    .post('/chrono3/update', {
      headers: { 'Content-Type': 'application/sparql-update', 'Sparkles-Commit-Message': message },
      data: `PREFIX ex: <${EX}> ${sparql}`,
    })
    .then((r) => expect(r.ok(), `update: ${message}`).toBe(true));

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', {
    data: { dbName: 'chrono3', dbType: 'persistent' },
  });
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
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'chrono3'));
});

type Drawn = { nodes: [string, string][]; edges: [string, string][] };

/** What the graph draws, from the text the view keeps for the tests. */
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};
const labels = async (page: Page) => (await drawn(page)).nodes.map(([l]) => l).join(' ');
const changeOf = async (page: Page, re: RegExp) =>
  (await drawn(page)).nodes.find(([l]) => re.test(l))?.[1];

const slider = (page: Page, name: string) =>
  page.getByRole('region', { name: `${name} time slider` });
const right = (page: Page, name: string) =>
  slider(page, name).getByRole('slider', { name: /^(Commit|Compared to)$/ });

async function openGraph(page: Page) {
  await page.goto(`/ui/explore?ds=chrono3&iri=${encodeURIComponent(`${EX}alice`)}`);
  await expect.poll(() => labels(page)).toMatch(/carol/);
  await page.getByRole('button', { name: 'Graph history' }).click();
  await expect(right(page, 'Graph history')).toHaveAttribute('aria-valuenow', '3');
}

test('A7: the graph is read again at the commit the slider names', async ({ page }) => {
  await openGraph(page);
  await right(page, 'Graph history').focus();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect(right(page, 'Graph history')).toHaveAttribute('aria-valuenow', '1');
  await expect.poll(() => labels(page)).toMatch(/bob/);
  expect(await labels(page)).not.toMatch(/carol/);
  // the address names the state
  expect(new URL(page.url()).searchParams.get('at')).toBe('commit:1');
  // the properties panel reads the same commit
  await expect(page.getByRole('complementary', { name: 'Resource details' })).toContainText('bob');
  await page.keyboard.press('End');
  await expect.poll(() => labels(page)).toMatch(/carol/);
  expect(await labels(page)).not.toMatch(/bob/);
});

test('U6: a graph read that a later move supersedes is aborted', async ({ page, request }) => {
  await openGraph(page);
  // hold every read of an earlier state, so that the next move arrives while it runs
  await page.route(/\/chrono3\/sparql\?.*at=/, (route) =>
    route.continue({ url: `${route.request().url()}&mockDelay=3000` }).catch(() => {}),
  );
  await right(page, 'Graph history').focus();
  await page.keyboard.press('ArrowLeft');
  await page.waitForTimeout(600);
  await page.keyboard.press('ArrowLeft');
  await page.waitForTimeout(600);
  await page.keyboard.press('ArrowLeft');
  await expect(right(page, 'Graph history')).toHaveAttribute('aria-valuenow', '0');
  await expect
    .poll(async () => {
      const aborted = (await (await request.get('/$/mock/aborted')).json()) as string[];
      return ['commit%3A2', 'commit%3A1'].map((c) =>
        aborted.some((r) => r.includes('/chrono3/sparql') && r.includes(c)),
      );
    })
    .toEqual([true, true]);
});

test('A7: compare marks what was added and what was removed', async ({ page }) => {
  await openGraph(page);
  await right(page, 'Graph history').focus();
  await page.keyboard.press('c');
  await expect(
    slider(page, 'Graph history').getByRole('slider', { name: 'Compared from' }),
  ).toHaveAttribute('aria-valuenow', '2');
  // carol is new, bob is a ghost of the earlier state
  await expect.poll(() => changeOf(page, /carol/)).toBe('added');
  await expect.poll(() => changeOf(page, /bob/)).toBe('removed');
  expect((await drawn(page)).edges.map(([, c]) => c).sort()).toEqual(['added', 'removed']);
  await expect(slider(page, 'Graph history').getByRole('status')).toHaveText(
    'Commit 3: 1 edge added, 1 removed',
  );
  // a link restores the comparison
  const url = new URL(page.url());
  expect(url.searchParams.get('from')).toBe('commit:2');
  await page.reload();
  await expect.poll(() => changeOf(page, /bob/)).toBe('removed');
  await expect.poll(() => changeOf(page, /carol/)).toBe('added');
});

test('A7: elements fade in and out, and not under reduced motion', async ({ page }) => {
  await openGraph(page);
  const wrap = page.locator('.wrap[data-graph]');
  await expect(wrap).toHaveAttribute('data-motion', 'on');
  await right(page, 'Graph history').focus();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect.poll(() => labels(page)).toMatch(/bob/);
  expect(Number(await wrap.getAttribute('data-animations'))).toBeGreaterThan(0);
});

test('A7: reduced motion starts no animation', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await openGraph(page);
  const wrap = page.locator('.wrap[data-graph]');
  await expect(wrap).toHaveAttribute('data-motion', 'off');
  await right(page, 'Graph history').focus();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect.poll(() => labels(page)).toMatch(/bob/);
  await page.keyboard.press('End');
  await expect.poll(() => labels(page)).toMatch(/carol/);
  await expect(wrap).toHaveAttribute('data-animations', '0');
});

test('A7: a commit that cannot be read leaves the graph where it was', async ({
  page,
  request,
}) => {
  // Backup tests replace foaf's history. This private fixture can only read its head.
  const ds = 'pruned-graph';
  const made = await request.post('/$/datasets', {
    data: { dbName: ds, dbType: 'persistent', seedHistory: true },
  });
  expect(made.ok() || made.status() === 409).toBe(true);
  const history = await request.get(`/$/commits/${ds}?limit=1`);
  expect(history.ok()).toBe(true);
  const head = (await history.json()).head as number;
  const rows = await request.post(`/${ds}/sparql`, {
    headers: { 'Content-Type': 'application/sparql-query' },
    data: 'SELECT ?s WHERE { ?s <http://xmlns.com/foaf/0.1/knows> ?o } ORDER BY ?s LIMIT 1',
  });
  expect(rows.ok()).toBe(true);
  const iri = (await rows.json()).results.bindings[0].s.value as string;
  // The catalog was read just before pruning. Known-gone stops are skipped by the
  // keyboard; a newly lost state must instead be refused by the real read endpoint.
  await page.route(`**/$/commits/${ds}?*`, async (route) => {
    const response = await route.fetch();
    const catalog = await response.json();
    await route.fulfill({
      response,
      json: {
        ...catalog,
        oldestReconstructable: head - 1,
        reconstructable: [{ from: head - 1, to: head }],
        commits: catalog.commits.map((commit: { seq: number }) => ({
          ...commit,
          reconstructable: commit.seq >= head - 1,
        })),
      },
    });
  });
  await page.addInitScript((name) => localStorage.setItem('sparkles.dataset', name), ds);
  await page.goto(`/ui/explore?ds=${ds}&iri=${encodeURIComponent(iri)}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(1);
  // Caption fallbacks can change when prefixes arrive. Compare the nodes and edges.
  const elements = () =>
    page.locator('.wrap[data-graph]').evaluate((el) => {
      type Graph = { elements(): { map(fn: (e: { id(): string }) => string): string[] } };
      return (el as unknown as { __cy: Graph }).__cy
        .elements()
        .map((e) => e.id())
        .sort();
    });
  const before = await elements();
  await page.getByRole('button', { name: 'Graph history' }).click();
  const handle = right(page, 'Graph history');
  await expect(handle).toHaveAttribute('aria-valuenow', String(head));
  const refused = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return (
      url.pathname === `/${ds}/sparql` &&
      url.searchParams.get('at') === `commit:${head - 1}` &&
      response.status() === 410
    );
  });
  await handle.focus();
  await page.keyboard.press('ArrowLeft');
  await refused;
  await expect(
    page.getByText(`Commit ${head - 1} is no longer available`, { exact: true }).first(),
  ).toBeVisible();
  // the handle came back, and the graph is the one it was
  await expect(handle).toHaveAttribute('aria-valuenow', String(head));
  expect(await elements()).toEqual(before);
});

async function schemaCell(page: Page, name: string, column: number) {
  const row = page
    .getByRole('row')
    .filter({ hasText: `example.org/${name}` })
    .first();
  return row.getByRole('cell').nth(column);
}

test('A9: the schema is read at a commit and compare shows the change of each count', async ({
  page,
}) => {
  await page.goto('/ui/explore?ds=chrono3&tab=schema');
  await page.getByRole('button', { name: 'Schema history' }).click();
  await page.getByRole('tab', { name: /^Properties/ }).click();
  const handle = right(page, 'Schema history');
  await expect(handle).toHaveAttribute('aria-valuenow', '3');
  await expect(await schemaCell(page, 'name', 1)).toContainText('2');
  await handle.focus();
  await page.keyboard.press('ArrowLeft');
  await page.keyboard.press('ArrowLeft');
  await expect(handle).toHaveAttribute('aria-valuenow', '1');
  await expect(await schemaCell(page, 'name', 1)).toHaveText('1');
  expect(new URL(page.url()).searchParams.get('at')).toBe('commit:1');
  // compare commit 1 with the head
  await page.goto('/ui/explore?ds=chrono3&tab=schema&from=commit:1');
  await page.getByRole('tab', { name: /^Properties/ }).click();
  const name = await schemaCell(page, 'name', 1);
  await expect(name).toContainText('+1');
  await expect(slider(page, 'Schema history').getByRole('status')).toContainText('changed');
});

test('A9: the compare dialog opens the slider on its two states', async ({ page }) => {
  await page.goto('/ui/explore?ds=chrono3&tab=schema');
  await page.getByRole('button', { name: 'Compare', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByPlaceholder('commit, time: or snapshot:').fill('commit:1');
  await dialog.getByRole('button', { name: 'Open on the slider' }).click();
  await expect(dialog).toBeHidden();
  await expect(slider(page, 'Schema history')).toBeVisible();
  await expect(
    slider(page, 'Schema history').getByRole('slider', { name: 'Compared from' }),
  ).toHaveAttribute('aria-valuenow', '1');
});

test.describe('U4: the class profile follows the schema slider', () => {
  const put = (request: APIRequestContext, sparql: string, message: string) =>
    request
      .post('/typed/update', {
        headers: {
          'Content-Type': 'application/sparql-update',
          'Sparkles-Commit-Message': message,
        },
        data: `PREFIX ex: <${EX}> ${sparql}`,
      })
      .then((r) => expect(r.ok(), `update: ${message}`).toBe(true));

  test.beforeAll(async ({ request }) => {
    const r = await request.post('/$/datasets', {
      data: { dbName: 'typed', dbType: 'persistent' },
    });
    if (r.status() === 409) return;
    expect(r.ok()).toBe(true);
    await put(request, 'INSERT DATA { ex:a a ex:Thing ; ex:size 1 }', 'First thing');
    await put(request, 'INSERT DATA { ex:b a ex:Thing ; ex:size 2 }', 'Second thing');
    await put(request, 'INSERT DATA { ex:c a ex:Thing ; ex:size 3 }', 'Third thing');
  });

  test('the profile is read at the commit the schema shows', async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'typed'));
    const asked: string[] = [];
    page.on('request', (req) => {
      const u = new URL(req.url());
      if (u.pathname === '/$/schema/typed/profiles') asked.push(u.searchParams.get('at') ?? 'head');
    });
    await page.goto('/ui/explore?ds=typed&tab=schema');
    await page.getByRole('button', { name: 'Schema history' }).click();
    await page.getByText('Thing', { exact: true }).first().click();
    const lines = page.locator('.profile-line');
    await expect(lines).toContainText('3 of 3');
    const handle = right(page, 'Schema history');
    await handle.focus();
    await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('ArrowLeft');
    await expect(handle).toHaveAttribute('aria-valuenow', '1');
    await expect(lines).toContainText('1 of 1');
    expect(asked.at(-1)).toBe('commit:1');
  });
});

// --- a history too long to list ---------------------------------------------------------------

const T0 = Date.parse('2026-01-01T00:00:00Z');
const HEAD = 20_000;
const commitAt = (seq: number) => ({
  seq,
  parent: seq ? seq - 1 : null,
  ref: `commit:${seq}`,
  timestamp: new Date(T0 + seq * 60_000).toISOString(),
  kind: 'update',
  inserted: 3,
  deleted: 1,
  quads: seq,
  generation: 'gen-0001',
  bulk: false,
  reconstructable: seq >= 15_000,
});

function timelineOf(from: number, to: number, buckets: number) {
  const width = Math.max(1, Math.ceil((to - from + 1) / buckets));
  const out = [];
  for (let first = from; first <= to; first += width) {
    const last = Math.min(to, first + width - 1);
    out.push({
      firstSeq: first,
      lastSeq: last,
      firstTime: commitAt(first).timestamp,
      lastTime: commitAt(last).timestamp,
      commits: last - first + 1,
      inserted: 3 * (last - first + 1),
      deleted: last - first + 1,
      kinds: { update: last - first + 1 },
      merges: 0,
      snapshots: 0,
    });
  }
  return {
    dataset: 'chrono3',
    datasetId: 'x',
    head: HEAD,
    firstSeq: from,
    lastSeq: to,
    commits: to - from + 1,
    bucketWidth: width,
    oldestReconstructable: 15_000,
    readable: [{ from: 15_000, to: HEAD }],
    buckets: out,
  };
}

test('a long history is drawn in buckets, and zooming reads the commits of the range', async ({
  page,
}) => {
  const seen: string[] = [];
  await page.route(/\/\$\/commits\/chrono3\/timeline/, async (route) => {
    const u = new URL(route.request().url());
    seen.push(`timeline ${u.search}`);
    const num = (k: string, d: number) =>
      u.searchParams.has(k) ? Number(u.searchParams.get(k)) : d;
    await route.fulfill({
      json: timelineOf(num('from', 0), num('to', HEAD), num('buckets', 200)),
    });
  });
  await page.route(/\/\$\/commits\/chrono3(\?.*)?$/, async (route) => {
    const u = new URL(route.request().url());
    seen.push(`commits ${u.search}`);
    const limit = Number(u.searchParams.get('limit') ?? 50);
    const before = u.searchParams.has('before') ? Number(u.searchParams.get('before')) : HEAD + 1;
    const commits = [];
    for (let seq = before - 1; seq >= Math.max(0, before - limit); seq--)
      commits.push(commitAt(seq));
    const last = commits.at(-1)?.seq ?? 0;
    await route.fulfill({
      json: {
        dataset: 'chrono3',
        datasetId: 'x',
        head: HEAD,
        firstRetained: 0,
        complete: true,
        oldestReconstructable: 15_000,
        reconstructable: [{ from: 15_000, to: HEAD }],
        commits,
        next: last > 0 ? `/$/commits/chrono3?before=${last}&limit=${limit}` : null,
      },
    });
  });
  await page.goto('/ui/query');
  await page.getByRole('button', { name: 'History', exact: true }).click();
  const bar = page.getByRole('region', { name: 'History time slider' });
  await expect(bar).toContainText('20,001 commits in buckets');
  // the first 15,000 commits are gone
  await expect(bar.getByText('gone')).toBeVisible();
  const handle = bar.getByRole('slider', { name: 'Commit' });
  await expect(handle).toHaveAttribute('aria-valuenow', String(HEAD));
  // zoom in until the range is small enough to read commit by commit
  await handle.focus();
  for (let i = 0; i < 14 && !seen.some((s) => s.startsWith('commits ?limit=500&before=')); i++) {
    await page.keyboard.press('+');
    await page.waitForTimeout(250);
  }
  expect(seen.some((s) => /^timeline .*from=\d+/.test(s))).toBe(true);
  expect(seen.some((s) => /^commits \?limit=500&before=\d+/.test(s))).toBe(true);
  // the handle stays on the head, and each commit has a stop now
  await expect(handle).toHaveAttribute('aria-valuenow', String(HEAD));
  await expect(bar).toContainText('20,001 commits in buckets');
});
