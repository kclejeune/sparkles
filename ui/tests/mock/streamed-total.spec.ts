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

/** Answer the page's queries as a streamed answer that the row limit stopped would be. */
async function streamAnswers(page: Page, urls: URL[]) {
  await page.route(`**/${DS}/sparql*`, async (route: Route) => {
    const url = new URL(route.request().url());
    urls.push(url);
    const res = await route.fetch();
    const body = (await res.json()) as Body;
    if (url.searchParams.get('execution') !== 'eager') {
      if (body.meta.sentRows < (body.meta.totalRows ?? 0)) {
        body.meta.totalRows = null;
        body.meta.status = 'stopped';
      } else body.meta.status = 'complete';
      if (body.triples) {
        body.quads = body.triples.map((t) => [...t, null]);
        body.triples = [];
      }
    }
    await route.fulfill({ response: res, json: body });
  });
}

const grid = (page: Page) => page.getByRole('grid');
const tableTab = (page: Page) => page.getByRole('tab', { name: /Table/ });

test('a streamed answer cut at the row limit has an unknown total', async ({ page }) => {
  const urls: URL[] = [];
  await streamAnswers(page, urls);
  await prepare(page, `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v }`);
  await page.goto('/ui/query');
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(grid(page)).toBeVisible();
  // the table leaves the mode to the server
  expect(urls.at(-1)?.searchParams.has('execution')).toBe(false);
  expect(urls.at(-1)?.searchParams.get('send')).toBe('1000');
  await expect(tableTab(page)).toContainText('1,000+');
  await expect(page.getByText('Showing the first 1,000 results.')).toBeVisible();
  await expect(page.getByText('so there may be more and the total is unknown')).toBeVisible();
  await expect(page.getByText(/Showing the first 1,000 of/)).toHaveCount(0);
});

test('a streamed answer within the row limit keeps its total', async ({ page }) => {
  await streamAnswers(page, []);
  await prepare(page, `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v } LIMIT 20`);
  await page.goto('/ui/query');
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(grid(page).getByRole('row')).toHaveCount(21);
  await expect(tableTab(page)).toContainText('20');
  await expect(tableTab(page)).not.toContainText('+');
  await expect(page.getByText('the total is unknown')).toHaveCount(0);
});

test('a streamed graph answer, sent as quads, fills the graph and the table', async ({ page }) => {
  await streamAnswers(page, []);
  await prepare(page, `${PREFIXES}\nCONSTRUCT { ?s ex:w ?v } WHERE { ?s ex:v ?v }`);
  await page.goto('/ui/query');
  await page.getByRole('button', { name: /^Run/ }).click();
  // a graph answer opens in the Graph view, whose 1,000 statements draw 400 nodes
  await expect(page.getByText('400 nodes, 200 edges')).toBeVisible();
  await expect(tableTab(page)).toContainText('1,000+');
  await tableTab(page).click();
  await expect(grid(page)).toContainText('http://example.org/w');
});

test('a comparison reads both states eagerly and compares their counts', async ({ page }) => {
  const urls: URL[] = [];
  await streamAnswers(page, urls);
  await prepare(page, `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v`);
  await page.goto('/ui/query');
  await page.getByRole('button', { name: /^Run/ }).click();
  await expect(grid(page)).toBeVisible();
  await page.getByRole('button', { name: 'History', exact: true }).click();
  const slider = page.getByRole('region', { name: 'History time slider' });
  await slider.getByRole('slider', { name: /^(Commit|Compared to)$/ }).focus();
  const before = urls.length;
  await page.keyboard.press('c');
  await expect(page.getByText('The results were truncated')).toBeVisible();
  await expect(page.getByText('(1,100 rows before, 1,101 after)')).toBeVisible();
  const compared = urls.slice(before);
  expect(compared.length).toBeGreaterThanOrEqual(2);
  for (const u of compared) expect(u.searchParams.get('execution')).toBe('eager');
  await expect(page.getByText('the total is unknown')).toHaveCount(0);
});
