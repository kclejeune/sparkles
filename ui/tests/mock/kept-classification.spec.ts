import { expect, test } from '@playwright/test';

const UNI = 'http://example.org/uni#';
function kept(url: URL, page: unknown = null) {
  const commit = Number((url.searchParams.get('at') ?? 'commit:1').split(':')[1]);
  return {
    commit,
    status: 'available',
    reason: null,
    result: {
      resultId: 'immutable-a',
      classifiedCommit: commit,
      boundStoreId: 'branch-a',
      familyId: 'family',
      inputFingerprint: 'input-a',
      sourceFingerprint: 'sources-a',
      contentFingerprint: 'content-a',
      imports: 'dataset',
      mode: 'lenient',
      assertions: false,
      elOnly: false,
      partialRequested: false,
      expansion: 'cautious',
      sourceCounts: { requestedGraphs: 1, resolvedGraphs: 1, imports: 0, warnings: 0 },
      capabilities: [
        'namedClassDirectParents',
        'namedClassEquivalents',
        'namedClassUnsatisfiability',
      ],
      justifications: 'unavailable',
    },
    outcome: {
      tier: 'el',
      complete: true,
      partial: false,
      consistency: 'consistent',
      excludedCount: 0,
      incompleteCount: 0,
    },
    entity: { iri: url.searchParams.get('about'), present: true, unsatisfiable: false },
    activity: null,
    staleCommit: null,
    page,
  };
}
function relationPage(url: URL, items: unknown[] = []) {
  return {
    part: url.searchParams.get('part'),
    about: url.searchParams.get('about'),
    offset: Number(url.searchParams.get('offset') ?? 0),
    limit: 100,
    total: items.length,
    items,
    nextOffset: null,
  };
}

test('an inferred URL and tree toggle never classify implicitly', async ({ page }) => {
  const posts: string[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && /\/classify(?:\?|$)/.test(r.url())) posts.push(r.url());
  });
  await page.goto('/ui/workbench/ontology?ds=uni&inferred=1');
  const asserted = page.getByRole('tree', { name: 'Classes (asserted)' });
  await expect(asserted.getByRole('button', { name: 'Agent', exact: true })).toBeVisible();
  expect(posts).toEqual([]);
  const toggle = page.getByRole('checkbox', { name: /inferred/i });
  await toggle.uncheck();
  await toggle.check();
  await expect(page.getByRole('heading', { name: 'Inferred', exact: true })).toBeVisible();
  expect(posts).toEqual([]);
  await page.getByRole('button', { name: 'Classify tree', exact: true }).click();
  await expect(page.getByRole('tree', { name: 'Classes (inferred)' })).toBeVisible();
  expect(posts).toHaveLength(1);
  expect(new URL(posts[0]).searchParams.get('at')).toMatch(/^commit:\d+$/);
});

test('a fresh empty class frame reads kept state without a session gate or POST', async ({
  page,
}) => {
  const posts: string[] = [];
  const reads: URL[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && /\/classify(?:\?|$)/.test(r.url())) posts.push(r.url());
  });
  await page.route('**/$/ontology/uni/classification?*', async (route) => {
    const url = new URL(route.request().url());
    reads.push(url);
    await route.fulfill({
      json: kept(url, url.searchParams.get('part') === 'status' ? null : relationPage(url)),
    });
  });
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Agent')}`);
  const card = page.getByRole('region', { name: 'Axioms', exact: true });
  await expect(card).toBeVisible();
  const region = page.getByRole('region', { name: 'Inferred class axioms', exact: true });
  await expect(region).toContainText('No additional direct named-class clauses');
  expect(reads.map((r) => r.searchParams.get('part')).sort()).toEqual([
    'equivalents',
    'parents',
    'status',
  ]);
  for (const url of reads.slice(1)) expect(url.searchParams.get('resultId')).toBe('immutable-a');
  expect(posts).toEqual([]);
});

test('full-IRI rendering uses the pinned IRI rather than a label or mutable prefix', async ({
  page,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem(
      'sparkles.rendering',
      JSON.stringify({ version: 1, mode: 'iri', languages: ['en'] }),
    ),
  );
  await page.route('**/$/ontology/uni/classification?*', async (route) => {
    const url = new URL(route.request().url());
    const part = url.searchParams.get('part');
    const items =
      part === 'parents'
        ? [
            {
              iri: UNI + 'Teacher',
              label: 'Do not render this label',
              labelLang: 'en',
              equivalenceCount: 1,
            },
          ]
        : [];
    await route.fulfill({ json: kept(url, part === 'status' ? null : relationPage(url, items)) });
  });
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
  const region = page.getByRole('region', { name: 'Inferred class axioms', exact: true });
  await expect(region.getByRole('link', { name: UNI + 'Teacher', exact: true })).toBeVisible();
  await expect(region).not.toContainText('Do not render this label');
});

