// The validation overlay of Explore against the mock backend
// (docs/specs/U07-data-quality-and-schema-diagram.md §11.8, A10 to A12, and the 390 px
// sheet). Dataset `quality` has a SHACL guard in warn mode: every ex:Person has an
// ex:name, and an ex:email matches an address pattern. ex:p1, ex:p2 and ex:p3 have no
// names, and ex:p4 has the email "a@x".

import { expect, test, type Page } from '@playwright/test';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';
const SHAPES = `@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <${EX}> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] ;
  sh:property [ sh:path ex:email ; sh:pattern "^[^@]+@[^@]+[.][^@]+$" ; sh:message "not an email address" ] .`;

test.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: 'quality', dbType: 'mem' } });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  const u = await request.post('/quality/update', {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: `PREFIX ex: <${EX}> INSERT DATA {
      ex:p1 a ex:Person ; ex:knows ex:p2, ex:p3, ex:p4 .
      ex:p2 a ex:Person . ex:p3 a ex:Person .
      ex:p4 a ex:Person ; ex:name "Dee" ; ex:email "a@x" . }`,
  });
  expect(u.ok()).toBe(true);
  const g = await request.put('/$/validation/quality', {
    data: { mode: 'warn', shapes: { inline: SHAPES } },
  });
  expect(g.ok(), await g.text()).toBe(true);
});

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'quality'));
});

const badgesOf = async (page: Page): Promise<Record<string, string>> => {
  const raw = await page.locator('.wrap[data-badges]').getAttribute('data-badges');
  return Object.fromEntries(raw ? JSON.parse(raw) : []);
};

async function open(page: Page, count = 4) {
  await page.goto(`/ui/explore?ds=quality&iri=${encodeURIComponent(`${EX}p1`)}`);
  await expect
    .poll(async () => Object.keys(await badgesOf(page)).length, { timeout: 15_000 })
    .toBe(count);
}

test('A10: the people without names have one violation each', async ({ page }) => {
  await open(page);
  const b = await badgesOf(page);
  for (const p of ['p1', 'p2', 'p3', 'p4']) expect(b[`<${EX}${p}>`]).toBe('✕1');
  await expect(page.getByRole('button', { name: 'Validation', exact: true })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  // the list's Validation column
  const overlay = page.getByRole('region', { name: 'Validation overlay' });
  await overlay.getByText(/Nodes with results \(4\)/).click();
  const rows = overlay.getByRole('row');
  await expect(rows).toHaveCount(5);
  await expect(rows.filter({ hasText: 'p2' })).toContainText('1 violation');
  // the badge is a button that names the node and its results
  await expect(page.getByRole('button', { name: /p3, 1 violation/ })).toBeVisible();
});

test('A12: Show triples marks the person, the email edge and the literal', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: /p4, 1 violation/ }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await expect(panel).toContainText('sh:pattern');
  await expect(panel).toContainText('not an email address');
  await panel.getByRole('button', { name: 'Show triples' }).click();
  await expect(page.locator('.wrap[data-vmarked]')).toHaveAttribute('data-vmarked', '3');
  // the literal now drawn as a node carries the mark of its result too
  await expect
    .poll(async () => Object.keys(await badgesOf(page)).some((k) => k.startsWith('lit:')))
    .toBe(true);
  // no fix for a pattern
  await expect(panel.getByRole('button', { name: 'Suggest fix' })).toHaveCount(0);
});

test('keys: Shift+V moves to the next node with a result and V opens it', async ({ page }) => {
  await open(page);
  await page.locator('body').click({ position: { x: 1, y: 1 } });
  await page.keyboard.press('Shift+V');
  await page.keyboard.press('v');
  await expect(page.getByRole('dialog', { name: /Validation results of/ })).toBeVisible();
});

test('A11: a suggested fix is previewed, run, and its badge goes away', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: /p2, 1 violation/ }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await panel.getByRole('button', { name: 'Suggest fix' }).click();
  const editor = panel.getByRole('textbox', { name: 'Suggested update' });
  const text = await editor.inputValue();
  expect(text).toContain(`INSERT DATA {\n  <${EX}p2> <${EX}name> ?value .\n}`);
  await expect(panel.getByRole('button', { name: 'Preview' })).toBeDisabled();
  await editor.fill(text.replace('?value .', '"Ana" .'));
  await panel.getByRole('button', { name: 'Preview' }).click();
  await expect(panel).toContainText('The update would commit.');
  await expect(panel).toContainText('The guard would resolve this result.');
  await expect(panel).toContainText('1 added, 0 removed.');
  await panel.getByRole('button', { name: 'Run' }).click();
  await expect.poll(async () => (await badgesOf(page))[`<${EX}p2>`]).toBeUndefined();
  expect((await badgesOf(page))[`<${EX}p3>`]).toBe('✕1');
});

test('at 390 px the results panel is a bottom sheet', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  // A11 gave ex:p2 a name
  await open(page, 3);
  await page.getByRole('button', { name: /p3, 1 violation/ }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await expect(panel).toBeVisible();
  const box = (await panel.boundingBox())!;
  expect(Math.round(box.x)).toBe(0);
  expect(Math.round(box.width)).toBe(390);
  expect(Math.round(box.y + box.height)).toBe(844);
});
