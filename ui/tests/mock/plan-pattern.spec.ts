// The plan drawn on the query's pattern (spec U04 §11, Phase 4) against the mock server,
// whose fabricated plans carry `patterns`, `filterVars` and `joinVars`: estimates on the
// edges (A14), a misestimate marked on its edge and in the tree (A15), selection between
// the edge and its operator both ways (A16), the overlay hidden on the editor's pane once
// its text differs from the text that ran (A19), the keyboard, and the 390 px sheet.

import { expect, test, type Locator, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, selectAll } from '../phone';

const editor = (page: Page) => page.locator('.cm-content');

async function setText(page: Page, text: string) {
  await editor(page).click();
  await selectAll(page);
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

const results = (page: Page) => page.getByRole('region', { name: 'Results' });
const ran = (page: Page) =>
  results(page).getByRole('region', { name: 'Pattern of the query that ran' });
const editorPane = (page: Page) => page.getByRole('region', { name: 'Pattern', exact: true });

type Overlay = {
  edges: [string, string, boolean, number][];
  nodes: [string, string[]][];
  lit: string[] | null;
};
const overlayOf = async (c: Locator): Promise<Overlay | null> => {
  const raw = await c.getAttribute('data-overlay');
  return raw ? JSON.parse(raw) : null;
};
const edgeText = async (c: Locator, label: string) =>
  (await overlayOf(c))?.edges.find((e) => e[0] === label)?.[1] ?? null;

const Q = `PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT * WHERE {
  ?a foaf:knows ?b .
  ?b foaf:name ?n .
}
LIMIT 20`;

async function runAndOpenPattern(page: Page, text = Q) {
  await page.goto('/ui/query');
  await expect(editor(page)).toContainText('SELECT', { timeout: 30_000 });
  await setText(page, text);
  await page.getByRole('button', { name: /^Run\b/ }).click();
  await results(page).getByRole('tab', { name: /Plan/ }).click();
  await results(page).getByRole('checkbox', { name: 'Plan on pattern' }).check();
  await results(page)
    .getByRole('tab', { name: /Pattern/ })
    .click();
  await expect(ran(page)).toHaveAttribute('data-state', 'ok');
}

test('A14 and A15: each edge shows its estimate and rows, and a misestimate is marked in both views', async ({
  page,
}) => {
  await runAndOpenPattern(page);
  const c = ran(page).locator('.pcanvas');
  // the mock estimates 400 rows for a scan with a variable subject and object
  await expect.poll(() => edgeText(c, 'foaf:name')).toMatch(/^est 400 · rows [\d,]+$/);
  await expect.poll(() => edgeText(c, 'foaf:knows')).toMatch(/^est 400 · rows 12,000 ⚠ ×30$/);
  const o = (await overlayOf(c))!;
  const knows = o.edges.find((e) => e[0] === 'foaf:knows')!;
  const name = o.edges.find((e) => e[0] === 'foaf:name')!;
  expect(knows[2]).toBe(true);
  expect(name[2]).toBe(false);
  // more rows draw a wider edge
  expect(knows[3]).toBeGreaterThan(name[3]);
  // the join is a badge on the variable it joins on
  const b = o.nodes.find((n) => n[0] === '?b');
  expect(b?.[1].join(' ')).toMatch(/① HashJoin · /);
  await expect(ran(page).getByTestId('plan-bar')).toContainText('Plan');
  await expect(ran(page).getByTestId('plan-bar')).toContainText('⚠ over ten times off');
  // the tree beside it marks the same operator
  const tree = results(page).locator('.result-pattern-tree');
  const scan = tree.locator('.tr').filter({ hasText: 'foaf:knows' });
  await expect(scan.locator('.mis')).toBeVisible();
});

test('A16: an edge selects its operator, and an operator lights its edges', async ({ page }) => {
  await runAndOpenPattern(page);
  const c = ran(page).locator('.pcanvas');
  await expect.poll(() => edgeText(c, 'foaf:knows')).not.toBeNull();
  // the edge's middle, from what the canvas exposes
  const g = JSON.parse((await c.getAttribute('data-graph')) ?? '{}');
  expect(g.edges.length).toBe(2);
  const screen = async () => JSON.parse((await c.getAttribute('data-screen')) ?? '{}');
  await expect
    .poll(async () => Object.keys(await screen()).some((k) => k.startsWith('e:')))
    .toBe(true);
  const id = Object.keys(await screen()).find((k) => k.startsWith('e:') && k.includes('knows'))!;
  const [x, y] = (await screen())[id];
  await c.click({ position: { x, y } });
  const tree = results(page).locator('.result-pattern-tree');
  await expect(tree.locator('.tr[aria-current="true"]')).toContainText('IndexScan');
  await expect(tree.locator('.tr[aria-current="true"]')).toContainText('knows');
  // the panel says what the operator read
  const info = ran(page).getByTestId('pattern-plan-info');
  await expect(info).toContainText('IndexScan');
  await expect(info).toContainText('Actual 12,000 rows');
  // selecting the join in the tree lights both edges below it
  await tree
    .locator('.tr')
    .filter({ hasText: 'HashJoin' })
    .getByRole('button', { name: 'HashJoin' })
    .click();
  await expect
    .poll(async () => (await overlayOf(c))?.lit?.filter((x) => x.startsWith('foaf:')).sort())
    .toEqual(['foaf:knows', 'foaf:name']);
  // selecting one scan lights only its edge
  await tree
    .locator('.tr')
    .filter({ hasText: 'foaf:name' })
    .getByRole('button', { name: 'IndexScan' })
    .click();
  await expect
    .poll(async () => (await overlayOf(c))?.lit?.filter((x) => x.startsWith('foaf:')))
    .toEqual(['foaf:name']);
});

test('J moves through the joins, T switches to time, and the description has the numbers', async ({
  page,
}) => {
  await runAndOpenPattern(page);
  const c = ran(page).locator('.pcanvas');
  await expect.poll(() => edgeText(c, 'foaf:knows')).not.toBeNull();
  await c.focus();
  await page.keyboard.press('j');
  const focus = ran(page).getByTestId('pattern-focus');
  await expect(focus).toContainText('variable ?b');
  await expect(focus).toContainText('join 1, HashJoin on ?b');
  // the edges below the focused join light up
  await expect
    .poll(async () => (await overlayOf(c))?.lit?.filter((x) => x.startsWith('foaf:')).sort())
    .toEqual(['foaf:knows', 'foaf:name']);
  await page.keyboard.press('Tab');
  await expect(focus).toContainText('edge');
  await expect(focus).toContainText('estimated 400 rows');
  await page.keyboard.press('t');
  await expect.poll(() => edgeText(c, 'foaf:knows')).toMatch(/ms own/);
  await expect(ran(page).getByRole('button', { name: 'Time' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  await page.keyboard.press('t');
  await expect.poll(() => edgeText(c, 'foaf:knows')).toMatch(/^est 400/);
});

test('A19: the editor pane hides the overlay once its text differs, and the result tab keeps it', async ({
  page,
}) => {
  await runAndOpenPattern(page);
  await page.getByRole('button', { name: 'Pattern', exact: true }).first().click();
  const pc = editorPane(page).locator('.pcanvas');
  await expect(editorPane(page)).toHaveAttribute('data-state', 'ok');
  await expect.poll(() => edgeText(pc, 'foaf:knows')).toMatch(/⚠/);
  // a change to the text hides it there
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type(' OFFSET 1');
  await expect(editorPane(page).getByTestId('plan-stale')).toBeVisible();
  await expect.poll(() => overlayOf(pc)).toBeNull();
  // the result's tab draws the text that ran, with the plan on it
  await expect.poll(() => edgeText(ran(page).locator('.pcanvas'), 'foaf:knows')).toMatch(/⚠/);
  // undoing the change brings it back
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+z');
  await expect.poll(() => edgeText(pc, 'foaf:knows')).toMatch(/⚠/);
  // the switch is shared: off in the Pattern tab, off everywhere
  await ran(page).getByRole('checkbox', { name: 'Plan on pattern' }).uncheck();
  await expect.poll(() => overlayOf(pc)).toBeNull();
  await expect.poll(() => overlayOf(ran(page).locator('.pcanvas'))).toBeNull();
});

test('an Explain after a run draws its own estimates in the pane, and T leaves them', async ({
  page,
}) => {
  await runAndOpenPattern(page);
  await page.getByRole('button', { name: 'Pattern', exact: true }).first().click();
  const pc = editorPane(page).locator('.pcanvas');
  await expect.poll(() => edgeText(pc, 'foaf:knows')).toMatch(/rows 12,000/);
  // another text, explained: the run's result stays, but the pane draws the explanation
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+End');
  await page.keyboard.type(' OFFSET 1');
  await expect(editorPane(page).getByTestId('plan-stale')).toBeVisible();
  await page.getByRole('button', { name: 'Explain', exact: true }).click();
  await expect(editorPane(page).getByTestId('plan-stale')).toHaveCount(0);
  await expect.poll(() => edgeText(pc, 'foaf:knows')).toBe('est 400');
  await expect(editorPane(page).getByTestId('plan-bar')).toContainText('Plan estimate');
  // an estimate has no times, so T changes nothing
  await pc.focus();
  await page.keyboard.press('t');
  await expect(editorPane(page).getByRole('button', { name: 'Time' })).toBeDisabled();
  await expect(editorPane(page).getByRole('button', { name: 'Time' })).toHaveAttribute(
    'aria-pressed',
    'false',
  );
  await expect.poll(() => edgeText(pc, 'foaf:knows')).toBe('est 400');
  // the result's own tab keeps the plan that ran
  await results(page)
    .getByRole('tab', { name: /Pattern/ })
    .click();
  await expect.poll(() => edgeText(ran(page).locator('.pcanvas'), 'foaf:knows')).toMatch(/⚠/);
});

test('J reaches each of two joins on one variable', async ({ page }) => {
  await runAndOpenPattern(
    page,
    `PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT * WHERE { ?a foaf:knows ?b . ?a foaf:name ?n . ?a foaf:mbox ?m }`,
  );
  const c = ran(page).locator('.pcanvas');
  await expect.poll(() => edgeText(c, 'foaf:knows')).not.toBeNull();
  const a = (await overlayOf(c))!.nodes.find((n) => n[0] === '?a');
  expect(a?.[1].filter((l) => /HashJoin|MergeJoin/.test(l)).length).toBe(2);
  const focus = ran(page).getByTestId('pattern-focus');
  const lit = async () => (await overlayOf(c))?.lit?.filter((x) => x.startsWith('foaf:')).sort();
  await c.focus();
  await page.keyboard.press('j');
  await expect(focus).toContainText('variable ?a');
  await expect(focus).toContainText('join 1,');
  await expect(focus).not.toContainText('join 2,');
  await expect.poll(lit).toEqual(['foaf:knows', 'foaf:name']);
  await page.keyboard.press('j');
  await expect(focus).toContainText('variable ?a');
  await expect(focus).toContainText('join 2,');
  await expect.poll(lit).toEqual(['foaf:knows', 'foaf:mbox', 'foaf:name']);
  // and round again
  await page.keyboard.press('j');
  await expect(focus).toContainText('join 1,');
  // the join a selection was made on stays selected once the focus leaves the canvas
  await page.keyboard.press('j');
  await expect(focus).toContainText('join 2,');
  const rows = Number(/join 2, \w+ on \?a, (\d+) rows/.exec((await focus.textContent()) ?? '')![1]);
  await page.keyboard.press('Enter');
  const info = ran(page).getByTestId('pattern-plan-info');
  const actual = `Actual ${rows.toLocaleString('en-US')} rows`;
  await expect(info).toContainText(actual);
  await editor(page).focus();
  await expect(c).not.toBeFocused();
  await expect(info).toContainText(actual);
});

test('an operator selected in the Plan tab stays selected in the Pattern tab after an Explain', async ({
  page,
}) => {
  await runAndOpenPattern(page);
  await page.getByRole('button', { name: 'Explain', exact: true }).click();
  await expect(results(page).getByRole('tab', { name: /Explain/ })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  await results(page).getByRole('tab', { name: /Plan/ }).click();
  await results(page)
    .locator('.tr')
    .filter({ hasText: 'HashJoin' })
    .getByRole('button', { name: 'HashJoin' })
    .click();
  await results(page)
    .getByRole('tab', { name: /Pattern/ })
    .click();
  await expect(
    results(page).locator('.result-pattern-tree .tr[aria-current="true"]'),
  ).toContainText('HashJoin');
  const c = ran(page).locator('.pcanvas');
  await expect
    .poll(async () => (await overlayOf(c))?.lit?.filter((x) => x.startsWith('foaf:')).sort())
    .toEqual(['foaf:knows', 'foaf:name']);
});

test('a pattern the canvas does not draw is listed with its operator', async ({ page }) => {
  // a string literal's pattern matches no edge in the mock's plans
  await runAndOpenPattern(
    page,
    `PREFIX foaf: <http://xmlns.com/foaf/0.1/>
SELECT * WHERE { ?a foaf:name "Alice" . ?a foaf:knows ?b }`,
  );
  const listed = ran(page).getByTestId('plan-not-drawn');
  await expect(listed).toHaveText('1 operator not drawn');
  await listed.click();
  await expect(ran(page).getByRole('list', { name: 'Operators not drawn' })).toContainText(
    'IndexScan',
  );
});

test.describe('at 390 px', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  test('a tap on an edge opens a sheet whose Show in plan selects the operator', async ({
    page,
  }) => {
    await runAndOpenPattern(page);
    const c = ran(page).locator('.pcanvas');
    await expect.poll(() => edgeText(c, 'foaf:knows')).toMatch(/^400 \/ 12k ⚠$/);
    // the tree is not beside the pattern on a phone
    await expect(results(page).locator('.result-pattern-tree')).toHaveCount(0);
    await expectNoSidewaysScroll(page, 'the Pattern tab with the plan');
    const screen = async () => JSON.parse((await c.getAttribute('data-screen')) ?? '{}');
    await expect
      .poll(async () => Object.keys(await screen()).some((k) => k.includes('knows')))
      .toBe(true);
    const id = Object.keys(await screen()).find((k) => k.startsWith('e:') && k.includes('knows'))!;
    const [x, y] = (await screen())[id];
    await c.tap({ position: { x, y } });
    const info = ran(page).getByTestId('pattern-plan-info');
    await expect(info).toContainText('IndexScan');
    await info.getByRole('button', { name: 'Show in plan' }).tap();
    await expect(results(page).getByRole('tab', { name: /Plan/ })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(results(page).locator('.tr[aria-current="true"]')).toContainText('knows');
  });
});
