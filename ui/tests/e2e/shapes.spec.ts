// The SHACL panel of the dataset page: the shapes graph is a code editor whose text
// Validate sends, and which formats as Turtle through the server.

import { expect, test } from './fixtures';
import { DATASET, EX } from './data';
import type { Page } from '@playwright/test';

const shapes = (page: Page) => page.getByRole('textbox', { name: 'Shapes graph (Turtle)' });

async function setText(page: Page, text: string) {
  await shapes(page).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

const SHAPES = (path: string) => `@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <${EX}> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

ex:PersonShape a sh:NodeShape ;
  sh:targetClass ex:Person ;
  sh:property [ sh:path ${path} ; sh:minCount 1 ] .
`;

test('the shapes editor feeds Validate and offers Format', async ({ page }) => {
  await page.goto(`/ui/datasets/${DATASET}`);
  const panel = page.locator('section.panel').filter({
    has: page.getByRole('heading', { name: 'Validate (SHACL)' }),
  });
  await expect(panel.getByRole('button', { name: 'Format', exact: true })).toHaveAttribute(
    'title',
    'Format (Shift+Alt+F)',
  );

  await setText(page, SHAPES('rdfs:label'));
  await panel.getByRole('button', { name: 'Validate', exact: true }).click();
  await expect(panel.locator('.panel-head .badge')).toHaveText(/Conforms/);

  // every person lacks an age: one result each
  await setText(page, SHAPES('ex:age'));
  await panel.getByRole('button', { name: 'Validate', exact: true }).click();
  await expect(panel.locator('.panel-head .badge')).toHaveText(/Does not conform/);
  await expect(panel.getByText(/^4 results/)).toBeVisible();
});

/** The editor's text, line by line (an empty line holds only a `<br>`). */
const textOf = async (page: Page) =>
  (await shapes(page).locator('.cm-line').allInnerTexts())
    .map((l) => l.replace(/\n$/, ''))
    .join('\n');

test('Format formats the shapes as Turtle, one undo restores them, a syntax error is shown', async ({
  page,
}) => {
  await page.goto(`/ui/datasets/${DATASET}`);
  const original = SHAPES('ex:age');
  await setText(page, original);
  await page.keyboard.press('Shift+Alt+F');
  await expect.poll(() => textOf(page)).toBe(`PREFIX ex: <${EX}>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX sh: <http://www.w3.org/ns/shacl#>

ex:PersonShape
  a sh:NodeShape ;
  sh:targetClass ex:Person ;
  sh:property [
    sh:path ex:age ;
    sh:minCount 1 ;
  ] ;
.
`);
  await page.keyboard.press('ControlOrMeta+z');
  await expect.poll(() => textOf(page)).toBe(original);

  await setText(page, `@prefix ex: <${EX}> .\nex:s ex:p ex:o ;\n  ex:q ] .\n`);
  await page.keyboard.press('Shift+Alt+F');
  await expect(page.getByText(/^Can't format: syntax error at line \d/)).toBeVisible();
  await expect(page.locator('.cm-error-line')).toHaveCount(1);
});
