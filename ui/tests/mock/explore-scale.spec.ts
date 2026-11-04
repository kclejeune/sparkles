// Explore at scale (ROADMAP item 725): a resource with a large fan-out is drawn as one
// "N via predicate" group node that opens a page at a time, the filters choose what is drawn
// without reading less, and the graph switches to WebGL past a threshold of visible nodes.
// The dataset `scale1` holds a hub with 300 members of three classes, each linked to the next.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, phone } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';
const N = 300;
const CLASSES = ['Alpha', 'Beta', 'Gamma'];

function data() {
  const t: string[] = [];
  t.push(`ex:hub a ex:Org ; rdfs:label "Hub" ; ex:partOf ex:root ; ex:founder ex:ada .`);
  t.push(`ex:ada a ex:Person ; rdfs:label "Ada" .`);
  for (let i = 0; i < N; i++) {
    t.push(
      `ex:hub ex:member ex:m${i} . ex:m${i} a ex:${CLASSES[i % 3]} ; rdfs:label "Member ${i}" ; ex:knows ex:m${(i + 1) % N} .`,
    );
  }
  return `PREFIX ex: <${EX}>\nPREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\nINSERT DATA {\n${t.join('\n')}\n}`;
}

async function create(request: APIRequestContext, name: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  const u = await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: data(),
  });
  expect(u.ok()).toBe(true);
}

test.beforeAll(async ({ request }) => {
  await create(request, 'scale1');
});

type Drawn = { nodes: [string, ...unknown[]][]; edges: [string, ...unknown[]][] };
const cyDrawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};

/** Open Explore on the hub, with the WebGL threshold at `from` visible nodes. */
async function open(page: Page, from = 1000) {
  await page.addInitScript(
    ([d, f]) => {
      localStorage.setItem('sparkles.dataset', d as string);
      localStorage.setItem('sparkles.explore.webglFrom', String(f));
    },
    ['scale1', from],
  );
  await page.goto(`/ui/explore?ds=scale1&iri=${encodeURIComponent(`${EX}hub`)}`);
  await expect(page.getByTestId('graph-counts')).toContainText(/\d+ nodes/, { timeout: 45_000 });
}

const counts = (page: Page) => page.getByTestId('graph-counts');
const panel = (page: Page) => page.getByTestId('group-panel');

async function selectGroup(page: Page) {
  await page.getByRole('button', { name: 'List', exact: true }).click();
  await page
    .getByRole('table', { name: 'Drawn nodes' })
    .getByRole('button', { name: /^300 via/ })
    .click();
  await expect(panel(page)).toBeVisible();
}

test('a fan-out of 300 is one group node, and the small predicates are drawn as before', async ({
  page,
}) => {
  await open(page);
  // the hub, its class, its root, its founder and the group
  await expect(counts(page)).toContainText('5 nodes');
  const g = await cyDrawn(page);
  const labels = g.nodes.map((n) => String(n[0]));
  expect(labels.some((l) => /^300 via .*member/.test(l))).toBe(true);
  expect(labels).toContain('Ada');
  expect(labels.some((l) => /Member/.test(l))).toBe(false);
});

test('the group opens a page at a time, filters inside and splits by class', async ({ page }) => {
  await open(page);
  await selectGroup(page);
  const range = page.getByTestId('group-range');
  await expect(range).toContainText('1–100 of 300');
  await panel(page).getByRole('button', { name: 'Next' }).click();
  await expect(range).toContainText('101–200 of 300');
  await panel(page).getByRole('button', { name: 'Previous' }).click();
  await expect(range).toContainText('1–100 of 300');
  // the split by class
  const alpha = panel(page).getByRole('button', { name: /Alpha/ });
  await expect(alpha).toContainText('100');
  await alpha.click();
  await expect(range).toContainText('1–100 of 100');
  await alpha.click();
  await expect(range).toContainText('of 300');
  // the filter inside the group
  await panel(page).getByLabel('Filter the members by name').fill('Member 25');
  await expect(range).toContainText('1–11 of 11');
  // drawing a member adds one node and its edge, not the group
  await panel(page).getByRole('button', { name: 'Draw', exact: true }).first().click();
  await expect(counts(page)).toContainText('6 nodes');
  await panel(page)
    .getByRole('button', { name: /^Draw this page/ })
    .click();
  await expect(counts(page)).toContainText('16 nodes');
});

