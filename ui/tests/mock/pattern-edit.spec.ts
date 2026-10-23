// The canvas edits of the Pattern pane (spec U08 Phase 2): waiting nodes, edges from the
// schema picker, filters by datatype, renames and deletions with their confirmations, the
// projection strip and Count matches, the schema failure note, the warning mark of a
// locked block, and the builds of A11 (keyboard) and A12 (taps at 390 px). The mock's
// `foaf` dataset declares `ex:` and `foaf:`, and its schema routes answer the pickers.

import { expect, test, type Locator, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, selectAll } from '../phone';

const editor = (page: Page) => page.locator('.cm-content');

async function setText(page: Page, text: string) {
  await editor(page).click();
  await selectAll(page);
  await page.keyboard.press('Delete');
  if (text) await page.keyboard.insertText(text);
}

/** The editor's text, without the placeholder an empty editor shows. */
const textOf = async (page: Page) =>
  (
    await editor(page)
      .locator('.cm-line')
      .evaluateAll((lines) =>
        lines.map((l) => {
          const c = l.cloneNode(true) as HTMLElement;
          c.querySelectorAll('.cm-placeholder').forEach((x) => x.remove());
          return c.textContent ?? '';
        }),
      )
  ).join('\n');

const pane = (page: Page) => page.getByRole('region', { name: 'Pattern', exact: true });
const canvas = (page: Page) => pane(page).locator('.pcanvas');
const live = (page: Page) => pane(page).getByTestId('pattern-live');
const focused = (page: Page) => pane(page).getByTestId('pattern-focus');
const panel = (page: Page) => pane(page).getByRole('dialog', { name: 'Pattern element' });
const graphText = async (c: Locator) => (await c.getAttribute('data-graph')) ?? '';

const PREFIXES = `PREFIX ex: <http://example.org/ontology#>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
`;

async function openPane(page: Page, text: string) {
  await page.goto('/ui/query');
  await setText(page, text);
  await page.getByRole('button', { name: 'Pattern', exact: true }).click();
  await expect(pane(page)).toHaveAttribute('data-state', 'ok');
}

/**
 * Tab through the canvas from its first element until the focused element's description
 * starts with `prefix`, trying again while the pane redraws after an edit.
 */
async function tabTo(page: Page, prefix: string) {
  await expect(async () => {
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    await canvas(page).focus();
    for (let i = 0; i < 30; i++) {
      if (((await focused(page).textContent()) ?? '').startsWith(prefix)) return;
      await page.keyboard.press('Tab');
      if (!(await canvas(page).evaluate((el) => el.contains(document.activeElement)))) break;
    }
    throw new Error(`no element of the canvas starts with ${prefix}`);
  }).toPass({ timeout: 10_000 });
}

test.describe.configure({ timeout: 60_000 });

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    if (!sessionStorage.getItem('pattern-test')) {
      localStorage.removeItem('sparkles.patternPane');
      localStorage.removeItem('sparkles.patternLayouts');
      sessionStorage.setItem('pattern-test', '1');
    }
  });
});

