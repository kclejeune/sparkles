// The ontology browser against the mock's `uni` dataset (mock/ontology.mjs): the landing
// page and its search, an entity's page with its sections, the lineage tree with its
// children and siblings, the graph view, the downloads, and the top bar's search opening
// a page. tests/e2e/browse.spec.ts checks the server's answers on a real dataset.

import { expect, test, type Page } from '@playwright/test';

test.describe.configure({ timeout: 90_000 });

const UNI = 'http://example.org/uni#';
const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const termUrl = (local: string, extra = '') =>
  `/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + local)}${extra}`;

async function openTerm(page: Page, local: string, extra = '') {
  await page.goto(termUrl(local, extra));
  await expect(page.locator('h1.term-title')).toBeVisible({ timeout: 30_000 });
}

/** Nothing on the page scrolls sideways. */
async function expectNoOverflow(page: Page) {
  const over = await page.evaluate(() => {
    const m = document.querySelector('main') ?? document.documentElement;
    return m.scrollWidth - m.clientWidth;
  });
  expect(over).toBeLessThanOrEqual(0);
}

test('the landing page shows the header, counts, top classes and a search', async ({ page }) => {
  await page.goto('/ui/browse?ds=uni');
  await expect(page.getByRole('heading', { level: 1, name: 'University ontology' })).toBeVisible({
    timeout: 30_000,
  });
  const contents = page.getByRole('region', { name: 'Contents' });
  await expect(contents.getByText('Classes', { exact: true })).toBeVisible();
  const top = page.getByRole('region', { name: 'Top classes' });
  await expect(top.getByRole('link', { name: 'Agent' })).toBeVisible();
  await expect(top.getByText('2 subclasses')).toBeVisible();

  await page.getByRole('searchbox').fill('prof');
  const hit = page.locator('.hits').getByRole('link', { name: 'Professor' });
  await expect(hit).toBeVisible();
  await page.getByRole('radio', { name: 'Properties' }).click();
  await expect(hit).toBeHidden();
  await page.getByRole('radio', { name: 'All' }).click();
  await hit.click();
  await expect(page).toHaveURL(/iri=http%3A%2F%2Fexample\.org%2Funi%23Professor/);
  await expect(page.getByRole('heading', { level: 1, name: 'Professor' })).toBeVisible();
});

test('an entity page shows its axioms as linked prose, its usage and its facts', async ({
  page,
}) => {
  await openTerm(page, 'Professor');
  await expect(page.getByRole('navigation', { name: 'Breadcrumbs' })).toContainText(
    'University ontology',
  );
  const logic = page.getByRole('region', { name: 'Logical definition' });
  await expect(logic.getByRole('heading', { name: 'Subclass of' })).toBeVisible();
  await expect(logic.getByRole('link', { name: 'Employee' })).toBeVisible();
  await expect(logic.locator('.kw', { hasText: 'some' })).toBeVisible();

  const usage = page.getByRole('region', { name: /What links here/ });
  await expect(usage.getByRole('heading', { name: /Instances/ })).toBeVisible();
  await expect(usage.getByRole('link', { name: 'Ada Lovelace' })).toBeVisible();
  await expect(usage.getByRole('heading', { name: /Range of/ })).toBeVisible();

  const facts = page.getByRole('complementary', { name: 'About this entity' });
  await expect(facts).toContainText('Class');
  await expect(facts).toContainText('1 parent, 0 subclasses');
  await expect(facts.getByRole('link', { name: 'Turtle' })).toBeVisible();

  await logic.getByRole('link', { name: 'Employee' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Employee' })).toBeVisible();
});

test('a definition leads the page and a mention names its restriction', async ({ page }) => {
  await openTerm(page, 'Person');
  await expect(page.locator('.lede')).toContainText('A human being');
  await openTerm(page, 'Course');
  const usage = page.getByRole('region', { name: /What links here/ });
  await expect(usage.getByRole('heading', { name: /teaches some Course/ })).toBeVisible();
  await expect(usage.getByRole('link', { name: 'Professor' })).toBeVisible();
});

test('the lineage tree opens children and siblings a level at a time', async ({ page }) => {
  await openTerm(page, 'Person');
  const tree = page.getByRole('complementary', { name: 'Hierarchy' });
  await expect(tree.getByRole('link', { name: 'Agent' })).toBeVisible();
  await expect(tree.locator('[aria-current="page"]')).toContainText('Person');
  await expect(tree.getByRole('link', { name: /Student/ }).first()).toBeVisible();

  const open = tree.getByRole('button', { name: 'Open Student' });
  await open.focus();
  await page.keyboard.press('Enter');
  await expect(tree.getByRole('button', { name: 'Close Student' })).toHaveAttribute(
    'aria-expanded',
    'true',
  );
  await expect(tree.getByRole('link', { name: /Graduate student/ })).toBeVisible();

  const sibs = tree.getByRole('button', { name: /1 other subclass of Agent/ });
  await sibs.click();
  await expect(tree.getByRole('link', { name: /Organization/ })).toBeVisible();
  await expect(tree.locator('.kids').getByRole('link', { name: /^Person/ })).toHaveCount(0);
});

test('the graph view draws the neighbourhood in the hierarchy card and switches back', async ({
  page,
}) => {
  await openTerm(page, 'Professor');
  await page.getByRole('link', { name: 'Graph', exact: true }).click();
  await expect(page).toHaveURL(/view=graph/);
  // the graph replaces the card's tree, and the article stays
  const card = page.getByRole('complementary', { name: 'Hierarchy' });
  await expect(card.locator('.canvas canvas').first()).toBeVisible({ timeout: 20_000 });
  await expect(card.locator('.tree-body')).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'Logical definition' })).toBeVisible();
  // a reload keeps the view
  await page.reload();
  await expect(card.locator('.canvas canvas').first()).toBeVisible({ timeout: 20_000 });
  await expect(page.getByRole('link', { name: 'Open in Explore' }).first()).toHaveAttribute(
    'href',
    /explore\?ds=uni&iri=/,
  );
  await page.getByRole('link', { name: 'Tree', exact: true }).click();
  await expect(card.locator('.tree-body')).toBeVisible();
  await expect(card.locator('.canvas')).toHaveCount(0);
});

