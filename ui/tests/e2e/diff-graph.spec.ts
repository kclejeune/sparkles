// The diff graph and the overview through time against a real server
// (docs/specs/U01-time-slider.md, Phase 4: acceptance examples A11, A12 and A14). The dataset
// has the three updates of §7 and a fourth that adds 600 people of class ex:Person who work
// for ex:acme. The tests read commit numbers from the receipts, because the numbering of the
// first commit is the server's business.

import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ mode: 'serial' });

const EX = 'http://example.org/';

async function update(request: APIRequestContext, sparql: string, message: string) {
  const r = await request.post('/chronodiff/update?receipt=true', {
    headers: {
      'Content-Type': 'application/sparql-update',
      Accept: 'application/json',
      'Sparkles-Commit-Message': message,
    },
    data: `PREFIX ex: <${EX}> ${sparql}`,
  });
  expect(r.ok(), `${message}: ${await r.text()}`).toBe(true);
  const body = await r.json();
  expect(body.committed, message).toBe(true);
  return body.commit.seq as number;
}

let third = 0;
let fourth = 0;

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', {
    data: { dbName: 'chronodiff', dbType: 'persistent' },
  });
  expect(r.ok(), await r.text()).toBe(true);
  await update(
    request,
    'INSERT DATA { ex:alice ex:name "Alice" . ex:alice ex:knows ex:bob . ex:acme a ex:Org . ex:alice a ex:Person }',
    'Load Alice',
  );
  await update(request, 'INSERT DATA { ex:bob ex:name "Bob" }', 'Add Bob');
  third = await update(
    request,
    'DELETE DATA { ex:alice ex:knows ex:bob } ; INSERT DATA { ex:alice ex:knows ex:carol }',
    'Alice knows Carol now',
  );
  const people = Array.from(
    { length: 600 },
    (_, i) => `ex:p${i} a ex:Person ; ex:worksFor ex:acme .`,
  ).join(' ');
  fourth = await update(request, `INSERT DATA { ${people} }`, 'Hire 600');
  expect(fourth).toBe(third + 1);
});

open.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'chronodiff'));
});

type Drawn = { nodes: [string, string][]; edges: [string, string][] };

const view = (page: Page) => page.locator('.dg');
const drawn = async (page: Page): Promise<Drawn> => {
  const raw = await view(page).locator('.wrap[data-graph]').getAttribute('data-graph');
  return raw ? JSON.parse(raw) : { nodes: [], edges: [] };
};

open(
  'A11: Show on graph in the History panel draws the commit and nothing else',
  async ({ page }) => {
    await page.goto('/ui/datasets/chronodiff');
    await page.getByRole('button', { name: `Show commit ${third} on the graph` }).click();
    await expect(page).toHaveURL(new RegExp(`diff=commit%3A${third}`));
    await expect.poll(async () => (await drawn(page)).edges.length).toBe(2);
    const d = await drawn(page);
    const change = (re: RegExp) => d.nodes.find(([l]) => re.test(l))?.[1];
    expect(change(/carol/)).toBe('added');
    expect(change(/bob/)).toBe('removed');
    expect(d.nodes).toHaveLength(3);
    await expect(view(page).getByLabel('1 added, 1 removed')).toBeVisible();
  },
);

open('A12: a large diff draws 500 changes and groups by class', async ({ page }) => {
  await page.goto(`/ui/explore?ds=chronodiff&diff=commit%3A${fourth}&diffFrom=commit%3A${third}`);
  await expect(view(page)).toContainText('Showing 500 of 1,200 changes.');
  const answer = page.waitForResponse((r) => r.url().includes('group=class'));
  await view(page).getByRole('button', { name: 'Group by class' }).click();
  const g = await (await answer).json();
  const works = g.groups.find((x: { predicate: string }) => x.predicate === `${EX}worksFor`);
  expect(works).toMatchObject({
    subjectClass: `${EX}Person`,
    object: { class: `${EX}Org` },
    added: 600,
    removed: 0,
  });
  await expect
    .poll(async () => (await drawn(page)).edges)
    .toContainEqual([expect.stringMatching(/worksFor \+600$/), 'added']);
});

open('A14: the overview compares the last two commits', async ({ page }) => {
  await page.goto('/ui/explore?ds=chronodiff');
  await expect(page.getByRole('navigation', { name: 'Overview levels' })).toBeVisible();
  await page.keyboard.press('Shift+H');
  const slider = page.getByRole('region', { name: 'Overview history time slider' });
  const right = slider.getByRole('slider', { name: /^(Commit|Compared to)$/ });
  await expect(right).toHaveAttribute('aria-valuenow', String(fourth));
  const grouped = page.waitForResponse((r) => r.url().includes('group=class'));
  await slider.getByRole('button', { name: 'Compare' }).click();
  const g = await (await grouped).json();
  expect(g.from.commit.seq).toBe(third);
  await page.keyboard.press('l');
  await expect(page.getByRole('row', { name: /Person.*\+600/ })).toBeVisible();
});
