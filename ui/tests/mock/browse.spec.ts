// The ontology browser against the mock's `uni` dataset (mock/ontology.mjs): the landing
// page and its search, an entity's page with its sections, the lineage tree with its
// children and siblings, the graph view, the downloads, and the top bar's search opening
// a page. tests/e2e/browse.spec.ts checks the server's answers on a real dataset.

import { expect, test, type Locator, type Page } from '@playwright/test';
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
  // with no definition, the comment leads the page, marked as a comment after its text
  const lede = page.locator('.lede');
  await expect(lede).toContainText('An academic who leads research');
  await expect(lede.locator('.pred')).toHaveText('rdfs:comment');
  await expect(lede.locator('.sr-only')).toHaveText('Comment:');
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
  await expect(counts.getByRole('link', { name: /subclass/ })).toHaveCount(0);
  // Download is the last part of Metadata, not a card of its own
  await expect(facts.getByRole('link', { name: 'Turtle' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Download' })).toHaveCount(0);

  await logic.getByRole('link', { name: 'Employee' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Employee' })).toBeVisible();
});

test('a definition leads the page and a mention names its restriction', async ({ page }) => {
  await openTerm(page, 'Person');
  await expect(page.locator('.lede')).toContainText('A human being');
  // one text leads with its property after it as a chip, and French folds under it
  const lead = page.locator('.lead');
  const chip = lead.locator('.lede .pred');
  await expect(chip).toHaveText('skos:definition');
  await expect(chip).toHaveAttribute('title', 'http://www.w3.org/2004/02/skos/core#definition');
  const more = (word: string) => lead.locator(`details[data-more="${word}"]`);
  const fr = more('translations');
  await expect(fr.locator('summary')).toHaveText('In 1 other language');
  await expect(fr.locator('li')).toBeHidden();
  await fr.locator('summary').click();
  await expect(fr.locator('li')).toContainText('Un être humain');
  await expect(fr.locator('.lang-tag')).toHaveText('fr');
  // each kind has its own disclosure, and a kind with no values has none
  await expect(lead.locator('details[data-more] > summary')).toHaveText([
    'In 1 other language',
    '2 comments',
    '4 notes',
  ]);
  await expect(more('definition')).toHaveCount(0);
  await expect(more('description')).toHaveCount(0);
  // they open on their own
  const notes = more('note');
  await expect(notes.locator('li').first()).toBeHidden();
  await notes.locator('summary').click();
  await expect(notes).toHaveAttribute('open');
  await expect(more('comment')).not.toHaveAttribute('open');
  // the chip tells the note kinds apart, and a screen reader hears each kind
  for (const [k, p] of [
    ['Scope note', 'skos:scopeNote'],
    ['Example', 'skos:example'],
    ['History note', 'skos:historyNote'],
    ['Editorial note', 'skos:editorialNote'],
  ]) {
    const row = notes.locator(`li[data-kind="${k}"]`);
    await expect(row.locator('.pred')).toHaveText(p);
    await expect(row.locator('.sr-only')).toHaveText(`${k}:`);
  }
  await expect(notes.getByRole('heading')).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'History note' })).toHaveCount(0);
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
  await expect(page.locator('.lede')).toHaveCount(0);
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
  // a predicate the ontology names links to its page, with its full IRI on hover
  const pred = lead.locator('.lede a.pred');
  await expect(pred).toHaveAttribute('title', 'http://purl.obolibrary.org/obo/IAO_0000115');
  await expect(pred).toHaveAttribute('href', /IAO_0000115/);
  await expect(lead.locator('.lede')).toContainText('GOC:fixture');
  // the description's blank lines are paragraphs and its newlines line breaks
  await lead.locator('details[data-more="description"] > summary').click();
  const desc = lead.locator('li[data-kind="Description"]');
  await expect(desc.locator('.para-gap')).toHaveCount(4);
  await expect(desc.locator('br')).toHaveCount(1);
  const syn = page.getByRole('region', { name: 'Synonyms' });
  await expect(syn).toContainText('brain-resident macrophage');
  await expect(syn).toContainText('PMID:00000002');
  // a typed synonym names its type, by label when it has one
  await expect(syn.locator('.syn-type')).toHaveText(['ABBREVIATION', 'plural']);
});