test('past the threshold the graph is drawn with WebGL and keeps its interactions', async ({
  page,
}) => {
  await open(page, 20);
  await selectGroup(page);
  await panel(page)
    .getByRole('button', { name: /^Draw this page/ })
    .click();
  await expect(counts(page)).toContainText('105 nodes');
  await expect(counts(page)).toContainText('WebGL');
  // the list gives way to the canvas
  await page.getByRole('button', { name: 'List', exact: true }).click();
  await expect(page.locator('.lg-host canvas').first()).toBeVisible();
  // the group is still selected and its panel still opens, and Fit and Re-run layout work
  await expect(panel(page)).toBeVisible();
  await page.getByRole('button', { name: 'Fit to view' }).click();
  await page.getByRole('button', { name: 'Re-run layout' }).click();
  await expect(page.locator('.lg-overlay')).toHaveAttribute('data-layout-runs', /[2-9]|\d\d/);
});

test('the filters choose what is drawn and nothing is read again', async ({ page }) => {
  await open(page, 20);
  await selectGroup(page);
  await panel(page)
    .getByRole('button', { name: /^Draw this page/ })
    .click();
  await expect(counts(page)).toContainText('105 nodes');
  const reads: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/sparql') || r.url().includes('/query')) reads.push(r.url());
  });
  await page.getByRole('button', { name: 'Filters', exact: true }).click();
  const filters = page.getByTestId('explore-filters');
  await expect(filters).toBeVisible();
  const summary = page.getByTestId('filter-counts');
  await expect(summary).toContainText('105 of 105 nodes');
  await expect(summary).toContainText('104 of 104 edges');
  // the edge types with their counts
  const types = filters.getByRole('list', { name: 'Edge types' });
  await expect(types).toContainText('member');
  await expect(types).toContainText('partOf');
  await types.getByRole('checkbox', { name: /member/ }).uncheck();
  await expect(summary).toContainText('105 of 105 nodes');
  await expect(summary).toContainText('3 of 104 edges');
  await types.getByRole('checkbox', { name: /member/ }).check();
  await expect(summary).toContainText('104 of 104 edges');
  // hide the untyped nodes: the typed hub stays, and the held graph does too
  const classes = filters.getByRole('list', { name: 'Classes' });
  await expect(classes).toContainText('Org');
  await classes.getByRole('checkbox', { name: /No class/ }).uncheck();
  await expect(counts(page)).not.toContainText('105 nodes');
  await expect(page.getByTestId('hidden-chip')).toBeVisible();
  await expect(summary).toContainText('of 105 nodes');
  await classes.getByRole('checkbox', { name: /No class/ }).check();
  await expect(counts(page)).toContainText('105 nodes');
  // a minimum degree and hiding leaves
  await filters.getByLabel('Minimum degree').fill('3');
  await expect(page.getByTestId('min-degree')).toHaveText('3');
  await expect(page.getByTestId('hidden-chip')).toBeVisible();
  await filters.getByRole('button', { name: 'Reset' }).click();
  await expect(counts(page)).toContainText('105 nodes');
  await filters.getByRole('checkbox', { name: /Hide leaves/ }).check();
  await expect(page.getByTestId('hidden-chip')).toBeVisible();
  await filters.getByRole('checkbox', { name: /Hide leaves/ }).uncheck();
  // the budget of visible nodes leaves out the least connected
  await filters.getByLabel('Nodes to draw at most').selectOption('1000');
  await expect(counts(page)).toContainText('105 nodes');
  // none of it read anything: the filters choose what is drawn, not what is loaded
  expect(reads).toEqual([]);
});

