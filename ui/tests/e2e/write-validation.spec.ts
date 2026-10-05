import { expect, open } from './fixtures';

for (const dbType of ['mem', 'persistent'] as const) {
  for (const language of ['shacl', 'shex'] as const) {
    open(
      `${dbType} ${language} guard configuration retains sources and rejects invalid writes`,
      async ({ page, request }) => {
        const name = `guard-form-${dbType}-${language}`;
        expect((await request.post('/$/datasets', { data: { dbName: name, dbType } })).ok()).toBe(
          true,
        );
        const initial = await request.post(`/${name}/update`, {
          headers: { 'Content-Type': 'application/sparql-update' },
          data: 'INSERT DATA { <urn:focus> a <urn:Person>; <urn:name> "Alice" }',
        });
        expect(initial.ok()).toBe(true);
        await page.goto(`/ui/datasets/${name}`);
        const panel = page.locator('section.panel', {
          has: page.getByRole('heading', { name: 'Write-time validation', exact: true }),
        });
        await panel.getByRole('button', { name: 'Configure validation', exact: true }).click();
        if (language === 'shex') {
          await panel.getByRole('combobox', { name: 'Language', exact: true }).selectOption('shex');
          await panel
            .getByRole('textbox', { name: 'Schema', exact: true })
            .fill('<urn:S> { <urn:name> . }');
          await panel
            .getByRole('textbox', { name: 'Shape map' })
            .fill('{FOCUS a <urn:Person>}@<urn:S>');
        } else {
          await panel
            .getByRole('textbox', { name: 'Shapes', exact: true })
            .fill(
              '@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape; sh:targetClass <urn:Person>; sh:property [ sh:path <urn:name>; sh:minCount 1 ] .',
            );
        }
        await panel.getByRole('button', { name: 'Save validation' }).click();
        await expect(panel.getByRole('button', { name: 'Configure', exact: true })).toBeVisible();
        await panel.getByRole('button', { name: 'Configure', exact: true }).click();
        await panel.getByRole('combobox', { name: 'Shapes or schema source' }).selectOption('keep');
        await panel.getByRole('spinbutton', { name: 'Report limit' }).fill('7');
        await panel.getByRole('combobox', { name: 'Mode', exact: true }).selectOption('reject');
        // Reject/strict must leave the installed warn guard intact on a bad head.
        expect(
          (
            await request.post(`/${name}/update`, {
              headers: { 'Content-Type': 'application/sparql-update' },
              data: 'INSERT DATA { <urn:bad-head> a <urn:Person> }',
            })
          ).ok(),
        ).toBe(true);
        await panel.getByRole('button', { name: 'Save validation' }).click();
        await expect(panel.getByRole('alert')).toBeVisible();
        expect((await (await request.get(`/$/validation/${name}`)).json()).config.mode).toBe(
          'warn',
        );
        expect(
          (
            await request.post(`/${name}/update`, {
              headers: { 'Content-Type': 'application/sparql-update' },
              data: 'DELETE DATA { <urn:bad-head> a <urn:Person> }',
            })
          ).ok(),
        ).toBe(true);
        await panel.getByRole('button', { name: 'Save validation' }).click();
        await expect(
          panel.getByRole('form', { name: 'Write validation configuration' }),
        ).toHaveCount(0);
        const config = await (await request.get(`/$/validation/${name}`)).json();
        expect(config.config.reportLimit).toBe(7);
        expect(config.config.mode).toBe('reject');
        await page.addInitScript(
          (dataset) => localStorage.setItem('sparkles.dataset', dataset),
          name,
        );
        await page.goto('/ui/query');
        await page.locator('.cm-content').click();
        await page.keyboard.press('ControlOrMeta+a');
        await page.keyboard.insertText('INSERT DATA { <urn:invalid> a <urn:Person> }');
        await page.getByRole('button', { name: 'Run update' }).click();
        await expect(page.getByRole('table', { name: 'Validation results' })).toContainText(
          'urn:invalid',
        );
        const ask = await request.get(`/${name}/sparql`, {
          params: { query: 'ASK { <urn:invalid> a <urn:Person> }' },
          headers: { Accept: 'application/sparql-results+json' },
        });
        expect((await ask.json()).boolean).toBe(false);
        await page.goto(`/ui/datasets/${name}`);
        await panel.getByRole('button', { name: 'Disable', exact: true }).click();
        await expect(panel).toContainText('Writes are not validated.');
      },
    );
  }
}