test('the labels of a synonym type are read at the state the page shows', async ({ page }) => {
  const asked: string[] = [];
  await page.route('**/cells/sparql**', (route) => {
    const req = route.request();
    if ((req.postData() ?? '').includes('ABBREVIATION')) asked.push(req.url());
    return route.fallback();
  });
  await page.goto(
    `/ui/browse?ds=cells&at=1&iri=${encodeURIComponent('http://purl.obolibrary.org/obo/CL_0000129')}`,
  );
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible({ timeout: 30_000 });
  await expect.poll(() => asked.length, { timeout: 15_000 }).toBeGreaterThan(0);
  expect(asked[0]).toMatch(/[?&]at=1\b/);
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

test('the phone page keeps the order head, counts, texts, metadata, hierarchy, axioms', async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openTerm(page, 'Person');
  const y = async (sel: string) => (await page.locator(sel).first().boundingBox())!.y;
  const inOrder = async (order: string[]) => {
    const ys = [];
    for (const sel of order) ys.push(await y(sel));
    for (let i = 1; i < ys.length; i++) expect(ys[i - 1], order[i]).toBeLessThan(ys[i]);
  };
  // Metadata follows the definition block directly, then Hierarchy, then Axioms
  const links = '.b-main > .term-sec';
  await inOrder(['.b-head', '.b-chips', '.b-def', '.b-info', '.b-tree', '.b-ax', links]);
  // the counts stay compact: at most two lines of text links
  const chips = (await page.locator('.b-chips').boundingBox())!;
  expect(chips.height).toBeLessThanOrEqual(2 * 22 + 2);
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
  // a term with no Axioms card keeps the same order without it
  await openTerm(page, 'Agent');
  await expect(page.locator('.b-ax')).toHaveCount(0);
  await inOrder(['.b-head', '.b-chips', '.b-info', '.b-tree', links]);
  // the width above a phone keeps Axioms before Metadata
  await page.setViewportSize({ width: 900, height: 900 });
  await openTerm(page, 'Person');
  await inOrder(['.b-def', '.b-ax', '.b-info', '.b-tree', links]);
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
  // the term's own cards, and the rail of facts in its order
  const cards = (col: string) =>
    page.evaluate(
      (c) =>
        [...document.querySelectorAll(`${c} > *`)]
          .filter((e) => (e as HTMLElement).offsetParent)
          .map((e) =>
            e.className
              .match(/b-(def|ax|tree|info|ids)|term-(sec)/)
              ?.slice(1)
              .join(''),
          ),
      col,
    );
  // Person states one axiom, so Axioms follows the texts, and then the links' own sections
  const main = await cards('.b-main');
  expect(main.slice(0, 2)).toEqual(['def', 'ax']);
  expect(main.length).toBeGreaterThan(2);
  expect(new Set(main.slice(2))).toEqual(new Set(['sec']));
  expect(await cards('.b-side')).toEqual(['info', 'tree']);
  // no card of the term's page moves between the columns
  await expect(page.locator('.browse [data-movable]')).toHaveCount(0);
  // at 1024 the cards are one column
  await page.setViewportSize({ width: 1024, height: 900 });
  await expect.poll(async () => (await box('.b-info')).x).toBeLessThan(def.x + 80);
});

const HEAVY = `/ui/browse?ds=cells&iri=${encodeURIComponent('http://purl.obolibrary.org/obo/CL_0009001')}`;
const OIO_XREF = 'http://www.geneontology.org/formats/oboInOwl#hasDbXref';

/** A term whose cross-references are `n` databases of one id each, and PMID with two. */
async function withXrefs(page: Page, n: number) {
  await page.route(/\/\$\/ontology\/cells\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    t.statements = t.statements.filter((s: { property: string }) => s.property !== OIO_XREF);
    const values = [
      ...Array.from({ length: n }, (_, i) => `DB${String(i).padStart(2, '0')}:${i}`),
      'PMID:1',
      'PMID:2',
    ];
    for (const v of values)
      t.statements.push({
        property: OIO_XREF,
        value: { '@value': v },
        graph: 'default',
        annotations: [],
      });
    await route.fulfill({ response: res, json: t });
  });
}

test('Identifiers groups the cross-references by database in the rail', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(HEAVY);
  const facts = page.locator('.b-side').getByRole('region', { name: 'Metadata' });
  const ids = facts.getByRole('region', { name: 'Identifiers' });
  await expect(ids.locator('dt')).toHaveText(
    ['Other', 'FMA', 'MESH', 'NCIT', 'PMID', 'Wikipedia'],
    {
      timeout: 30_000,
    },
  );
  // a database's ids share a row, and a well-known database links, with its id exact
  const pmid = ids.locator('dd').nth(4);
  const first = pmid.getByRole('link');
  await expect(first).toHaveText(['00000011', '00000012']);
  await expect(first.first()).toHaveAttribute('title', 'PMID:00000011');
  // a name that reads better than its reference, with the reference kept
  const wiki = ids.getByRole('link', { name: 'Border-associated macrophage' });
  await expect(wiki).toHaveAttribute(
    'href',
    'https://en.wikipedia.org/wiki/Border-associated_macrophage',
  );
  await expect(wiki).toHaveAttribute('title', 'Wikipedia:Border-associated_macrophage');
  // an IRI this dataset knows says its label, and an outside one its readable last part
  const known = ids.getByRole('link', { name: 'antigen-presenting border cell' });
  await expect(known).toHaveAttribute('href', 'http://purl.obolibrary.org/obo/CL_0009006');
  await expect(known).toHaveAttribute('title', 'http://purl.obolibrary.org/obo/CL_0009006');
  const outside = ids.getByRole('link', { name: 'Foam cell' });
  await expect(outside).toHaveAttribute('href', 'https://en.wikipedia.org/wiki/Foam_cell');
  await expect(ids.getByRole('button', { name: /more/ })).toHaveCount(0);
  // the identifiers left See also, and the rail keeps its order
  const also = page.getByRole('region', { name: 'See also' });
  await expect(also).not.toContainText('Border-associated_macrophage');
  await expect(also.getByRole('heading', { name: 'See also' })).toBeVisible();
  // in the rail: the facts, the identifiers, the downloads and then the hierarchy
  const ys: number[] = [];
  for (const el of [
    facts.locator('dl').first(),
    ids,
    facts.locator('[data-format]').first(),
    page.locator('.b-tree'),
  ])
    ys.push((await el.boundingBox())!.y);
  expect(ys).toEqual([...ys].sort((x, y) => x - y));
});