test('a language change keeps all new reads pinned to the completed result', async ({ page }) => {
  await page.addInitScript(() =>
    localStorage.setItem(
      'sparkles.rendering',
      JSON.stringify({ version: 1, mode: 'label', languages: ['en'] }),
    ),
  );
  const reads: URL[] = [];
  await page.route('**/$/ontology/uni/classification?*', async (route) => {
    const url = new URL(route.request().url());
    reads.push(url);
    const part = url.searchParams.get('part');
    const french = url.searchParams.get('lang')?.startsWith('fr');
    const items =
      part === 'parents'
        ? [
            {
              iri: UNI + 'Teacher',
              label: french ? 'Enseignant historique' : 'Historical teacher',
              labelLang: french ? 'fr' : 'en',
              equivalenceCount: 1,
            },
          ]
        : [];
    await route.fulfill({ json: kept(url, part === 'status' ? null : relationPage(url, items)) });
  });
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
  const region = page.getByRole('region', { name: 'Inferred class axioms', exact: true });
  await expect(region.getByRole('link', { name: 'Historical teacher', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Rendering preferences', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Rendering preferences' });
  await dialog.getByRole('textbox', { name: 'Language order' }).fill('fr, en');
  await dialog.getByRole('button', { name: 'Apply', exact: true }).click();
  await expect(
    region.getByRole('link', { name: 'Enseignant historique', exact: true }),
  ).toBeVisible();
  const changed = reads.filter((r) => r.searchParams.get('lang')?.startsWith('fr'));
  expect(changed).toHaveLength(3);
  for (const url of changed) expect(url.searchParams.get('resultId')).toBe('immutable-a');
});

test('a failed inference page preserves valid pinned rows and retries only the read', async ({
  page,
}) => {
  let fail = true;
  const reads: URL[] = [];
  const posts: string[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && /\/classify(?:\?|$)/.test(r.url())) posts.push(r.url());
  });
  await page.route('**/$/ontology/uni/classification?*', async (route) => {
    const url = new URL(route.request().url());
    reads.push(url);
    const part = url.searchParams.get('part');
    if (part === 'equivalents' && fail)
      return route.fulfill({ status: 503, json: { error: 'Pinned page failed' } });
    const items =
      part === 'parents'
        ? [
            {
              iri: UNI + 'Teacher',
              label: 'Historical teacher',
              labelLang: 'en',
              equivalenceCount: 1,
            },
          ]
        : [];
    await route.fulfill({ json: kept(url, part === 'status' ? null : relationPage(url, items)) });
  });
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
  const region = page.getByRole('region', { name: 'Inferred class axioms', exact: true });
  await expect(region.getByRole('link', { name: 'Historical teacher', exact: true })).toBeVisible();
  await expect(region).toContainText('Pinned page failed');
  await expect(region).not.toContainText('No additional');
  fail = false;
  await region.getByRole('button', { name: 'Retry equivalents', exact: true }).click();
  await expect(region).not.toContainText('Pinned page failed');
  await expect(region.getByRole('link', { name: 'Historical teacher', exact: true })).toBeVisible();
  expect(reads.filter((r) => r.searchParams.get('part') === 'equivalents')).toHaveLength(2);
  for (const url of reads.slice(1)) expect(url.searchParams.get('resultId')).toBe('immutable-a');
  expect(posts).toEqual([]);
});

test('replacement between actual clause reads discards the whole assembled result', async ({
  page,
}) => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route('**/$/ontology/uni/classification?*', async (route) => {
    const url = new URL(route.request().url());
    const part = url.searchParams.get('part');
    if (part === 'equivalents') {
      await held;
      return route
        .fulfill({
          json: { ...kept(url), status: 'mismatch', result: null, outcome: null, entity: null },
        })
        .catch(() => {});
    }
    const items =
      part === 'parents'
        ? [
            {
              iri: UNI + 'Teacher',
              label: 'Discard this parent',
              labelLang: 'en',
              equivalenceCount: 1,
            },
          ]
        : [];
    await route.fulfill({ json: kept(url, part === 'status' ? null : relationPage(url, items)) });
  });
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
  const region = page.getByRole('region', { name: 'Inferred class axioms', exact: true });
  await expect(
    region.getByRole('link', { name: 'Discard this parent', exact: true }),
  ).toBeVisible();
  release();
  await expect(region).toContainText('does not match this source or result pin');
  await expect(region.getByRole('link', { name: 'Discard this parent', exact: true })).toHaveCount(
    0,
  );
});

