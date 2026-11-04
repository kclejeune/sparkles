// The ontology browser against the mock's `uni` dataset (mock/ontology.mjs): the landing
// page and its search, an entity's page with its sections, the lineage tree with its
// children and siblings, the graph view, the downloads, and the top bar's search opening
// a page. tests/e2e/browse.spec.ts checks the server's answers on a real dataset.

import { expect, test, type Page } from '@playwright/test';
import { pageAction } from '../page-actions';

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
  // with no definition, the comment leads the Definition card, marked as a comment
  const lede = page.locator('.lede');
  await expect(lede).toContainText('An academic who leads research');
  await expect(page.locator('.lead [data-kind]').first().locator('h3')).toHaveText(
    /^Comment\s*·\s*rdfs:comment$/,
  );
  await expect(page.getByRole('region', { name: 'Comment' })).toHaveCount(0);
  // the frame in Manchester syntax: asserted clauses, then what the classification infers
  const logic = page.getByRole('region', { name: 'Axioms' });
  const sub = logic.locator('[data-section="sub"]');
  await expect(sub.locator('dt')).toHaveText('SubClassOf:');
  await expect(sub.getByRole('link', { name: 'Employee' })).toBeVisible();
  await expect(sub.locator('.kw', { hasText: 'some' })).toBeVisible();
  // a class with no EquivalentTo axiom has no logical definition
  await expect(page.getByRole('region', { name: 'Logical definition' })).toHaveCount(0);
  await logic.getByRole('button', { name: 'Classify' }).click();
  const inferred = logic.locator('[data-section="inferred-sub"]');
  await expect(inferred.getByRole('link', { name: 'Teacher' })).toBeVisible();
  await expect(inferred).toContainText('Inferred');
  // the asserted superclass is not repeated as inferred
  await expect(inferred.getByRole('link', { name: 'Employee' })).toHaveCount(0);

  const usage = page.getByRole('region', { name: /What links here/ });
  await expect(usage.getByRole('heading', { name: /Instances/ })).toBeVisible();
  await expect(usage.getByRole('link', { name: 'Ada Lovelace' })).toBeVisible();
  await expect(usage.getByRole('heading', { name: /Range of/ })).toBeVisible();

  const facts = page.getByRole('region', { name: 'Metadata' });
  await expect(facts.getByRole('heading', { name: 'Metadata' })).toBeVisible();
  await expect(facts).toContainText('Class');
  // the counts under the header carry the hierarchy, so the card does not
  await expect(facts).not.toContainText('1 parent');
  const counts = page.getByRole('navigation', { name: 'Counts' });
  await expect(counts.getByRole('link', { name: '1 parent' })).toHaveAttribute('href', '#tree-h');
  await expect(counts.getByRole('link', { name: '0 subclasses' })).toBeVisible();
  const download = page.getByRole('region', { name: 'Download' });
  await expect(download.getByRole('link', { name: 'Turtle' })).toBeVisible();

  await logic.getByRole('link', { name: 'Employee' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Employee' })).toBeVisible();
});