test('Identifiers shows the first eight databases and the rest on request', async ({ page }) => {
  await withXrefs(page, 11);
  await page.goto(HEAVY);
  const ids = page.getByRole('region', { name: 'Identifiers' });
  // eleven databases and PMID are twelve rows
  await expect(ids.locator('dt')).toHaveCount(8, { timeout: 30_000 });
  await ids.getByRole('button', { name: 'and 4 more' }).click();
  await expect(ids.locator('dt')).toHaveCount(12);
  await expect(ids.getByRole('button', { name: /more/ })).toHaveCount(0);
});

test('Identifiers shows a ninth database rather than offering one more', async ({ page }) => {
  await withXrefs(page, 8);
  await page.goto(HEAVY);
  const ids = page.getByRole('region', { name: 'Identifiers' });
  await expect(ids.locator('dt')).toHaveCount(9, { timeout: 30_000 });
  await expect(ids.getByRole('button', { name: /more/ })).toHaveCount(0);
});

test('the main column has one size of heading and skips no level', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const levels = () =>
    page.evaluate(() =>
      [...document.querySelectorAll('.browse h1, .browse h2, .browse h3, .browse h4')]
        .filter((h) => (h as HTMLElement).offsetParent)
        .map((h) => Number(h.tagName[1])),
    );
  await page.goto(HEAVY);
  await expect(page.getByRole('heading', { level: 2, name: 'See also' })).toBeVisible({
    timeout: 30_000,
  });
  const size = (h: Locator) => h.evaluate((el) => getComputedStyle(el).fontSize);
  for (const name of [
    'Logical definition',
    'Synonyms',
    'Axioms',
    'See also',
    /What links here/,
    'Metadata',
    'Hierarchy',
  ]) {
    const h = page.locator('.browse').getByRole('heading', { level: 2, name });
    expect(await size(h.first()), String(name)).toBe('17px');
  }
  // no heading stands over the links, and the groups of What links here stay smaller
  await expect(page.getByRole('heading', { name: 'References' })).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'References' })).toHaveCount(0);
  const usage = page.getByRole('region', { name: /What links here/ });
  const group = usage.getByRole('heading', { level: 3 }).first();
  expect(parseFloat(await size(group))).toBeLessThan(17);
  for (const name of ['See also', /What links here/])
    await expect(page.locator('.b-main > section').getByRole('heading', { name })).toBeVisible();
  const ok = (l: number[]) => l.every((n, i) => i === 0 || n <= l[i - 1] + 1);
  expect(ok(await levels()), (await levels()).join(' ')).toBe(true);
  // on a concept Labels is at the same level, with the same check in edit mode
  await page.goto(
    `/ui/browse?ds=thes&iri=${encodeURIComponent('http://example.org/thesaurus/fish')}`,
  );
  const labels = page.getByRole('heading', { level: 2, name: 'Labels' });
  await expect(labels).toBeVisible({ timeout: 30_000 });
  expect(await size(labels)).toBe('17px');
  expect(ok(await levels()), (await levels()).join(' ')).toBe(true);
  await page.goto(`${HEAVY}&edit=1`);
  await expect(page.getByRole('heading', { level: 2, name: 'Axioms' })).toBeVisible({
    timeout: 30_000,
  });
  expect(ok(await levels()), (await levels()).join(' ')).toBe(true);
});