test('a deliberate scoped projection keeps clauses and reports its unavailable continuation', async ({
  page,
}) => {
  const posts: string[] = [];
  let reads = 0;
  await page.route('**/$/ontology/uni/classification?*', async (route) => {
    reads++;
    const url = new URL(route.request().url());
    await route.fulfill({
      json: {
        ...kept(url),
        status: 'unavailable',
        reason: 'unsupportedScope',
        result: null,
        outcome: null,
        entity: null,
      },
    });
  });
  await page.route('**/$/ontology/uni/classify?*', async (route) => {
    posts.push(route.request().url());
    const args = route.request().postDataJSON();
    expect(args.projection).toBe('frame');
    const url = new URL(route.request().url());
    const status = kept(url);
    const names = Array.from({ length: 100 }, (_, i) => ({
      iri: UNI + `Alias${i}`,
      label: `Scoped alias ${i}`,
      labelLang: 'en',
    }));
    const empty = { items: [], total: 0, limit: 100, truncated: false };
    await route.fulfill({
      json: {
        commit: status.commit,
        frame: {
          iri: UNI + 'Professor',
          result: status.result,
          outcome: status.outcome,
          unsatisfiable: false,
          parents: empty,
          equivalents: { items: names, total: 101, truncated: true, limit: 100 },
          excluded: empty,
          sources: {
            items: [
              { kind: 'requestedGraph', ordinal: 0, graph: 'default' },
              { kind: 'resolvedGraph', ordinal: 1, graph: 'default', requested: true },
            ],
            total: 2,
            truncated: false,
          },
          retention: { kept: false, resultId: null },
        },
      },
    });
  });
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Professor')}`);
  const region = page.getByRole('region', { name: 'Inferred class axioms', exact: true });
  await region.getByRole('button', { name: 'Classify', exact: true }).click();
  await expect(region.getByRole('link', { name: 'Scoped alias 0', exact: true })).toBeVisible();
  await expect(region).toContainText('Read 100 of 101 equivalents');
  await expect(region).toContainText('Further pages are unavailable for this isolated result');
  await expect(region.getByRole('button', { name: 'Show more equivalents' })).toHaveCount(0);
  await expect(region).not.toContainText('No additional');
  expect(posts).toHaveLength(1);
  expect(reads).toBe(1);
});

