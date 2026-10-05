import { expect, test } from '@playwright/test';

const shapes =
  '@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape; sh:targetClass <urn:Person>; sh:property [ sh:path <urn:name>; sh:minCount 1 ] .';
test('configure, edit and disable a write guard', async ({ page, request }) => {
  expect(
    (
      await request.post('/$/datasets', { data: { dbName: 'validation-form', dbType: 'mem' } })
    ).ok(),
  ).toBe(true);
  await page.goto('/ui/datasets/validation-form');
  const panel = page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'Write-time validation', exact: true }),
  });
  await panel.getByRole('button', { name: 'Configure validation', exact: true }).click();
  await panel.getByRole('textbox', { name: 'Shapes', exact: true }).fill(shapes);
  await panel.getByRole('spinbutton', { name: 'Report limit' }).fill('12');
  await panel.getByRole('button', { name: 'Save validation' }).click();
  await expect(panel).toContainText('warn');
  await panel.getByRole('button', { name: 'Configure', exact: true }).click();
  await expect(panel.getByRole('spinbutton', { name: 'Report limit' })).toHaveValue('12');
  await panel.getByRole('combobox', { name: 'Shapes or schema source' }).selectOption('keep');
  await panel.getByRole('combobox', { name: 'Mode', exact: true }).selectOption('reject');
  await panel.getByRole('button', { name: 'Save validation' }).click();
  await expect(panel).toContainText('reject');
  await panel.getByRole('button', { name: 'Disable', exact: true }).click();
  await expect(panel).toContainText('Writes are not validated.');
  const response = await request.get('/$/validation/validation-form');
  expect((await response.json()).config).toBeNull();
});

for (const language of ['shacl', 'shex'] as const) {
  test(`query updates display structured ${language} rejection results`, async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'validation-form'));
    const results =
      language === 'shacl'
        ? [
            {
              focusNode: { type: 'uri', value: 'urn:focus' },
              sourceShape: { type: 'uri', value: 'urn:Shape' },
              resultPath: { type: 'uri', value: 'urn:name' },
              messages: ['Missing required name'],
            },
          ]
        : [
            {
              node: { type: 'uri', value: 'urn:focus' },
              shape: { type: 'start' },
              reason: 'Missing required name',
            },
          ];
    await page.route('**/validation-form/update**', (route) =>
      route.fulfill({
        status: 422,
        contentType: 'application/json',
        body: JSON.stringify({
          error: 'Validation refused the update',
          validation: { language, blocking: 3, total: 3, truncated: true, results },
        }),
      }),
    );
    await page.goto('/ui/query');
    await page.locator('.cm-content').click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.insertText('INSERT DATA { <urn:focus> a <urn:Person> }');
    await page.getByRole('button', { name: 'Run update' }).click();
    const table = page.getByRole('table', { name: 'Validation results' });
    await expect(table).toContainText('Missing required name');
    await expect(table).toContainText('urn:focus');
    if (language === 'shex') await expect(table).toContainText('START');
    await expect(page.getByRole('region', { name: 'Results' })).toContainText('3 blocking results');
    await expect(page.getByRole('region', { name: 'Results' })).toContainText('first 1 results');
  });
}

test('branch changes hide the prior validation configuration until the selected status loads', async ({
  page,
  request,
}) => {
  const name = 'validation-race';
  expect((await request.post('/$/datasets', { data: { dbName: name, dbType: 'mem' } })).ok()).toBe(
    true,
  );
  expect(
    (
      await request.put(`/$/validation/${name}`, {
        data: { mode: 'warn', shapes: { inline: shapes, syntax: 'text/turtle' } },
      })
    ).ok(),
  ).toBe(true);
  expect((await request.post(`/$/branches/${name}`, { data: { name: 'other' } })).ok()).toBe(true);
  expect((await request.delete(`/$/validation/${name}?branch=other`)).ok()).toBe(true);
  await page.goto(`/ui/datasets/${name}`);
  const panel = page.locator('section.panel', {
    has: page.getByRole('heading', { name: 'Write-time validation', exact: true }),
  });
  await panel.getByRole('button', { name: 'Configure', exact: true }).click();
  await expect(panel.getByRole('form', { name: 'Write validation configuration' })).toBeVisible();
  let release!: () => void;
  let requested!: () => void;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  const started = new Promise<void>((resolve) => {
    requested = resolve;
  });
  await page.route(`**/$/validation/${name}?branch=other`, async (route) => {
    requested();
    await pending;
    await route.continue();
  });
  try {
    await page.getByRole('combobox', { name: 'Branch', exact: true }).selectOption('other');
    await started;
    await expect(panel).toContainText('Loading…');
    await expect(panel.getByRole('button', { name: /^Configure/ })).toHaveCount(0);
    await expect(panel.getByRole('button', { name: 'Disable', exact: true })).toHaveCount(0);
    await expect(panel.getByRole('form', { name: 'Write validation configuration' })).toHaveCount(
      0,
    );
  } finally {
    release();
  }
  await expect(panel).toContainText('Writes are not validated.');
  await panel.getByRole('button', { name: 'Configure validation', exact: true }).click();
  await expect(panel.getByRole('textbox', { name: 'Shapes', exact: true })).toHaveValue('');
});
