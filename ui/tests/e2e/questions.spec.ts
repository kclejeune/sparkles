// Questions and memory against a real server (C18 Phase 1): a handoff link opens a tab
// with the question header and the terms of `/check` and does not run (A1), a link with
// an update is refused and leaves the editor empty (A2), and the memory views render.

import { expect, test } from './fixtures';
import { DATASET, EX } from './data';
import { encodeHandoff, type Handoff } from '../../src/lib/handoff';

const link = (h: Omit<Handoff, 'dataset'>) =>
  `/ui/query?ds=${DATASET}#ask=${encodeHandoff({ dataset: DATASET, ...h })}`;

test('a handoff link opens a tab with the question and does not run it (A1)', async ({ page }) => {
  const runs: string[] = [];
  page.on('request', (r) => {
    if (new URL(r.url()).pathname.endsWith('/sparql')) runs.push(r.url());
  });
  await page.goto(
    link({
      query: `PREFIX ex: <${EX}>\nSELECT ?person WHERE { ?person ex:knows ex:grace }`,
      question: 'Who knows Grace Hopper?',
      explanation: 'Finds the people who know Grace Hopper.',
      assumptions: ['"Grace Hopper" = ex:grace'],
    }),
  );
  const header = page.getByRole('region', { name: 'Question' });
  await expect(header).toContainText('Who knows Grace Hopper?');
  await expect(header).toContainText('Finds the people who know Grace Hopper.');
  await expect(header).toContainText('"Grace Hopper" = ex:grace');
  const terms = header.getByRole('list', { name: 'Terms used' });
  await expect(terms.getByRole('listitem')).toHaveCount(2);
  await expect(terms.getByRole('listitem').first()).toContainText('property');
  await expect(terms.getByRole('listitem').nth(1)).toContainText('"Grace Hopper"');
  await expect(terms.getByRole('listitem').nth(1)).toContainText('entity');
  await expect(header.getByText(/estimated|no issues|warning/).first()).toBeVisible();

  expect(page.url()).not.toContain('#ask=');
  await expect(page.locator('.cm-content')).toContainText('ex:knows ex:grace');
  await expect(page.getByText('Run a query to see results here.')).toBeVisible();
  expect(runs).toEqual([]);

  // Run runs the query through the normal path
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toContainText('ada');
});

test('a link with an update is refused and leaves the editor empty (A2)', async ({ page }) => {
  await page.goto(
    link({
      query: `PREFIX ex: <${EX}>\nINSERT DATA { ex:mallory ex:knows ex:grace }`,
      question: 'Add Mallory',
    }),
  );
  await expect(page.getByRole('alert')).toContainText(
    'This link contains an update. Links can only open queries.',
  );
  await expect(page.locator('.cm-content')).not.toContainText('INSERT');
  await expect(page.locator('.cm-placeholder')).toBeVisible();
  await expect(page.getByRole('region', { name: 'Question' })).toHaveCount(0);
});

test('the Memory tab and the Memory page render for a dataset without memory', async ({ page }) => {
  await page.goto(`/ui/explore?ds=${DATASET}&iri=${encodeURIComponent(`${EX}ada`)}&view=memory`);
  const memory = page.getByLabel('Memory', { exact: true });
  await expect(memory.getByRole('region', { name: /Facts about/ })).toContainText(
    'analytical engine',
  );
  await expect(memory.getByRole('list', { name: 'History' })).toHaveCount(0);
  await expect(memory).toContainText('Nothing about this resource was superseded or retracted.');

  await page.goto(`/ui/memory?ds=${DATASET}`);
  await expect(page.getByRole('region', { name: 'Agents' })).toContainText(
    'No agent wrote facts you can read.',
  );
  await expect(page.getByRole('region', { name: 'Sources' })).toContainText(
    'No fact you can read names a source.',
  );
  await expect(page.getByRole('region', { name: 'Memory settings' })).toContainText(
    'none, so no fact counts as unreviewed',
  );
});