test('downloads answer the entity in each format as an attachment', async ({ page }) => {
  await openTerm(page, 'Professor');
  const links = page.locator('[data-format]');
  await expect(links).toHaveCount(4);
  const href = await page.locator('[data-format="ttl"]').getAttribute('href');
  expect(href).toContain('format=ttl');
  const res = await page.request.get(href!);
  expect(res.status()).toBe(200);
  expect(res.headers()['content-disposition']).toContain('attachment');
  expect(await res.text()).toContain(UNI + 'Professor');
});

test('an individual has no tree, shows its types and edits in place or in the ontology editor', async ({
  page,
}) => {
  await openTerm(page, 'ada');
  await expect(page.getByRole('complementary', { name: 'Hierarchy' })).toHaveCount(0);
  await expect(
    page.getByRole('region', { name: 'Logical definition' }).getByRole('heading', {
      name: 'Types',
    }),
  ).toBeVisible();
  // Edit switches the page into edit mode (item 630), whose bar links the full editor
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(
    page.getByRole('toolbar', { name: 'Edit mode' }).getByRole('link', { name: 'Ontology editor' }),
  ).toHaveAttribute('href', /\/ui\/workbench\/ontology\?ds=uni&iri=.*ada&kind=NamedIndividual/);
});

test('an IRI the graphs do not mention says so', async ({ page }) => {
  await page.goto('/ui/browse?ds=uni&iri=' + encodeURIComponent(UNI + 'Nothing'));
  await expect(page.getByRole('alert')).toContainText('do not mention this IRI', {
    timeout: 30_000,
  });
  await expect(page.getByRole('link', { name: 'Back to uni' })).toBeVisible();
});

test('the top bar search opens the entity’s page', async ({ page }) => {
  await page.goto('/ui/browse?ds=uni');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible({ timeout: 30_000 });
  await page.keyboard.press('Control+k');
  await page.keyboard.type('Graduate course');
  const option = page.getByRole('option', { name: /Graduate course/ }).first();
  await expect(option).toBeVisible();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/\/ui\/browse\?ds=uni&iri=.*GraduateCourse/);
  await expect(page.getByRole('heading', { level: 1, name: 'Graduate course' })).toBeVisible();
});

for (const [w, h] of [
  [1440, 900],
  [900, 900],
  [390, 844],
] as const) {
  test(`the pages fit ${w} px without sideways scrolling`, async ({ page }) => {
    await page.setViewportSize({ width: w, height: h });
    for (const url of [
      termUrl('Professor'),
      termUrl('teaches'),
      termUrl('ada'),
      termUrl('Professor', '&view=graph'),
    ]) {
      await page.goto(url);
      await expect(page.locator('h1.term-title')).toBeVisible({ timeout: 30_000 });
      await page.waitForTimeout(300);
      await expectNoOverflow(page);
    }
    await page.goto('/ui/browse?ds=uni');
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible({ timeout: 30_000 });
    await expectNoOverflow(page);
  });
}

