// Where formatting runs: in the browser when the UI was built with the formatter's
// WebAssembly module (`mise run ui:wasm`; loaded the first time something is formatted),
// and through POST /$/format when the build has no module or it does not load. The
// fixtures fail a test on a Content Security Policy violation, so the browser path also
// shows that the pages' policy lets the module compile.

import { expect, test } from './fixtures';
import { DATASET, EX } from './data';
import type { Page } from '@playwright/test';

/** The module's WebAssembly binary (its bindings are a chunk with a hashed name). */
const MODULE = /\/sparkles_fmt_wasm_bg\.[^/]*\.wasm$/;

/** Every request for the module and for the endpoint, from now on. */
function watch(page: Page) {
  const seen = { module: [] as string[], endpoint: 0 };
  page.on('request', (r) => {
    const url = new URL(r.url());
    if (MODULE.test(url.pathname)) seen.module.push(url.pathname);
    if (url.pathname === '/$/format') seen.endpoint++;
  });
  return seen;
}

const queryEditor = (page: Page) => page.locator('.cm-content');
const shapesEditor = (page: Page) => page.getByRole('textbox', { name: 'Shapes graph (Turtle)' });

async function setText(page: Page, editor: ReturnType<typeof queryEditor>, text: string) {
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

/** The editor's text, line by line (an empty line holds only a `<br>`). */
const textOf = async (editor: ReturnType<typeof queryEditor>) =>
  (await editor.locator('.cm-line').allInnerTexts()).map((l) => l.replace(/\n$/, '')).join('\n');

const QUERY = 'select ?s where { ?s ?p ?o filter(?o>1&&?o<10) } limit 5';
const FORMATTED = 'SELECT ?s\nWHERE {\n  ?s ?p ?o .\n  FILTER(?o > 1 && ?o < 10)\n}\nLIMIT 5\n';

test('formats in the browser when the build has the module, loading it once', async ({ page }) => {
  const seen = watch(page);
  await page.goto('/ui/query');
  const editor = queryEditor(page);
  // the editor lints while typing, which loads the module before the first format
  await setText(page, editor, QUERY);

  await page.keyboard.press('Shift+Alt+F');
  await expect.poll(() => textOf(editor)).toBe(FORMATTED);
  test.skip(seen.module.length === 0, 'the UI was built without the browser formatter');
  expect(seen.module).toHaveLength(1);
  expect(seen.endpoint).toBe(0);

  // a syntax error reads as the endpoint's
  await setText(page, editor, 'PREFIX ex: <http://example.org/>\nSELECT * WHERE { ?s ?p ?o ) }');
  await page.keyboard.press('Shift+Alt+F');
  await expect(page.getByText("Can't format: syntax error at line 2")).toBeVisible();
  await expect(page.locator('.cm-error-line')).toContainText('SELECT * WHERE { ?s ?p ?o ) }');

  // the shapes editor of the dataset page formats Turtle with it too
  await page.goto(`/ui/datasets/${DATASET}`);
  const shapes = shapesEditor(page);
  await setText(page, shapes, `@prefix ex: <${EX}> .\nex:s   ex:p ex:o .`);
  await page.keyboard.press('Shift+Alt+F');
  await expect.poll(() => textOf(shapes)).toBe(`PREFIX ex: <${EX}>\n\nex:s ex:p ex:o .\n`);
  expect(seen.endpoint).toBe(0);
});

test('formats through the endpoint when the module does not load', async ({ page }) => {
  // a build without the module, or a network that fails to deliver it
  await page.route(MODULE, (route) => route.abort());
  const seen = watch(page);
  await page.goto('/ui/query');
  const editor = queryEditor(page);
  await setText(page, editor, QUERY);
  await page.keyboard.press('Shift+Alt+F');
  await expect.poll(() => textOf(editor)).toBe(FORMATTED);
  expect(seen.endpoint).toBe(1);

  await setText(page, editor, 'PREFIX ex: <http://example.org/>\nSELECT * WHERE { ?s ?p ?o ) }');
  await page.keyboard.press('Shift+Alt+F');
  await expect(page.getByText("Can't format: syntax error at line 2")).toBeVisible();
  expect(seen.endpoint).toBe(2);

  await page.goto(`/ui/datasets/${DATASET}`);
  const shapes = shapesEditor(page);
  await setText(page, shapes, `@prefix ex: <${EX}> .\nex:s   ex:p ex:o .`);
  await page.keyboard.press('Shift+Alt+F');
  await expect.poll(() => textOf(shapes)).toBe(`PREFIX ex: <${EX}>\n\nex:s ex:p ex:o .\n`);
  expect(seen.endpoint).toBe(3);
});
