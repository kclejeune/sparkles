// Why is this inferred? (docs/specs/U10-canvas-lenses.md §2.4, Phase 3): acceptance
// examples A10 (a click on the dashed edge of A1 shows A7's tree, and Show on graph draws
// the premises highlighted) and A12 (the keyboard alone, and taps alone at 390 px), with
// Next and the stale note. The mock's reasoner materializes RDFS and its explainer answers
// `POST /$/reason/{ds}/explain` with the same tree shape as the server.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, phone } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';
const DATA = `PREFIX ex: <${EX}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
INSERT DATA {
  ex:Manager rdfs:subClassOf ex:Employee .
  ex:manages rdfs:domain ex:Manager .
  ex:alice ex:manages ex:team7 .
  ex:reportsTo rdfs:range ex:Employee .
  ex:bob ex:reportsTo ex:alice .
  ex:alice ex:knows ex:carol .
}`;

async function create(request: APIRequestContext, name: string) {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  const u = await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: DATA,
  });
  expect(u.ok()).toBe(true);
  const t = await request.post(`/$/reason/${name}`, { form: { profile: 'rdfs' } });
  expect(t.ok()).toBe(true);
  await expect
    .poll(async () => (await (await request.get(`/$/reason/${name}`)).json()).profile, {
      timeout: 20_000,
    })
    .toBe('rdfs');
}

test.beforeAll(async ({ request }) => {
  await create(request, 'why1');
  await create(request, 'why2');
});

type Drawn = { nodes: [string, string][]; edges: [string, string, string?][] };
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};

