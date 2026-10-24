// The path finder against a real server (docs/specs/U09-path-finder.md, acceptance examples
// A1, A4, A6 and A7). The dataset `paths-e2e` is U03 §7's people, organizations and cities,
// loaded in two commits, the second adding `ex:alice ex:knows ex:bob`. A protection hides
// `ex:alice ex:worksFor ex:acme` from the limited reader.

import type { Page } from '@playwright/test';
import { anonymous, expect, test } from './fixtures';
import { PASSWORD, PATHS_DATASET, PATHS_EX, READER } from './data';

const panel = (page: Page) => page.getByRole('region', { name: 'Paths' });
const list = (page: Page) => panel(page).getByRole('listbox', { name: 'Paths found' });

async function openAlice(page: Page, extra = '') {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), PATHS_DATASET);
  await page.goto(
    `/ui/explore?ds=${PATHS_DATASET}&iri=${encodeURIComponent(`${PATHS_EX}alice`)}${extra}`,
  );
  await expect(page.locator('.wrap[data-graph]')).toBeVisible({ timeout: 30_000 });
  await page.getByRole('button', { name: 'Paths', exact: true }).click();
  await expect(panel(page).locator('#path-from').locator('..')).toContainText('Alice');
}

async function choose(page: Page, end: 'From' | 'To', text: string) {
  await panel(page).getByRole('combobox', { name: end }).fill(text);
  await panel(page)
    .getByRole('option', { name: new RegExp(`^${text}\\b`) })
    .first()
    .click();
}

/** Each path's row text in the list. */
const names = async (page: Page) =>
  (await list(page).getByRole('option').all()).map((o) => o.getAttribute('aria-label'));

test('A1: rdf:type is skipped, so Alice and Bob are one edge apart through ex:knows', async ({
  page,
}) => {
  await openAlice(page);
  await choose(page, 'To', 'Bob');
  await panel(page).getByLabel('k shortest').check();
  const req = page.waitForRequest(
    (r) => r.method() === 'POST' && (r.postData() ?? '').includes('path:search'),
  );
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  expect((await req).postData()).toContain(
    'path:excludePredicate <http://www.w3.org/1999/02/22-rdf-syntax-ns#type>',
  );
  await expect(list(page).getByRole('option').first()).toHaveAccessibleName(
    'path 1, length 1, Alice knows Bob',
  );
  // no path passes through a class
  for (const name of await Promise.all(await names(page))) expect(name).not.toMatch(/Person/);
});

test('A4: the k shortest paths from Alice to Lyon are listed in order of length', async ({
  page,
}) => {
  await openAlice(page);
  await choose(page, 'To', 'Lyon');
  await panel(page).getByLabel('k shortest').check();
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  const rows = list(page).getByRole('option');
  await expect(rows.first()).toHaveAccessibleName(
    'path 1, length 2, Alice works for Acme, Acme located in Lyon',
  );
  const lengths = (await Promise.all(await names(page))).map((n) =>
    Number(/length (\d+)/.exec(n ?? '')?.[1]),
  );
  expect(lengths.length).toBeGreaterThan(1);
  expect(lengths).toEqual([...lengths].sort((a, b) => a - b));
  // the graph carries path 1's number; the edge's label is prefixed once Explore has read
  // the dataset's prefixes, which can come after the edge is drawn
  await expect
    .poll(async () => {
      const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
      return (JSON.parse(raw ?? '{"edges":[]}') as { edges: [string][] }).edges.some(([l]) =>
        /^(ex:)?worksFor · 1$/.test(l),
      );
    })
    .toBe(true);
});

anonymous(
  'A6: a reader who cannot see where Alice works finds no path through Acme',
  async ({ page }) => {
    const login = await page.request.post('/$/auth/login', {
      data: { user: READER, password: PASSWORD },
    });
    expect(login.ok()).toBe(true);
    await openAlice(page);
    await choose(page, 'To', 'Lyon');
    await panel(page).getByLabel('k shortest').check();
    await panel(page).getByRole('button', { name: 'Find paths' }).click();
    await expect(list(page).getByRole('option').first()).toHaveAccessibleName(
      'path 1, length 3, Alice knows Bob, Bob works for Beta, Beta located in Lyon',
    );
    for (const name of await Promise.all(await names(page))) expect(name).not.toMatch(/Acme/);
  },
);

test('A7: before Alice knew Bob there is no path through ex:knows, and the header says when', async ({
  page,
}) => {
  await openAlice(page, '&at=commit:1');
  await choose(page, 'To', 'Bob');
  await panel(page).getByRole('button', { name: 'Find paths' }).click();
  const row = list(page).getByRole('option').first();
  await expect(row).toHaveAccessibleName(/^path 1, length 4,/);
  await expect(row).not.toContainText('knows');
  await expect(panel(page)).toContainText('at commit 1 · main');
});