test('In other datasets says what it found, and is absent with no other dataset', async ({
  page,
}) => {
  await page.route(/\/\$\/datasets(\?|$)/, async (route) => {
    const res = await route.fetch();
    const j = await res.json();
    const key = Array.isArray(j) ? null : Object.keys(j).find((k) => Array.isArray(j[k]));
    const keep = (l: { name?: string; 'ds.name'?: string }[]) =>
      l.filter((d) => (d.name ?? d['ds.name'] ?? '').replace(/^\//, '') === 'uni');
    await route.fulfill({
      response: res,
      json: key ? { ...j, [key]: keep(j[key]) } : keep(j),
    });
  });
  await openTerm(page, 'Person');
  await expect(page.getByRole('region', { name: /What links here/ })).toBeVisible();
  await page.waitForLoadState('networkidle');
  await expect(page.getByRole('region', { name: /In other datasets/ })).toHaveCount(0);
  await expect(page.getByText(/no other dataset/)).toHaveCount(0);
  await page.unrouteAll();
  await page.reload();
  const reuse = page.getByRole('region', { name: /In other datasets/ });
  await expect(reuse).toBeVisible({ timeout: 30_000 });
  await expect(reuse).toContainText(/Checked|default graph/);
});

test('one text leads, and each other kind waits behind a disclosure with its count', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(HEAVY);
  const lead = page.locator('.lead');
  await expect(lead.locator('.lede')).toContainText('A macrophage that sits in the meninges', {
    timeout: 30_000,
  });
  const summaries = lead.locator('details[data-more] > summary');
  await expect(summaries).toHaveText([
    'In 1 other language',
    '1 more definition',
    '1 description',
    '3 comments',
    '6 notes',
  ]);
  await expect(lead.locator('details[data-more][open]')).toHaveCount(0);
  // they share one quiet line, and stay where they are when one opens
  const line = async () =>
    (await Promise.all((await summaries.all()).map((s) => s.boundingBox())))!;
  const before = await line();
  expect(new Set(before.map((b) => b!.y)).size).toBe(1);
  await summaries.nth(1).click();
  await summaries.last().click();
  expect((await line()).map((b) => [b!.x, b!.y])).toEqual(before.map((b) => [b!.x, b!.y]));
  await expect(lead.locator('details[data-more="definition"] li')).toHaveCount(1);
  await expect(lead.locator('details[data-more="description"] li')).toBeHidden();
  await expect(lead.locator('details[data-more="note"] li')).toHaveCount(6);
  await expect(lead.locator('details[data-more="note"] li').first()).toBeVisible();
  // the lead's text is not repeated
  await expect(lead.locator('details[data-more]')).not.toContainText([
    'A macrophage that sits in the meninges',
  ]);
  // no card or heading stands for the block, which is still a named region
  await expect(page.getByRole('heading', { name: 'Definition', exact: true })).toHaveCount(0);
  await expect(page.locator('.b-main > .panel, .b-main > section.panel')).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'About this class' })).toBeVisible();
  // Logical definition and Synonyms stay in view without opening anything
  await expect(page.getByRole('heading', { level: 2, name: 'Logical definition' })).toBeVisible();
  await expect(page.getByRole('heading', { level: 2, name: 'Synonyms' })).toBeVisible();
  // the sections on the page background have no card behind them
  const bg = await page
    .locator('.b-main > .term-sec')
    .first()
    .evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(bg).toBe('rgba(0, 0, 0, 0)');
});

test('a chip stays with the last word of its text at any width', async ({ page }) => {
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(HEAVY);
    await expect(page.locator('.lede .pred')).toBeVisible({ timeout: 30_000 });
    for (const s of await page.locator('details[data-more] > summary').all()) await s.click();
    const orphans = await page.evaluate(() =>
      [...document.querySelectorAll('.lead .pred')].flatMap((chip) => {
        const before = chip.previousElementSibling;
        // a second chip of the same text follows the first
        if (before?.classList.contains('pred')) return [];
        if (!before) return [chip.textContent];
        const range = document.createRange();
        range.selectNodeContents(before);
        const rects = [...range.getClientRects()].filter((r) => r.width > 0);
        const last = rects[rects.length - 1].bottom;
        return chip.getClientRects()[0].top < last ? [] : [chip.textContent];
      }),
    );
    expect(orphans, `chips alone on a line at ${width} px`).toEqual([]);
    await expectNoOverflow(page);
  }
});

test('a lone text has no disclosure, and a term with no text has no block', async ({ page }) => {
  await page.route(/\/\$\/ontology\/uni\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    t.statements = t.statements.filter(
      (s: { property: string; value: { '@language'?: string } }) =>
        !/#(comment|scopeNote|note|example|historyNote|editorialNote)$/.test(s.property) &&
        !(s.property.endsWith('#definition') && s.value['@language'] === 'fr'),
    );
    await route.fulfill({ response: res, json: t });
  });
  await openTerm(page, 'Person');
  await expect(page.locator('.lede')).toContainText('A human being');
  await expect(page.locator('.lead details')).toHaveCount(0);
  await page.unrouteAll();
  await openTerm(page, 'GraduateCourse');
  await expect(page.locator('.lede')).toHaveCount(0);
  await expect(page.getByText(/No definition/)).toHaveCount(0);
  await expect(page.locator('.lead')).toHaveCount(0);
  await expect(page.locator('.b-def')).toBeHidden();
});