test('A11: a query is built from an empty editor with the keyboard alone', async ({ page }) => {
  await page.goto('/ui/query');
  await expect(editor(page)).toContainText('SELECT');
  await setText(page, '');
  await expect.poll(() => textOf(page)).toBe('');
  await page.keyboard.press('Alt+P');
  await expect(pane(page)).toHaveAttribute('data-state', 'ok');
  // the empty state offers + Node and the classes with the most instances
  await expect(pane(page)).toContainText('Add a node to start');
  await expect(pane(page).getByRole('button', { name: 'ex:Engineer' })).toBeVisible();
  // N asks for a node: a class becomes a waiting ?engineer
  await canvas(page).focus();
  await page.keyboard.press('n');
  await expect(pane(page).getByLabel('Node', { exact: true })).toBeFocused();
  await page.keyboard.type('ex:Engineer');
  await page.keyboard.press('Enter');
  await expect(live(page)).toHaveText('Added ?engineer, a ex:Engineer, waiting for an edge');
  await expect(canvas(page)).toBeFocused();
  await page.keyboard.press('n');
  await page.keyboard.type('ex:Company');
  await page.keyboard.press('Enter');
  await expect(live(page)).toHaveText('Added ?company, a ex:Company, waiting for an edge');
  // waiting nodes write nothing
  expect(await textOf(page)).toBe('');
  // E on the focused node opens the predicate picker, ranked by the class's profile
  await tabTo(page, 'variable ?engineer');
  await expect(focused(page)).toContainText('waiting for an edge');
  await page.keyboard.press('e');
  const predicate = pane(page).getByLabel('Predicate', { exact: true });
  await expect(predicate).toBeFocused();
  await expect(pane(page).getByRole('option').first()).toContainText('used by the class');
  await page.keyboard.type('ex:worksFor');
  await page.keyboard.press('Tab');
  await page.keyboard.type('?company');
  await page.keyboard.press('Enter');
  await expect.poll(() => textOf(page)).toContain('?engineer ex:worksFor ?company .');
  const text = await textOf(page);
  expect(text).toContain('PREFIX ex: <http://example.org/ontology#>');
  expect(text).toContain('?engineer a ex:Engineer .');
  expect(text).toContain('?company a ex:Company .');
  await expect(live(page)).toContainText('Added ?engineer a ex:Engineer and');
  // a second edge to a new variable, then a filter on it with F
  await expect(pane(page)).not.toContainText('Add a node to start');
  await tabTo(page, 'variable ?company');
  await page.keyboard.press('e');
  await page.keyboard.type('ex:basedIn');
  await page.keyboard.press('Tab');
  await page.keyboard.type('?city');
  await page.keyboard.press('Enter');
  await expect.poll(() => textOf(page)).toContain('?company ex:basedIn ?city .');
  await expect(live(page)).toContainText('Added ?company ex:basedIn ?city, 3 nodes, 2 edges');
  await tabTo(page, 'variable ?city');
  await page.keyboard.press('f');
  await expect(panel(page).getByLabel('Filter form')).toBeFocused();
  await page.keyboard.press('Tab');
  await page.keyboard.type('<http://example.org/resource/Lyon>');
  await page.keyboard.press('Enter');
  await expect
    .poll(() => textOf(page))
    .toContain('FILTER(?city = <http://example.org/resource/Lyon>)');
  await expect(live(page)).toContainText('Added FILTER');
});

test('filters offer forms by datatype, and a chip opens again to edit or delete', async ({
  page,
}) => {
  await openPane(
    page,
    `${PREFIXES}SELECT ?person ?age WHERE {\n  ?person a ex:Engineer ; foaf:age ?age .\n}`,
  );
  await tabTo(page, 'variable ?age');
  await page.keyboard.press('f');
  const form = panel(page).getByLabel('Filter form');
  // foaf:age holds integers, so the form offers ranges and no string tests
  await expect(form.locator('option', { hasText: 'contains' })).toHaveCount(0);
  await expect(form.locator('option', { hasText: 'more than' })).toHaveCount(1);
  await form.selectOption({ label: 'more than' });
  await panel(page).getByLabel('Filter value').fill('30');
  await panel(page).getByRole('button', { name: 'Add', exact: true }).click();
  await expect.poll(() => textOf(page)).toContain('FILTER(?age > 30)');
  await expect.poll(() => graphText(canvas(page))).toContain('> 30');
  // the chip opens in the same form
  await tabTo(page, 'variable ?age');
  await page.keyboard.press('Enter');
  await panel(page).getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(panel(page).getByLabel('Filter form')).toHaveValue('>');
  await expect(panel(page).getByLabel('Filter value')).toHaveValue('30');
  await panel(page).getByLabel('Filter value').fill('40');
  await panel(page).getByRole('button', { name: 'Change', exact: true }).click();
  await expect.poll(() => textOf(page)).toContain('FILTER(?age > 40)');
  await expect.poll(() => graphText(canvas(page))).toContain('> 40');
  await tabTo(page, 'variable ?age');
  await page.keyboard.press('Enter');
  await panel(page)
    .getByRole('button', { name: /^Delete the filter/ })
    .click();
  await expect.poll(() => textOf(page)).not.toContain('FILTER');
});

test('a predicate changes from the panel, with paths', async ({ page }) => {
  await openPane(page, `${PREFIXES}SELECT * WHERE {\n  ?a foaf:knows ?b .\n}`);
  await tabTo(page, 'edge foaf:knows');
  await page.keyboard.press('Enter');
  await panel(page).getByRole('button', { name: 'one or more' }).click();
  await expect.poll(() => textOf(page)).toContain('?a foaf:knows+ ?b .');
  await expect.poll(() => graphText(canvas(page))).toContain('foaf:knows+');
  await tabTo(page, 'edge foaf:knows+ ');
  await page.keyboard.press('Enter');
  await panel(page).getByLabel('New predicate').fill('ex:manages');
  await panel(page).getByRole('button', { name: 'Change', exact: true }).click();
  await expect.poll(() => textOf(page)).toContain('?a ex:manages ?b .');
  await expect(live(page)).toContainText('Changed the predicate to ex:manages');
});