test('a definition leads the page and a mention names its restriction', async ({ page }) => {
  await openTerm(page, 'Person');
  await expect(page.locator('.lede')).toContainText('A human being');
  // the lead block lists each kind under its heading, and folds French under Definition
  const lead = page.locator('.lead');
  await expect(lead.locator('[data-kind] > h3')).toHaveText([
    /^Definition\s*·\s*skos:definition$/,
    /^Comment\s*·\s*rdfs:comment$/,
    /^Scope note\s*·\s*skos:scopeNote$/,
  ]);
  const comments = lead.getByRole('group', { name: 'Comment' });
  await expect(comments.locator(':scope > .entry')).toHaveCount(2);
  const def = lead.getByRole('group', { name: 'Definition' });
  const fr = def.locator('details.fold');
  await expect(fr.locator('summary')).toHaveText('In 1 other language');
  await expect(fr.locator('.entry')).toBeHidden();
  await fr.locator('summary').click();
  await expect(fr.locator('.entry')).toContainText('Un être humain');
  await expect(fr.locator('.lang-tag')).toHaveText('fr');
  // the notes keep their own kinds below
  for (const k of ['Example', 'History note', 'Editorial note'])
    await expect(page.getByRole('region', { name: k })).toBeVisible();
  await expect(page.getByRole('region', { name: 'History note' }).locator('h2')).toContainText(
    'skos:historyNote',
  );
  await expect(page.getByRole('region', { name: 'Comment' })).toHaveCount(0);
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
  await expect(page.getByRole('region', { name: 'Axioms' })).toBeVisible();
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

test('a defined class keeps its logical definition, and a class with no axioms has no card', async ({
  page,
  context,
}) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await openTerm(page, 'Teacher');
  const def = page.getByRole('region', { name: 'Logical definition' });
  await expect(def.getByRole('link', { name: 'Person' })).toBeVisible();
  await expect(def.locator('.kw', { hasText: 'and' })).toBeVisible();
  const axioms = page.getByRole('region', { name: 'Axioms' });
  await expect(axioms.locator('[data-section="equivalent"] dt')).toHaveText('EquivalentTo:');
  await axioms.getByRole('button', { name: 'Copy' }).click();
  const text = await page.evaluate(() => navigator.clipboard.readText());
  expect(text).toContain('Class: Teacher');
  expect(text).toContain('    EquivalentTo: ');
  expect(text).toContain('Person and (teaches some Course)');
  await axioms.getByRole('button', { name: 'Classify' }).click();
  await expect(axioms).toContainText('infers nothing more');

  // a classification run in this tab is asked again on the next class page
  await openTerm(page, 'Professor');
  await expect(
    page
      .getByRole('region', { name: 'Axioms' })
      .locator('[data-section="inferred-sub"]')
      .getByRole('link', { name: 'Teacher' }),
  ).toBeVisible();

  // Agent states no axiom of its own and the classification infers nothing more
  await openTerm(page, 'Agent');
  await expect(page.getByRole('heading', { level: 1, name: 'Agent' })).toBeVisible();
  await expect(page.locator('.lede')).toHaveText('No definition in this ontology.');
  await expect(page.getByRole('region', { name: 'Axioms' })).toHaveCount(0);
});

test('no section or card shows a heading without content', async ({ page }) => {
  const pages = [
    termUrl('Professor'),
    termUrl('Teacher'),
    termUrl('Agent'),
    termUrl('teaches'),
    termUrl('ada'),
    `/ui/browse?ds=thes&iri=${encodeURIComponent('http://example.org/thesaurus/fish')}`,
  ];
  for (const url of pages) {
    await page.goto(url);
    await expect(page.locator('h1.term-title')).toBeVisible({ timeout: 30_000 });
    await page.waitForLoadState('networkidle');
    const empty = await page.evaluate(() => {
      const out: string[] = [];
      const text = (el: Element | null) => (el as HTMLElement | null)?.innerText.trim() ?? '';
      for (const sec of document.querySelectorAll('.browse section.term-sec, .browse .panel')) {
        const head = sec.querySelector(':scope > h2, :scope > .panel-head');
        if (!head || !(sec as HTMLElement).offsetParent) continue;
        if (text(sec).length <= text(head).length) out.push(text(head));
      }
      return out;
    });
    expect(empty, url).toEqual([]);
  }
  // a concept's raw statements never show as axioms or a logical definition
  await expect(page.getByRole('region', { name: 'Axioms' })).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'Logical definition' })).toHaveCount(0);
});

