import type { APIRequestContext, Page } from '@playwright/test';
import { expect, open } from './fixtures';

open.describe.configure({ timeout: 120_000 });
const SH = 'http://www.w3.org/ns/shacl#';
const GRAPH = 'urn:shape-real:shapes';
const DATA = 'urn:shape-real:data';
const S = 'urn:shape-real:PersonShape';
const NAME = 'urn:shape-real:name';

async function update(request: APIRequestContext, target: string, text: string) {
  const r = await request.post(`/${target}/update?receipt=true`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: text,
  });
  expect(r.ok(), await r.text()).toBe(true);
}
async function setup(request: APIRequestContext) {
  const ds = `shapes-real-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
  const r = await request.post('/$/datasets', { data: { dbName: ds, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  await update(
    request,
    ds,
    `INSERT DATA {
    <urn:default:keep> <urn:p> "default graph is unchanged" .
    GRAPH <${GRAPH}> {
      <${S}> a <${SH}NodeShape>; <${SH}name> "Person"; <${SH}targetClass> <urn:shape-real:Person>;
        <${SH}property> _:name, _:knows; <urn:custom> [ <urn:program> "exact unknown RDF" ] .
      <urn:shape-real:Mirror> a <${SH}NodeShape>; <${SH}name> "Mirror"; <${SH}property> _:name .
      _:name a <${SH}PropertyShape>; <${SH}name> "Name"; <${SH}path> <${NAME}>; <${SH}minCount> 1;
        <${SH}datatype> <http://www.w3.org/2001/XMLSchema#string> .
      _:knows <${SH}name> "Knows"; <${SH}path> <urn:shape-real:knows>; <${SH}hasValue> _:value;
        <${SH}in> (_:value) .
    }
    GRAPH <${DATA}> { <urn:shape-real:alice> a <urn:shape-real:Person>; <${NAME}> "Alice"; <urn:shape-real:knows> _:value . }
  }`,
  );
  const branch = await request.post(`/$/branches/${ds}`, {
    data: { name: 'shape-edits', from: 'main' },
  });
  expect(branch.ok(), await branch.text()).toBe(true);
  return ds;
}
async function rows(request: APIRequestContext, target: string, pattern: string) {
  const r = await request.post(`/${target}/query`, {
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: `SELECT ?s ?p ?o WHERE { ${pattern} }`,
  });
  expect(r.ok(), await r.text()).toBe(true);
  return (await r.json()).results.bindings as Record<string, { type: string; value: string }>[];
}
const exact = (r: unknown[]) => r.map((row) => JSON.stringify(row)).sort();
async function show(page: Page, ds: string) {
  await page.goto(
    `/ui/workbench/shapes?${new URLSearchParams({ ds, branch: 'shape-edits', graph: GRAPH })}`,
  );
  await expect(page.getByRole('tree', { name: 'SHACL shapes' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'New node shape', exact: true })).toBeEnabled();
}
async function chooseName(page: Page) {
  const tree = page.getByRole('tree', { name: 'SHACL shapes' });
  await tree.getByRole('button', { name: 'Expand Person', exact: true }).click();
  await tree.getByRole('button', { name: 'Name', exact: true }).first().click();
  await expect(page.getByRole('textbox', { name: 'minCount value 1', exact: true })).toHaveValue(
    '1',
  );
}
async function commit(page: Page) {
  const dialog = page.getByRole('dialog');
  await expect(dialog.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await dialog.getByRole('button', { name: 'Commit', exact: true }).click();
  await expect(dialog).toBeHidden();
}

open(
  'sample checks retain blank constants, graph edits preserve shared identity and undo is exact',
  async ({ page, request }, info) => {
    const ds = await setup(request),
      target = `${ds}@shape-edits`;
    const initial = await rows(request, ds, `GRAPH <${GRAPH}> { ?s ?p ?o }`);
    const data = await rows(request, ds, `GRAPH <${DATA}> { ?s ?p ?o }`);
    const defaults = await rows(request, ds, 'GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o }');
    const property = initial.find((r) => r.p.value === SH + 'name' && r.o.value === 'Name')!.s
      .value;
    await show(page, ds);
    await page.getByRole('textbox', { name: 'Data graph', exact: true }).fill(`<${DATA}>`);
    await page
      .getByRole('textbox', { name: 'Sample nodes', exact: true })
      .fill('<urn:shape-real:alice>');
    await expect(
      page.getByText('These sample nodes conform to the draft.', { exact: true }),
    ).toBeVisible();
    await chooseName(page);
    await page.getByRole('textbox', { name: 'minCount value 1', exact: true }).fill('2');
    const checked = page.waitForResponse(
      (r) =>
        r.url().includes(`/${target}/shacl?`) &&
        (r.request().postData()?.includes('"2"^^') ?? false),
    );
    await page.getByRole('button', { name: 'Apply minCount', exact: true }).click();
    const report = await (await checked).json();
    expect(report.conforms).toBe(false);
    expect(
      report.results.every(
        (r: { sourceConstraintComponent: { value: string } }) =>
          r.sourceConstraintComponent.value === SH + 'MinCountConstraintComponent',
      ),
    ).toBe(true);
    await page.screenshot({ path: info.outputPath('shapes-real-desktop.png'), fullPage: true });
    await page.getByRole('button', { name: 'Preview commit', exact: true }).click();
    await commit(page);
    const changed = await rows(request, target, `GRAPH <${GRAPH}> { ?s ?p ?o }`);
    expect(changed.find((r) => r.p.value === SH + 'minCount')?.o.value).toBe('2');
    expect(changed.find((r) => r.p.value === SH + 'minCount')?.s.value).toBe(property);
    expect(
      changed.filter((r) => r.p.value === SH + 'property' && r.o.value === property),
    ).toHaveLength(2);
    expect(exact(changed.filter((r) => r.p.value.startsWith('urn:')))).toEqual(
      exact(initial.filter((r) => r.p.value.startsWith('urn:'))),
    );
    expect(exact(await rows(request, ds, `GRAPH <${GRAPH}> { ?s ?p ?o }`))).toEqual(exact(initial));
    expect(exact(await rows(request, target, `GRAPH <${DATA}> { ?s ?p ?o }`))).toEqual(exact(data));
    expect(
      exact(await rows(request, target, 'GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o }')),
    ).toEqual(exact(defaults));
    await expect(page.getByRole('button', { name: 'Undo commit', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Undo commit', exact: true }).click();
    await commit(page);
    expect(exact(await rows(request, target, `GRAPH <${GRAPH}> { ?s ?p ?o }`))).toEqual(
      exact(initial),
    );
    await expect(page.getByRole('button', { name: 'Redo commit', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Redo commit', exact: true }).click();
    await commit(page);
    expect(exact(await rows(request, target, `GRAPH <${GRAPH}> { ?s ?p ?o }`))).toEqual(
      exact(changed),
    );
  },
);

open(
  'a writer between preview and commit refuses the patch and keeps the original draft',
  async ({ page, request }) => {
    const ds = await setup(request),
      target = `${ds}@shape-edits`;
    await page.route('**/changes?**', (route) => route.abort());
    await show(page, ds);
    await chooseName(page);
    await page.getByRole('textbox', { name: 'minCount value 1', exact: true }).fill('9');
    await page.getByRole('button', { name: 'Apply minCount', exact: true }).click();
    await page.getByRole('button', { name: 'Preview commit', exact: true }).click();
    await expect(page.getByRole('dialog').locator('[data-outcome]')).toHaveAttribute(
      'data-outcome',
      'commit',
    );
    await update(request, target, 'INSERT DATA { <urn:another-writer> <urn:p> "kept" }');
    const refused = page.waitForResponse((r) => r.url().includes(`/${target}/patch?receipt=true`));
    await page.getByRole('dialog').getByRole('button', { name: 'Commit', exact: true }).click();
    expect((await refused).status()).toBe(412);
    await expect(
      page.getByRole('button', { name: 'Discard draft and reload', exact: true }),
    ).toBeVisible();
    await expect(page.getByRole('textbox', { name: 'minCount value 1', exact: true })).toHaveValue(
      '9',
    );
    const stored = await rows(
      request,
      target,
      `GRAPH <${GRAPH}> { ?s ?p ?o FILTER(?p = <${SH}minCount>) }`,
    );
    expect(stored[0].o.value).toBe('1');
  },
);

open(
  'authors the stored default graph directly while preserving every named graph',
  async ({ page, request }) => {
    const ds = await setup(request),
      target = `${ds}@shape-edits`;
    const initial = await rows(request, target, 'GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o }');
    const shapes = await rows(request, target, `GRAPH <${GRAPH}> { ?s ?p ?o }`);
    const data = await rows(request, target, `GRAPH <${DATA}> { ?s ?p ?o }`);
    await page.goto(`/ui/workbench/shapes?${new URLSearchParams({ ds, branch: 'shape-edits' })}`);
    await expect(
      page.getByText('This graph has no shapes. Create a node shape to begin.', { exact: true }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'New node shape', exact: true }).click();
    await page.getByLabel('Shape IRI', { exact: true }).fill('<urn:shape-real:DefaultShape>');
    await page.getByRole('button', { name: 'Create shape', exact: true }).click();
    const preview = page.waitForRequest((r) => r.url().includes(`/${target}/patch?dryRun=true`));
    await page.getByRole('button', { name: 'Preview commit', exact: true }).click();
    const patch = (await preview).postData()!;
    const delta = patch.split('\n').filter((line) => /^[AD] /.test(line));
    expect(delta).toEqual([
      `A <urn:shape-real:DefaultShape> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <${SH}NodeShape> .`,
    ]);
    await commit(page);
    const changed = await rows(request, target, 'GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o }');
    expect(changed).toHaveLength(initial.length + 1);
    expect(exact(await rows(request, target, `GRAPH <${GRAPH}> { ?s ?p ?o }`))).toEqual(
      exact(shapes),
    );
    expect(exact(await rows(request, target, `GRAPH <${DATA}> { ?s ?p ?o }`))).toEqual(exact(data));
    expect(exact(await rows(request, ds, 'GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o }'))).toEqual(
      exact(initial),
    );
    await expect(page.getByRole('button', { name: 'Undo commit', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Undo commit', exact: true }).click();
    await commit(page);
    expect(
      exact(await rows(request, target, 'GRAPH <urn:x-arq:DefaultGraph> { ?s ?p ?o }')),
    ).toEqual(exact(initial));
  },
);