test('a rename asks before it touches a locked block, and deleting one shows its text', async ({
  page,
}) => {
  const q = `${PREFIXES}SELECT * WHERE {\n  ?person ex:born ?b .\n  BIND(YEAR(?b) AS ?y) # year\n  ?person foaf:name ?name .\n}`;
  await openPane(page, q);
  await tabTo(page, 'variable ?b');
  await page.keyboard.press('Enter');
  await panel(page).getByLabel('New name').fill('birth');
  await panel(page).getByRole('button', { name: 'Rename' }).click();
  const dialog = page.getByRole('dialog', { name: 'Rename ?b to ?birth' });
  await expect(dialog).toContainText('BIND(YEAR(?b) AS ?y) # year');
  await dialog.getByRole('button', { name: 'Rename in the locked blocks too' }).click();
  await expect.poll(() => textOf(page)).toContain('BIND(YEAR(?birth) AS ?y) # year');
  expect(await textOf(page)).toContain('?person ex:born ?birth .');
  await expect.poll(() => graphText(canvas(page))).toContain('?birth');
  // deleting the only triple that binds ?birth leaves the block with a warning mark
  await tabTo(page, 'edge ex:born');
  await page.keyboard.press('Delete');
  await expect.poll(() => textOf(page)).not.toContain('ex:born');
  await expect
    .poll(() => graphText(canvas(page)))
    .toContain('uses ?birth, which nothing binds now');
  // the block goes only after a confirmation that shows its text
  await tabTo(page, 'locked BIND');
  await expect(focused(page)).toContainText('which nothing binds now');
  await page.keyboard.press('Delete');
  const confirm = page.getByRole('dialog', { name: 'Delete this locked block' });
  await expect(confirm).toContainText('BIND(YEAR(?birth) AS ?y)');
  await confirm.getByRole('button', { name: 'Delete', exact: true }).click();
  await expect.poll(() => textOf(page)).not.toContain('BIND');
  expect(await textOf(page)).toContain('?person foaf:name ?name .');
});

test('deleting a node removes its triples and the filters left without a variable', async ({
  page,
}) => {
  await openPane(
    page,
    `${PREFIXES}SELECT * WHERE {\n  ?p foaf:knows ?q .\n  ?q foaf:age ?age .\n  FILTER(?age > 10)\n}`,
  );
  await tabTo(page, 'variable ?q');
  await page.keyboard.press('Delete');
  await expect.poll(() => textOf(page)).not.toContain('foaf:knows');
  const text = await textOf(page);
  expect(text).not.toContain('foaf:age');
  expect(text).not.toContain('FILTER');
});

test('the strip edits the projection and modifiers, and Count matches counts', async ({ page }) => {
  await openPane(
    page,
    `${PREFIXES}SELECT * WHERE {\n  ?person a ex:Engineer ; ex:worksFor ?org .\n}`,
  );
  const strip = pane(page).getByRole('group', { name: 'Projection and modifiers' });
  // SELECT * becomes an explicit list on the first change
  await strip.getByRole('checkbox', { name: '?org' }).uncheck();
  await expect.poll(() => textOf(page)).toContain('SELECT ?person WHERE');
  await strip.getByRole('checkbox', { name: 'DISTINCT' }).check();
  await expect.poll(() => textOf(page)).toContain('SELECT DISTINCT ?person WHERE');
  await strip.getByLabel('LIMIT', { exact: true }).fill('10');
  await strip.getByLabel('LIMIT', { exact: true }).press('Enter');
  await expect.poll(() => textOf(page)).toContain('LIMIT 10');
  await strip.getByLabel('OFFSET', { exact: true }).fill('5');
  await strip.getByLabel('OFFSET', { exact: true }).press('Enter');
  await expect.poll(() => textOf(page)).toContain('OFFSET 5');
  await strip.getByLabel('LIMIT', { exact: true }).fill('');
  await strip.getByLabel('LIMIT', { exact: true }).press('Enter');
  await expect.poll(() => textOf(page)).not.toContain('LIMIT');
  // the count runs over the pattern
  await strip.getByRole('button', { name: 'Count matches' }).click();
  await expect(pane(page).getByTestId('pattern-count')).toHaveText(/^\d+ match(es)?$/);
  // a count that runs out of time says so
  await page.route(/\/sparql/, async (route) => {
    if ((route.request().postData() ?? '').includes('COUNT'))
      return route.fulfill({
        status: 408,
        contentType: 'application/json',
        body: '{"error":"The query timed out"}',
      });
    return route.fallback();
  });
  await strip.getByRole('checkbox', { name: '?org' }).check();
  await expect.poll(() => textOf(page)).toContain('SELECT DISTINCT ?person ?org WHERE');
  await strip.getByRole('button', { name: 'Count matches' }).click();
  await expect(pane(page).getByTestId('pattern-count')).toHaveText(
    'More than the count could finish',
  );
});