test('a schema.org description leads when nothing else does', async ({ page }) => {
  await page.route(/\/\$\/ontology\/uni\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    t.statements = [
      ...t.statements,
      {
        property: 'http://schema.org/description',
        value: { '@value': 'From schema.org.', '@language': 'en' },
        graph: t.statements[0]?.graph ?? '',
        annotations: [],
      },
    ];
    await route.fulfill({ response: res, json: t });
  });
  await openTerm(page, 'GraduateCourse');
  await expect(page.locator('.lede')).toContainText('From schema.org.');
  await expect(page.locator('.lede .pred')).toHaveAttribute(
    'title',
    'http://schema.org/description',
  );
});

test('edit mode opens the other texts', async ({ page }) => {
  await openTerm(page, 'Person', '&edit=1');
  const notes = page.locator('details[data-more="note"]');
  await expect(notes).toHaveAttribute('open');
  await expect(notes.locator('li[data-kind="History note"]')).toBeVisible();
  await expect(page.locator('details[data-more="comment"] li').first()).toBeVisible();
});

test('the header writes the IRI as the switch says and copies it in full', async ({
  page,
  context,
}) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto(HEAVY);
  const full = 'http://purl.obolibrary.org/obo/CL_0009001';
  const iri = page.locator('.term-head .iri-text');
  await expect(iri.locator('[aria-hidden="true"]')).toHaveText('obo:CL_0009001', {
    timeout: 30_000,
  });
  await expect(iri).toHaveAttribute('title', full);
  await expect(iri.locator('.sr-only')).toHaveText(full);
  // the short form would repeat the prefixed name, and the Metadata row keeps the id
  const chip = page.locator('.term-head .sf');
  await expect(chip).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'Metadata' })).toContainText('CL:0009001');
  await page.getByRole('button', { name: 'Copy the IRI' }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(full);
  // who chose full IRIs sees the full form
  await page.getByRole('button', { name: 'Prefixed' }).click();
  await expect(iri).toHaveText(full);
  await expect(iri.locator('[aria-hidden="true"]')).toHaveCount(0);
  await expect(chip).toHaveText('CL_0009001');
  await page.getByRole('button', { name: 'Full IRIs' }).click();
  await expect(iri.locator('[aria-hidden="true"]')).toHaveText('obo:CL_0009001');
  await expect(chip).toHaveCount(0);
  // with no prefix the IRI is whole, and copying gives the same
  const none = 'http://example.org/no-prefix-here/Thing';
  await page.route(/\/\$\/ontology\/cells\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    await route.fulfill({ response: res, json: { ...t, iri: none } });
  });
  await page.reload();
  await expect(iri).toHaveText(none, { timeout: 30_000 });
  await expect(iri).toHaveAttribute('title', none);
  await expect(iri.locator('.sr-only')).toHaveCount(0);
  await expect(chip).toHaveText('CL_0009001');
  await page.getByRole('button', { name: 'Copy the IRI' }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(none);
});

test('typed literals read as their value with a datatype chip, and copy as syntax', async ({
  page,
  context,
}) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(HEAVY);
  const axioms = page.getByRole('region', { name: 'Axioms' });
  const sub = axioms.locator('[data-section="sub"]');
  await expect(sub).toContainText('first seen', { timeout: 30_000 });
  const chip = (scope: Locator, name: string) => scope.locator('.datatype', { hasText: name });
  await expect(chip(sub, 'xsd:date')).toHaveAttribute(
    'title',
    'http://www.w3.org/2001/XMLSchema#date',
  );
  await expect(chip(sub, 'xsd:integer')).toHaveCount(1);
  await expect(chip(sub, 'xsd:date').locator('.sr-only')).toHaveText('datatype');
  await expect(sub).toContainText('2026-10-01');
  // Metadata shows its created date exactly, with its chip
  const facts = page.getByRole('region', { name: 'Metadata' });
  await expect(facts.locator('dd', { hasText: '2026-10-01' }).locator('.datatype')).toHaveText(
    /xsd:date/,
  );
  // the other statements: typed ones have a chip and a string has none
  await page.getByText('All other statements').click();
  const other = page.locator('details.ann');
  await expect(
    other.locator('tr', { hasText: '2026-10-02T09:30:00Z' }).locator('.datatype'),
  ).toHaveText(/xsd:dateTime/);
  await expect(other.locator('tr', { hasText: 'reviewCount' }).locator('.datatype')).toHaveText(
    /xsd:integer/,
  );
  await expect(other.locator('tr', { hasText: 'a plain string' }).locator('.datatype')).toHaveCount(
    0,
  );
  // nothing rendered shows the syntax
  for (const s of await page.locator('details[data-more] > summary').all()) await s.click();
  const shown = await page.locator('.browse').evaluate((el) => {
    const c = el.cloneNode(true) as HTMLElement;
    c.querySelectorAll('.other code').forEach((x) => x.remove());
    return c.textContent ?? '';
  });
  expect(shown).not.toContain('^^');
  // the Copy button's Manchester text stays valid syntax
  await axioms.getByRole('button', { name: /Copy/ }).click();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain('"2026-10-01"^^xsd:date');
  expect(copied).not.toContain('datatype');
});

