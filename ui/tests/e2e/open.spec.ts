// The server without `--auth-config` (the default `sparkles serve`): the UI it serves
// still manages datasets and writes, while requests from other sites are refused.

import { expect, open } from './fixtures';

open('the UI of an open server creates a dataset and runs an update', async ({ page }) => {
  // the dataset the query page opens (another test adds one of its own)
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'scratch'));
  await page.goto('/ui/datasets');
  await page.getByRole('button', { name: 'New dataset' }).click();
  await page.getByPlaceholder('my-dataset').fill('scratch');
  await page.getByRole('radio', { name: /In-memory/ }).check();
  await page.getByRole('button', { name: 'Create dataset' }).click();
  await expect(page.getByText('Created dataset scratch')).toBeVisible();

  await page.goto('/ui/query');
  const editor = page.locator('.cm-content');
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText('INSERT DATA { <urn:a> <urn:p> "from the UI" }');
  await page.getByRole('button', { name: 'Run update' }).click();
  await expect(page.getByText('Update applied').first()).toBeVisible();

  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText('SELECT ?o WHERE { <urn:a> <urn:p> ?o }');
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('grid')).toContainText('from the UI');
});

open('the new dataset dialog enables full-text search', async ({ page, request }) => {
  await page.goto('/ui/datasets');
  await page.getByRole('button', { name: 'New dataset' }).click();
  await page.getByPlaceholder('my-dataset').fill('searchable');
  await page.getByRole('radio', { name: /In-memory/ }).check();
  await page.getByRole('checkbox', { name: /Enable full-text search/ }).check();
  await page.getByPlaceholder(/rdfs:label/).fill('rdfs:label');
  await page.getByRole('button', { name: 'Create dataset' }).click();
  await expect(page.getByText('Created dataset searchable')).toBeVisible();
  const status = await (await request.get('/$/text/searchable')).json();
  expect(status.state).toBe('ready');
  expect(status.config.predicates).toEqual(['http://www.w3.org/2000/01/rdf-schema#label']);
});

open(
  'an open server refuses writes from other sites and sends them no CORS',
  async ({ request }) => {
    await request.post('/$/datasets', { data: { dbName: 'guarded', dbType: 'mem' } });
    const update = 'INSERT DATA { <urn:x> <urn:p> 1 }';
    const sites: Record<string, string>[] = [
      { Origin: 'https://evil.example' },
      { 'Sec-Fetch-Site': 'cross-site' },
    ];
    for (const headers of sites) {
      const r = await request.post('/guarded/update', {
        headers: { ...headers, 'Content-Type': 'application/sparql-update' },
        data: update,
      });
      expect(r.status()).toBe(403);
      expect((await r.json()).error).toBe('cross-origin request refused');
    }
    const read = await request.get('/$/datasets', { headers: { Origin: 'https://evil.example' } });
    expect(read.ok()).toBe(true);
    expect(read.headers()['access-control-allow-origin']).toBeUndefined();
    // a client that is not a browser still writes
    const cli = await request.post('/guarded/update', {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: update,
    });
    expect(cli.ok()).toBe(true);
  },
);