test('a rich OBO term keeps its paragraphs, sources and synonym types', async ({ page }) => {
  await page.goto(
    `/ui/browse?ds=cells&iri=${encodeURIComponent('http://purl.obolibrary.org/obo/CL_0000129')}`,
  );
  await expect(page.getByRole('heading', { level: 1, name: 'microglial cell' })).toBeVisible({
    timeout: 30_000,
  });
  const lead = page.locator('.lead');
  await expect(lead.locator('[data-kind] > h3')).toHaveText([
    /^Definition\s*·\s*obo:IAO_0000115$/,
    /^Description\s*·\s*dcterms:description$/,
    /^Comment\s*·\s*rdfs:comment$/,
  ]);
  // a predicate the ontology names links to its page, with its full IRI on hover
  const pred = lead.locator('[data-kind] > h3 a').first();
  await expect(pred).toHaveAttribute('title', 'http://purl.obolibrary.org/obo/IAO_0000115');
  await expect(pred).toHaveAttribute('href', /IAO_0000115/);
  await expect(lead.locator('.lede')).toContainText('GOC:fixture');
  // the description's blank lines are paragraphs and its newlines line breaks
  const desc = lead.getByRole('group', { name: 'Description' });
  await expect(desc.locator('.para-gap')).toHaveCount(4);
  await expect(desc.locator('br')).toHaveCount(1);
  const syn = page.getByRole('region', { name: 'Synonyms' });
  await expect(syn).toContainText('brain-resident macrophage');
  await expect(syn).toContainText('PMID:00000002');
  // a typed synonym names its type, by label when it has one
  await expect(syn.locator('.syn-type')).toHaveText(['ABBREVIATION', 'plural']);
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
    page.getByRole('region', { name: 'Axioms' }).locator('[data-section="types"] dt'),
  ).toHaveText('Types:');
  // Edit switches the page into edit mode (item 630), whose bar links the full editor
  await pageAction(page, 'Edit');
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

test('the phone page keeps the order head, counts, definition, metadata, hierarchy', async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openTerm(page, 'Person');
  const y = async (sel: string) => (await page.locator(sel).first().boundingBox())!.y;
  const order = ['.b-head', '.b-chips', '.b-def', '.b-info', '.b-tree', '.b-refs', '.b-dl'];
  const ys = [];
  for (const sel of order) ys.push(await y(sel));
  for (let i = 1; i < ys.length; i++) expect(ys[i - 1]).toBeLessThan(ys[i]);
  // the counts stay compact: at most two lines of chips
  const chips = (await page.locator('.b-chips').boundingBox())!;
  expect(chips.height).toBeLessThanOrEqual(2 * 32 + 8);
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

test('a large screen shows the term in two columns of cards whose bottoms stay close', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openTerm(page, 'Person');
  const box = async (sel: string) => (await page.locator(sel).first().boundingBox())!;
  const def = await box('.b-def');
  const info = await box('.b-info');
  expect(def.x).toBeLessThan(info.x);
  expect(Math.abs(def.y - info.y)).toBeLessThan(2);
  // the side column is sized to its cards, narrower than the term's own
  expect(info.width).toBeLessThan(def.width);
  expect(info.width).toBeLessThanOrEqual(400);
  // the bottoms of the two columns stay within a card of each other
  const bottom = (sel: string) =>
    page
      .locator(sel)
      .evaluate((c) => (c.lastElementChild as HTMLElement).getBoundingClientRect().bottom);
  await expect
    .poll(async () => Math.abs((await bottom('.b-main')) - (await bottom('.b-side'))))
    .toBeLessThan(400);
  // the cards keep their reading order: the term's own, then the side's
  const order = await page.evaluate(() =>
    [...document.querySelectorAll('.b-main > .panel, .b-side > *')].map((e) =>
      e.classList.contains('b-info') ? 'meta' : e.className.match(/b-(def|ax|tree|refs|dl)/)?.[1],
    ),
  );
  // Person states one axiom, so its Axioms card follows the definition
  expect(order.slice(0, 3)).toEqual(['def', 'ax', 'tree']);
  expect(order).toContain('meta');
  // at 1024 the cards are one column
  await page.setViewportSize({ width: 1024, height: 900 });
  await expect.poll(async () => (await box('.b-info')).x).toBeLessThan(def.x + 80);
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

test('the incoming count matches What links here', async ({ page }) => {
  await openTerm(page, 'Professor');
  const usage = page.getByRole('region', { name: /What links here/ });
  const shown = (
    await usage.getByRole('heading', { level: 2 }).locator('.count-badge').innerText()
  ).trim();
  await expect(
    page
      .getByRole('navigation', { name: 'Counts' })
      .getByRole('link', { name: `${shown} incoming` }),
  ).toBeVisible();
});
