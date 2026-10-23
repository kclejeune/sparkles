// The Pattern pane against a real server (spec U08 §8): A1 draws the query, A6 marks an
// edge optional with exactly one splice, Count matches counts on the server, and A13
// shows that the predicate picker reads under the caller's view. The dataset protects
// `ex:worksFor`, which the signed-in user lifts and the reader does not.

import type { Page } from '@playwright/test';
import { anonymous, expect, test } from './fixtures';
import { PASSWORD, PATTERN_DATASET, READER } from './data';

const editor = (page: Page) => page.locator('.cm-content');
const pane = (page: Page) => page.getByRole('region', { name: 'Pattern', exact: true });
const canvas = (page: Page) => pane(page).locator('.pcanvas');

async function setText(page: Page, text: string) {
  await editor(page).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

const textOf = async (page: Page) =>
  (await editor(page).locator('.cm-line').allInnerTexts())
    .map((l) => l.replace(/\n$/, ''))
    .join('\n');

const A1 = `PREFIX ex: <http://example.org/>
SELECT ?person ?org WHERE {
  ?person a ex:Person ; ex:worksFor ?org .
  ?org a ex:Org ; ex:locatedIn ex:lyon .
}`;

async function openPane(page: Page, text: string) {
  // `at` makes the page take its dataset from the address
  await page.goto(`/ui/query?ds=${PATTERN_DATASET}&at=`);
  await expect(page.getByRole('button', { name: new RegExp(PATTERN_DATASET) })).toBeVisible();
  await setText(page, text);
  await page.getByRole('button', { name: 'Pattern', exact: true }).click();
  await expect(pane(page)).toHaveAttribute('data-state', 'ok');
}

/** The predicate picker's options for an edge from `from`, as their text. */
async function predicateOptions(page: Page, from: string) {
  await pane(page).getByRole('button', { name: 'Edge', exact: true }).click();
  await pane(page).getByLabel('From', { exact: true }).fill(from);
  await pane(page).getByLabel('Predicate', { exact: true }).focus();
  const list = pane(page).getByRole('listbox', { name: 'Predicate choices' });
  await expect(list.getByRole('option').first()).toBeVisible();
  return list.getByRole('option').allInnerTexts();
}

test('A1, A6 and a count: the pane draws, edits and counts on the server', async ({ page }) => {
  await openPane(page, A1);
  // A1: three nodes and three edges, classes as subtitles
  const graph = JSON.parse((await canvas(page).getAttribute('data-graph')) ?? '{}');
  expect(graph.nodes).toEqual([['?person', 'ex:Person'], ['?org', 'ex:Org'], ['ex:lyon']]);
  // the count runs on the server under the caller's view
  await pane(page).getByRole('button', { name: 'Count matches' }).click();
  await expect(pane(page).getByTestId('pattern-count')).toHaveText('2 matches');
  // A6: Optional on ex:locatedIn is one splice
  await pane(page).getByRole('button', { name: 'List' }).click();
  await pane(page)
    .locator('li')
    .filter({ hasText: 'edge ex:locatedIn' })
    .getByRole('button', { name: 'Optional' })
    .click();
  await expect
    .poll(() => textOf(page))
    .toBe(
      A1.replace(
        '?org a ex:Org ; ex:locatedIn ex:lyon .',
        '?org a ex:Org .\n  OPTIONAL { ?org ex:locatedIn ex:lyon . }',
      ),
    );
  // the user lifts the protection, so the picker lists ex:worksFor with its count
  const options = await predicateOptions(page, '?person');
  expect(options.some((o) => o.includes('ex:worksFor') && /2/.test(o))).toBe(true);
});

anonymous(
  'A13: a limited reader’s picker leaves out what it cannot see, and a typed IRI still works',
  async ({ page }) => {
    const login = await page.request.post('/$/auth/login', {
      data: { user: READER, password: PASSWORD },
    });
    expect(login.ok()).toBe(true);
    await openPane(
      page,
      'PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?person a ex:Person .\n}',
    );
    const options = await predicateOptions(page, '?person');
    expect(options.some((o) => o.includes('ex:name'))).toBe(true);
    expect(options.some((o) => o.includes('ex:worksFor'))).toBe(false);
    // typed, it is accepted without a count
    await pane(page).getByLabel('Predicate', { exact: true }).fill('ex:worksFor');
    await expect(pane(page).getByRole('listbox', { name: 'Predicate choices' })).toHaveCount(0);
    await pane(page).getByLabel('To', { exact: true }).fill('?org');
    await pane(page).getByRole('button', { name: 'Add', exact: true }).click();
    await expect.poll(() => textOf(page)).toContain('?person ex:worksFor ?org .');
    await expect
      .poll(async () => (await canvas(page).getAttribute('data-graph')) ?? '')
      .toContain('ex:worksFor');
    // and the count, under the reader's view, finds nothing
    await pane(page).getByRole('button', { name: 'Count matches' }).click();
    await expect(pane(page).getByTestId('pattern-count')).toHaveText('0 matches');
  },
);
