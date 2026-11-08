import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });
const SH = 'http://www.w3.org/ns/shacl#';
const GRAPH = 'urn:shape-editor:shapes';
const DATA = 'urn:shape-editor:data';
const S = 'urn:shape-editor:PersonShape';
const P = 'urn:shape-editor:name';
const TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const seed = `INSERT DATA {
  <urn:unrelated> <urn:keep> "unchanged default" .
  GRAPH <${GRAPH}> {
    <${S}> a <${SH}NodeShape>; <${SH}name> "Person"; <${SH}targetClass> <urn:shape-editor:Person>;
      <${SH}property> _:name; <urn:custom> [ <urn:program> "preserve this" ] .
    _:name a <${SH}PropertyShape>; <${SH}name> "Name"; <${SH}path> <${P}>;
      <${SH}minCount> 1; <${SH}datatype> <http://www.w3.org/2001/XMLSchema#string> .
    <urn:shape-editor:Other> a <${SH}NodeShape>; <${SH}name> "Other" .
  }
  GRAPH <${DATA}> { <urn:shape-editor:alice> a <urn:shape-editor:Person> . }
}`;
async function setup(request: APIRequestContext, suffix = '') {
  const ds = `shape-editor-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}${suffix}`;
  const created = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'persistent' } });
  expect(created.ok(), await created.text()).toBe(true);
  await update(request, ds, seed);
  return ds;
}
async function update(request: APIRequestContext, ds: string, text: string) {
  const r = await request.post(`/${ds}/update?receipt=true`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: text,
  });
  expect(r.ok(), await r.text()).toBe(true);
}
async function open(page: Page, ds: string, branch?: string) {
  await page.goto(
    `/ui/workbench/shapes?${new URLSearchParams({ ds, graph: GRAPH, ...(branch ? { branch } : {}) })}`,
  );
  await expect(page.getByRole('tree', { name: 'SHACL shapes' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'New node shape', exact: true })).toBeEnabled();
}
async function reportHeaders(request: APIRequestContext, ds: string) {
  const response = await request.get(`/$/branches/${ds}`);
  expect(response.ok()).toBe(true);
  const body = await response.json();
  const main = body.branches.find((b: { name: string }) => b.name === 'main');
  return { 'Sparkles-Dataset-Id': body.datasetId as string, 'Sparkles-Commit': String(main.head) };
}
const tree = (page: Page) => page.getByRole('tree', { name: 'SHACL shapes' });
async function pick(page: Page, name: string) {
  if ((await tree(page).getByRole('button', { name, exact: true }).count()) === 0)
    await tree(page).getByRole('button', { name: 'Expand Person', exact: true }).click();
  await tree(page).getByRole('button', { name, exact: true }).first().click();
  await expect(page.locator('.definition h2')).toBeVisible();
}
async function changeCount(page: Page, value: string) {
  await pick(page, 'Name');
  await page.getByRole('textbox', { name: 'minCount value 1', exact: true }).fill(value);
  await page.getByRole('button', { name: 'Apply minCount', exact: true }).click();
}
async function commit(page: Page) {
  const dialog = page.getByRole('dialog');
  await expect(dialog.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await dialog.getByRole('button', { name: 'Commit', exact: true }).click();
  await expect(dialog).toBeHidden();
}

test('edits nested paths and constraints as one exact graph patch, and reverts the commit', async ({
  page,
  request,
}, info) => {
  const ds = await setup(request);
  await open(page, ds);
  await changeCount(page, '2');
  await page
    .getByRole('combobox', { name: 'Path kind at depth 0', exact: true })
    .selectOption('sequence');
  const members = page.getByRole('textbox', { name: 'Path predicate at depth 1', exact: true });
  await members.nth(0).fill('<urn:shape-editor:given>');
  await members.nth(1).fill('<urn:shape-editor:family>');
  await page.getByRole('button', { name: 'Apply path', exact: true }).click();
  await page.getByRole('combobox', { name: 'Add constraints parameter' }).selectOption('maxCount');
  await page.getByRole('button', { name: 'Add constraint', exact: true }).click();
  const max = page.locator('[data-field="maxCount"]');
  await max.getByRole('button', { name: 'Enter value', exact: true }).click();
  await max.getByRole('textbox', { name: 'maxCount value 1', exact: true }).fill('4');
  await max.getByRole('button', { name: 'Apply maxCount', exact: true }).click();
  await page.getByRole('button', { name: 'View Turtle', exact: true }).click();
  await expect(page.locator('.source pre')).toContainText('preserve this');
  await expect(page.locator('.source pre')).toContainText(
    'http://www.w3.org/1999/02/22-rdf-syntax-ns#rest',
  );
  await expect(page.getByRole('button', { name: 'Preview commit', exact: true })).toBeEnabled();
  await page.screenshot({ path: info.outputPath('shapes-desktop.png'), fullPage: true });
  const bodies: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes(`/${ds}/patch?`)) bodies.push(r.postData() ?? '');
  });
  await page.getByRole('button', { name: 'Preview commit', exact: true }).click();
  await commit(page);
  expect(bodies).toHaveLength(2);
  expect(bodies[0]).toBe(bodies[1]);
  expect(bodies[0]).toContain('H prev <urn:uuid:');
  expect(
    bodies[0]
      .split('\n')
      .filter((l) => /^[AD] /.test(l))
      .every((l) => l.endsWith(`<${GRAPH}> .`)),
  ).toBe(true);
  await expect(page.getByRole('button', { name: 'Undo commit', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Undo commit', exact: true }).click();
  await commit(page);
  await expect(page.getByRole('button', { name: 'Redo commit', exact: true })).toBeEnabled();
});

test('retains field buffers across refresh, blocks source conflicts and reloads only on explicit discard', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  await open(page, ds);
  await pick(page, 'Name');
  const input = page.getByRole('textbox', { name: 'minCount value 1', exact: true });
  await input.fill('7');
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await expect(input).toHaveValue('7');
  await expect(page.getByRole('button', { name: 'Preview commit', exact: true })).toBeDisabled();
  await tree(page).getByRole('button', { name: 'Other', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Apply or discard');
  await update(request, ds, 'INSERT DATA { <urn:another> <urn:p> 1 }');
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await expect(
    page.getByText('The source changed or could not be refreshed.', { exact: false }),
  ).toBeVisible();
  await expect(input).toHaveValue('7');
  await page.getByRole('button', { name: 'Discard draft and reload', exact: true }).click();
  await expect(page.getByRole('textbox', { name: 'minCount value 1', exact: true })).toHaveValue(
    '1',
  );
});

test('creates named/anonymous shapes, Core targets, severity and ordered logical lists with local undo', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  await open(page, ds);
  await page.getByRole('button', { name: 'New node shape', exact: true }).click();
  await page
    .getByRole('textbox', { name: 'Shape IRI', exact: true })
    .fill('<urn:shape-editor:New>');
  await page.getByRole('button', { name: 'Create shape', exact: true }).click();
  await expect(page.locator('.definition h2')).toContainText('urn:shape-editor:New');
  await page.getByRole('combobox', { name: 'Add targets parameter' }).selectOption('targetClass');
  await page.getByRole('button', { name: 'Add target', exact: true }).click();
  await page
    .locator('[data-field="targetClass"]')
    .getByRole('button', { name: 'Enter value', exact: true })
    .click();
  await page
    .getByRole('textbox', { name: 'targetClass value 1', exact: true })
    .fill('<urn:shape-editor:Person>');
  await page.getByRole('button', { name: 'Apply targetClass', exact: true }).click();
  await page.getByRole('combobox', { name: 'Add description parameter' }).selectOption('severity');
  await page.getByRole('button', { name: 'Add description', exact: true }).click();
  await page
    .locator('[data-field="severity"]')
    .getByRole('button', { name: 'Enter value', exact: true })
    .click();
  await page.getByLabel('severity value 1', { exact: true }).fill('sh:Warning');
  await page.getByRole('button', { name: 'Apply severity', exact: true }).click();
  await page.getByRole('combobox', { name: 'Add constraints parameter' }).selectOption('or');
  await page.getByRole('button', { name: 'Add constraint', exact: true }).click();
  const list = page.locator('[data-field="or"]');
  await list.getByRole('button', { name: 'Add list item', exact: true }).click();
  await page.getByLabel('or value 1', { exact: true }).fill(`<${S}>`);
  await list.getByRole('button', { name: 'Add list item', exact: true }).click();
  await page.getByLabel('or value 2', { exact: true }).fill('<urn:shape-editor:Other>');
  await page.getByRole('button', { name: 'Apply or', exact: true }).click();
  await page.getByRole('button', { name: 'Undo draft', exact: true }).click();
  await expect(page.locator('[data-field="or"]')).toHaveCount(0);
  await page.getByRole('button', { name: 'Redo draft', exact: true }).click();
  await expect(page.locator('[data-field="or"]')).toBeVisible();
  await page.getByRole('button', { name: 'New property shape', exact: true }).first().click();
  await page.getByRole('button', { name: 'Create shape', exact: true }).click();
  await expect(page.getByRole('list', { name: 'Shape definition problems' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Preview commit', exact: true })).toBeDisabled();
});

test('checks explicit samples at their pinned source, aborts superseded work and clears previous approval', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  const headers = await reportHeaders(request, ds);
  const calls: { url: URL; body: string }[] = [];
  await page.route('**/shacl?**', async (route) => {
    const r = route.request();
    calls.push({ url: new URL(r.url()), body: r.postData() ?? '' });
    await route.fulfill({ headers, json: { conforms: true, results: [] } });
  });
  await open(page, ds);
  await page.getByRole('textbox', { name: 'Data graph', exact: true }).fill(`<${DATA}>`);
  await page
    .getByRole('textbox', { name: 'Sample nodes', exact: true })
    .fill('<urn:shape-editor:alice>');
  await expect(
    page.getByText('These sample nodes conform to the draft.', { exact: true }),
  ).toBeVisible();
  expect(calls).toHaveLength(1);
  expect(calls[0].url.searchParams.get('storedBlankNodes')).toBe('true');
  expect(calls[0].url.searchParams.get('graph')).toBe(DATA);
  expect(calls[0].url.searchParams.has('at')).toBe(true);
  expect(calls[0].body).toContain('preserve this');
  await page.unroute('**/shacl?**');
  let release!: () => void;
  const wait = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route('**/shacl?**', async (route) => {
    await wait;
    await route.fulfill({ headers, json: { conforms: true, results: [] } }).catch(() => {});
  });
  await page
    .getByRole('textbox', { name: 'Sample nodes', exact: true })
    .fill('<urn:shape-editor:bob>');
  await expect(page.getByRole('button', { name: 'Cancel check', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Cancel check', exact: true }).click();
  release();
  await expect(
    page.getByText('These sample nodes conform to the draft.', { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText('Sample check canceled or timed out.', { exact: true }),
  ).toBeVisible();
  await page.getByRole('textbox', { name: 'Sample nodes', exact: true }).fill('not an IRI');
  await expect(page.getByRole('alert')).toContainText('absolute IRI');
});

test('refuses truncated reads and does not expose an editable partial graph', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  await page.route(`**/${ds}/sparql?**`, async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    if (route.request().postData()?.includes('SELECT ?s ?p ?o'))
      body.meta.totalRows = body.rows.length + 1;
    await route.fulfill({ response, json: body });
  });
  await page.goto(`/ui/workbench/shapes?${new URLSearchParams({ ds, graph: GRAPH })}`);
  await expect(page.getByRole('alert')).toContainText('incomplete');
  await expect(page.getByRole('button', { name: 'Preview commit', exact: true })).toHaveCount(0);
  await expect(page.locator('.definition')).toHaveCount(0);
});

test('blocks protected/read-only writes and server guard refusals without losing the draft', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  const protectedResponse = await request.patch(`/$/branches/${ds}/main`, {
    data: { protected: true },
  });
  expect(protectedResponse.ok()).toBe(true);
  await page.goto(`/ui/workbench/shapes?${new URLSearchParams({ ds, graph: GRAPH })}`);
  await expect(page.getByRole('button', { name: 'New node shape', exact: true })).toBeDisabled();
  await expect(page.getByText('This branch is protected.', { exact: false })).toBeVisible();
  await request.patch(`/$/branches/${ds}/main`, { data: { protected: false } });
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await expect(page.getByRole('button', { name: 'New node shape', exact: true })).toBeEnabled();
  await changeCount(page, '3');
  await page.route(`**/${ds}/patch?**`, (route) =>
    route.fulfill({
      status: 422,
      json: {
        error: 'Validation refused the write: 1 blocking result',
        dryRun: true,
        outcome: 'rejected',
        wouldCommit: false,
        head: 1,
        validation: { status: 'rejected', blocking: 1, total: 1, results: [] },
      },
    }),
  );
  await page.getByRole('button', { name: 'Preview commit', exact: true }).click();
  await expect(page.getByRole('dialog').locator('[data-outcome]')).toHaveAttribute(
    'data-outcome',
    'rejected',
  );
  await expect(
    page.getByRole('dialog').getByRole('button', { name: 'Commit', exact: true }),
  ).toBeDisabled();
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(page.getByRole('textbox', { name: 'minCount value 1', exact: true })).toHaveValue(
    '3',
  );
});

test('keeps the tree, forms and path controls inside a phone viewport', async ({
  page,
  request,
}, info) => {
  const ds = await setup(request);
  await page.setViewportSize({ width: 390, height: 844 });
  await open(page, ds);
  await changeCount(page, '2');
  await page
    .getByRole('combobox', { name: 'Path kind at depth 0', exact: true })
    .selectOption('alternativePath');
  const members = page.getByRole('textbox', { name: 'Path predicate at depth 1', exact: true });
  await members.nth(0).fill('<urn:shape-editor:one>');
  await members.nth(1).fill('<urn:shape-editor:two>');
  await page.getByRole('button', { name: 'Apply path', exact: true }).click();
  await expectNoSidewaysScroll(page, 'visual SHACL forms and paths');
  await page.screenshot({ path: info.outputPath('shapes-phone.png'), fullPage: true });
});

test('keeps readable graphs visible to readers and refuses triple-filtered authoring', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  const who = {
    authEnabled: true,
    principal: { kind: 'token', name: 'reader' },
    method: 'bearer',
    server: [],
    datasets: { [ds]: 'read' },
    canMintTokens: false,
    logout: false,
  };
  await page.route('**/$/whoami', (route) => route.fulfill({ json: who }));
  await page.goto(`/ui/workbench/shapes?${new URLSearchParams({ ds, graph: GRAPH })}`);
  await expect(tree(page)).toBeVisible();
  await expect(page.getByRole('button', { name: 'New node shape', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Preview commit', exact: true })).toBeDisabled();
  await page.unroute('**/$/whoami');
  await page.route('**/$/whoami', (route) =>
    route.fulfill({
      json: { ...who, datasets: { [ds]: 'write' }, restricted: { [ds]: { triples: true } } },
    }),
  );
  await page.reload();
  await expect(page.getByRole('alert')).toContainText('Triple-filtered');
  await expect(page.locator('.definition')).toHaveCount(0);
});

test('keeps drafts after an endpoint refusal and ignores a canceled delayed preview', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  await open(page, ds);
  await changeCount(page, '5');
  await page.route(`**/${ds}/patch?**`, (route) =>
    route.fulfill({ status: 403, json: { error: 'Patch endpoint is denied' } }),
  );
  await page.getByRole('button', { name: 'Preview commit', exact: true }).click();
  await expect(page.getByRole('dialog')).toContainText('Patch endpoint is denied');
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(page.getByLabel('minCount value 1', { exact: true })).toHaveValue('5');
  await page.unroute(`**/${ds}/patch?**`);
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(`**/${ds}/patch?**`, async (route) => {
    await gate;
    await route
      .fulfill({ json: { dryRun: true, outcome: 'commit', wouldCommit: true, head: 1 } })
      .catch(() => {});
  });
  const started = page.waitForRequest((r) => r.url().includes(`/${ds}/patch?dryRun=true`));
  await page.getByRole('button', { name: 'Preview commit', exact: true }).click();
  await started;
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click();
  release();
  await expect(page.getByRole('dialog')).toBeHidden();
  await expect(page.getByLabel('minCount value 1', { exact: true })).toHaveValue('5');
});

test('checks literal targets through a selected sample graph without explicit resource filters', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  await update(
    request,
    ds,
    `INSERT DATA { GRAPH <${GRAPH}> { <urn:literal-shape> <${SH}targetNode> "literal focus"; <${SH}nodeKind> <${SH}IRI> } }`,
  );
  const headers = await reportHeaders(request, ds);
  let sample: URL | null = null;
  await page.route('**/shacl?**', async (route) => {
    sample = new URL(route.request().url());
    await route.fulfill({ headers, json: { conforms: false, results: [] } });
  });
  await open(page, ds);
  await page.getByLabel('Data graph', { exact: true }).fill(`<${DATA}>`);
  await page.getByLabel('Sample selection', { exact: true }).selectOption('targets');
  await expect(
    page.getByText('The sample check has validation results.', { exact: true }),
  ).toBeVisible();
  expect(sample).not.toBeNull();
  expect((sample as unknown as URL).searchParams.has('target')).toBe(false);
  expect((sample as unknown as URL).searchParams.get('graph')).toBe(DATA);
});

test('transfers a complete draft with source identity and retains the SHACLC text option', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  await open(page, ds);
  await page.getByRole('button', { name: 'View Turtle', exact: true }).click();
  await page.getByRole('button', { name: 'Use in text validator', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(
    dialog.getByRole('button', { name: 'Open text validator', exact: true }),
  ).toBeDisabled();
  await dialog.getByLabel('Replace the saved text draft', { exact: true }).check();
  await dialog.getByRole('button', { name: 'Open text validator', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`/ui/datasets/${ds}#validate`));
  const stored = await page.evaluate((ds) => localStorage.getItem(`sparkles.shacl.${ds}`), ds);
  expect(stored).toContain('# Source');
  expect(stored).toContain(GRAPH);
  expect(stored).toContain('preserve this');
  await expect(
    page.getByLabel('Shapes syntax', { exact: true }).locator('option[value="shaclc"]'),
  ).toHaveCount(1);
});

test('refuses virtual authoring graph selectors without opening a partial editor', async ({
  page,
  request,
}) => {
  const ds = await setup(request);
  for (const graph of ['urn:x-arq:DefaultGraph', 'urn:x-arq:UnionGraph']) {
    await page.goto(`/ui/workbench/shapes?${new URLSearchParams({ ds, graph })}`);
    await expect(page.getByRole('alert')).toContainText('not named authoring graphs');
    await expect(page.locator('.definition')).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Preview commit', exact: true })).toHaveCount(0);
  }
});

test('clears sample approval when a replacement source answers at the same commit', async ({
  page,
  request,
}) => {
  const ds = await setup(request),
    headers = await reportHeaders(request, ds);
  let replaced = false;
  await page.route('**/shacl?**', (route) =>
    route.fulfill({
      headers: replaced
        ? { ...headers, 'Sparkles-Dataset-Id': '00000000-0000-4000-8000-000000000001' }
        : headers,
      json: { conforms: true, results: [] },
    }),
  );
  await open(page, ds);
  await page.getByLabel('Sample nodes', { exact: true }).fill('<urn:shape-editor:alice>');
  await expect(
    page.getByText('These sample nodes conform to the draft.', { exact: true }),
  ).toBeVisible();
  replaced = true;
  await page.getByLabel('Sample nodes', { exact: true }).fill('<urn:shape-editor:bob>');
  await expect(page.getByRole('alert')).toContainText('captured source UUID and commit');
  await expect(
    page.getByText('These sample nodes conform to the draft.', { exact: true }),
  ).toHaveCount(0);
});
