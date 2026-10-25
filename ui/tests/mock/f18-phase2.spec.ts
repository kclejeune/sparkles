import { expect, test, type APIRequestContext } from '@playwright/test';
import type { DiagnosticFinding, EntailmentDiff, Justification } from '../../src/lib/api';

const uri = (value: string) => ({ type: 'uri' as const, value });
const blank = { type: 'bnode' as const, value: 'b0' };
const type = uri('http://www.w3.org/1999/02/22-rdf-syntax-ns#type');
const cat = uri('urn:Cat');
const dog = uri('urn:Dog');
const support: Justification = {
  facts: [
    {
      triple: [cat, uri('http://www.w3.org/2002/07/owl#disjointWith'), dog],
      graph: 'default',
      role: 'schema',
    },
    { triple: [blank, type, cat], graph: 'default', role: 'assertion' },
    { triple: [blank, type, dog], graph: 'default', role: 'assertion' },
  ],
};
const finding: DiagnosticFinding = {
  check: 'disjoint-classes',
  rule: 'cax-dw',
  severity: 'inconsistency',
  focus: blank,
  evidence: { classes: [cat, dog] },
  basis: 'asserted',
  message: 'The individual belongs to two disjoint classes.',
  justification: support,
  justificationComplete: true,
};
const diff: EntailmentDiff = {
  entailmentDiffFormat: 1,
  from: { selector: 'before', commit: 42 },
  to: { selector: 'after', commit: 43 },
  added: 1,
  removed: 0,
  sample: { added: [[uri('urn:alice'), type, uri('urn:Employee')]], removed: [] },
  byPredicate: { [type.value]: { added: 1, removed: 0 } },
  byClass: { 'urn:Employee': { added: 1, removed: 0 } },
  newFindings: [],
  sampleTruncated: false,
  findingsComplete: true,
  warnings: [],
};
async function create(request: APIRequestContext, name: string) {
  const response = await request.post('/$/datasets', {
    data: { dbName: name, dbType: 'persistent' },
  });
  expect(response.ok()).toBe(true);
}

test('diagnostic supports preview an inherited blank removal before applying it on a draft branch', async ({
  page,
  request,
}, info) => {
  const name = 'f18-support';
  await create(request, name);
  await page.route(`**/$/reason/${name}/diagnostics**`, (route) =>
    route.fulfill({
      json: {
        diagnosticsFormat: 1,
        dataset: name,
        commit: 7,
        computedAt: '2026-10-25T00:00:00Z',
        scope: { graph: 'default', inferences: { included: false }, closure: 'subclass' },
        status: 'violations-found',
        note: 'Checks selected consistency rules.',
        checks: [
          {
            id: finding.check,
            rules: ['cax-dw'],
            severity: 'inconsistency',
            status: 'violations',
            findings: 1,
            millis: 1,
          },
        ],
        findings: [finding],
      },
    }),
  );
  let branchName = '';
  const branchId = '9d0c41e2-0000-4000-8000-000000000000';
  let previews = 0;
  let applies = 0;
  let creates = 0;
  await page.route(`**/$/branches/${name}`, async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    const body = route.request().postDataJSON();
    creates++;
    branchName = body.name;
    expect(body).toMatchObject({ from: 'main', at: 'commit:7' });
    await route.fulfill({ status: 201, json: { name: branchName, id: branchId, head: 42 } });
  });
  await page.route(/\/f18-support(?:%40|@)support-[^/]+\/patch\?/, async (route) => {
    const url = new URL(route.request().url());
    expect(decodeURIComponent(url.pathname)).not.toBe(`/${name}/patch`);
    const patch = route.request().postData()!;
    expect(patch).toContain(`H prev <urn:uuid:${branchId}#commit:42>`);
    expect(patch).toContain('D <_:b0>');
    if (url.searchParams.get('dryRun') === 'true') {
      previews++;
      expect(applies).toBe(0);
      expect(url.searchParams.get('entailments')).toBe('true');
      expect(url.searchParams.get('profile')).toBe('rules');
      expect(url.searchParams.get('rules')).toBe('');
      await route.fulfill({
        json: {
          dryRun: true,
          wouldCommit: true,
          outcome: 'commit',
          head: 42,
          changes: { total: 1, limit: 1, truncated: false, quads: [] },
          entailments: {
            ...diff,
            added: 0,
            removed: 1,
            sample: { added: [], removed: diff.sample.added },
          },
        },
      });
    } else {
      applies++;
      await route.fulfill({
        json: {
          dataset: name,
          datasetId: branchId,
          committed: true,
          commit: { seq: 43, kind: 'patch' },
        },
      });
    }
  });
  await page.goto(`/ui/datasets/${name}`);
  await page.getByRole('checkbox', { name: 'Find minimal supports' }).check();
  await page.getByRole('button', { name: 'Run checks', exact: true }).click();
  await page.getByText('Minimal asserted supports', { exact: true }).first().click();
  await page.getByRole('button', { name: 'Remove on a branch', exact: true }).nth(1).click();
  const modal = page.getByRole('dialog', { name: 'Remove a support fact on a branch' });
  await expect(modal).toBeVisible();
  expect(creates).toBe(0);
  expect(applies).toBe(0);
  await expect(modal.getByRole('button', { name: 'Apply removal on branch' })).toBeDisabled();
  await modal.getByRole('button', { name: 'Create branch and preview' }).click();
  await expect(modal).toContainText('1 changed quad');
  await expect(modal).toContainText('1 removed');
  expect(creates).toBe(1);
  expect(previews).toBe(1);
  expect(applies).toBe(0);
  await page.screenshot({ path: info.outputPath('f18-support-preview.png'), fullPage: true });
  await modal.getByRole('button', { name: 'Apply removal on branch' }).click();
  await expect(modal).toContainText('in commit 43');
  expect(applies).toBe(1);
  expect(branchName).toMatch(/^support-/);
});

