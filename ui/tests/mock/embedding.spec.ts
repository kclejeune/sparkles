// The embedding map (docs/specs/U11-linked-views.md, Phase 3) against the mock's
// projection route (mock/projection.mjs). `foaf`'s index `embedding` is a real PCA of its
// 8-dimensional vectors, computed in the request, and `world`'s index `shipments` is 6,000
// invented shipments computed as a task. The tests run A7 (a lasso becomes a VALUES block),
// Open as graph, the task and its legend, staleness, the states without an index and
// without access, and the page at 390 px.

import { devices, expect as baseExpect, test, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });
test.use({ actionTimeout: 15_000 });

const expect = baseExpect.configure({ timeout: 15_000 });

type Pt = { entity: string; x: number; y: number };

async function openMap(page: Page, ds: string, index: string) {
  await page.goto(`/ui/embedding?ds=${ds}&index=${index}`);
  const map = page.locator('.emap[data-ready="true"]');
  await expect(map).toBeVisible();
  await expect(page.getByTestId('facts')).toBeVisible();
  return map;
}

/** viewport = a × point + b, as the map reports it once drawn. */
async function transform(page: Page) {
  const map = page.locator('.emap');
  await expect(map).toHaveAttribute('data-transform', /,/);
  const [ax, bx, ay, by] = (await map.getAttribute('data-transform'))!.split(',').map(Number);
  const box = (await map.boundingBox())!;
  return (p: { x: number; y: number }) => ({
    x: box.x + ax * p.x + bx,
    y: box.y + ay * p.y + by,
  });
}

async function projectionPoints(page: Page, ds: string, index: string): Promise<Pt[]> {
  const r = await page.request.get(`/$/vector/${ds}/${index}/projection`);
  expect(r.ok()).toBe(true);
  return (await r.json()).points;
}

async function editorText(page: Page) {
  return page.evaluate(() =>
    [...document.querySelectorAll('.cm-content .cm-line')].map((l) => l.textContent).join('\n'),
  );
}

test('A7. A lasso selects the points it holds, and Use in query writes them as a VALUES block', async ({
  page,
}) => {
  await openMap(page, 'foaf', 'embedding');
  await expect(page.getByTestId('facts')).toContainText('57 entities from 58 vectors of 8');
  await expect(page.getByTestId('facts')).toContainText('of the variance');
  const points = await projectionPoints(page, 'foaf', 'embedding');
  expect(points).toHaveLength(57);
  // let the camera settle before reading the transform
  await page.waitForTimeout(400);
  const toView = await transform(page);
  const view = points.map((p) => ({ ...p, v: toView(p) }));
  const box = (await page.locator('.emap').boundingBox())!;
  // a lasso around the left half of the map
  const mid = box.x + box.width / 2;
  const want = view.filter((p) => p.v.x < mid).map((p) => `<${p.entity}>`);
  expect(want.length).toBeGreaterThan(3);
  expect(want.length).toBeLessThan(points.length);
  await page.getByRole('radio', { name: 'Lasso' }).click();
  const path = [
    { x: box.x + 2, y: box.y + 2 },
    { x: mid, y: box.y + 2 },
    { x: mid, y: box.y + box.height / 2 },
    { x: mid, y: box.y + box.height - 2 },
    { x: box.x + 2, y: box.y + box.height - 2 },
  ];
  await page.mouse.move(path[0].x, path[0].y);
  await page.mouse.down();
  for (const p of path.slice(1)) await page.mouse.move(p.x, p.y, { steps: 4 });
  await page.mouse.up();
  const bar = page.getByRole('region', { name: 'Selection' });
  await expect(bar).toContainText(`${want.length} selected by the embedding map`);
  await expect(page.locator('.emap')).toHaveAttribute('data-selected', String(want.length));
  // the selection bar moved the map down
  box.y = (await page.locator('.emap').boundingBox())!.y;
  // Shift adds with a box around the whole map
  await page.getByRole('radio', { name: 'Box' }).click();
  await page.keyboard.down('Shift');
  await page.mouse.move(box.x + 2, box.y + 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width - 2, box.y + box.height - 2, { steps: 4 });
  await page.mouse.up();
  await page.keyboard.up('Shift');
  await expect(bar).toContainText('57 selected by the embedding map');
  // Alt takes the left half out again
  await page.keyboard.down('Alt');
  await page.mouse.move(box.x + 2, box.y + 2);
  await page.mouse.down();
  await page.mouse.move(mid, box.y + box.height - 2, { steps: 4 });
  await page.mouse.up();
  await page.keyboard.up('Alt');
  const right = view.filter((p) => p.v.x >= mid).map((p) => `<${p.entity}>`);
  await expect(bar).toContainText(`${right.length} selected by the embedding map`);
  await bar.getByRole('button', { name: 'Use in query' }).click();
  await expect(page).toHaveURL(/\/ui\/query/);
  await expect(page.locator('.cm-content')).toContainText('VALUES ?entity {');
  const text = await editorText(page);
  for (const iri of right) expect(text).toContain(iri);
  for (const iri of want) expect(text).not.toContain(iri);
});

