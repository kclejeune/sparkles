// The query page's reasoning selector against a real server (docs/specs/F17-query-time-reasoning.md
// §2.1 and §10). The dataset holds `ex:ann a ex:Professor` and
// `ex:Professor rdfs:subClassOf ex:Person` and has no materialization, so a query for the
// people finds ex:ann only when it reasons at query time. The selector sends
// `reasoning=` and `reasoningProfile=`, and the result shows what the server derived from
// its `Sparkles-Reasoning` header.

import type { Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

const EX = 'http://example.org/';
const QUERY = `SELECT ?x WHERE { ?x a <${EX}Person> }`;

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', {
    data: { dbName: 'query-reasoning', dbType: 'persistent' },
  });
  expect(r.ok() || r.status() === 409, await r.text()).toBe(true);
  if (r.status() === 409) return;
  const l = await request.post('/query-reasoning/data', {
    headers: { 'Content-Type': 'text/turtle' },
    data: `@prefix ex: <${EX}> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:ann a ex:Professor .
ex:Professor rdfs:subClassOf ex:Person .
`,
  });
  expect(l.ok(), await l.text()).toBe(true);
});

open.beforeEach(async ({ page }) => {
  await page.addInitScript(
    ([query]) => {
      localStorage.setItem('sparkles.dataset', 'query-reasoning');
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({ tabs: [{ id: 't1', title: 'Query 1', query }], active: 't1' }),
      );
    },
    [QUERY],
  );
});

const grid = (page: Page) => page.getByRole('grid');
const options = (page: Page) => page.getByRole('button', { name: /^Options\b/ });

/** Runs the query and returns the URL of the request it sent. */
async function run(page: Page) {
  const sent = page.waitForRequest((r) => r.url().includes('/query-reasoning/sparql'));
  await page.getByRole('button', { name: /^Run/ }).click();
  return new URL((await sent).url());
}

open('the selector derives on demand with the profile it names', async ({ page }) => {
  await page.goto('/ui/query');
  // the dataset has no materialization and no query-time default: nothing is inferred
  let url = await run(page);
  expect(url.searchParams.has('reasoning')).toBe(false);
  await expect(page.getByText(/^No results/).first()).toBeVisible();

  await options(page).click();
  const mode = page.getByRole('combobox', { name: 'Reasoning', exact: true });
  await expect(mode).toHaveValue('');
  // the profile applies only to the query-time modes
  await expect(page.getByRole('combobox', { name: 'Reasoning profile' })).toHaveCount(0);
  await mode.selectOption('demand');
  await page.getByRole('combobox', { name: 'Reasoning profile' }).selectOption('rdfs');
  await page.keyboard.press('Escape');
  await expect(options(page)).toContainText('demand rdfs');

  url = await run(page);
  expect(url.searchParams.get('reasoning')).toBe('demand');
  expect(url.searchParams.get('reasoningProfile')).toBe('rdfs');
  await expect(grid(page)).toContainText('ex:ann');
  await expect(grid(page).getByRole('row')).toHaveCount(2);
  const badge = page.locator('.badge.derivation');
  await expect(badge).toContainText(/^demand · \d+ derived · \d+\/\d+ cached$/);

  // the choice outlives a reload
  await page.reload();
  await options(page).click();
  await expect(mode).toHaveValue('demand');
  await expect(page.getByRole('combobox', { name: 'Reasoning profile' })).toHaveValue('rdfs');

  // auto has no materialization to read here, so it derives too
  await mode.selectOption('auto');
  await page.keyboard.press('Escape');
  url = await run(page);
  expect(url.searchParams.get('reasoning')).toBe('auto');
  expect(url.searchParams.get('reasoningProfile')).toBe('rdfs');
  await expect(grid(page)).toContainText('ex:ann');
  await expect(badge).toContainText(/^auto · /);

  // the plan of a demand query shows the derivation
  await options(page).click();
  await mode.selectOption('demand');
  await page.keyboard.press('Escape');
  const explained = page.waitForRequest((r) => r.url().includes('/query-reasoning/explain'));
  await page
    .getByRole('button', { name: /^Explain/ })
    .first()
    .click();
  url = new URL((await explained).url());
  expect(url.searchParams.get('reasoning')).toBe('demand');
  expect(url.searchParams.get('reasoningProfile')).toBe('rdfs');
});

open('materialized and off send their values and leave the profile out', async ({ page }) => {
  await page.goto('/ui/query');
  await options(page).click();
  const mode = page.getByRole('combobox', { name: 'Reasoning', exact: true });
  await mode.selectOption('materialized');
  await page.keyboard.press('Escape');
  await expect(options(page)).toContainText('materialized');
  let url = await run(page);
  expect(url.searchParams.get('reasoning')).toBe('materialized');
  expect(url.searchParams.has('reasoningProfile')).toBe(false);
  await expect(page.getByText(/^No results/).first()).toBeVisible();
  await expect(page.locator('.badge.derivation')).toHaveCount(0);

  await options(page).click();
  await mode.selectOption('false');
  await page.keyboard.press('Escape');
  await expect(options(page)).toContainText('no inferences');
  url = await run(page);
  // `false` matters only to a dataset with inferences to leave out, which this one lacks
  expect(url.searchParams.has('reasoning')).toBe(false);
});