test('merge reasoning uses the complete candidate and updates the requested program', async ({
  page,
  request,
}, info) => {
  const name = 'f18-merge';
  await create(request, name);
  await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: 'INSERT DATA {<urn:base> <urn:p> 1}',
  });
  await request.post(`/$/branches/${name}`, { data: { name: 'dev' } });
  await request.post(`/${name}@dev/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: 'INSERT DATA {<urn:alice> <urn:p> 2}',
  });
  await request.post(`/${name}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: 'INSERT DATA {<urn:bob> <urn:p> 3}',
  });
  const profiles: string[] = [];
  await page.route(`**/$/merge/${name}**`, async (route) => {
    const response = await route.fetch();
    const value = await response.json();
    const url = new URL(route.request().url());
    const body = route.request().method() === 'POST' ? route.request().postDataJSON() : null;
    if (url.searchParams.get('entailments') === 'true' || body?.entailments) {
      value.entailments = diff;
      profiles.push(url.searchParams.get('profile') ?? body?.profile ?? 'recorded');
    }
    await route.fulfill({ response, json: value });
  });
  await page.goto(`/ui/datasets/${name}/merge?source=dev`);
  await page.getByRole('checkbox', { name: 'Include reasoning changes' }).check();
  const panel = page.getByRole('region', { name: 'Merge reasoning changes' });
  await expect(panel).toContainText('1 inferred triple added');
  await page.getByRole('combobox', { name: 'Program', exact: true }).selectOption('rdfs-simple');
  await expect.poll(() => profiles.includes('rdfs-simple')).toBe(true);
  await expect(panel).toContainText('Employee');
  await page.screenshot({ path: info.outputPath('f18-merge-reasoning.png'), fullPage: true });
});

test('review entailment comparison pins the actual upstream and review heads', async ({
  page,
  request,
}, info) => {
  const branch = 'ingest.standup-2026-10-08-1';
  const metadata = await (await request.get('/$/branches/org')).json();
  const selected = metadata.branches.find((entry: { name: string }) => entry.name === branch);
  const upstream = metadata.branches.find(
    (entry: { name: string }) => entry.name === selected.upstream,
  );
  let selectors: [string | null, string | null] | null = null;
  await page.route('**/$/reason/org/diff**', async (route) => {
    const url = new URL(route.request().url());
    selectors = [url.searchParams.get('from'), url.searchParams.get('to')];
    await route.fulfill({ json: diff });
  });
  await page.goto(`/ui/datasets/org/review/${branch}`);
  await page.getByRole('button', { name: 'Compare entailments' }).click();
  await expect
    .poll(() => selectors)
    .toEqual([`${selected.upstream}@${upstream.head}`, `${branch}@${selected.head}`]);
  await expect(page.getByRole('region', { name: 'Reasoning changes' })).toContainText('Employee');
  await page.screenshot({ path: info.outputPath('f18-review-reasoning.png'), fullPage: true });
});