test('the explicit legacy tree pins both POST pages and refuses mixed content', async ({
  page,
}) => {
  const posts: URL[] = [];
  await page.route('**/$/ontology/uni/classify?*', async (route) => {
    const url = new URL(route.request().url());
    posts.push(url);
    const commit = Number(url.searchParams.get('at')!.split(':')[1]);
    const first = posts.length === 1;
    await route.fulfill({
      json: {
        commit,
        partial: false,
        consistent: true,
        complete: true,
        classes: 1,
        nodeCount: 1,
        excludedCount: 0,
        excluded: [],
        unsatisfiableCount: 0,
        unsatisfiable: [],
        stats: {},
        nodes: [
          { node: 2, classes: [UNI + 'Teacher'], parents: [], parentClasses: [], children: 0 },
        ],
        classificationSource: {
          boundStoreId: 'branch',
          familyId: 'family',
          commit,
          inputFingerprint: 'input',
          contentFingerprint: first ? 'first-content' : 'different-content',
        },
        ...(first ? { next: 1 } : {}),
      },
    });
  });
  await page.goto('/ui/workbench/ontology?ds=uni&inferred=1');
  await expect(page.getByRole('button', { name: 'Classify tree', exact: true })).toBeEnabled();
  expect(posts).toHaveLength(0);
  await page.getByRole('button', { name: 'Classify tree', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('source or content changed between pages');
  expect(posts).toHaveLength(2);
  expect(posts[0].searchParams.get('at')).toBe(posts[1].searchParams.get('at'));
  await expect(page.getByRole('tree', { name: 'Classes (inferred)' })).toHaveCount(0);
});

test('the selected inferred superclass clause shares pinned tree labels across branch replacement', async ({
  page,
  request,
}) => {
  const reads: URL[] = [];
  let holdSummary = false;
  const heldSummaries: (() => Promise<void>)[] = [];
  await page.route('**/$/ontology/uni?*', async (route) => {
    if (holdSummary) heldSummaries.push(() => route.continue());
    else await route.continue();
  });
  let delayOld = false;
  const delayed: (() => Promise<void>)[] = [];
  await page.route('**/uni*/sparql*', async (route) => {
    const response = await route.fetch();
    const query = route.request().postData() ?? '';
    if (!query.includes('?l')) return route.fulfill({ response });
    const url = new URL(route.request().url());
    const value = await response.json();
    const pinned = url.searchParams.has('at');
    const branch =
      url.searchParams.get('branch') ??
      (url.pathname.includes('@') ? url.pathname.split('@')[1]?.split('/')[0] : null);
    if (pinned) reads.push(url);
    for (const row of value.results?.bindings ?? []) {
      if (row.s?.value === UNI + 'Teacher' && row.l)
        row.l.value = pinned
          ? branch === 'pinned-names-next'
            ? 'Branch teacher'
            : 'Pinned teacher'
          : 'Head teacher';
    }
    const fulfill = () => route.fulfill({ response, json: value });
    if (pinned && branch !== 'pinned-names-next' && delayOld) delayed.push(fulfill);
    else await fulfill();
  });
  expect(
    (await request.post('/$/branches/uni', { data: { name: 'pinned-names-start' } })).ok(),
  ).toBe(true);
  await page.goto(
    `/ui/workbench/ontology?ds=uni&branch=pinned-names-start&inferred=1&iri=${encodeURIComponent(UNI + 'Professor')}`,
  );
  await expect(page.locator('.frame[data-iri]')).toContainText('Employee');
  await page.getByRole('button', { name: 'Classify tree', exact: true }).click();
  const tree = page.getByRole('tree', { name: 'Classes (inferred)' });
  await expect(tree.getByRole('button', { name: 'Pinned teacher', exact: true })).toBeVisible();
  const clause = page.locator('.inferred-sup');
  holdSummary = true;
  const renamed = await request.post('/$/ontology/uni/axioms?branch=pinned-names-start', {
    data: {
      remove: [
        {
          type: 'AnnotationAssertion',
          property: 'http://www.w3.org/2000/01/rdf-schema#label',
          subject: UNI + 'Teacher',
          value: { '@value': 'Teacher', '@language': 'en' },
        },
      ],
      add: [
        {
          type: 'AnnotationAssertion',
          property: 'http://www.w3.org/2000/01/rdf-schema#label',
          subject: UNI + 'Teacher',
          value: { '@value': 'Head teacher', '@language': 'en' },
        },
      ],
    },
  });
  expect(renamed.ok(), await renamed.text()).toBe(true);
  await expect.poll(() => heldSummaries.length).toBeGreaterThan(0);
  await expect(clause.getByRole('button', { name: 'Pinned teacher', exact: true })).toBeVisible();
  await expect(clause).not.toContainText('Head teacher');
  expect(reads.length).toBeGreaterThan(0);
  expect(new Set(reads.map((r) => r.searchParams.get('at'))).size).toBe(1);

  // Release the asserted-summary refresh, then classify its new current commit.
  holdSummary = false;
  for (const release of heldSummaries) await release();
  await expect(tree).toHaveCount(0);
  // Delayed names from a second old-scope tree must not populate a replacement branch.
  delayOld = true;
  await page.getByRole('button', { name: 'Classify tree', exact: true }).click();
  await expect.poll(() => delayed.length).toBeGreaterThan(0);
  expect(
    (await request.post('/$/branches/uni', { data: { name: 'pinned-names-next' } })).ok(),
  ).toBe(true);
  await page.goto(
    `/ui/workbench/ontology?ds=uni&branch=pinned-names-next&inferred=1&iri=${encodeURIComponent(UNI + 'Professor')}`,
  );
  for (const release of delayed) await release().catch(() => {});
  await expect(page.locator('.frame[data-iri]')).toContainText('Employee');
  await expect(clause).toHaveCount(0);
  await page.getByRole('button', { name: 'Classify tree', exact: true }).click();
  await expect(tree.getByRole('button', { name: 'Branch teacher', exact: true })).toBeVisible();
  await expect(clause.getByRole('button', { name: 'Branch teacher', exact: true })).toBeVisible();
  await expect(clause).not.toContainText('Pinned teacher');
});
