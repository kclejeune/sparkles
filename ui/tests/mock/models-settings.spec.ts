// The model configuration form of the server page against the mock (mock/models.mjs):
// allowed models as a list with add, reorder and remove, per-model options as cards with
// typed fields and inline errors, role lists as rows, a server refusal shown beside the
// field it names, the JSON view that shares the form's change, and the layout at a
// phone's width. Each test starts from the declared configuration.

import { expect, test, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

const URL = '/$/server/settings/models';

test.beforeEach(async ({ request }) => {
  expect((await request.delete(URL)).ok()).toBe(true);
});

async function openPanel(page: Page) {
  await page.goto('/ui/server');
  const panel = page.getByTestId('settings-model-configuration');
  await expect(panel.locator('[data-field="providers.claude.allowedModels"]')).toBeVisible();
  return {
    panel,
    row: (field: string) => panel.locator(`[data-field="${field}"]`),
    save: () => panel.getByRole('button', { name: 'Save', exact: true }).click(),
  };
}

const runtime = async (page: Page) =>
  (await (await page.request.get(URL)).json()).runtime as Record<string, unknown>;

test('allowed models are a list with suggestions, reorder and remove', async ({ page }) => {
  const { panel, row, save } = await openPanel(page);
  const allowed = row('providers.claude.allowedModels');
  const list = allowed.getByRole('list', { name: 'Allowed models' });
  await expect(list.getByRole('listitem')).toHaveCount(2);
  await expect(allowed.getByLabel('Allowed models entry 1')).toHaveValue('claude-haiku-5-5');

  // a duplicate is refused in place, a new one is added from the input
  const add = allowed.getByPlaceholder('Add a model');
  await add.fill('claude-haiku-5-5');
  await add.press('Enter');
  await expect(allowed.getByText('claude-haiku-5-5 is in the list already')).toBeVisible();
  await add.fill('claude-opus-5-5');
  await allowed.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(list.getByRole('listitem')).toHaveCount(3);

  // reorder and remove with the keyboard-reachable buttons
  await allowed.getByRole('button', { name: 'Move claude-opus-5-5 up' }).click();
  await expect(allowed.getByLabel('Allowed models entry 2')).toHaveValue('claude-opus-5-5');
  await allowed.getByRole('button', { name: 'Remove claude-haiku-5-5' }).click();
  // the summarize role names the removed model, so the form says so before a save
  const summarize = row('roles.summarize');
  await expect(summarize.getByText('claude does not allow claude-haiku-5-5')).toBeVisible();
  await expect(panel.getByText('1 field needs a fix')).toBeVisible();
  await allowed.getByRole('button', { name: 'Add claude-haiku-5-5' }).click();
  await expect(summarize.getByText('claude does not allow')).toHaveCount(0);

  await expect(allowed.getByText('unsaved')).toBeVisible();
  await save();
  await expect(page.getByText('Saved the model configuration settings')).toBeVisible();
  expect(await runtime(page)).toEqual({
    providers: {
      claude: { allowedModels: ['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-haiku-5-5'] },
    },
  });
  await expect(allowed).toHaveAttribute('data-source', 'runtime');
});

test('per-model options are cards with typed fields and inline errors', async ({ page }) => {
  const { panel, row, save } = await openPanel(page);
  const models = row('providers.local.models');
  const card = models.locator('[data-entry="qwen3:14b"]');
  await expect(card).toContainText('Context tokens 32768');

  // a new model's card opens, suggested from what the provider reports
  const add = models.getByPlaceholder('Add options for a model');
  await expect(models.locator('datalist option[value="llama3.3:70b"]')).toHaveCount(1);
  await add.fill('llama3.3:70b');
  await models.getByRole('button', { name: 'Add model' }).click();
  const added = models.locator('[data-entry="llama3.3:70b"]');
  await expect(added.getByLabel('Structured output')).toHaveValue('auto');
  // the ollama kind offers no tool level
  await expect(added.getByLabel('Structured output').locator('option[value="tool"]')).toHaveCount(
    0,
  );

  // a value out of range and text in a number field are errors beside the field
  const temperature = added.getByLabel('Temperature');
  await temperature.fill('3');
  await expect(added.getByText('Temperature is at most 2')).toBeVisible();
  await expect(temperature).toHaveAttribute('aria-invalid', 'true');
  const ctx = added.getByLabel('Context tokens');
  await ctx.fill('lots');
  await expect(added.getByText('Context tokens must be a whole number')).toBeVisible();
  await expect(added.getByText('2 errors')).toBeVisible();
  await save();
  // nothing is sent while a field does not read
  expect(await runtime(page)).toEqual({});
  await temperature.fill('0.2');
  await ctx.fill('65536');
  await expect(added.locator('.error')).toHaveCount(0);
  await expect(panel.getByText('needs a fix')).toHaveCount(0);

  // an inherited value is the placeholder: the timeout comes from the defaults
  await expect(added.getByLabel('Request timeout (s)')).toHaveAttribute('placeholder', '60');
  await save();
  await expect(page.getByText('Saved the model configuration settings')).toBeVisible();
  expect(await runtime(page)).toEqual({
    providers: {
      local: {
        models: {
          'llama3.3:70b': { structuredOutput: 'auto', temperature: 0.2, contextTokens: 65536 },
        },
      },
    },
  });
  // a declared model cannot be removed at runtime, an added one can
  await models.locator('[data-entry="qwen3:14b"] summary').click();
  await expect(
    models.getByRole('button', { name: 'Remove the options of qwen3:14b' }),
  ).toBeDisabled();
  // the added card stays open after the save
  await models.getByRole('button', { name: 'Remove the options of llama3.3:70b' }).click();
  await save();
  await expect(models.locator('[data-entry="llama3.3:70b"]')).toHaveCount(0);
});

test('a server refusal shows beside the field it names', async ({ page }) => {
  const { panel, row, save } = await openPanel(page);
  const headers = row('providers.gateway.headers');
  await headers.getByPlaceholder('New header').fill('Authorization');
  await headers.getByLabel('Value of the new header').fill('Bearer sk-not-here');
  await headers.getByRole('button', { name: 'Add', exact: true }).click();
  await save();
  // the server's message is on the header's row, and the banner repeats it
  await expect(headers.getByText('the header Authorization carries a key').first()).toBeVisible();
  await expect(panel.getByRole('alert')).toContainText('carries a key');
  await expect(headers.getByLabel('Headers header 2')).toHaveAttribute('aria-invalid', 'true');
  await headers.getByRole('button', { name: 'Remove Authorization' }).click();
  await expect(headers.getByText('carries a key')).toHaveCount(0);

  // a role entry the server refuses names its row
  const draft = row('roles.draft');
  await draft.getByRole('button', { name: 'Add entry' }).click();
  const third = draft.getByRole('listitem').nth(2);
  await third.getByLabel('Provider').selectOption('gateway');
  await third.getByLabel('Model').fill('gpt-5');
  await draft.getByRole('button', { name: 'Move gateway/gpt-5 up' }).click();
  await expect(draft.getByRole('listitem').nth(1).getByLabel('Model')).toHaveValue('gpt-5');
  await save();
  await expect(page.getByText('Saved the model configuration settings')).toBeVisible();
  const r = await runtime(page);
  expect((r.roles as Record<string, unknown>).draft).toEqual([
    { provider: 'local', model: 'qwen3:14b' },
    { provider: 'gateway', model: 'gpt-5' },
    { provider: 'claude', model: 'claude-sonnet-5-5', maxOutputTokens: 1024 },
  ]);
});

test('the JSON view shares the form change and keeps unknown members', async ({ page }) => {
  const { panel, row, save } = await openPanel(page);
  await row('providers.claude.budget.tokensPerDay').getByRole('textbox').fill('500');
  await panel.getByText('Advanced: the runtime layer as JSON').click();
  const json = panel.getByRole('textbox', { name: 'Model configuration runtime layer' });
  await expect(json).toHaveValue(/"tokensPerDay": 500/);
  // an edit of the JSON shows in the form, and the form's edit is still in it
  const layer = JSON.parse(await json.inputValue());
  layer.providers.gateway = { concurrency: 2 };
  await json.fill(JSON.stringify(layer, null, 2));
  await expect(row('providers.gateway.concurrency').getByRole('textbox')).toHaveValue('2');
  await expect(row('providers.claude.budget.tokensPerDay').getByRole('textbox')).toHaveValue('500');
  // and an edit of the form after it keeps what the JSON set
  await row('providers.local.keepAlive').getByRole('textbox').fill('1h');
  await expect(json).toHaveValue(/"concurrency": 2/);
  await expect(json).toHaveValue(/"keepAlive": "1h"/);
  await save();
  await expect(page.getByText('Saved the model configuration settings')).toBeVisible();
  expect(await runtime(page)).toEqual({
    providers: {
      claude: { budget: { tokensPerDay: 500 } },
      gateway: { concurrency: 2 },
      local: { keepAlive: '1h' },
    },
  });
  // JSON that does not parse holds the save
  await json.fill('{ "providers": ');
  await expect(panel.getByText(/Not JSON/)).toBeVisible();
  await expect(panel.getByText('1 field needs a fix')).toBeVisible();
});

test('the form fits a phone and works from the keyboard', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const { row } = await openPanel(page);
  const models = row('providers.claude.models');
  await models.locator('[data-entry="claude-haiku-5-5"] summary').click();
  await expectNoSidewaysScroll(page, 'the models form at 390 px');
  // the label names the add input, and Enter adds from it
  await row('providers.claude.allowedModels')
    .getByRole('combobox', { name: 'Allowed models', exact: true })
    .focus();
  await page.keyboard.type('claude-opus-5-5');
  await page.keyboard.press('Enter');
  await expect(
    row('providers.claude.allowedModels').getByLabel('Allowed models entry 3'),
  ).toHaveValue('claude-opus-5-5');
});
