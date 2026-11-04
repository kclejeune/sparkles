// The thesaurus editor against the mock's `thes` dataset (mock/skos.mjs): the concept tree
// read one level at a time with its polyhierarchy, label search in any language, the label
// table with its SKOS clashes, the integrity checks of the built-in shapes, and then edits
// on a session branch, each previewed and committed: labels, make preferred, SKOS-XL
// conversion, a drag that moves a concept, a mapping picked in another scheme, the
// ordered collection's list editor and the notes form, with undo, the refusal of a stale
// edit and the refresh after another writer's commit. The tests share the mock's state and
// run in order.

import { expect, test, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';
import { pageAction } from '../page-actions';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const TH = 'http://example.org/thesaurus/';
const tree = (page: Page) => page.getByRole('tree', { name: 'Concepts' });
const node = (page: Page, name: string) =>
  tree(page).getByRole('button', { name, exact: true }).first();
const expand = (page: Page, name: string) =>
  tree(page)
    .getByRole('button', { name: `Expand ${name}` })
    .first()
    .click();
const preview = (page: Page) => page.getByRole('dialog');
const labels = (page: Page) => page.getByRole('region', { name: 'Labels' });

async function commitPreview(page: Page) {
  const d = preview(page);
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden();
}

async function open(page: Page, query = '') {
  await page.goto(`/ui/workbench/thesaurus?ds=thes${query}`);
  await expect(node(page, 'Vertebrates')).toBeVisible({ timeout: 30_000 });
}

test('the tree, search, labels and checks read the thesaurus', async ({ page }) => {
  await open(page);
  await expect(page.getByRole('combobox', { name: 'Concept scheme' })).toHaveValue(`${TH}animals`);
  await expand(page, 'Vertebrates');
  await expand(page, 'Mammals');
  // the whale sits under Mammals and Marine animals
  const whales = tree(page).getByRole('treeitem', { name: 'Whales' }).first();
  await expect(whales).toContainText('2 broader');
  await expand(page, 'Marine animals');
  await expect(tree(page).getByRole('button', { name: 'Whales', exact: true })).toHaveCount(2);
  // a concept of the scheme without a broader one is a root, marked as not a top concept
  await expect(tree(page).getByRole('treeitem', { name: 'Flying animals' })).toContainText(
    'not a top concept',
  );

  // search in French finds the whale and opens it
  await page.getByRole('searchbox', { name: 'Search labels' }).fill('baleine');
  const hit = page.getByRole('region', { name: 'Search results' }).getByRole('button').first();
  await expect(hit).toContainText('Whales');
  await expect(hit).toContainText('“Baleines”@fr');
  await hit.click();
  await expect(page).toHaveURL(/iri=http%3A%2F%2Fexample.org%2Fthesaurus%2Fwhales/);
  await expect(page.getByRole('heading', { name: 'Whales' })).toBeVisible();
  await expect(labels(page).locator('tr[data-lang="fr"]')).toContainText('Baleines');
  // a search in one language only
  await page.getByRole('combobox', { name: 'Search language' }).selectOption('en');
  await page.getByRole('searchbox', { name: 'Search labels' }).fill('baleine');
  await expect(page.getByRole('region', { name: 'Search results' })).toContainText(
    'No label matches',
  );
  await page.getByRole('searchbox', { name: 'Search labels' }).fill('');

  // labels in French in the tree
  await page.getByRole('combobox', { name: 'Label language' }).selectOption('fr');
  await expect(node(page, 'Vertébrés')).toBeVisible();
  await page.getByRole('combobox', { name: 'Label language' }).selectOption('');

  // Owls has the same label as preferred and alternative: S13
  await page.getByRole('searchbox', { name: 'Search labels' }).fill('owls');
  await page.getByRole('region', { name: 'Search results' }).getByRole('button').first().click();
  await expect(page.getByRole('list', { name: 'Label problems' })).toContainText('S13');

  // the SKOS-XL labels of Dogs
  await page.goto(`/ui/workbench/thesaurus?ds=thes&iri=${encodeURIComponent(TH + 'dogs')}`);
  const fr = labels(page).locator('tr[data-lang="fr"] [data-kind="pref"]');
  await expect(fr).toContainText('Chiens');
  await expect(fr.locator('.badge')).toHaveText('XL');

  // the checks of the built-in shapes
  await page.getByRole('tab', { name: 'Validation' }).click();
  const results = page.getByRole('list', { name: 'Check results' });
  await expect(results).toContainText('S13');
  await expect(results.getByRole('button', { name: 'Owls' })).toBeVisible();
  await expectNoSidewaysScroll(page, 'the thesaurus editor');
});

test('edits on a session branch: labels, XL, a move, a mapping', async ({ page, request }) => {
  const count = async (target: string) => {
    const r = await request.post(`/${target}/query`, {
      headers: {
        'Content-Type': 'application/sparql-query',
        Accept: 'application/sparql-results+json',
      },
      data: 'SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }',
    });
    return Number((await r.json()).results.bindings[0].n.value);
  };
  const before = await count('thes');
  await open(page, `&iri=${encodeURIComponent(TH + 'fish')}`);
  await page.getByRole('button', { name: 'Start session' }).click();
  await expect(page).toHaveURL(/branch=skos\./);
  const branch = new URL(page.url()).searchParams.get('branch')!;
  await expect(page.getByRole('heading', { name: 'Fish' })).toBeVisible();

  // an alternative label, then undo
  const add = page.getByRole('form', { name: 'Add a label' });
  await add.getByRole('combobox', { name: 'Kind' }).selectOption('alt');
  await add.getByRole('textbox', { name: 'Label' }).fill('Pisces');
  await add.getByRole('combobox', { name: 'Language' }).fill('la');
  await add.getByRole('button', { name: 'Add' }).click();
  await expect(preview(page)).toContainText('Add the alternative label “Pisces”@la to Fish');
  await commitPreview(page);
  await expect(labels(page).locator('tr[data-lang="la"]')).toContainText('Pisces');
  await pageAction(page, 'Undo');
  await expect(preview(page)).toContainText('Undo:');
  await commitPreview(page);
  await expect(labels(page).locator('tr[data-lang="la"]')).toHaveCount(0);

  // a second English preferred label is refused, and replacing keeps the old one as alternative
  await add.getByRole('combobox', { name: 'Kind' }).selectOption('pref');
  await add.getByRole('textbox', { name: 'Label' }).fill('Fishes');
  await add.getByRole('combobox', { name: 'Language' }).fill('en');
  await expect(add.getByRole('button', { name: 'Add' })).toBeDisabled();
  await expect(page.getByRole('alert').filter({ hasText: 'S14' })).toBeVisible();
  await page.getByRole('button', { name: /Make it preferred and keep/ }).click();
  await commitPreview(page);
  const en = labels(page).locator('tr[data-lang="en"]');
  await expect(en.locator('[data-kind="pref"]')).toContainText('Fishes');
  await expect(en.locator('[data-kind="alt"]')).toContainText('Fish');

  // a plain label made SKOS-XL and back
  await labels(page).getByRole('button', { name: 'Make Poissons a SKOS-XL label' }).click();
  await expect(preview(page)).toContainText('a SKOS-XL label of Fishes');
  await commitPreview(page);
  await expect(labels(page).locator('tr[data-lang="fr"] .badge')).toHaveText('XL');
  await labels(page).getByRole('button', { name: 'Make Poissons a plain label' }).click();
  await commitPreview(page);
  await expect(labels(page).locator('tr[data-lang="fr"] .badge')).toHaveCount(0);

  // drag Bats from Mammals onto Flying animals
  // the selection of Fish opened the path to it
  await expect(tree(page).getByRole('button', { name: 'Collapse Vertebrates' })).toBeVisible();
  await expand(page, 'Mammals');
  await node(page, 'Bats').dragTo(node(page, 'Flying animals'));
  await expect(preview(page)).toContainText('Move Bats under Flying animals');
  await commitPreview(page);
  // the tree lists a concept's children as the rows after it, one level down, so the
  // moved concept is now the row below Flying animals and no longer under Mammals
  const bats = tree(page).getByRole('treeitem', { name: 'Bats' });
  await expect(bats).toHaveCount(1);
  const flying = tree(page).getByRole('treeitem', { name: 'Flying animals' });
  const level = Number(await flying.getAttribute('aria-level'));
  await expect(bats).toHaveAttribute('aria-level', String(level + 1));
  await expect(flying.locator('xpath=following-sibling::li[1]')).toContainText('Bats');
  // moving a concept under its own narrower concept is refused
  await node(page, 'Vertebrates').dragTo(node(page, 'Mammals'));
  await expect(page.locator('.toast').filter({ hasText: 'narrower concepts' })).toBeVisible();

  // a close match picked in the habitats scheme
  await node(page, 'Whales').click();
  await page.getByRole('button', { name: 'Relations', exact: true }).click();
  await expect(page.locator('[data-section="broader"]')).toContainText('Mammals');
  await page.getByRole('button', { name: 'Add close match' }).click();
  const picker = page.getByRole('group', { name: 'Close match picker' });
  await picker.getByRole('combobox', { name: 'Scheme' }).selectOption(`${TH}habitats`);
  await picker.getByRole('searchbox').fill('oce');
  await picker.getByRole('button', { name: /Ocean/ }).click();
  await expect(preview(page)).toContainText('Add Ocean as close match of Whales');
  await commitPreview(page);
  await expect(page.locator('[data-section="closeMatch"]')).toContainText('Ocean');

  // main kept none of it, and the branch kept the unrelated triple
  expect(await count('thes')).toBe(before);
  const kept = await request.post(`/thes@${branch}/query`, {
    headers: {
      'Content-Type': 'application/sparql-query',
      Accept: 'application/sparql-results+json',
    },
    data: 'ASK { <urn:unrelated:s> <urn:unrelated:p> "kept as it is" }',
  });
  expect((await kept.json()).boolean).toBe(true);
});

test('collections, notes, a new concept and a stale edit', async ({ page, request }) => {
  await open(page);
  const branch = (await (await request.get('/$/branches/thes')).json()).branches
    .map((b: { name: string }) => b.name)
    .find((n: string) => n.startsWith('skos.'));
  await page.goto(`/ui/workbench/thesaurus?ds=thes&branch=${branch}`);
  await expect(node(page, 'Vertebrates')).toBeVisible({ timeout: 30_000 });

  // the ordered collection's members in the list editor
  await page.getByRole('combobox', { name: 'List' }).selectOption('collections');
  await page
    .getByRole('list', { name: 'Collections' })
    .getByRole('button', { name: /Mammals by size/ })
    .click();
  await page.getByRole('button', { name: /^Members/ }).click();
  const form = page.locator('.detail form').first();
  await expect(form).toContainText('Ordered members');
  // the dataset keeps no other graph, so the list's cells can be edited
  await expect(
    form.getByRole('button', { name: 'Move Ordered members member 2 up' }),
  ).toBeEnabled();

  // the notes form of a concept, from the built-in SKOS shapes
  await page.getByRole('combobox', { name: 'List' }).selectOption('concepts');
  await node(page, 'Vertebrates').click();
  await page.getByRole('button', { name: 'Notes', exact: true }).click();
  await expect(page.getByRole('region', { name: 'Details' })).toContainText('built-in SKOS shapes');
  await expect(page.getByRole('region', { name: 'Details' })).toContainText('Definition');
  await expect(page.getByRole('region', { name: 'Details' })).not.toContainText('Preferred label');

  // a new concept under the selection
  await page.getByRole('button', { name: 'Concept', exact: true }).click();
  const create = page.getByRole('dialog', { name: 'New concept' });
  await expect(create).toContainText('Under Vertebrates');
  await create.getByRole('textbox', { name: 'Preferred label' }).fill('Reptiles');
  await create.getByRole('button', { name: 'Preview' }).click();
  await commitPreview(page);
  await expect(page.getByRole('heading', { name: 'Reptiles' })).toBeVisible();
  await expect(tree(page).getByRole('button', { name: 'Reptiles', exact: true })).toBeVisible();

  // another writer commits while a preview is open: the commit is refused
  await expect(page.getByText('Following changes')).toBeVisible();
  const add = page.getByRole('form', { name: 'Add a label' });
  await add.getByRole('textbox', { name: 'Label' }).fill('Reptilien');
  await add.getByRole('combobox', { name: 'Language' }).fill('de');
  await add.getByRole('button', { name: 'Add' }).click();
  await expect(preview(page).locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  const r = await request.post(`/thes@${branch}/update`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: `INSERT DATA { <${TH}snakes> a <http://www.w3.org/2004/02/skos/core#Concept> }`,
  });
  expect(r.ok()).toBe(true);
  await expect(page.getByRole('status').filter({ hasText: 'by another writer' })).toBeVisible();
  await preview(page).getByRole('button', { name: 'Commit' }).click();
  await expect(preview(page)).toBeHidden();
  await expect(labels(page).locator('tr[data-lang="de"]')).toHaveCount(0);
});

test('a protected branch is read-only', async ({ page, request }) => {
  const p = await request.patch('/$/branches/thes/main', { data: { protected: true } });
  expect(p.ok()).toBe(true);
  try {
    await open(page, `&iri=${encodeURIComponent(TH + 'fish')}`);
    await expect(page.getByRole('status').filter({ hasText: 'is protected' })).toBeVisible();
    await expect(page.getByRole('form', { name: 'Add a label' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Delete' })).toHaveCount(0);
    // the notes form follows the page
    await page.getByRole('button', { name: 'Notes', exact: true }).click();
    const details = page.getByRole('region', { name: 'Details' });
    await expect(details.getByRole('status')).toContainText('the form is too');
    await expect(details.getByRole('button', { name: /^\+ Add/ }).first()).toBeDisabled();
  } finally {
    await request.patch('/$/branches/thes/main', { data: { protected: false } });
  }
});

test('without the head tag the page cannot guard its edits and is read-only', async ({ page }) => {
  await page.route('**/thes/data?default', (route) =>
    route.request().method() === 'HEAD' ? route.fulfill({ status: 503 }) : route.fallback(),
  );
  await open(page, `&iri=${encodeURIComponent(TH + 'fish')}`);
  await expect(
    page.getByRole('status').filter({ hasText: 'cannot guard its edits' }),
  ).toBeVisible();
  await expect(page.getByRole('form', { name: 'Add a label' })).toHaveCount(0);
});

test('the forms are read-only while the dataset keeps another graph', async ({ page, request }) => {
  const other = `GRAPH <urn:other> { <urn:x> <urn:y> "z" }`;
  const up = (u: string) =>
    request.post('/thes/update', {
      headers: { 'Content-Type': 'application/sparql-update' },
      data: u,
    });
  expect((await up(`INSERT DATA { ${other} }`)).ok()).toBe(true);
  try {
    await open(page, `&iri=${encodeURIComponent(TH + 'bySize')}&view=members`);
    const details = page.getByRole('region', { name: 'Details' });
    await expect(details.getByRole('status')).toContainText("a form's save could change them");
    await expect(
      details.getByRole('button', { name: 'Move Ordered members member 2 up' }),
    ).toBeDisabled();
  } finally {
    await up(`DELETE DATA { ${other} }`);
  }
});

test('undo waits for the head tag', async ({ page }) => {
  await page.addInitScript(() =>
    sessionStorage.setItem(
      'sparkles.skos.undo.thes|',
      JSON.stringify({ done: [{ commit: 1, label: 'An earlier edit' }], undone: [] }),
    ),
  );
  let release = () => {};
  const held = new Promise<void>((r) => (release = r));
  await page.route('**/thes/data?default', async (route) => {
    if (route.request().method() !== 'HEAD') return route.fallback();
    await held;
    await route.fallback();
  });
  await page.goto('/ui/workbench/thesaurus?ds=thes');
  const undo = page.getByRole('button', {
    name: /^Undo/,
    includeHidden: true,
  });
  await expect(undo).toHaveText(/Undo \(1\)/);
  await expect(undo).toBeDisabled();
  release();
  await expect(node(page, 'Vertebrates')).toBeVisible({ timeout: 30_000 });
  await expect(undo).toBeEnabled();
});

test('the forms are read-only for a caller whose grants cover some graphs', async ({ page }) => {
  await page.route('**/$/whoami', (route) =>
    route.fulfill({
      json: {
        authEnabled: true,
        principal: { kind: 'token', name: 'writer' },
        method: 'bearer',
        server: [],
        datasets: { thes: 'write' },
        restricted: { thes: { graphs: true } },
        canMintTokens: false,
        logout: false,
      },
    }),
  );
  await open(page, `&iri=${encodeURIComponent(TH + 'fish')}&view=notes`);
  const details = page.getByRole('region', { name: 'Details' });
  await expect(details.getByRole('status')).toContainText('covers only some graphs');
  // the page's own edits stay, scoped to its graph by their updates
  await expect(page.getByRole('button', { name: /^Labels/ })).toBeVisible();
});

test('under auth the forms are read-only on a branch whose grants the page cannot read', async ({
  page,
  request,
}) => {
  await page.route('**/$/whoami', (route) =>
    route.fulfill({
      json: {
        authEnabled: true,
        principal: { kind: 'token', name: 'writer' },
        method: 'bearer',
        server: [],
        datasets: { thes: 'write' },
        canMintTokens: false,
        logout: false,
      },
    }),
  );
  const branch = (await (await request.get('/$/branches/thes')).json()).branches
    .map((b: { name: string }) => b.name)
    .find((n: string) => n.startsWith('skos.'));
  await page.goto(
    `/ui/workbench/thesaurus?ds=thes&branch=${branch}&iri=${encodeURIComponent(TH + 'fish')}&view=notes`,
  );
  const details = page.getByRole('region', { name: 'Details' });
  await expect(details.getByText(`your access covers on ${branch}`)).toBeVisible({
    timeout: 30_000,
  });
  // on main, where whoami answers, the same caller may edit the forms
  await page.goto(
    `/ui/workbench/thesaurus?ds=thes&iri=${encodeURIComponent(TH + 'fish')}&view=notes`,
  );
  await expect(details).toContainText('built-in SKOS shapes', { timeout: 30_000 });
  await expect(details.getByText('your access covers')).toHaveCount(0);
});