test('class probes distinguish an incomplete negative from a satisfiable result', async ({
  page,
  request,
}) => {
  const name = 'f18-class-incomplete';
  await create(request, name);
  await page.route(`**/$/reason/${name}/justify`, (route) =>
    route.fulfill({
      json: {
        justificationFormat: 1,
        commit: 1,
        checks: 1,
        complete: false,
        results: [
          { class: 'urn:CatDog', finding: null, holds: false, complete: false, justifications: [] },
        ],
      },
    }),
  );
  await page.goto(`/ui/datasets/${name}`);
  await page.getByText('Class satisfiability', { exact: true }).click();
  await page.getByLabel('Class IRI', { exact: true }).fill('urn:CatDog');
  await page.getByRole('button', { name: 'Test class', exact: true }).click();
  await expect(
    page.getByText('The search did not establish a result within its budget.', { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText('No inconsistency was found for a fresh instance.', { exact: true }),
  ).toHaveCount(0);
});

test('a program change discards an in-flight review comparison', async ({ page }) => {
  let started: () => void = () => {};
  const start = new Promise<void>((resolve) => {
    started = resolve;
  });
  let release: () => void = () => {};
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route('**/$/reason/org/diff**', async (route) => {
    started();
    await pending;
    await route.fulfill({ json: diff }).catch(() => {});
  });
  await page.goto('/ui/datasets/org/review/ingest.standup-2026-10-08-1');
  const panel = page.getByRole('region', { name: 'Reasoning changes' });
  await panel.getByRole('button', { name: 'Compare entailments' }).click();
  await start;
  await panel.getByRole('combobox', { name: 'Program' }).selectOption('rdfs-simple');
  release();
  await expect(panel.getByRole('button', { name: 'Compare entailments' })).toBeEnabled();
  await expect(panel.getByText('1 inferred triple added', { exact: false })).toHaveCount(0);
});

test('switching branches discards pending diagnostics before a support can be removed', async ({
  page,
  request,
}) => {
  const name = 'f18-diagnostics-race';
  await create(request, name);
  await request.post(`/$/branches/${name}`, { data: { name: 'dev' } });
  let started: () => void = () => {};
  const start = new Promise<void>((resolve) => {
    started = resolve;
  });
  let release: () => void = () => {};
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  let handled: () => void = () => {};
  const done = new Promise<void>((resolve) => {
    handled = resolve;
  });
  await page.route(`**/$/reason/${name}/diagnostics**`, async (route) => {
    started();
    await pending;
    await route
      .fulfill({
        json: {
          diagnosticsFormat: 1,
          dataset: name,
          commit: 7,
          computedAt: '2026-10-25T00:00:00Z',
          scope: { graph: 'default', inferences: { included: false }, closure: 'subclass' },
          status: 'violations-found',
          note: 'Checks selected rules.',
          checks: [],
          findings: [{ ...finding, message: 'Stale main-branch support must not appear on dev.' }],
        },
      })
      .catch(() => {});
    handled();
  });
  await page.goto(`/ui/datasets/${name}`);
  await page.getByRole('button', { name: 'Run checks', exact: true }).click();
  await start;
  await page.getByRole('combobox', { name: 'Branch', exact: true }).selectOption('dev');
  await expect(page).toHaveURL(/branch=dev/);
  release();
  await done;
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  await expect(
    page.getByText('Stale main-branch support must not appear on dev.', { exact: true }),
  ).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Run checks', exact: true })).toBeEnabled();
});

test('authenticated branch writers can remove supports and open premises on their branch', async ({
  page,
  request,
}) => {
  const name = 'f18-auth-support';
  await create(request, name);
  await request.post(`/$/branches/${name}`, { data: { name: 'dev' } });
  await page.route('**/$/whoami', (route) =>
    route.fulfill({
      json: {
        authEnabled: true,
        principal: { kind: 'token', name: 'writer' },
        method: 'bearer',
        server: [],
        datasets: { [name]: 'write' },
        canMintTokens: false,
        logout: false,
      },
    }),
  );
  await page.route(`**/$/reason/${name}/diagnostics**`, (route) =>
    route.fulfill({
      json: {
        diagnosticsFormat: 1,
        dataset: name,
        commit: 7,
        computedAt: '2026-10-25T00:00:00Z',
        scope: { graph: 'default', inferences: { included: false }, closure: 'subclass' },
        status: 'violations-found',
        note: 'Checks selected rules.',
        checks: [],
        findings: [finding],
      },
    }),
  );
  await page.goto(`/ui/datasets/${name}?branch=dev`);
  await page.getByRole('button', { name: 'Run checks', exact: true }).click();
  await page.getByText('Minimal asserted supports', { exact: true }).first().click();
  const support = page.getByRole('region', { name: 'Justification 1', exact: true });
  await expect(
    support.getByRole('button', { name: 'Remove on a branch', exact: true }),
  ).toHaveCount(3);
  await support.getByRole('button', { name: 'urn:Cat', exact: true }).first().click();
  await expect(page).toHaveURL(
    (url) =>
      url.pathname === '/ui/explore' &&
      url.searchParams.get('ds') === name &&
      url.searchParams.get('branch') === 'dev' &&
      url.searchParams.get('iri') === 'urn:Cat',
  );
});
