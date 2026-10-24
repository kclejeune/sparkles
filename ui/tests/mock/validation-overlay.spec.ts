// The validation overlay of Explore against the mock backend
// (docs/specs/U07-data-quality-and-schema-diagram.md §11.8, A10 to A12, and the 390 px
// sheet). Dataset `quality` has a SHACL guard in warn mode: every ex:Person has an
// ex:name, and an ex:email matches an address pattern. ex:p1, ex:p2 and ex:p3 have no
// names, and ex:p4 has the email "a@x".

import { expect, test, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { expectNoSidewaysScroll } from '../phone';

const SHOTS = '../target/audits/u07-row120';
mkdirSync(SHOTS, { recursive: true });

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const EX = 'http://example.org/';
const SHAPES = `@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <${EX}> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] ;
  sh:property [ sh:path ex:email ; sh:pattern "^[^@]+@[^@]+[.][^@]+$" ; sh:message "not an email address" ] .`;

test.beforeAll(async ({ request }) => {
  // a dataset without a guard to switch to
  const plain = await request.post('/$/datasets', { data: { dbName: 'quality2', dbType: 'mem' } });
  expect(plain.ok() || plain.status() === 409).toBe(true);
  if (plain.ok()) {
    const d = await request.post('/quality2/update', {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: `PREFIX ex: <${EX}> INSERT DATA {
        ex:q1 a ex:Person ; <http://www.w3.org/2000/01/rdf-schema#label> "Quinn" ; ex:knows ex:q2 .
        ex:q2 a ex:Person . }`,
    });
    expect(d.ok()).toBe(true);
  }
  const r = await request.post('/$/datasets', {
    data: { dbName: 'quality', dbType: 'persistent' },
  });
  if (r.status() === 409) return;
  expect(r.ok()).toBe(true);
  const u = await request.post('/quality/update', {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: `PREFIX ex: <${EX}> INSERT DATA {
      ex:p1 a ex:Person ; ex:knows ex:p2, ex:p3, ex:p4 .
      ex:p2 a ex:Person . ex:p3 a ex:Person .
      ex:p4 a ex:Person ; ex:name "Dee" ; ex:email "a@x" .
      ex:p3 ex:knows ex:p5 .
      ex:p5 a ex:Person ; <http://www.w3.org/2000/01/rdf-schema#label> "Eve" . }`,
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
  // the badge channel's validation lens, on by default with a guard, and its legend
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await expect(menu.getByRole('combobox', { name: 'Badges' })).toHaveValue('validation');
  await expect(menu.getByRole('combobox', { name: 'Validation source' })).toHaveValue('guard');
  await expect(menu.getByTestId('lens-status-badge')).toContainText('4 nodes have results');
  await page.screenshot({ path: `${SHOTS}/desktop-lens-menu.png` });
  await page.keyboard.press('Escape');
  await expect(page.getByText('nodes whose worst result is a violation')).toBeVisible();
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

test('review: an edit while a preview runs keeps Run disabled', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: /p3, 1 violation/ }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await panel.getByRole('button', { name: 'Suggest fix' }).click();
  const editor = panel.getByRole('textbox', { name: 'Suggested update' });
  const text = await editor.inputValue();
  await editor.fill(text.replace('?value .', '"Cy" .'));
  let release = () => {};
  const held = new Promise<void>((r) => (release = r));
  await page.route('**/quality/update?dryRun=true*', async (route) => {
    await held;
    await route.continue();
  });
  await panel.getByRole('button', { name: 'Preview' }).click();
  // the editor changes while the dry run is pending
  await editor.evaluate(
    (el: HTMLTextAreaElement, t) => {
      el.value = t;
      el.dispatchEvent(new Event('input', { bubbles: true }));
    },
    text.replace('?value .', '"Other" .'),
  );
  release();
  await expect(panel).toContainText('The update would commit.');
  await expect(panel.getByRole('button', { name: 'Run' })).toBeDisabled();
  await expect(panel).toContainText('The text changed since the preview.');
  await page.unroute('**/quality/update?dryRun=true*');
});

test('review: closing a playing history does not leave the overlay paused', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: 'Graph history' }).click();
  const bar = page.getByRole('region', { name: 'Graph history time slider' });
  await bar.getByRole('slider', { name: /^(Commit|Compared to)$/ }).focus();
  await page.keyboard.press('Home');
  await bar.getByRole('button', { name: 'Play', exact: true }).click();
  const overlay = page.getByRole('region', { name: 'Validation overlay' });
  await expect(overlay).toContainText('Paused while the history plays.');
  await page.getByRole('button', { name: 'Graph history' }).click();
  await expect(overlay).not.toContainText('Paused while the history plays.');
  await expect.poll(async () => Object.keys(await badgesOf(page)).length).toBe(4);
});