test('a cross-reference that closes an IRI is plain text and reaches no query', async ({
  page,
}) => {
  const attack =
    'http://example.org/x> } SERVICE <https://attacker.example/sparql> { ?s ?p ?d } } #';
  await page.route(/\/\$\/ontology\/cells\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    t.statements = [
      ...t.statements,
      {
        property: 'http://www.geneontology.org/formats/oboInOwl#hasDbXref',
        value: { '@value': attack },
        graph: t.statements[0]?.graph ?? '',
        annotations: [],
      },
    ];
    await route.fulfill({ response: res, json: t });
  });
  const sent: string[] = [];
  page.on('request', (r) => sent.push(`${r.url()} ${r.postData() ?? ''}`));
  await page.goto(HEAVY);
  const ids = page.getByRole('region', { name: 'Identifiers' });
  await expect(ids.getByText(attack, { exact: true })).toBeVisible({ timeout: 30_000 });
  await expect(ids.getByRole('link', { name: /attacker/ })).toHaveCount(0);
  // the others still get their labels from the one query
  await expect(ids.getByRole('link', { name: 'antigen-presenting border cell' })).toBeVisible();
  await page.waitForLoadState('networkidle');
  expect(
    sent
      .filter((s) => /SERVICE|attacker/.test(decodeURIComponent(s.replace(/\+/g, ' '))))
      .filter((s) => !s.includes('/term?')),
  ).toEqual([]);
});

test('a typed literal keeps its spaces, tabs and newlines as they are', async ({ page }) => {
  const XSD = 'http://www.w3.org/2001/XMLSchema#';
  const value = '  a  b\tc\n\n  d';
  const lexical = { '@value': value, '@type': XSD + 'token' };
  await page.route(/\/\$\/ontology\/cells\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    const graph = t.statements[0]?.graph ?? '';
    const stated = (property: string) => ({ property, value: lexical, graph, annotations: [] });
    t.statements = [
      ...t.statements,
      stated('http://www.w3.org/2000/01/rdf-schema#comment'),
      stated('http://www.w3.org/2000/01/rdf-schema#seeAlso'),
      stated('http://purl.org/dc/terms/created'),
      stated('http://example.org/spaced'),
    ];
    t.axioms = [
      ...t.axioms,
      {
        axiom: {
          type: 'SubClassOf',
          subClass: t.iri,
          superClass: { type: 'DataHasValue', property: 'http://example.org/p', value: lexical },
        },
        ofn: '',
        graph,
      },
    ];
    await route.fulfill({ response: res, json: t });
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(HEAVY);
  await expect(page.getByRole('region', { name: 'Axioms' })).toContainText('a', {
    timeout: 30_000,
  });
  await page.locator('details[data-more="comment"] > summary').click();
  await page.getByText('All other statements').click();
  // the text a reader sees, in each place a typed literal shows
  const seen = (l: Locator) =>
    l.evaluate((chip) => {
      const box = chip.parentElement!.closest('li, dd, td, .prose, .row, p') ?? chip.parentElement!;
      return (box as HTMLElement).innerText;
    });
  const chips = page.locator('.browse .datatype', { hasText: 'xsd:token' });
  await expect(chips).toHaveCount(5);
  for (const chip of await chips.all()) expect(await seen(chip)).toContain(value);
});

test('a concept keeps its hidden labels behind their toggle', async ({ page }) => {
  const concept = (l: string) =>
    `/ui/browse?ds=thes&iri=${encodeURIComponent(`http://example.org/thesaurus/${l}`)}`;
  await page.goto(concept('insects'));
  const labels = page.getByRole('region', { name: 'Labels', exact: true });
  await expect(labels.locator('dt').first()).toHaveText('Preferred', { timeout: 30_000 });
  // off at first, and nothing else on the page says them or counts them
  await expect(labels.locator('dt')).not.toContainText(['Hidden']);
  await expect(labels).toContainText('1 hidden label is not shown.');
  await expect(page.locator('main')).not.toContainText('insekts');
  await expect(
    page.getByRole('navigation', { name: 'Counts' }).getByRole('link', { name: /synonym/ }),
  ).toHaveCount(0);
  // on, a row of its own, and off again
  await labels.getByRole('button', { name: 'Show them' }).click();
  const row = labels.locator('.term-row', { hasText: 'Hidden' });
  await expect(row.locator('dd')).toContainText('insekts');
  await expect(row.locator('.lang-tag')).toHaveText('en');
  await expect(labels).toContainText('Hidden labels are shown.');
  await expect(page.locator('#syn-h')).toHaveCount(0);
  await labels.getByRole('button', { name: 'Hide them' }).click();
  await expect(page.locator('main')).not.toContainText('insekts');
  // a concept with none has no toggle
  await page.goto(concept('fish'));
  await expect(labels.locator('dt').first()).toHaveText('Preferred', { timeout: 30_000 });
  await expect(labels.getByRole('button')).toHaveCount(0);
  await expect(labels).not.toContainText('hidden');
});

test('labels whose language and text run together both show', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(e.message));
  const pref = 'http://www.w3.org/2004/02/skos/core#prefLabel';
  await page.route(/\/\$\/ontology\/thes\/term\?/, async (route) => {
    const res = await route.fetch();
    const t = await res.json();
    const stated = (value: object) => ({
      property: pref,
      value,
      graph: t.statements[0]?.graph ?? '',
      annotations: [],
    });
    t.statements = [
      ...t.statements,
      stated({ '@value': 'enclosure' }),
      stated({ '@value': 'closure', '@language': 'en' }),
      // the same label twice, as two graphs can state it
      stated({ '@value': 'closure', '@language': 'en' }),
    ];
    await route.fulfill({ response: res, json: t });
  });
  await page.goto(
    `/ui/browse?ds=thes&iri=${encodeURIComponent('http://example.org/thesaurus/fish')}`,
  );
  const row = page
    .getByRole('region', { name: 'Labels', exact: true })
    .locator('.term-row', { hasText: 'Preferred' });
  await expect(row).toContainText('enclosure', { timeout: 30_000 });
  await expect(row.locator('[data-lang="en"]', { hasText: /^closure/ })).toHaveCount(1);
  expect(errors).toEqual([]);
});