test('a schema that does not load leaves typed IRIs, and the picker says why', async ({ page }) => {
  await page.route(/\/\$\/schema\//, (route) =>
    route.fulfill({ status: 403, contentType: 'application/json', body: '{"error":"forbidden"}' }),
  );
  await openPane(page, `${PREFIXES}SELECT * WHERE {\n  ?a foaf:knows ?b .\n}`);
  await pane(page).getByRole('button', { name: 'Edge', exact: true }).click();
  await pane(page).getByLabel('From', { exact: true }).fill('?b');
  await pane(page).getByLabel('Predicate', { exact: true }).focus();
  await expect(pane(page).getByRole('note')).toContainText('The schema did not load');
  await pane(page).getByLabel('Predicate', { exact: true }).fill('<http://xmlns.com/foaf/0.1/age>');
  await pane(page).getByLabel('To', { exact: true }).fill('?age');
  await pane(page).getByRole('button', { name: 'Add', exact: true }).click();
  await expect.poll(() => textOf(page)).toContain('?b <http://xmlns.com/foaf/0.1/age> ?age .');
});

test.describe('at 390 px', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  /** Tap a node of the canvas by its key. */
  async function tapNode(page: Page, id: string) {
    const c = canvas(page);
    await expect
      .poll(async () => JSON.parse((await c.getAttribute('data-screen')) ?? '{}')[id])
      .toBeDefined();
    const [x, y] = JSON.parse((await c.getAttribute('data-screen')) ?? '{}')[id];
    await c.tap({ position: { x, y } });
  }

  test('A12: a query is built with taps alone, and nothing scrolls sideways', async ({ page }) => {
    await page.goto('/ui/query');
    await expect(editor(page)).toContainText('SELECT');
    await setText(page, '');
    await expect.poll(() => textOf(page)).toBe('');
    await page.getByRole('button', { name: 'Pattern', exact: true }).tap();
    await expect(pane(page)).toHaveAttribute('data-state', 'ok');
    // the empty state's classes add waiting nodes with one tap each
    await pane(page).getByRole('button', { name: 'ex:Engineer' }).tap();
    await expect(live(page)).toHaveText('Added ?engineer, a ex:Engineer, waiting for an edge');
    await pane(page).getByRole('button', { name: 'Node', exact: true }).tap();
    const nodeForm = pane(page).getByRole('form', { name: 'Add a node' });
    await nodeForm.getByLabel('Node', { exact: true }).fill('ex:Company');
    await expectNoSidewaysScroll(page, 'the node form');
    await nodeForm.getByRole('button', { name: 'Add', exact: true }).tap();
    await expect(live(page)).toHaveText('Added ?company, a ex:Company, waiting for an edge');
    // + Edge is two taps and a full-screen picker
    await pane(page).getByRole('button', { name: 'Edge', exact: true }).tap();
    await expect(pane(page)).toContainText('Tap the source node');
    await tapNode(page, 'v:engineer');
    await expect(pane(page)).toContainText('Tap the target node');
    await tapNode(page, 'v:company');
    const edgeForm = pane(page).getByRole('form', { name: 'Add an edge' });
    await expect(edgeForm.getByLabel('From', { exact: true })).toHaveValue('?engineer');
    await expect(edgeForm.getByLabel('To', { exact: true })).toHaveValue('?company');
    const box = await edgeForm.boundingBox();
    expect(box!.width).toBeGreaterThanOrEqual(380);
    await edgeForm.getByLabel('Predicate', { exact: true }).fill('ex:worksFor');
    await expectNoSidewaysScroll(page, 'the edge picker');
    await edgeForm.getByRole('button', { name: 'Add', exact: true }).tap();
    await expect(live(page)).toContainText('Added ?engineer a ex:Engineer and');
    // the strip folds into Query options
    await pane(page).getByRole('button', { name: 'Query options' }).tap();
    const sheet = pane(page).getByRole('dialog', { name: 'Query options' });
    await sheet.getByRole('checkbox', { name: 'DISTINCT' }).tap();
    const distinct = await sheet.locator('label', { hasText: 'DISTINCT' }).boundingBox();
    expect(distinct!.height).toBeGreaterThanOrEqual(44);
    await expectNoSidewaysScroll(page, 'the Query options sheet');
    await sheet.getByRole('button', { name: 'Close' }).tap();
    const tabs = page.getByRole('tablist', { name: 'Query views' });
    await tabs.getByRole('tab', { name: 'Text' }).tap();
    await expect.poll(() => textOf(page)).toContain('?engineer ex:worksFor ?company .');
    expect(await textOf(page)).toContain('SELECT DISTINCT');
  });
});