test('merge review: a node the path finder draws is validated too', async ({ page }) => {
  await open(page);
  expect((await badgesOf(page))[`<${EX}p5>`]).toBeUndefined();
  await page.getByRole('button', { name: 'Paths', exact: true }).click();
  const paths = page.getByRole('region', { name: 'Paths' });
  await paths.getByRole('combobox', { name: 'To' }).fill('Eve');
  await paths.getByRole('option', { name: /Eve/ }).first().click();
  await paths.getByRole('button', { name: 'Find paths' }).click();
  await expect
    .poll(async () => (await badgesOf(page))[`<${EX}p5>`], { timeout: 15_000 })
    .toBe('✕1');
  // its results open, and Show triples marks it
  await page.getByRole('button', { name: /Eve, 1 violation/ }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await panel.getByRole('button', { name: 'Show triples' }).click();
  await expect(page.locator('.wrap[data-vmarked]')).toHaveAttribute('data-vmarked', '1');
});

test('merge review: a source without shapes drops the badges', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await menu.getByRole('combobox', { name: 'Validation source' }).selectOption('graph');
  await expect.poll(async () => Object.keys(await badgesOf(page)).length).toBe(0);
});

test('merge review: the List view has a Validation column', async ({ page }) => {
  await open(page);
  await page.getByRole('button', { name: 'List', exact: true }).click();
  const header = page.getByRole('columnheader', { name: 'validation results' });
  await expect(header).toBeVisible();
  const row = page.getByRole('row').filter({ hasText: 'p2' }).first();
  await expect(row).toContainText('1 violation');
});

test('merge review: badges follow their nodes when a size lens changes them', async ({ page }) => {
  await open(page);
  const badge = page.getByRole('button', { name: /p1, 1 violation/ });
  await page.waitForTimeout(500);
  const before = (await badge.boundingBox())!;
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await menu.getByRole('combobox', { name: 'Size' }).selectOption('degree');
  await page.keyboard.press('Escape');
  await expect
    .poll(async () => {
      const b = (await badge.boundingBox())!;
      return Math.abs(b.x - before.x) + Math.abs(b.y - before.y);
    })
    .toBeGreaterThan(2);
});

test('item 120: the badge channel turns validation off, remembers it, and Shift+3 turns it on', async ({
  page,
}) => {
  await open(page);
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await menu.getByRole('combobox', { name: 'Badges' }).selectOption('none');
  await page.keyboard.press('Escape');
  await expect.poll(async () => Object.keys(await badgesOf(page)).length).toBe(0);
  await expect(page.getByRole('region', { name: 'Validation overlay' })).toHaveCount(0);
  // the choice holds when the page opens again
  await page.reload();
  await expect(page.locator('.wrap[data-graph]')).toBeVisible({ timeout: 15_000 });
  await page.waitForTimeout(800);
  expect(Object.keys(await badgesOf(page)).length).toBe(0);
  // Shift+3 turns the badge channel's last lens on again
  await page.locator('body').click({ position: { x: 1, y: 1 } });
  await page.keyboard.press('Shift+Digit3');
  await expect.poll(async () => Object.keys(await badgesOf(page)).length).toBe(4);
});

test('item 120 review: a guard default does not follow the person to a dataset without one', async ({
  page,
}) => {
  await open(page);
  await page.locator('.ds-button').click();
  await page
    .getByRole('listbox', { name: 'Datasets' })
    .getByRole('option', { name: /quality2/ })
    .click();
  const search = page.getByRole('combobox', { name: /Find a resource/ });
  await search.fill('Quinn');
  await page.locator('#search-results').getByRole('option', { name: /Quinn/ }).first().click();
  await expect(page.locator('.wrap[data-graph]')).toBeVisible({ timeout: 15_000 });
  await page.waitForTimeout(800);
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await expect(menu.getByRole('combobox', { name: 'Badges' })).toHaveValue('none');
  await expect(page.getByRole('region', { name: 'Validation overlay' })).toHaveCount(0);
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
  // The graph lays out from random starting positions, and in the short canvas of a phone
  // the legend that docks at its top can cover p3's badge. The keyboard reaches the badge
  // wherever the layout puts it, and the test is about the sheet that opens.
  const badge = page.getByRole('button', { name: /p3, 1 violation/ });
  await badge.focus();
  await page.keyboard.press('Enter');
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await expect(panel).toBeVisible();
  const box = (await panel.boundingBox())!;
  expect(Math.round(box.x)).toBe(0);
  expect(Math.round(box.width)).toBe(390);
  expect(Math.round(box.y + box.height)).toBe(844);
  await page.screenshot({ path: `${SHOTS}/phone-390-results.png` });
  await panel.getByRole('button', { name: /Close/ }).first().click();
  await page.getByRole('button', { name: /^Lenses/ }).click();
  await expect(page.getByRole('combobox', { name: 'Validation source' })).toBeVisible();
  await expectNoSidewaysScroll(page, 'the lens menu');
  await page.screenshot({ path: `${SHOTS}/phone-390-lens-menu.png` });
});