async function open(page: Page, ds: string) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto(`/ui/explore?ds=${ds}&iri=${encodeURIComponent(`${EX}alice`)}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(3);
  // the inferred-edge lens marks the type edges
  await expect
    .poll(async () => (await drawn(page)).edges.some(([, , i]) => i === 'inferred'))
    .toBe(true);
}

/** Where the canvas draws the middle of the edge from `from` to the node labelled `to`. */
async function edgeOn(page: Page, from: string, to: string) {
  return await page
    .locator('.wrap canvas')
    .first()
    .evaluate(
      (canvas, [from, to]) => {
        type Node = { data(k: string): unknown };
        type Edge = {
          source(): Node;
          target(): Node;
          renderedMidpoint(): { x: number; y: number };
        };
        let el: HTMLElement | null = canvas.parentElement;
        while (el && !('_cyreg' in el)) el = el.parentElement;
        const cy = (el as unknown as { _cyreg: { cy: { edges(): Edge[] } } })._cyreg.cy;
        const e = [...cy.edges()].find(
          (x) => x.source().data('label') === from && x.target().data('label') === to,
        )!;
        const p = e.renderedMidpoint();
        const box = el!.getBoundingClientRect();
        return { x: box.left + p.x, y: box.top + p.y };
      },
      [from, to],
    );
}

const panel = (page: Page) => page.getByTestId('why-panel');

test('A10: a click on a dashed edge shows its tree, and Show on graph draws the premises', async ({
  page,
}) => {
  await open(page, 'why1');
  await expect
    .poll(async () => (await drawn(page)).nodes.some(([l]) => l === 'Manager'))
    .toBe(true);
  const at = await edgeOn(page, 'alice', 'Manager');
  await page.mouse.click(at.x, at.y);
  await expect(panel(page)).toBeVisible();
  const tree = panel(page).getByRole('tree', { name: 'Derivation' });
  // alice a Manager by rdfs2, from her manages triple and the domain of manages
  await expect(tree).toContainText('derived by rdfs2 (domain)');
  await expect(tree.locator('li.asserted')).toHaveCount(2);
  await expect(tree).toContainText('asserted · graph default');
  await expect(panel(page)).toContainText(/1 of 1 derivation · depth 1 · from commit \d+/);
  await expect(panel(page).getByRole('button', { name: 'Next' })).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(panel(page)).toHaveCount(0);

  // alice a Employee: rdfs3 by the range of reportsTo, the shallower derivation first, and
  // A7's rdfs9 over the rdfs2 subtree as the next one, whose top rule differs
  const at2 = await edgeOn(page, 'alice', 'Employee');
  await page.mouse.click(at2.x, at2.y);
  await expect(tree).toBeVisible();
  const root = tree.locator('> li');
  await expect(root).toHaveAttribute('data-rule', 'rdfs3');
  await expect(panel(page)).toContainText(/1 of 2 derivations · depth 1/);
  await panel(page).getByRole('button', { name: 'Next' }).click();
  await expect(root).toHaveAttribute('data-rule', 'rdfs9');
  await expect(panel(page)).toContainText(/2 of 2 derivations · depth 2/);
  await expect(tree.locator('li[data-rule="rdfs2"]')).toHaveCount(1);
  await expect(tree.locator('li.asserted')).toHaveCount(3);
  await panel(page).getByRole('button', { name: 'Show on graph' }).click();
  // the domain and subclass triples join the graph, and every premise stands out
  await expect
    .poll(async () => (await drawn(page)).edges.some(([l]) => /domain/.test(l)))
    .toBe(true);
  await expect
    .poll(async () => (await drawn(page)).edges.some(([l]) => /subClassOf/.test(l)))
    .toBe(true);
  // the explained edge, its four premise triples, and their six distinct nodes
  await expect
    .poll(async () => Number(await page.locator('.wrap').first().getAttribute('data-whymarked')))
    .toBeGreaterThanOrEqual(10);
  await panel(page).getByRole('button', { name: 'Next' }).click();
  await expect(root).toHaveAttribute('data-rule', 'rdfs3');
  await panel(page).getByRole('button', { name: 'Next' }).click();
  await expect(root).toHaveAttribute('data-rule', 'rdfs9');
  // folding a premise hides its own premises
  await tree.locator('li[data-rule="rdfs2"] > .row .fold').click();
  await expect(tree.locator('li.asserted')).toHaveCount(1);
});

test('stale inferences say so above the tree', async ({ page, request }) => {
  const u = await request.post('/why2/update', {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: `INSERT DATA { <${EX}zoe> <${EX}knows> <${EX}alice> }`,
  });
  expect(u.ok()).toBe(true);
  await open(page, 'why2');
  const at = await edgeOn(page, 'alice', 'Manager');
  await page.mouse.click(at.x, at.y);
  await expect(panel(page).getByRole('note')).toContainText(
    /These inferences are out of date\. This tree shows them as computed at commit \d+\. 1 commit since\./,
  );
});

test('at a past state Why? says that it explains the latest commit', async ({ page, request }) => {
  // why2 has a commit after its run (the stale test above), so the run's commit is past
  const asked0 = await request.post('/$/reason/why2/explain', {
    data: {
      triple: {
        s: { type: 'uri', value: `${EX}alice` },
        p: { type: 'uri', value: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type' },
        o: { type: 'uri', value: `${EX}Manager` },
      },
      mode: 'why',
    },
  });
  const commit = (await asked0.json()).commit;
  expect(typeof commit).toBe('number');
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), 'why2');
  await page.goto(`/ui/explore?ds=why2&iri=${encodeURIComponent(`${EX}alice`)}&at=${commit}`);
  await expect.poll(async () => (await drawn(page)).nodes.length).toBeGreaterThan(3);
  const why = page.getByRole('button', { name: /Why is rdf:type inferred\?/ }).first();
  await expect(why).toBeVisible();
  let asked = 0;
  await page.route('**/$/reason/why2/explain', (route) => {
    asked++;
    return route.continue();
  });
  await why.click();
  await expect(panel(page).getByRole('status')).toContainText(
    'Why? explains inferences at the latest commit only.',
  );
  await expect(panel(page).getByRole('tree')).toHaveCount(0);
  await expect(panel(page).getByRole('button', { name: 'Show on graph' })).toBeDisabled();
  expect(asked).toBe(0);
});

test('Expand refuses a subtree from inferences computed since the tree was read', async ({
  page,
}) => {
  const T = (s: string, p: string, o: string) => ({
    s: { type: 'uri', value: `${EX}${s}` },
    p: { type: 'uri', value: p.startsWith('http') ? p : `${EX}${p}` },
    o: { type: 'uri', value: `${EX}${o}` },
  });
  const TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
  const answer = (commit: number, trees: unknown[]) => ({
    mode: 'why',
    entailed: true,
    commit,
    source: 'materialized',
    trees,
    more: false,
    stale: false,
    commitsSince: 0,
  });
  let calls = 0;
  await page.route('**/$/reason/why1/explain', (route) => {
    calls++;
    const body =
      calls === 1
        ? // the tree is cut at its first premise
          answer(5, [
            {
              id: 1,
              triple: T('alice', TYPE, 'Manager'),
              kind: 'derived',
              depth: 2,
              rule: { name: 'rdfs2', profile: 'rdfs' },
              premises: [{ id: 2, triple: T('alice', 'manages', 'team7'), kind: 'ref', depth: 1 }],
            },
          ])
        : // the premise's own tree, from a run at a later commit
          answer(9, [
            {
              id: 1,
              triple: T('alice', 'manages', 'team7'),
              kind: 'asserted',
              depth: 0,
              graph: 'default',
            },
          ]);
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(body),
    });
  });
  await open(page, 'why1');
  const at = await edgeOn(page, 'alice', 'Manager');
  await page.mouse.click(at.x, at.y);
  const tree = panel(page).getByRole('tree', { name: 'Derivation' });
  await expect(tree.locator('li.ref')).toHaveCount(1);
  await panel(page).getByRole('button', { name: 'Expand' }).click();
  await expect(panel(page).getByRole('status')).toContainText('The inferences have changed.');
  // the cut node stays cut, and nothing from the later run joins the tree
  await expect(tree.locator('li.ref')).toHaveCount(1);
  await expect(tree.locator('li.asserted')).toHaveCount(0);
});

test('a triple with no visible derivation says only that', async ({ page }) => {
  await page.route('**/$/reason/why1/explain', (route) =>
    route.fulfill({
      status: 404,
      contentType: 'application/json',
      body: JSON.stringify({
        error: 'no derivation from facts you can read was found',
        code: 'no-visible-derivation',
      }),
    }),
  );
  await open(page, 'why1');
  const at = await edgeOn(page, 'alice', 'Manager');
  await page.mouse.click(at.x, at.y);
  await expect(panel(page).getByRole('status')).toHaveText('No derivation found.');
  await expect(panel(page).getByRole('tree')).toHaveCount(0);
});

test('A12: the keyboard opens Why? from the properties with W, and Escape closes it', async ({
  page,
}) => {
  await open(page, 'why1');
  const details = page.getByRole('complementary', { name: 'Resource details' });
  const why = details.getByRole('button', { name: /Why is rdf:type inferred\?/ }).first();
  await expect(why).toBeVisible();
  await why.focus();
  await page.keyboard.press('w');
  await expect(panel(page)).toBeVisible();
  await expect(panel(page).getByRole('heading', { name: 'Why is this inferred?' })).toBeFocused();
  // Tab reaches Show on graph, past the close button and the triple's terms
  const show = panel(page).getByRole('button', { name: 'Show on graph' });
  for (let i = 0; i < 8 && !(await show.evaluate((b) => b === document.activeElement)); i++)
    await page.keyboard.press('Tab');
  await expect(show).toBeFocused();
  await page.keyboard.press('Enter');
  await expect
    .poll(async () => Number(await page.locator('.wrap').first().getAttribute('data-whymarked')))
    .toBeGreaterThan(0);
  await page.keyboard.press('Escape');
  await expect(panel(page)).toHaveCount(0);
  await expect(
    details.getByRole('button', { name: /Why is rdf:type inferred\?/ }).first(),
  ).toBeVisible();
});

test('A12: at 390 px Why? is a full-height sheet opened by a tap, without scrolling sideways', async ({
  browser,
}) => {
  const ctx = await browser.newContext({
    ...phone,
    viewport: { width: 390, height: 844 },
    baseURL: test.info().project.use.baseURL,
  });
  const page = await ctx.newPage();
  await open(page, 'why1');
  const why = page.getByRole('button', { name: /Why is rdf:type inferred\?/ }).first();
  await why.scrollIntoViewIfNeeded();
  await why.tap();
  await expect(panel(page)).toBeVisible();
  const box = await panel(page).boundingBox();
  expect(box?.height).toBeGreaterThan(800);
  expect(box?.width).toBeLessThanOrEqual(390);
  await expect(panel(page).getByRole('tree')).toBeVisible();
  await expectNoSidewaysScroll(page, 'the Why? sheet');
  await panel(page).getByRole('button', { name: 'Close Why?' }).tap();
  await expect(panel(page)).toHaveCount(0);
  await ctx.close();
});

test('why not lists verified missing facts for the selected resource', async ({
  page,
  request,
}) => {
  const ds = 'why-not-missing';
  await create(request, ds);
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto(`/ui/explore?ds=${ds}&iri=${encodeURIComponent(`${EX}alice`)}`);
  const panel = page.getByRole('region', { name: 'Why not?', exact: true });
  await expect(panel).toBeVisible();
  await panel.getByText('Why not…?', { exact: true }).click();
  await panel.getByLabel('Why not value').fill(`<${EX}Director>`);
  await panel.getByRole('button', { name: 'Find missing facts' }).click();
  await expect(panel.getByText('Possibility 1: add 1 fact', { exact: true })).toBeVisible();
  await panel.getByText('Possibility 1: add 1 fact', { exact: true }).click();
  await expect(panel.getByText('Verified derivation', { exact: true }).first()).toBeVisible();
  await panel.getByText('Verified derivation', { exact: true }).first().click();
  await expect(panel.locator('li').filter({ hasText: 'missing' }).first()).toBeVisible();
});