test('a concept lists its labels by kind, in every language', async ({ page }) => {
  await page.goto(
    `/ui/browse?ds=thes&iri=${encodeURIComponent('http://example.org/thesaurus/fish')}`,
  );
  const labels = page.getByRole('region', { name: 'Labels', exact: true });
  await expect(labels.getByRole('table')).toHaveCount(0, { timeout: 30_000 });
  const row = labels.locator('.term-row', { hasText: 'Preferred' });
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(row.locator('dd [data-lang="en"]')).toContainText('Fish');
  await expect(row.locator('dd [data-lang="fr"] .lang-tag')).toHaveText('fr');
  // the alternative and hidden labels are rows here, not synonyms as well
  await expect(page.locator('#syn-h')).toHaveCount(0);
});

test('the counts line goes when every count is zero', async ({ page }) => {
  // an individual has no hierarchy, and this one nothing links to
  await openTerm(page, 'alan');
  await expect(page.getByRole('heading', { level: 1, name: 'Alan Turing' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Metadata' })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Counts' })).toHaveCount(0);
  await expect(page.locator('.b-chips')).toHaveCount(0);
  await expect(page.getByRole('region', { name: /What links here/ })).toHaveCount(0);
});

test('the counts are one line of text links', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(HEAVY);
  const counts = page.getByRole('navigation', { name: 'Counts' });
  await expect(counts.getByRole('link', { name: '3 subclasses' })).toBeVisible({ timeout: 30_000 });
  const box = (await counts.boundingBox())!;
  expect(box.height).toBeLessThanOrEqual(22);
  const chip = counts.getByRole('link', { name: '3 subclasses' });
  expect(await chip.evaluate((e) => getComputedStyle(e).borderTopStyle)).toBe('none');
  // the pills stay on the pages that keep them
  await page.goto('/ui/datasets/uni');
  const pill = page.locator('.stat-chip').first();
  await expect(pill).toBeVisible({ timeout: 30_000 });
  expect(await pill.evaluate((e) => getComputedStyle(e).borderTopStyle)).toBe('solid');
});

test('the rail sticks, and one taller than the window stays by its bottom edge', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 500 });
  await page.goto(HEAVY);
  const side = page.locator('.b-side');
  await expect(side).toHaveAttribute('data-stick', '', { timeout: 30_000 });
  const measure = () =>
    page.evaluate(() => {
      const s = document.querySelector<HTMLElement>('.b-side')!;
      const m = document.querySelector('main')!;
      return {
        top: parseFloat(getComputedStyle(s).top),
        box: m.clientHeight,
        height: s.getBoundingClientRect().height,
        bottom: s.getBoundingClientRect().bottom - m.getBoundingClientRect().top,
      };
    });
  await expect.poll(async () => (await measure()).top).toBeLessThan(0);
  const a = await measure();
  expect(a.height).toBeGreaterThan(a.box);
  expect(a.top).toBeCloseTo(a.box - a.height - 24, 0);
  // far down the page, short of the column's end, the rail's bottom edge rests a gutter above the
  // window's bottom, with the other texts open so that the main column outruns the rail
  for (const s of await page.locator('details[data-more] > summary').all()) await s.click();
  await page.evaluate(() => {
    const m = document.querySelector('main')!;
    m.scrollTo(0, m.scrollHeight - m.clientHeight - 100);
  });
  await expect.poll(async () => Math.abs((await measure()).bottom - (a.box - 24))).toBeLessThan(2);
  // a window tall enough for the rail holds it by its top
  await page.setViewportSize({ width: 1440, height: 1400 });
  await expect.poll(async () => (await measure()).top).toBe(24);
});

