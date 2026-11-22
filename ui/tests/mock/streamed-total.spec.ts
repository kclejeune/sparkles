// The query page's table takes the server's default execution, which streams the plans
// that are faster as cursors (docs/specs/X05-streaming-query-execution.md). A streamed
// answer cut at the row limit has no total, so the page says the total is unknown. The
// mock always answers eagerly, so these tests rewrite its answers into the shapes that the
// server's cursor writers send: `totalRows: null` with `status: "stopped"`, and a graph
// answer whose statements come as quads with a null graph. A comparison asks for eager
// execution on both reads, since it compares whole results or exact counts.

import { expect, test, type Page, type Route } from '@playwright/test';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const DS = 'streamed';
const PREFIXES = 'PREFIX ex: <http://example.org/>';

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  const rows = Array.from({ length: 1100 }, (_, i) => `ex:n${i} ex:v ${i} .`).join(' ');
  for (const [sparql, message] of [
    [`INSERT DATA { ${rows} }`, 'Load'],
    ['INSERT DATA { ex:x1 ex:v 5000 }', 'One more'],
  ]) {
    const u = await request.post(`/${DS}/update`, {
      headers: {
        'Content-Type': 'application/sparql-update',
        'Sparkles-Commit-Message': message,
      },
      data: `${PREFIXES} ${sparql}`,
    });
    expect(u.ok()).toBe(true);
  }
});

async function prepare(page: Page, query: string, limit = 1_000) {
  await page.addInitScript(
    ([ds, query, limit]) => {
      localStorage.setItem('sparkles.dataset', ds as string);
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Query 1', query }], active: 't1', limit }),
      );
    },
    [DS, query, limit] as const,
  );
}

type Body = {
  triples?: unknown[][];
  quads?: unknown[][];
  meta: { totalRows: number | null; sentRows: number; status?: string };
};

/**
 * Answer the page's reads of `query` as the server's cursor writers answer a streamed
 * read, and record their URLs. Other requests, such as the table's label lookups, pass
 * through. A read the row limit cut, or one with exactly `stopAt` rows, stops with no
 * total, since a cursor that reaches the limit does not look for another row.
 */
async function streamAnswers(page: Page, query: string, stopAt?: number) {
  const urls: URL[] = [];
  await page.route(`**/${DS}/sparql*`, async (route: Route) => {
    const req = route.request();
    if (req.method() !== 'POST' || req.postData() !== query) return route.fallback();
    const url = new URL(req.url());
    urls.push(url);
    const res = await route.fetch();
    const body = (await res.json()) as Body;
    if (url.searchParams.get('execution') !== 'eager') {
      const m = body.meta;
      if (m.sentRows < (m.totalRows ?? 0) || m.sentRows === stopAt) {
        m.totalRows = null;
        m.status = 'stopped';
      } else m.status = 'complete';
      if (body.triples) {
        body.quads = body.triples.map((t) => [...t, null]);
        body.triples = [];
      }
    }
    await route.fulfill({ response: res, json: body });
  });
  return urls;
}

// let a handler that is still reading finish before the page closes
test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: 'wait' });
});

const grid = (page: Page) => page.getByRole('grid');
const tableTab = (page: Page) => page.getByRole('tab', { name: /Table/ });
const run = (page: Page) => page.getByRole('button', { name: /^Run/ }).click();

test('a streamed answer cut at the row limit has an unknown total', async ({ page }) => {
  const query = `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v }`;
  const urls = await streamAnswers(page, query);
  await prepare(page, query);
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toBeVisible();
  // the table leaves the mode to the server
  expect(urls).toHaveLength(1);
  expect(urls[0].searchParams.has('execution')).toBe(false);
  expect(urls[0].searchParams.get('send')).toBe('1000');
  await expect(tableTab(page)).toContainText('1,000+');
  await expect(page.getByText('Showing the first 1,000 results.')).toBeVisible();
  await expect(page.getByText('so there may be more and the total is unknown')).toBeVisible();
  await expect(page.getByText(/Showing the first 1,000 of/)).toHaveCount(0);
});

test('a streamed answer of exactly the row limit may have more rows', async ({ page }) => {
  const query = `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v } LIMIT 1000`;
  await streamAnswers(page, query, 1_000);
  await prepare(page, query);
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toBeVisible();
  await expect(tableTab(page)).toContainText('1,000+');
  await expect(page.getByText('so there may be more and the total is unknown')).toBeVisible();
});

test('a streamed answer within the row limit keeps its total', async ({ page }) => {
  const query = `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v } LIMIT 20`;
  await streamAnswers(page, query);
  await prepare(page, query);
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page).getByRole('row')).toHaveCount(21);
  await expect(tableTab(page)).toContainText('20');
  await expect(tableTab(page)).not.toContainText('+');
  await expect(page.getByText('the total is unknown')).toHaveCount(0);
});

test('a streamed graph answer, sent as quads, fills the graph and the table', async ({ page }) => {
  const query = `${PREFIXES}\nCONSTRUCT { ?s ex:w ?v } WHERE { ?s ex:v ?v }`;
  await streamAnswers(page, query);
  await prepare(page, query);
  await page.goto('/ui/query');
  await run(page);
  // a graph answer opens in the Graph view, whose 1,000 statements draw 400 nodes
  await expect(page.getByText('400 nodes, 200 edges')).toBeVisible();
  await expect(tableTab(page)).toContainText('1,000+');
  await tableTab(page).click();
  await expect(grid(page)).toContainText('http://example.org/w');
});

test('a comparison reads both states eagerly and compares their counts', async ({ page }) => {
  const query = `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v`;
  const urls = await streamAnswers(page, query);
  await prepare(page, query);
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toBeVisible();
  await page.getByRole('button', { name: 'History', exact: true }).click();
  const slider = page.getByRole('region', { name: 'History time slider' });
  await slider.getByRole('slider', { name: /^(Commit|Compared to)$/ }).focus();
  const before = urls.length;
  await page.keyboard.press('c');
  await expect(page.getByText('The results were truncated')).toBeVisible();
  await expect(page.getByText('(1,100 rows before, 1,101 after)')).toBeVisible();
  const compared = urls.slice(before);
  expect(compared).toHaveLength(2);
  for (const u of compared) expect(u.searchParams.get('execution')).toBe('eager');
  await expect(page.getByText('the total is unknown')).toHaveCount(0);
});