test('Open as graph draws the selected entities in Explore', async ({ page }) => {
  await openMap(page, 'foaf', 'embedding');
  const box = (await page.locator('.emap').boundingBox())!;
  await page.getByRole('radio', { name: 'Box' }).click();
  await page.mouse.move(box.x + 2, box.y + 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width - 2, box.y + box.height - 2, { steps: 4 });
  await page.mouse.up();
  const bar = page.getByRole('region', { name: 'Selection' });
  await expect(bar).toContainText('57 selected');
  await bar.getByRole('button', { name: 'Open as graph' }).click();
  await expect(page).toHaveURL(/\/ui\/explore/);
  await expect(page.getByText(/\d+ nodes, \d+ edges/)).toBeVisible();
  await expect(page.getByText('Drew 57 entities')).toBeVisible();
});

test('A large projection runs as a task with progress, then colours by class with a legend and lists the points in view', async ({
  page,
}) => {
  await page.goto('/ui/embedding?ds=world&index=shipments');
  await expect(page.getByTestId('progress')).toBeVisible();
  await expect(page.getByTestId('progress')).toContainText('Projecting shipments');
  const map = page.locator('.emap[data-ready="true"]');
  await expect(map).toBeVisible();
  await expect(map).toHaveAttribute('data-points', '6000');
  await expect(page.getByTestId('progress')).toHaveCount(0);
  await expect(page.getByTestId('facts')).toContainText('6,000 entities');
  const legend = page.getByRole('complementary', { name: 'Legend' });
  await expect(legend).toContainText('Colour by class');
  await expect(legend.locator('li')).toHaveCount(8);
  await expect(legend).toContainText('ElectronicsCargo');
  await page.getByRole('button', { name: 'List' }).click();
  const list = page.getByRole('region', { name: 'Points in view' });
  await expect(list.locator('tbody tr')).toHaveCount(200);
  // a checkbox in the list selects too
  await list.locator('tbody tr').first().getByRole('checkbox').check();
  await expect(page.getByRole('region', { name: 'Selection' })).toContainText(
    '1 selected by the embedding map',
  );
  await page.getByRole('heading', { name: 'Embedding map' }).click();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('region', { name: 'Selection' })).toHaveCount(0);
});

test('Cancel stops a projection task', async ({ page }) => {
  await page.goto('/ui/embedding?ds=world&index=shipments&method=neighbours');
  const progress = page.getByTestId('progress');
  await expect(progress).toBeVisible();
  await progress.getByRole('button', { name: 'Cancel' }).click();
  await expect(progress).toHaveCount(0);
  await expect(page.locator('.emap')).toHaveCount(0);
});

test('A change to the data marks the projection stale, and Recompute builds it again', async ({
  page,
}) => {
  await openMap(page, 'foaf', 'embedding');
  const triple =
    '<http://example.org/resource/zz-stale> <http://www.w3.org/2000/01/rdf-schema#label> "stale"';
  const write = (u: string) =>
    page.request.post('/foaf/update', {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: u,
    });
  expect((await write(`INSERT DATA { ${triple} }`)).ok()).toBe(true);
  try {
    await page.reload();
    await expect(page.getByTestId('stale')).toBeVisible();
    await page.getByRole('button', { name: 'Recompute' }).click();
    await expect(page.getByTestId('stale')).toHaveCount(0);
  } finally {
    expect((await write(`DELETE DATA { ${triple} }`)).ok()).toBe(true);
  }
});

test('A dataset without a vector index links to the dataset page', async ({ page }) => {
  await page.goto('/ui/embedding?ds=scratch');
  const empty = page.locator('[data-state="no-index"]');
  await expect(empty).toContainText('This dataset has no vector index to map');
  await expect(
    empty.getByRole('link', { name: 'Create one on the dataset page.' }),
  ).toHaveAttribute('href', /\/ui\/datasets\/scratch/);
});

test('A caller limited to some graphs is told the map needs the whole dataset', async ({
  page,
}) => {
  await page.route('**/$/vector/foaf/embedding/projection*', (r) =>
    r.fulfill({
      status: 403,
      contentType: 'application/json',
      body: JSON.stringify({ error: 'requires access to the whole dataset foaf' }),
    }),
  );
  await page.goto('/ui/embedding?ds=foaf&index=embedding');
  await expect(page.locator('[data-state="forbidden"]')).toContainText(
    'Embedding maps need access to every graph',
  );
});

test('The dataset page and the Similar page link to the map', async ({ page }) => {
  await page.goto('/ui/similar?ds=foaf&index=embedding');
  await page.getByRole('link', { name: 'Embedding map' }).click();
  await expect(page).toHaveURL(/\/ui\/embedding\?ds=foaf&index=embedding/);
  await expect(page.locator('.emap[data-ready="true"]')).toBeVisible();
});

test.describe('at 390 px', () => {
  const { defaultBrowserType: _ignored, ...iphone } = devices['iPhone 12'];
  test.use(iphone);
  test('the map fits the screen and a Lasso toggle replaces the pointer modes', async ({
    page,
  }) => {
    await openMap(page, 'foaf', 'embedding');
    await expect(page.getByRole('radiogroup', { name: 'Pointer' })).toBeHidden();
    const toggle = page.locator('.lasso-toggle');
    await expect(toggle).toBeVisible();
    await toggle.tap();
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await expect(page.locator('.emap')).toHaveAttribute('data-mode', 'lasso');
    await expectNoSidewaysScroll(page, 'the embedding map');
  });
});
