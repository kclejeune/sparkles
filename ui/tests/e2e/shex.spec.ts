// The ShEx side of the dataset page's Validate panel, on the server without auth: each
// test loads the people-and-organisation example into a dataset of its own.

import { readFileSync } from 'node:fs';
import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

const DATA = `@prefix ex: <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name "Alice" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob   a ex:Person ; foaf:name "Bob" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
ex:acme  a ex:Org ; foaf:name "ACME" ; ex:city "Paris" ; ex:mayor ex:bob .
`;

const SCHEMA = `PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ; foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city ["Paris" "Kyoto"] }
`;

const MAP = '{FOCUS a ex:Person}@ex:Person,ex:acme@ex:Org';

async function dataset(request: APIRequestContext, name: string) {
  const made = await request.post('/$/datasets', { data: { dbName: name, dbType: 'mem' } });
  expect(made.ok()).toBe(true);
  const loaded = await request.post(`/${name}/data`, {
    headers: { 'Content-Type': 'text/turtle' },
    data: DATA,
  });
  expect(loaded.ok()).toBe(true);
}

const panel = (page: Page) =>
  page.locator('section.panel').filter({
    has: page.getByRole('heading', { name: 'Validate', exact: true }),
  });

/** Replace the schema editor's text (one insertion, so brackets are not auto-closed). */
async function setSchema(page: Page, text: string) {
  await page.getByRole('textbox', { name: 'ShEx schema' }).click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.press('Delete');
  await page.keyboard.insertText(text);
}

async function showShex(page: Page, name: string) {
  await page.goto(`/ui/datasets/${name}`);
  await panel(page).getByRole('tab', { name: 'ShEx' }).click();
  await expect(page.getByRole('textbox', { name: 'ShEx schema' })).toBeVisible();
}

open(
  'ShEx validates the example: results in map order, failures, download',
  async ({ page, request }) => {
    await dataset(request, 'shex-b1');
    await showShex(page, 'shex-b1');
    await setSchema(page, SCHEMA);
    await page.getByRole('textbox', { name: 'Shape map' }).fill(MAP);
    const p = panel(page);
    await p.getByRole('button', { name: 'Validate', exact: true }).click();
    await expect(p.locator('.panel-head .badge')).toHaveText(/Does not conform/);
    await expect(p.getByText(/^2 conformant · 2 nonconformant · \d/)).toBeVisible();

    const rows = p.locator('.shex-results > table > tbody > tr');
    await expect(rows).toHaveCount(4);
    const nodes = await rows.locator('td:first-child a').allInnerTexts();
    expect(nodes.map((n) => n.trim())).toEqual(['ex:alice', 'ex:bob', 'ex:carol', 'ex:acme']);
    const statuses = await rows.locator('td:nth-child(3)').allInnerTexts();
    expect(statuses.map((s) => s.trim())).toEqual([
      'conformant',
      'conformant',
      'nonconformant',
      'nonconformant',
    ]);
    await expect(rows.nth(0).locator('td:nth-child(2)')).toHaveText('ex:Person');
    await expect(rows.nth(3).locator('td:nth-child(2)')).toHaveText('ex:Org');
    await expect(rows.nth(0).getByRole('link')).toHaveAttribute('href', /explore\?ds=shex-b1&iri=/);

    // carol: no name, and an age over 150
    await rows.nth(2).getByRole('button', { name: 'Show the failures' }).click();
    const failures = p.locator('table.failures');
    await expect(failures).toBeVisible();
    await expect(failures).toContainText('cardinality');
    await expect(failures).toContainText('foaf:name');
    await expect(failures).toContainText('0 found, exactly 1 expected');
    await expect(failures).toContainText('facet');
    await expect(failures).toContainText('fails MAXINCLUSIVE 150');

    const download = page.waitForEvent('download');
    await p.getByRole('button', { name: /Result \(\.json\)/ }).click();
    const file = await download;
    expect(file.suggestedFilename()).toBe('shex-b1-shex-result.json');
    const map = JSON.parse(readFileSync(await file.path(), 'utf8'));
    expect(map).toHaveLength(4);
    expect(map[2]).toMatchObject({ node: '<http://ex.org/carol>', status: 'nonconformant' });
  },
);

open('a schema syntax error shows its line and column', async ({ page, request }) => {
  await dataset(request, 'shex-syntax');
  await showShex(page, 'shex-syntax');
  await setSchema(page, 'PREFIX ex: <http://ex.org/>\nex:S {\n  ex:p . ;;\n}\n');
  await page.getByRole('textbox', { name: 'Shape map' }).fill('ex:alice@ex:S');
  await panel(page).getByRole('button', { name: 'Validate', exact: true }).click();
  await expect(page.getByText(/schema syntax error at line 3, column \d+/)).toBeVisible();
  await expect(panel(page).locator('.cm-error-line')).toHaveCount(1);
  await expect(panel(page).locator('.cm-error-line')).toContainText('ex:p');
});

open('the language and the ShEx draft survive a reload', async ({ page, request }) => {
  await dataset(request, 'shex-draft');
  await showShex(page, 'shex-draft');
  // the example comes from the dataset's classes
  await expect(page.getByRole('textbox', { name: 'Shape map' })).toHaveValue(
    /^\{FOCUS a (ex:Person|<http:\/\/ex\.org\/Person>)\}@/,
  );
  await setSchema(page, SCHEMA);
  await page.getByRole('textbox', { name: 'Shape map' }).fill(MAP);

  await page.reload();
  await expect(panel(page).getByRole('tab', { name: 'ShEx' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  await expect(page.getByRole('textbox', { name: 'Shape map' })).toHaveValue(MAP);
  await expect(page.getByRole('textbox', { name: 'ShEx schema' })).toContainText('ex:Org CLOSED');

  // back to SHACL, which is kept too
  await panel(page).getByRole('tab', { name: 'SHACL' }).click();
  await page.reload();
  await expect(page.getByRole('textbox', { name: 'Shapes graph (Turtle)' })).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'ShEx schema' })).toHaveCount(0);
});
