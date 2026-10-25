// The validation overlay of Explore against a real server
// (docs/specs/U07-data-quality-and-schema-diagram.md §11.8, A10, A11 and A13). The guard
// is configured through the API as write-validation.spec.ts configures it through the UI.

import type { APIRequestContext, Page } from '@playwright/test';
import { anonymous, expect, open, server } from './fixtures';

const EX = 'http://example.org/';
const SHAPES = `@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <${EX}> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] .`;
const DATA = `PREFIX ex: <${EX}> INSERT DATA {
  ex:p1 a ex:Person ; ex:knows ex:p2, ex:p3 . ex:p2 a ex:Person . ex:p3 a ex:Person .
  ex:p4 a ex:Person ; ex:name "Dee" . ex:p1 ex:knows ex:p4 . }`;

async function dataset(request: APIRequestContext, name: string, headers = {}) {
  const made = await request.post('/$/datasets', {
    data: { dbName: name, dbType: 'mem' },
    headers,
  });
  expect(made.ok() || made.status() === 409, await made.text()).toBe(true);
  const u = await request.post(`/${name}/update`, {
    headers: { ...headers, 'Content-Type': 'application/sparql-update' },
    data: DATA,
  });
  expect(u.ok(), await u.text()).toBe(true);
  const g = await request.put(`/$/validation/${name}`, {
    headers,
    data: { mode: 'warn', shapes: { inline: SHAPES } },
  });
  expect(g.ok(), await g.text()).toBe(true);
}

const badgesOf = async (page: Page): Promise<Record<string, string>> => {
  const raw = await page.locator('.wrap[data-badges]').getAttribute('data-badges');
  return Object.fromEntries(raw ? JSON.parse(raw) : []);
};

open('A10 and A11: badges, and a fix previewed and run', async ({ page, request }) => {
  await dataset(request, 'overlay');
  await page.goto(`/ui/explore?ds=overlay&iri=${encodeURIComponent(`${EX}p1`)}`);
  await expect
    .poll(async () => Object.keys(await badgesOf(page)).length, { timeout: 20_000 })
    .toBe(3);
  const b = await badgesOf(page);
  for (const p of ['p1', 'p2', 'p3']) expect(b[`<${EX}${p}>`]).toBe('✕1');
  expect(b[`<${EX}p4>`]).toBeUndefined();
  // the badge channel's validation lens, on by default with a guard
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await expect(menu.getByRole('combobox', { name: 'Badges' })).toHaveValue('validation');
  await page.keyboard.press('Escape');
  // the list view's column
  await page.getByRole('button', { name: 'List', exact: true }).click();
  await expect(page.getByRole('row').filter({ hasText: 'p2' }).first()).toContainText(
    '1 violation',
  );
  await page.getByRole('button', { name: 'List', exact: true }).click();

  await page.getByRole('button', { name: /p2, 1 violation/ }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await panel.getByRole('button', { name: 'Suggest fix' }).click();
  const editor = panel.getByRole('textbox', { name: 'Suggested update' });
  const text = await editor.inputValue();
  expect(text).toContain(`INSERT DATA {\n  <${EX}p2> <${EX}name> ?value .\n}`);
  await editor.fill(text.replace('?value .', '"Ana" .'));
  await panel.getByRole('button', { name: 'Preview' }).click();
  await expect(panel).toContainText('The update would commit.');
  // the guard validates incrementally, and p1 and p3 keep their results, so its summary
  // does not list every result of the state and cannot say this one is resolved
  await expect(panel).toContainText('The preview cannot tell whether this resolves the result.');
  // nothing committed by the preview
  const before = await (await request.get('/$/commits/overlay?limit=1')).json();
  await panel.getByRole('button', { name: 'Run' }).click();
  await expect.poll(async () => (await badgesOf(page))[`<${EX}p2>`]).toBeUndefined();
  expect((await badgesOf(page))[`<${EX}p3>`]).toBe('✕1');
  const after = await (await request.get('/$/commits/overlay?limit=1')).json();
  expect(after.head).toBe(before.head + 1);
});

open('a ShEx guard badges the nodes that do not conform', async ({ page, request }) => {
  const made = await request.post('/$/datasets', {
    data: { dbName: 'overlay-shex', dbType: 'mem' },
  });
  expect(made.ok() || made.status() === 409).toBe(true);
  expect(
    (
      await request.post('/overlay-shex/update', {
        headers: { 'Content-Type': 'application/sparql-update' },
        data: DATA,
      })
    ).ok(),
  ).toBe(true);
  const g = await request.put('/$/validation/overlay-shex', {
    data: {
      language: 'shex',
      mode: 'warn',
      schema: { inline: `PREFIX ex: <${EX}> ex:S { ex:name . }`, format: 'shexc' },
      shapeMap: `{FOCUS a <${EX}Person>}@<${EX}S>`,
    },
  });
  expect(g.ok(), await g.text()).toBe(true);
  await page.goto(`/ui/explore?ds=overlay-shex&iri=${encodeURIComponent(`${EX}p1`)}`);
  await expect
    .poll(async () => Object.keys(await badgesOf(page)).length, { timeout: 20_000 })
    .toBe(3);
  // the lens menu's source picker names the guard's language
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await expect(menu.getByRole('combobox', { name: 'Validation source' })).toContainText('ShEx');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: /p3, 1 violation/ }).click();
  const panel = page.getByRole('dialog', { name: /Validation results of/ });
  await expect(panel).toContainText('cardinality');
  await panel.getByRole('button', { name: 'Suggest fix' }).click();
  await expect(panel.getByRole('textbox', { name: 'Suggested update' })).toHaveValue(
    new RegExp(`<${EX}p3> <${EX}name> \\?value`),
  );
});

anonymous('A13: a caller limited to some graphs gets no overlay', async ({ browser, request }) => {
  const admin = { Authorization: `Bearer ${server.token}` };
  await dataset(request, 'overlay-limited', admin);
  const ctx = await browser.newContext({
    baseURL: server.url,
    extraHTTPHeaders: { Authorization: `Bearer ${server.limitedToken}` },
  });
  const page = await ctx.newPage();
  // the new request forms answer 403
  for (const [path, body] of [
    ['/overlay-limited/shacl?shapes=guard', `target=${encodeURIComponent(`<${EX}p1>`)}`],
    ['/overlay-limited/shex?schema=guard', `node=${encodeURIComponent(`<${EX}p1>`)}`],
  ]) {
    const r = await page.request.post(path, {
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      data: body,
    });
    expect(r.status(), path).toBe(403);
  }
  await page.goto(`/ui/explore?ds=overlay-limited&iri=${encodeURIComponent(`${EX}p1`)}`);
  await expect(page.locator('.wrap[data-graph]')).toBeVisible({ timeout: 20_000 });
  // the lens menu has no validation lens, and says why
  await expect(page.getByRole('region', { name: 'Validation overlay' })).toHaveCount(0);
  await page.getByRole('button', { name: /^Lenses/ }).click();
  const menu = page.getByRole('dialog', { name: 'Lenses' });
  await expect(menu.getByTestId('lens-validation-note')).toHaveText(
    'Validation needs access to every graph.',
  );
  await expect(
    menu.getByRole('combobox', { name: 'Badges' }).locator('option[value="validation"]'),
  ).toHaveCount(0);
  expect(await badgesOf(page)).toEqual({});
  await ctx.close();
});