test('communities of the memory store group the graph, and a group can be hidden', async ({
  page,
}) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'org'));
  await page.goto(
    `/ui/explore?ds=org&iri=${encodeURIComponent('http://example.org/resource/ana')}`,
  );
  await expect(counts(page)).toContainText(/\d+ nodes/, { timeout: 45_000 });
  const nodesOf = async () =>
    Number(/(\d+) nodes/.exec((await counts(page).textContent()) ?? '')?.[1]);
  await expect.poll(nodesOf).toBeGreaterThan(3);
  const before = await nodesOf();
  await page.getByRole('button', { name: 'Filters', exact: true }).click();
  const filters = page.getByTestId('explore-filters');
  await filters.getByRole('button', { name: 'Group by communities' }).click();
  const groups = filters.getByRole('list', { name: 'Communities' });
  await expect(groups).toContainText('The payments team', { timeout: 15_000 });
  // the focus is never hidden, so a group without it is the one to hide
  await groups.getByRole('checkbox', { name: /no value/ }).uncheck();
  await expect.poll(nodesOf).toBeLessThan(before);
  // the hidden community stays in the list, so that it can come back
  await expect(groups).toContainText('no value');
  await groups.getByRole('checkbox', { name: /no value/ }).check();
  await expect.poll(nodesOf).toBe(before);
});

test('the performance hook is installed only on request and expands a node', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.explore.perfHook', '1'));
  await open(page);
  const hook = () =>
    page.evaluate(async () => {
      const h = (window as unknown as { __perfHook: Record<string, (...a: unknown[]) => unknown> })
        .__perfHook;
      const before = h.nodeCount() as number;
      await h.expand('<http://example.org/ada>');
      return {
        before,
        after: h.nodeCount() as number,
        drawn: h.drawnCount() as number,
        ids: (h.nodeIds() as string[]).length,
        renderer: h.renderer() as string,
      };
    });
  const r = await hook();
  expect(r.renderer).toBe('cytoscape');
  expect(r.drawn).toBe(r.after);
  expect(r.ids).toBe(r.after);
  expect(r.after).toBeGreaterThanOrEqual(r.before);
});

test('members drawn from a group stay when the graph is read at an earlier commit', async ({
  page,
  request,
}) => {
  // commit 1 loads a hub with 30 members, and commit 2 adds a triple that touches none of them
  const made = await request.post('/$/datasets', {
    data: { dbName: 'scalet', dbType: 'persistent' },
  });
  if (made.status() !== 409) {
    expect(made.ok()).toBe(true);
    const members = Array.from(
      { length: 30 },
      (_, i) => `ex:hub ex:member ex:t${i} . ex:t${i} rdfs:label "T${i}" .`,
    );
    for (const [msg, body] of [
      ['Load the hub', members.join(' ')],
      ['An unrelated triple', 'ex:other ex:p ex:q .'],
    ]) {
      const u = await request.post('/scalet/update', {
        headers: { 'Content-Type': 'application/sparql-update', 'Sparkles-Commit-Message': msg },
        data: `PREFIX ex: <${EX}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA { ${body} }`,
      });
      expect(u.ok()).toBe(true);
    }
  }
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'scalet'));
  await page.goto(`/ui/explore?ds=scalet&iri=${encodeURIComponent(`${EX}hub`)}`);
  await expect(counts(page)).toContainText(/\d+ nodes/, { timeout: 45_000 });
  await page.getByRole('button', { name: 'List', exact: true }).click();
  await page
    .getByRole('table', { name: 'Drawn nodes' })
    .getByRole('button', { name: /^30 via/ })
    .click();
  await panel(page)
    .getByRole('button', { name: /^Draw this page/ })
    .click();
  await expect(counts(page)).toContainText('32 nodes');
  await page.getByRole('button', { name: 'Graph history' }).click();
  const slider = page
    .getByRole('region', { name: 'Graph history time slider' })
    .getByRole('slider', { name: /^(Commit|Compared to)$/ });
  await expect(slider).toHaveAttribute('aria-valuenow', '2');
  await slider.focus();
  await page.keyboard.press('ArrowLeft');
  await expect(slider).toHaveAttribute('aria-valuenow', '1');
  // the graph is read again at commit 1, where every drawn member is a member too
  await expect.poll(() => new URL(page.url()).searchParams.get('at')).toBe('commit:1');
  await page.waitForTimeout(2000);
  await expect(counts(page)).toContainText('32 nodes');
});