test('the phone page keeps the order head, hierarchy, facts, article', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openTerm(page, 'Person');
  const y = async (sel: string) => (await page.locator(sel).first().boundingBox())!.y;
  const head = await y('.b-head');
  const info = await y('.b-info');
  const tree = await y('.b-tree');
  const article = await y('.b-article');
  expect(head).toBeLessThan(tree);
  expect(tree).toBeLessThan(info);
  expect(info).toBeLessThan(article);
  // the hierarchy card opens on request
  const card = page.getByRole('complementary', { name: 'Hierarchy' });
  await expect(card.locator('.tree-body')).toBeHidden();
  await card.getByRole('button', { name: 'Show the hierarchy' }).click();
  await expect(card.locator('.tree-body')).toBeVisible();
  await expect(card.getByRole('button', { name: 'Hide the hierarchy' })).toHaveAttribute(
    'aria-expanded',
    'true',
  );
  // the shell's search icon stands in for the field
  await expect(page.getByRole('button', { name: 'Open search' })).toBeVisible();
});

test('the hierarchy is a card in the side column above the facts', async ({ page }) => {
  for (const width of [1440, 1024]) {
    await page.setViewportSize({ width, height: 900 });
    await openTerm(page, 'Person');
    const box = async (sel: string) => (await page.locator(sel).first().boundingBox())!;
    const head = await box('.b-head');
    const tree = await box('.b-tree');
    const info = await box('.b-info');
    const article = await box('.b-article');
    // the content starts at the gutter, with the side column beside the head
    expect(head.x).toBeLessThan(tree.x);
    expect(Math.abs(tree.y - head.y)).toBeLessThan(24);
    expect(tree.y + tree.height).toBeLessThanOrEqual(info.y);
    expect(Math.abs(tree.x - info.x)).toBeLessThan(1);
    expect(article.x + article.width).toBeLessThanOrEqual(tree.x);
    await expect(page.locator('.b-tree .tree-toggle')).toBeHidden();
    await expect(page.locator('.b-tree .tree-body')).toBeVisible();
  }
});

test('a list whose count is a lower bound pages on until a short page', async ({ page }) => {
  const shown = Array.from({ length: 3 }, (_, n) => `${UNI}x0${n}`);
  // the server stopped counting at the subjects it shows, and SPARQL has 50 and then 7 more
  await page.route(/\/\$\/ontology\/uni\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    t.incoming = [{ property: RDF_TYPE, count: 3, more: true, subjects: shown }];
    await route.fulfill({ response: res, json: t });
  });
  const pages = [
    Array.from({ length: 50 }, (_, n) => `${UNI}x${String(n).padStart(2, '0')}`),
    Array.from({ length: 7 }, (_, n) => `${UNI}y${n}`),
  ];
  let asked = 0;
  await page.route(/\/uni\/sparql/, async (route) => {
    const q = route.request().postData() ?? '';
    if (!q.includes(`?s <${RDF_TYPE}> <${UNI}Professor>`)) return route.fallback();
    const rows = pages[asked++] ?? [];
    // the UI's own result format: variables and rows
    await route.fulfill({
      json: { vars: ['s'], rows: rows.map((value) => [{ type: 'uri', value }]) },
    });
  });
  await openTerm(page, 'Professor');
  const usage = page.getByRole('region', { name: /What links here/ });
  const grp = usage.locator('.grp', { has: page.getByRole('heading', { name: /Instances/ }) });
  await expect(grp.getByRole('link')).toHaveCount(3);
  await grp.getByRole('button', { name: 'Show more' }).click();
  // the first page repeats the three subjects already shown, which come once
  await expect(grp.getByRole('link')).toHaveCount(50);
  await grp.getByRole('button', { name: 'Show more' }).click();
  await expect(grp.getByRole('link')).toHaveCount(57);
  await expect(grp.getByRole('button', { name: /Show more/ })).toHaveCount(0);
});

test('`/` focuses the landing page search and opens the palette on an entity page', async ({
  page,
}) => {
  await page.goto('/ui/browse?ds=uni');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible({ timeout: 30_000 });
  await page.locator('main h1').click();
  await page.keyboard.press('/');
  await expect(page.locator('main').getByRole('searchbox')).toBeFocused();

  await openTerm(page, 'Professor');
  await page.locator('main h1').click();
  await page.keyboard.press('/');
  await expect(page.getByRole('listbox')).toBeVisible();
});

test('a landing page at a past state has no search box', async ({ page }) => {
  await page.goto('/ui/browse?ds=uni&at=1');
  await expect(page.getByRole('heading', { level: 1, name: 'University ontology' })).toBeVisible({
    timeout: 30_000,
  });
  await expect(page.locator('main').getByRole('searchbox')).toHaveCount(0);
});