test('the hierarchy stays in the rail across Tree and Graph', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(HEAVY);
  const rail = page.locator('.b-side');
  const card = rail.getByRole('complementary', { name: 'Hierarchy' });
  await expect(card.locator('.tree-body')).toBeVisible({ timeout: 30_000 });
  await expect(card).not.toHaveAttribute('data-movable');
  const before = (await card.boundingBox())!;
  await card.getByRole('link', { name: 'Graph', exact: true }).click();
  await expect(page).toHaveURL(/view=graph/);
  await expect(card.locator('.canvas canvas').first()).toBeVisible({ timeout: 20_000 });
  const graph = (await card.boundingBox())!;
  expect(graph.x).toBe(before.x);
  expect(graph.y).toBe(before.y);
  expect(graph.width).toBe(before.width);
  // the main column keeps its cards and the rail its height for a moment after the layout
  await page.waitForTimeout(500);
  await expect(page.locator('.b-side > .b-tree')).toHaveCount(1);
  await card.getByRole('link', { name: 'Tree', exact: true }).click();
  await expect(card.locator('.tree-body')).toBeVisible();
  expect((await card.boundingBox())!.y).toBe(before.y);
});

test('the rail graph shows the parents and first children, and Expand opens the whole graph', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`${HEAVY}&view=graph`);
  const card = page.getByRole('complementary', { name: 'Hierarchy' });
  const canvas = card.locator('.canvas');
  await expect(canvas.locator('canvas').first()).toBeVisible({ timeout: 30_000 });
  type Cy = { nodes: () => { map: (f: (n: { id: () => string }) => string) => string[] } };
  const local = (host: import('@playwright/test').Locator) =>
    expect.poll(
      () =>
        host.evaluate((el) => {
          const w = [...el.querySelectorAll('*')].find((e) => '__cy' in e) as
            | (Element & { __cy: Cy })
            | undefined;
          return (w?.__cy.nodes().map((n) => n.id().split('/').pop()!) ?? []).sort();
        }),
      { timeout: 20_000 },
    );
  await local(canvas).toEqual([
    'CL_0000235',
    'CL_0009001',
    'CL_0009002',
    'CL_0009003',
    'CL_0009004',
  ]);
  // the rail's tools are Fit and Expand, and Explore waits in the dialog
  await expect(canvas.getByRole('link', { name: 'Open in Explore' })).toHaveCount(0);
  await canvas.getByRole('button', { name: 'Expand' }).click();
  const dialog = page.getByRole('dialog', { name: /Graph of border-associated macrophage/ });
  await expect(dialog.locator('canvas').first()).toBeVisible();
  await expect(dialog.getByRole('link', { name: 'Open in Explore' })).toBeVisible();
  // the whole neighbourhood: the axioms' fillers and the entities that mention this one
  await local(dialog).toEqual(
    expect.arrayContaining(['UBERON_0002360', 'GO_0006909', 'CL_0009006', 'CL_0009002']),
  );
  const box = (await dialog.boundingBox())!;
  expect(box.width).toBeGreaterThan(900);
  await dialog.getByRole('button', { name: 'Close' }).click();
  await expect(dialog).toBeHidden();
  await expect(canvas.locator('canvas').first()).toBeVisible();
});

test('a phone opens the graph at its width, and Expand takes the whole screen width', async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(HEAVY);
  const card = page.getByRole('complementary', { name: 'Hierarchy' });
  await expect(card.locator('.tree-body')).toBeHidden({ timeout: 30_000 });
  // choosing a view opens the card
  await card.getByRole('link', { name: 'Graph', exact: true }).click();
  await expect(page).toHaveURL(/view=graph/);
  await expect(card.locator('.canvas canvas').first()).toBeVisible({ timeout: 20_000 });
  await expectNoOverflow(page);
  const c = (await card.boundingBox())!;
  expect(c.width).toBeLessThanOrEqual(390);
  await card.getByRole('button', { name: 'Expand' }).click();
  const dialog = page.getByRole('dialog', { name: /Graph of/ });
  await expect(dialog.locator('canvas').first()).toBeVisible();
  const d = (await dialog.boundingBox())!;
  expect(d.x).toBeGreaterThanOrEqual(0);
  expect(d.x + d.width).toBeLessThanOrEqual(390);
  await dialog.getByRole('button', { name: 'Close' }).click();
  // an address that asks for the graph shows it open
  await page.goto(`${HEAVY}&view=graph`);
  await expect(card.locator('.canvas canvas').first()).toBeVisible({ timeout: 30_000 });
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
