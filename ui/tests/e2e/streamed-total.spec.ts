// The query page's table against a real server's automatic execution
// (docs/specs/X05-streaming-query-execution.md). The table sends no `execution`, so the
// server's default `auto` streams a grouping over 30,000 rows and a plain scan of them. A
// streamed answer cut at the row limit has no total, and the page says so. The dataset is
// compacted first, because automatic execution streams only over an empty delta.

import type { Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

const DS = 'streamed-total';
const ROWS = 30_000;
// one group per subject, so the grouping has 30,000 rows; a text no other test runs, so
// the server has no history that keeps it eager
const QUERY = `SELECT ?s (SUM(STRLEN(?o)) AS ?n) WHERE { ?s <urn:streamed:v> ?o } GROUP BY ?s`;

open.beforeAll(async ({ request }) => {
  open.setTimeout(120_000);
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok() || r.status() === 409, await r.text()).toBe(true);
  if (r.status() === 409) return;
  const lines = Array.from(
    { length: ROWS },
    (_, i) => `<urn:streamed:s${i}> <urn:streamed:v> "${i}" .`,
  );
  const l = await request.post(`/${DS}/data`, {
    headers: { 'Content-Type': 'application/n-triples' },
    data: lines.join('\n'),
  });
  expect(l.ok(), await l.text()).toBe(true);
  const c = await request.post(`/$/compact/${DS}`);
  expect(c.ok() || c.status() === 409, await c.text()).toBe(true);
  await expect
    .poll(
      async () => {
        const s = await (await request.get(`/$/ready/${DS}`)).json();
        return s.datasets?.[0]?.deltaQuads ?? -1;
      },
      { timeout: 60_000 },
    )
    .toBe(0);
});

function seed(page: Page, query: string, limit: number) {
  return page.addInitScript(
    ([ds, query, limit]) => {
      if (sessionStorage.getItem('seeded')) return;
      sessionStorage.setItem('seeded', '1');
      localStorage.setItem('sparkles.dataset', ds as string);
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Query 1', query }], active: 't1', limit }),
      );
    },
    [DS, query, limit] as const,
  );
}

type Meta = { totalRows: number | null; sentRows: number; status?: string; plan: object };

/** Runs the query and returns the URL it sent and the metadata the server answered. */
async function run(page: Page, query: string) {
  // the editor's read, not the label lookups that the table sends after it
  const answered = page.waitForResponse(
    (r) => r.url().includes(`/${DS}/sparql`) && r.request().postData() === query,
  );
  await page.getByRole('button', { name: /^Run/ }).click();
  const res = await answered;
  const body = (await res.json()) as { meta: Meta };
  return { url: new URL(res.url()), meta: body.meta };
}

const tableTab = (page: Page) => page.getByRole('tab', { name: /Table/ });

open('a streamed answer cut at the row limit shows an unknown total', async ({ page }) => {
  await seed(page, QUERY, 1_000);
  await page.goto('/ui/query');
  const { url, meta } = await run(page, QUERY);
  expect(url.searchParams.has('execution')).toBe(false);
  // the server streamed it: a cursor plan, stopped at the row limit, with no total
  expect(meta).toMatchObject({ totalRows: null, sentRows: 1_000, status: 'stopped' });
  expect(meta.plan).toHaveProperty('materializes');
  await expect(tableTab(page)).toContainText('1,000+');
  await expect(page.getByText('so there may be more and the total is unknown')).toBeVisible();
  await expect(page.getByRole('grid')).toContainText('urn:streamed:s');
});

open('a streamed answer within the row limit shows its total', async ({ page }) => {
  // the same grouping with a different text, under a limit the 30,000 groups fit in
  const whole = `${QUERY} # whole`;
  await seed(page, whole, 100_000);
  await page.goto('/ui/query');
  const { meta } = await run(page, whole);
  expect(meta).toMatchObject({ totalRows: ROWS, sentRows: ROWS, status: 'complete' });
  expect(meta.plan).toHaveProperty('materializes');
  await expect(tableTab(page)).toContainText('30,000');
  await expect(tableTab(page)).not.toContainText('+');
  await expect(page.getByText('the total is unknown')).toHaveCount(0);
});

open('a plain scan of the dataset streams, and its total is unknown', async ({ page }) => {
  // automatic execution streams plain scans of a thousand rows or more
  const scan = 'SELECT ?s ?o WHERE { ?s <urn:streamed:v> ?o } # scan';
  await seed(page, scan, 1_000);
  await page.goto('/ui/query');
  const { url, meta } = await run(page, scan);
  expect(url.searchParams.has('execution')).toBe(false);
  expect(meta).toMatchObject({ totalRows: null, sentRows: 1_000, status: 'stopped' });
  expect(meta.plan).toHaveProperty('materializes');
  await expect(tableTab(page)).toContainText('1,000+');
});
