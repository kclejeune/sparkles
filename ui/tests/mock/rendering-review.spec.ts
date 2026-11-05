import { expect, test, type Locator, type Page } from '@playwright/test';

const UNI = 'http://example.org/uni#';
const TH = 'http://example.org/thesaurus/';
async function preferences(page: Page, mode: 'label' | 'prefixed' | 'iri', languages = 'fr, en') {
  await page.getByRole('button', { name: 'Rendering preferences', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Rendering preferences' });
  await dialog.getByRole('combobox', { name: 'Show entities by' }).selectOption(mode);
  await dialog.getByRole('textbox', { name: 'Language order' }).fill(languages);
  await dialog.getByRole('button', { name: 'Apply', exact: true }).click();
}

test('changing language keeps both unsubmitted Browse drafts', async ({ page }) => {
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Student')}&edit=1`);
  const synonym = page.getByRole('textbox', { name: 'New synonym', exact: true });
  await synonym.fill('unsubmitted pupil');
  const axiom = page.locator('#browse-add-axiom .cm-content');
  await axiom.fill('teaches some Course');
  await preferences(page, 'label');
  await expect(page.locator('h1.term-title')).toHaveText('Étudiant');
  await expect(synonym).toHaveValue('unsubmitted pupil');
  await expect(axiom).toHaveText('teaches some Course');
});

test('the unselected Individuals tree retains its complete labels across languages', async ({
  page,
}) => {
  await page.goto('/ui/workbench/ontology?ds=uni');
  await page.getByRole('combobox', { name: 'Entity kind' }).selectOption('NamedIndividual');
  const tree = page.getByRole('tree', { name: 'Individuals (asserted)' });
  await expect(tree.getByRole('button', { name: 'Ada Lovelace', exact: true })).toBeVisible();
  await preferences(page, 'label');
  await expect(tree.getByRole('button', { name: 'Ada Lovelace', exact: true })).toBeVisible();
  expect(new URL(page.url()).searchParams.has('iri')).toBe(false);
  await page.getByRole('combobox', { name: 'Entity kind' }).selectOption('Class');
  await expect(page.getByRole('tree', { name: 'Classes (asserted)' })).toContainText('Agent');
});

test('a failed language refresh also leaves the Browse draft mounted', async ({ page }) => {
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Student')}&edit=1`);
  const synonym = page.getByRole('textbox', { name: 'New synonym', exact: true });
  await synonym.fill('keep this draft');
  await page.route('**/$/ontology/uni/term?**', (route) =>
    route.fulfill({
      status: 503,
      json: { error: 'test refresh failure' },
    }),
  );
  await preferences(page, 'label');
  await expect(
    page.locator('.toast').filter({ hasText: 'Could not refresh the entity names' }),
  ).toBeVisible();
  await expect(synonym).toHaveValue('keep this draft');
});

type Cy = {
  nodes: () => {
    map: (fn: (n: { id: () => string; data: (key: string) => string }) => string) => string[];
  };
  getElementById: (id: string) => { emit: (event: string) => void };
};
const captions = (host: Locator) =>
  host.evaluate((el) => {
    const wrap = [...el.querySelectorAll('*')].find((e) => '__cy' in e) as
      | (Element & { __cy: Cy })
      | undefined;
    return wrap?.__cy.nodes().map((n) => n.data('label')) ?? [];
  });
async function expand(host: Locator, id: string) {
  await host.evaluate((el, iri) => {
    const wrap = [...el.querySelectorAll('*')].find((e) => '__cy' in e) as Element & { __cy: Cy };
    wrap.__cy.getElementById(iri).emit('dbltap');
  }, id);
}

test('expanded graph nodes retain their names and respond to display mode changes', async ({
  page,
}) => {
  await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Person')}&view=graph`);
  await preferences(page, 'label', 'en');
  const card = page.getByRole('complementary', { name: 'Hierarchy' });
  await expect.poll(() => captions(card)).toContain('Student');
  await card.getByRole('button', { name: 'Expand', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Graph of Person', exact: true });
  await expect.poll(() => captions(dialog)).toContain('Student');
  await expand(dialog, UNI + 'Student');
  await expect.poll(() => captions(dialog)).toContain('Graduate student');
  await dialog.getByRole('button', { name: 'Close', exact: true }).click();
  await preferences(page, 'prefixed', 'en');
  await card.getByRole('button', { name: 'Expand', exact: true }).click();
  const prefixed = page.getByRole('dialog', { name: /Graph of/ });
  await expect(prefixed).toHaveAccessibleName('Graph of uni:Person');
  await expect.poll(() => captions(prefixed)).toContain('uni:GraduateStudent');
  await prefixed.getByRole('button', { name: 'Close', exact: true }).click();
  await preferences(page, 'label', 'en');
  await card.getByRole('button', { name: 'Expand', exact: true }).click();
  await expect
    .poll(() => captions(page.getByRole('dialog', { name: /Graph of/ })))
    .toContain('Graduate student');
});

test('an open scheme picker reacts to preferences without losing its selection', async ({
  page,
}) => {
  await page.goto(`/ui/workbench/thesaurus?ds=thes&iri=${encodeURIComponent(TH + 'whales')}`);
  await page.getByRole('button', { name: 'Relations', exact: true }).click();
  await page.getByRole('button', { name: 'Add close match', exact: true }).click();
  const schemes = page
    .getByRole('group', { name: 'Close match picker' })
    .getByRole('combobox', { name: 'Scheme' });
  const animals = schemes.locator(`option[value="${TH}animals"]`);
  await expect(animals).toHaveText('Animals');
  await schemes.selectOption(TH + 'animals');
  await preferences(page, 'label');
  await expect(animals).toHaveText('Animaux');
  await expect(schemes).toHaveValue(TH + 'animals');
  await preferences(page, 'prefixed');
  await expect(animals).toHaveText('th:animals');
  await preferences(page, 'iri');
  await expect(animals).toHaveText(TH + 'animals');
  await expect(schemes).toHaveValue(TH + 'animals');
});

test('an expansion completing after navigation cannot change the new entity graph', async ({
  page,
}) => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let started!: () => void;
  const reading = new Promise<void>((resolve) => {
    started = resolve;
  });
  await page.route('**/$/ontology/uni/term?**', async (route) => {
    if (new URL(route.request().url()).searchParams.get('iri') !== UNI + 'Student')
      return route.continue();
    const response = await route.fetch();
    started();
    await held;
    await route.fulfill({ response });
  });
  try {
    await page.goto(`/ui/browse?ds=uni&iri=${encodeURIComponent(UNI + 'Person')}&view=graph`);
    const card = page.getByRole('complementary', { name: 'Hierarchy' });
    await expect.poll(() => captions(card)).toContain('Student');
    await card.getByRole('button', { name: 'Expand', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: /Graph of/ });
    await expect.poll(() => captions(dialog)).toContain('Student');
    await expand(dialog, UNI + 'Student');
    await reading;
    await dialog.getByRole('button', { name: 'Close', exact: true }).click();
    // Use client-side navigation so the pending expansion remains alive.
    await page
      .locator('a')
      .filter({ hasText: /^Agent$/ })
      .first()
      .click();
    await expect(page.locator('h1.term-title')).toHaveText('Agent');
    await card.getByRole('link', { name: 'Graph', exact: true }).click();
    await expect.poll(() => captions(card)).toContain('Organization');
    const delivered = page.waitForResponse(
      (r) => new URL(r.url()).searchParams.get('iri') === UNI + 'Student',
    );
    release();
    await delivered;
    // A render frame after the response makes a stale merge observable.
    await page.evaluate(() => new Promise(requestAnimationFrame));
    await card.getByRole('button', { name: 'Expand', exact: true }).click();
    const nextGraph = page.getByRole('dialog', { name: /Graph of Agent/ });
    await expect.poll(() => captions(nextGraph)).toContain('Organization');
    await expect.poll(() => captions(nextGraph)).not.toContain('uni:GraduateStudent');
    await expect.poll(() => captions(nextGraph)).not.toContain('Graduate student');
  } finally {
    release();
  }
});
