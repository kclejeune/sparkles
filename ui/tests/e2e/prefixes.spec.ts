// Declared prefixes (spec C20) against a real server: the Prefixes section of the
// Settings tab with prefixes the settings file declares and locks, adding, editing,
// deleting and resetting them, the warning for a prefix that shadows a well-known one,
// and the query editor completing a declared prefix with its PREFIX line.

import { KCLJ, PREFIXES_DATASET, PREFIXES_QUERY_DATASET } from './data';
import { expect, open } from './fixtures';

const ds = PREFIXES_DATASET;

open('the Prefixes section shows, changes and resets prefixes', async ({ page, request }) => {
  const created = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'mem' } });
  expect(created.ok()).toBe(true);
  await page.goto(`/ui/datasets/${ds}?tab=settings`);
  const section = page.getByTestId('settings-prefixes');
  const row = (name: string) => section.locator(`tr[data-prefix="${name}"]`);

  // declared, locked, well-known, and the shadowing warning
  await expect(row('kclj')).toHaveAttribute('data-source', 'server config');
  await expect(row('kclj')).toContainText(KCLJ);
  await expect(row('fixed')).toHaveAttribute('data-source', 'locked');
  await expect(row('fixed').getByRole('button')).toHaveCount(0);
  await expect(row('rdf')).toHaveAttribute('data-source', 'well-known');
  await expect(section.getByTestId('prefix-warnings')).toContainText('urn:x-sparkles:mem:');

  // add a prefix
  await section.getByLabel('New prefix name').fill('ex');
  await section.getByLabel('New prefix IRI').fill('http://ex.org/');
  await section.getByRole('button', { name: 'Add' }).click();
  await expect(page.getByText('Added the prefix ex:')).toBeVisible();
  await expect(row('ex')).toHaveAttribute('data-source', 'changed');
  // a bad name is refused before it is sent
  await section.getByLabel('New prefix name').fill('1bad');
  await section.getByLabel('New prefix IRI').fill('http://ex.org/');
  await section.getByRole('button', { name: 'Add' }).click();
  await expect(section.getByText('starts with a letter')).toBeVisible();

  // edit a declared prefix: it overrides the server config
  await row('kclj').getByRole('button', { name: 'Edit kclj:' }).click();
  await row('kclj').getByLabel('IRI of kclj:').fill('https://kclj.io/x/');
  await row('kclj').getByRole('button', { name: 'Save' }).click();
  await expect(row('kclj')).toContainText('overrides server config');
  await expect(row('kclj')).toContainText(`server config: ${KCLJ}`);
  await expect(section.getByTestId('overrides-prefixes')).toBeVisible();

  // delete a declared prefix: it stays listed as removed with Use server config
  await row('notes').getByRole('button', { name: 'Delete notes:' }).click();
  await expect(row('notes')).toHaveAttribute('data-source', 'removed');
  let k = await (await request.get(`/$/settings/${ds}/prefixes`)).json();
  expect(k.runtime).toEqual({ ex: 'http://ex.org/', kclj: 'https://kclj.io/x/', notes: null });
  const effective = await (await request.get(`/$/prefixes/${ds}`)).json();
  expect(effective.prefixes.notes).toBeUndefined();

  // a write to the locked prefix through the API is refused
  const refused = await request.post(`/${ds}/prefixes?prefix=fixed&uri=http://other/`);
  expect(refused.status()).toBe(409);

  // Use server config for all brings back the declared values and keeps ex
  await section.getByRole('button', { name: 'Use server config for all' }).click();
  await expect(row('notes')).toHaveAttribute('data-source', 'server config');
  await expect(row('kclj')).toHaveAttribute('data-source', 'server config');
  k = await (await request.get(`/$/settings/${ds}/prefixes`)).json();
  expect(k.runtime).toEqual({ ex: 'http://ex.org/' });

  // delete the added prefix
  await row('ex').getByRole('button', { name: 'Delete ex:' }).click();
  await expect(row('ex')).toHaveCount(0);
});

open(
  'the query editor completes a declared prefix and adds its PREFIX line',
  async ({ page, request }) => {
    const q = PREFIXES_QUERY_DATASET;
    const created = await request.post('/$/datasets', { data: { dbName: q, dbType: 'mem' } });
    expect(created.ok()).toBe(true);
    await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), q);
    await page.goto('/ui/query');
    const editor = page.locator('.cm-content');
    await editor.click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.insertText('SELECT * WHERE { ?s ?p ');
    await page.keyboard.type('kcl');
    const option = page.locator('.cm-tooltip-autocomplete li', { hasText: 'kclj:' });
    await expect(option.first()).toBeVisible();
    await option.first().click();
    await expect(editor).toContainText(`PREFIX kclj: <${KCLJ}>`);
  },
);
