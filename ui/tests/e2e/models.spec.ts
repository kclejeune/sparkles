// The server page's Models section (C19 §11.4) against a real server with a model
// configuration, a declared key and a locked endpoint: the providers and their status, a
// locked field, a change that overrides the server config and its reset, adding and
// removing a provider, keys that are write-only, and a section that people without
// server-admin do not see.

import { DECLARED_KEY } from './data';
import { expect, open, test } from './fixtures';

const RUNTIME_KEY = 'sk-e2e-runtime-key-never-shown';

open('the Models section edits providers, roles and keys', async ({ page, request }) => {
  // a clean runtime layer, also when the test is retried
  expect((await request.delete('/$/server/settings/models')).ok()).toBe(true);
  for (const name of ['anthropic', 'gw2'])
    expect((await request.delete(`/$/server/secrets/${name}`)).ok()).toBe(true);

  await page.goto('/ui/server');
  const models = page.getByTestId('models-section');
  const providers = models.getByRole('table', { name: 'Model providers' });
  await expect(providers.locator('[data-provider=claude]')).toContainText('anthropic');
  await expect(providers.locator('[data-provider=local]')).toContainText('127.0.0.1:9');

  const panel = page.getByTestId('settings-model-configuration');
  const row = (field: string) => panel.locator(`[data-field="${field}"]`);
  // the operator locked claude's endpoint, so the provider cannot be removed either
  await expect(row('providers.claude.endpoint')).toHaveAttribute('data-source', 'locked');
  await expect(row('providers.claude.endpoint').getByRole('textbox')).toBeDisabled();
  await expect(panel.getByRole('button', { name: 'Remove provider claude' })).toBeDisabled();

  // a budget change overrides the declared one, and says so with the declared value
  const budget = row('providers.claude.budget.tokensPerDay');
  await expect(budget).toHaveAttribute('data-source', 'declared');
  await budget.getByRole('textbox').fill('500');
  await panel.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText('Saved the model configuration settings')).toBeVisible();
  await expect(budget).toHaveAttribute('data-source', 'runtime');
  await expect(budget.getByText('overrides server config')).toBeVisible();
  await expect(budget.getByText('server config: 1000')).toBeVisible();
  await expect(page.getByTestId('overrides-model-configuration')).toContainText(
    '1 field overrides the server config',
  );
  // a tall window, so the screenshot holds the whole section
  await page.setViewportSize({ width: 1280, height: 3600 });
  await models.screenshot({ path: '../target/models-section.png' });
  await page.setViewportSize({ width: 1280, height: 720 });

  // a provider added through the form, with its key set write-only
  await models.getByRole('button', { name: 'Add provider' }).click();
  const add = models.getByRole('form', { name: 'Add a provider' });
  await add.getByLabel('Name', { exact: true }).fill('gw2');
  await add.getByLabel('Protocol').selectOption('openai');
  await add.getByLabel('Endpoint', { exact: true }).fill('http://127.0.0.1:9/v2');
  await add.getByLabel('Key (secret name)').fill('gw2');
  await add.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(page.getByText('Added provider gw2')).toBeVisible();
  await expect(providers.locator('[data-provider=gw2]')).toContainText('key missing');
  await expect(row('providers.gw2.endpoint')).toHaveAttribute('data-source', 'runtime');
  await expect(
    row('providers.gw2.endpoint').getByRole('button', { name: 'Reset Endpoint to default' }),
  ).toBeVisible();

  const keys = page.getByTestId('model-keys');
  const gw2 = keys.locator('[data-secret=gw2]');
  await expect(gw2).toContainText('not set');
  await gw2.getByRole('button', { name: 'Replace the key of gw2' }).click();
  await gw2.getByLabel('New key for gw2').fill(RUNTIME_KEY);
  await gw2.getByRole('button', { name: 'Save key' }).click();
  await expect(page.getByText('Stored a new key for gw2')).toBeVisible();
  await expect(gw2).toContainText('set here');
  await expect(gw2).toContainText('Used by gw2');
  await expect(gw2.getByLabel('New key for gw2')).toHaveCount(0);
  await expect(providers.locator('[data-provider=gw2]')).toContainText('ok');

  // a key stored over the declared one overrides it, and Use server config removes it
  const anthropic = keys.locator('[data-secret=anthropic]');
  await expect(anthropic).toContainText('server config');
  await anthropic.getByRole('button', { name: 'Replace the key of anthropic' }).click();
  await anthropic.getByLabel('New key for anthropic').fill(RUNTIME_KEY);
  await anthropic.getByRole('button', { name: 'Save key' }).click();
  await expect(anthropic).toContainText('overrides server config');
  await keys.screenshot({ path: '../target/models-keys.png' });
  await anthropic.getByRole('button', { name: 'Use server config for anthropic' }).click();
  const useKey = page.getByRole('dialog', { name: "Use the server config's key for anthropic?" });
  await useKey.getByRole('button', { name: 'Use server config' }).click();
  await expect(anthropic.getByText('overrides server config')).toHaveCount(0);
  await expect(anthropic).toContainText('server config');

  // no key reaches the page or an answer of the API
  expect(await page.content()).not.toContain(RUNTIME_KEY);
  expect(await page.content()).not.toContain(DECLARED_KEY);
  for (const path of ['/$/server/secrets', '/$/server/settings/models', '/$/models'])
    expect(await (await request.get(path)).text()).not.toContain(RUNTIME_KEY);

  // a role list and the removal of a declared provider override the server config too
  await row('roles.draft').getByRole('textbox').fill('[{"provider": "claude", "model": "c"}]');
  await panel.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(row('roles.draft').getByText('overrides server config')).toBeVisible();
  await panel.getByRole('button', { name: 'Remove provider local' }).click();
  await page
    .getByRole('dialog', { name: 'Remove provider local?' })
    .getByRole('button', { name: 'Remove' })
    .click();
  await expect(page.getByText('Removed provider local')).toBeVisible();
  await expect(models.locator('[data-removed=local]')).toBeVisible();
  await expect(providers.locator('[data-provider=local]')).toHaveCount(0);

  // Use server config for all clears the overrides and keeps the added provider
  const bar = page.getByTestId('overrides-model-configuration');
  await expect(bar).toContainText('3 fields override the server config');
  await bar.getByRole('button', { name: 'Use server config for all' }).click();
  const confirm = page.getByRole('dialog', { name: 'Use the server config for these fields?' });
  await expect(confirm).toContainText('providers.local');
  await confirm.getByRole('button', { name: 'Use server config' }).click();
  await expect(providers.locator('[data-provider=local]')).toBeVisible();
  await expect(page.getByTestId('overrides-model-configuration')).toHaveCount(0);
  await expect(row('providers.claude.budget.tokensPerDay').getByRole('textbox')).toHaveValue(
    '1000',
  );
  await expect(row('providers.gw2.endpoint')).toHaveAttribute('data-source', 'runtime');

  // the added provider is removed, and its stored key with Remove runtime value
  await panel.getByRole('button', { name: 'Remove provider gw2' }).click();
  await page
    .getByRole('dialog', { name: 'Remove provider gw2?' })
    .getByRole('button', { name: 'Remove' })
    .click();
  await expect(providers.locator('[data-provider=gw2]')).toHaveCount(0);
  await gw2.getByRole('button', { name: 'Remove the runtime value of gw2' }).click();
  await page
    .getByRole('dialog', { name: 'Remove the key of gw2?' })
    .getByRole('button', { name: 'Remove' })
    .click();
  await expect(gw2).toHaveCount(0);

  // a preset fills the Add form, and its fields stay editable
  await models.getByRole('button', { name: 'Add provider' }).click();
  await add.getByLabel('Preset').selectOption('openai');
  await expect(add.getByLabel('Protocol')).toHaveValue('openai');
  await expect(add.getByLabel('Endpoint', { exact: true })).toHaveValue(
    'https://api.openai.com/v1',
  );
  await expect(add.getByLabel('Key (secret name)')).toHaveValue('openai');
  await expect(add.getByLabel('Model', { exact: true })).toHaveValue('gpt-5-mini');
  await add.getByLabel('Name', { exact: true }).fill('oai');
  await add.getByLabel('Key (secret name)').fill('oai-key');
  await add.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(page.getByText('Added provider oai')).toBeVisible();
  const oai = (await (await request.get('/$/server/settings/models')).json()).effective.providers
    .oai;
  expect(oai).toEqual({
    kind: 'openai',
    endpoint: 'https://api.openai.com/v1',
    apiKey: { secret: 'oai-key' },
    models: { 'gpt-5-mini': { structuredOutput: 'auto' } },
  });

  // turning certificate checks off needs the acknowledgement; Cancel sends nothing
  const skip = row('providers.oai.tls.insecureSkipVerify').getByRole('checkbox');
  await expect(skip).not.toBeChecked();
  await skip.check();
  await panel.getByRole('button', { name: 'Save', exact: true }).click();
  const ask = page.getByRole('dialog', { name: 'Turn off certificate verification?' });
  await expect(ask).toContainText('can be read');
  const turnOff = ask.getByRole('button', { name: 'Turn off verification' });
  await expect(turnOff).toBeDisabled();
  await ask.getByRole('button', { name: 'Cancel' }).click();
  await expect(ask).toHaveCount(0);
  expect(
    (await (await request.get('/$/server/settings/models')).json()).effective.providers.oai.tls,
  ).toBeUndefined();
  await panel.getByRole('button', { name: 'Save', exact: true }).click();
  await ask.getByRole('checkbox').check();
  await turnOff.click();
  await expect(ask).toHaveCount(0);
  await expect(providers.locator('[data-provider=oai]')).toContainText('unverified');
  const listed = (await (await request.get('/$/models')).json()).providers.find(
    (p: { name: string }) => p.name === 'oai',
  );
  expect(listed.unverified).toBe(true);
  await panel.getByRole('button', { name: 'Remove provider oai' }).click();
  await page
    .getByRole('dialog', { name: 'Remove provider oai?' })
    .getByRole('button', { name: 'Remove' })
    .click();
  await expect(providers.locator('[data-provider=oai]')).toHaveCount(0);

  const k = await (await request.get('/$/server/settings/models')).json();
  expect(k.runtime).toEqual({});
});

test('people without server-admin see no Models section', async ({ page }) => {
  await page.goto('/ui/server');
  await expect(page.getByRole('heading', { name: 'Readiness' })).toBeVisible();
  await expect(page.getByTestId('models-section')).toHaveCount(0);
});