test.describe('at 390 px', () => {
  test.use({ ...phone, viewport: { width: 390, height: 844 } });

  test('the group panel and the filters fit the screen', async ({ page }) => {
    await open(page, 20);
    await selectGroup(page);
    await expect(panel(page)).toBeVisible();
    await expectNoSidewaysScroll(page, 'the group panel');
    await panel(page)
      .getByRole('button', { name: /^Draw this page/ })
      .click();
    await page.getByRole('button', { name: 'Filters', exact: true }).click();
    await expect(page.getByTestId('explore-filters')).toBeVisible();
    await expectNoSidewaysScroll(page, 'the filters');
  });
});

test('a right click on a node of the WebGL view opens the node menu', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.explore.perfHook', '1'));
  await open(page, 20);
  await selectGroup(page);
  await panel(page)
    .getByRole('button', { name: /^Draw this page/ })
    .click();
  await expect(counts(page)).toContainText('WebGL');
  await page.getByRole('button', { name: 'List', exact: true }).click();
  const canvas = page.locator('.lg-host canvas').first();
  await expect(canvas).toBeVisible();
  // the layout has settled, so the node stays where it is found
  await expect(page.getByText(/^Layout \d+%/)).toHaveCount(0, { timeout: 30_000 });
  await expect
    .poll(() =>
      page.evaluate(
        (id) =>
          (
            window as unknown as {
              __perfHook: { screenOf: (id: string) => unknown };
            }
          ).__perfHook.screenOf(id),
        '<http://example.org/ada>',
      ),
    )
    .not.toBeNull();
  const at = (await page.evaluate(
    (id) =>
      (
        window as unknown as {
          __perfHook: { screenOf: (id: string) => { x: number; y: number } };
        }
      ).__perfHook.screenOf(id),
    '<http://example.org/ada>',
  )) as { x: number; y: number };
  const box = (await canvas.boundingBox())!;
  await page.mouse.click(box.x + at.x, box.y + at.y, { button: 'right' });
  const menu = page.getByRole('menu', { name: 'Node actions' });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole('menuitem', { name: 'Paths from here' })).toBeVisible();
});

test('hiding the type edges does not forget the classes of the nodes', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.explore.perfHook', '1'));
  await open(page);
  // the page installs the hook when it mounts, and an expansion asked before the graph is
  // ready does nothing, so it is asked until the node has its class
  await expect
    .poll(
      () =>
        page.evaluate(async () => {
          const h = (
            window as unknown as {
              __perfHook: { expand: (id: string) => Promise<void>; nodeIds: () => string[] };
            }
          ).__perfHook;
          await h.expand('<http://example.org/ada>');
          return h.nodeIds().includes('<http://example.org/Person>');
        }),
      { timeout: 20_000 },
    )
    .toBe(true);
  await expect(counts(page)).toContainText('6 nodes');
  await page.getByRole('button', { name: 'Filters', exact: true }).click();
  const filters = page.getByTestId('explore-filters');
  const classes = filters.getByRole('list', { name: 'Classes' });
  await classes.getByRole('checkbox', { name: /Person/ }).uncheck();
  await expect(counts(page)).toContainText('5 nodes');
  await page.getByRole('button', { name: 'View', exact: true }).click();
  await page.getByRole('checkbox', { name: /Hide rdf:type/ }).check();
  // the class is still excluded and still listed, and the nodes of the class stay out
  await expect(counts(page)).toContainText('5 nodes');
  await expect(classes).toContainText('Person');
});
