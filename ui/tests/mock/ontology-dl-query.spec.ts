// The ontology editor's view strip, the DL Query tab and the Manchester add fields
// against the mock's `uni` dataset (mock/ontology.mjs). The mock reads a subset of
// Manchester syntax and answers a query with its told classifier, so these tests check
// the page and its wiring. tests/e2e/ontology-dl-query.spec.ts checks the server's answers.

import { expect, test, type Page } from '@playwright/test';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';
const VIEWS = ['Tree', 'DL Query', 'Header', 'History'];

async function open(page: Page, url = '/ui/workbench/ontology?ds=uni') {
  await page.goto(url);
  await expect(page.getByRole('tablist', { name: 'Ontology views' })).toBeVisible({
    timeout: 30_000,
  });
}

/** Every view tab lies inside the strip and the window, with its whole label shown. */
async function expectWholeTabs(page: Page) {
  const strip = page.getByRole('tablist', { name: 'Ontology views' });
  const box = (await strip.boundingBox())!;
  const width = page.viewportSize()!.width;
  for (const name of VIEWS) {
    const tab = strip.getByRole('tab', { name: new RegExp(`^${name}`) });
    await expect(tab).toBeVisible();
    const b = (await tab.boundingBox())!;
    expect(b.x, name).toBeGreaterThanOrEqual(box.x - 0.5);
    expect(b.x + b.width, name).toBeLessThanOrEqual(Math.min(box.x + box.width, width) + 0.5);
    const clipped = await tab.evaluate((el) => el.scrollWidth > el.clientWidth + 1);
    expect(clipped, name).toBe(false);
  }
}

test('the view strip shows every tab whole at the default width', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 });
  await open(page);
  await expectWholeTabs(page);
});

test('the view strip wraps at phone width instead of hiding tabs', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await open(page);
  await expectWholeTabs(page);
});

const editor = (page: Page) => page.getByRole('textbox', { name: 'Class expression' });

test('a DL query lists its answers with the reasoning that found them', async ({ page }) => {
  await open(page, '/ui/workbench/ontology?ds=uni&tab=dlquery');
  await editor(page).click();
  await page.keyboard.type('Person and (teaches some Course)');
  await page.keyboard.press('Escape');
  await page.keyboard.press('Enter');
  const result = page.getByLabel('DL query answers');
  await expect(result.getByRole('status')).toContainText('OWL 2 EL classifier');
  await expect(result.getByRole('status')).toContainText('Complete');
  await expect(result.getByRole('region', { name: 'Equivalent classes' })).toContainText('Teacher');
  await expect(result.getByRole('region', { name: 'Direct superclasses' })).toContainText('Person');
  await expect(result.getByRole('region', { name: 'Instances' })).toContainText('Ada Lovelace');
  await expect(page).toHaveURL(/dl=Person/);
  // an answer opens its frame beside the query
  await result
    .getByRole('region', { name: 'Direct subclasses' })
    .getByRole('button', { name: 'Professor' })
    .click();
  await expect(page.getByRole('heading', { name: 'Professor', level: 2 })).toBeVisible();
  await expect(page).toHaveURL(new RegExp(encodeURIComponent(UNI + 'Professor')));
  await expect(page.getByRole('tab', { name: 'DL Query' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
});

test('a link to the tab runs its query', async ({ page }) => {
  await open(page, `/ui/workbench/ontology?ds=uni&tab=dlquery&dl=${encodeURIComponent('Student')}`);
  const result = page.getByLabel('DL query answers');
  await expect(result.getByRole('region', { name: 'Direct subclasses' })).toContainText(
    'Graduate student',
  );
  await expect(editor(page)).toHaveText('Student');
});

test('a query that does not parse is underlined where the reader stopped', async ({ page }) => {
  await open(page, '/ui/workbench/ontology?ds=uni&tab=dlquery');
  await editor(page).click();
  await page.keyboard.type('Person and Robot');
  await page.keyboard.press('Escape');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('alert')).toContainText('no class is named Robot');
  await expect(page.locator('.cm-lint-error')).toHaveText('Robot');
  await expect(page.getByLabel('DL query answers')).toHaveCount(0);
});

test('names complete as the expression is typed, quoted where they need it', async ({ page }) => {
  await open(page, '/ui/workbench/ontology?ds=uni&tab=dlquery');
  await editor(page).click();
  await page.keyboard.type('Grad');
  const list = page.locator('.cm-tooltip-autocomplete');
  await expect(list).toContainText('Graduate student');
  await list.getByRole('option', { name: /^Graduate student/ }).click();
  await expect(editor(page)).toHaveText("'Graduate student'");
});

test('a frame section takes a Manchester expression and previews its axiom', async ({ page }) => {
  await open(page, `/ui/workbench/ontology?ds=uni&iri=${encodeURIComponent(UNI + 'Student')}`);
  const sub = page.locator('.frame [data-section="sub"]');
  await sub.getByRole('textbox', { name: /^Add to/ }).click();
  // the quote closes itself as it is typed, and the second steps over it
  await page.keyboard.type("'enrolled in' some Course");
  await page.keyboard.press('Escape');
  await page.keyboard.press('Enter');
  const d = page.getByRole('dialog');
  await expect(d).toContainText("SubClassOf 'enrolled in' some Course");
  await d.getByRole('button', { name: 'Cancel' }).click();
  await expect(d).toBeHidden();
});

test('an unknown name in a frame section is reported with its span', async ({ page }) => {
  await open(page, `/ui/workbench/ontology?ds=uni&iri=${encodeURIComponent(UNI + 'Student')}`);
  const sub = page.locator('.frame [data-section="sub"]');
  await sub.getByRole('textbox', { name: /^Add to/ }).click();
  await page.keyboard.type('attends some Course');
  await page.keyboard.press('Escape');
  await page.keyboard.press('Enter');
  await expect(sub.getByRole('alert')).toContainText('attends');
  await expect(sub.locator('.cm-lint-error')).toHaveText('attends');
});

test('the editor follows the address through the history', async ({ page }) => {
  await open(page, '/ui/workbench/ontology?ds=uni&tab=dlquery');
  const result = page.getByLabel('DL query answers');
  for (const q of ['Student', 'Course']) {
    await editor(page).click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.press('Delete');
    await page.keyboard.type(q);
    await page.keyboard.press('Escape');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(new RegExp(`dl=${q}`));
    await expect(result.getByRole('region', { name: 'Equivalent classes' })).toContainText(q);
  }
  // a frame opened from the answers keeps the query, and going back shows the earlier one
  await result
    .getByRole('region', { name: 'Direct subclasses' })
    .getByRole('button', { name: 'Graduate course' })
    .click();
  await expect(editor(page)).toHaveText('Course');
  await page.goBack();
  await page.goBack();
  await expect(editor(page)).toHaveText('Student');
  await expect(result.getByRole('region', { name: 'Direct subclasses' })).toContainText(
    'Graduate student',
  );
});

test('a problem is underlined where it is when the text starts with spaces', async ({ page }) => {
  await open(page, '/ui/workbench/ontology?ds=uni&tab=dlquery');
  await editor(page).click();
  await page.keyboard.type('  Person and Robot');
  await page.keyboard.press('Escape');
  await page.keyboard.press('Enter');
  await expect(page.locator('.cm-lint-error')).toHaveText('Robot');
});
