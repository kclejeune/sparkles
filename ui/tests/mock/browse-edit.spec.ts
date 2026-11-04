// Edit mode on the ontology browser's pages (ROADMAP item 630) against the mock's `uni`
// and `thes` datasets: the toggle and its access checks, a protected main that asks for a
// session branch, each kind of edit previewed and committed (definition, synonym,
// annotation, parent, axiom, SKOS label and broader concept), undo and redo, a refused
// write, the keyboard and the palette commands, and the phone. Edits run on session
// branches that each test deletes, so the other suites see the mock's data unchanged.

import { expect, test, type Page, type APIRequestContext } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ mode: 'serial', timeout: 120_000 });
test.use({ actionTimeout: 15_000 });

const UNI = 'http://example.org/uni#';
const TH = 'http://example.org/thesaurus/';
const term = (ds: string, iri: string, extra = '') =>
  `/ui/browse?ds=${ds}&iri=${encodeURIComponent(iri)}${extra}`;
const bar = (page: Page) => page.getByRole('toolbar', { name: 'Edit mode' });
const preview = (page: Page) => page.getByRole('dialog');

async function open(page: Page, url: string) {
  await page.goto(url);
  await expect(page.locator('h1.term-title')).toBeVisible({ timeout: 30_000 });
}

async function commit(page: Page) {
  const d = preview(page);
  await expect(d.locator('[data-outcome]')).toHaveAttribute('data-outcome', 'commit');
  await d.getByRole('button', { name: 'Commit' }).click();
  await expect(d).toBeHidden();
}

/** Protects `main` of `ds`, runs `body`, then lifts the protection and drops `branch`. */
async function onSession(
  request: APIRequestContext,
  ds: string,
  body: () => Promise<string | null>,
) {
  expect((await request.patch(`/$/branches/${ds}/main`, { data: { protected: true } })).ok()).toBe(
    true,
  );
  let branch: string | null = null;
  try {
    branch = await body();
  } finally {
    await request.patch(`/$/branches/${ds}/main`, { data: { protected: false } });
    if (branch) await request.delete(`/$/branches/${ds}/${encodeURIComponent(branch)}`);
  }
}

/** Starts a session from the protected main and answers its branch. */
async function startSession(page: Page, prefix: string) {
  await expect(bar(page)).toContainText('main is protected');
  await bar(page).getByRole('button', { name: 'Start session' }).click();
  await expect(page).toHaveURL(new RegExp(`branch=${prefix}\\.anonymous\\.\\d{8}`));
  await expect(page).toHaveURL(/edit=1/);
  await expect(bar(page)).toContainText('Each change is previewed');
  return new URL(page.url()).searchParams.get('branch');
}

test('a read-only caller sees no edit affordance', async ({ page }) => {
  await page.route('**/$/whoami', (route) =>
    route.fulfill({
      json: {
        authEnabled: true,
        principal: { kind: 'token', name: 'reader' },
        method: 'bearer',
        server: [],
        datasets: { uni: 'read' },
        canMintTokens: false,
        logout: false,
      },
    }),
  );
  await open(page, term('uni', UNI + 'Professor', '&edit=1'));
  await expect(page.getByRole('region', { name: 'Axioms' })).toBeVisible();
  await expect(page).not.toHaveURL(/edit=1/);
  await expect(page.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0);
  await expect(bar(page)).toHaveCount(0);
  await expect(page.locator('[data-edit-form]')).toHaveCount(0);
  // the shortcut does nothing, and the palette offers no edit command
  await page.locator('h1.term-title').click();
  await page.keyboard.press('e');
  await expect(bar(page)).toHaveCount(0);
  await page.keyboard.press('Control+k');
  await page.keyboard.type('>edit this');
  const list = page.getByRole('listbox', { name: 'Search results' });
  await expect(list.getByRole('option', { name: /^Edit this page/ })).toHaveCount(0);
  await expect(list.getByRole('option', { name: /^Add synonym/ })).toHaveCount(0);
});

test('OWL edits on a session branch: definition, synonym, annotation, parent and axiom', async ({
  page,
  request,
}) => {
  await onSession(request, 'uni', async () => {
    await open(page, term('uni', UNI + 'Professor'));
    await page.getByRole('button', { name: 'Edit', exact: true }).click();
    await expect(page).toHaveURL(/edit=1/);
    // on the protected main the fields wait for a session
    await expect(page.getByRole('textbox', { name: 'Definition', exact: true })).toBeDisabled();
    await expect(page.getByRole('form', { name: 'Add a synonym' })).toHaveCount(0);
    const branch = await startSession(page, 'onto');

    // the definition
    const def = page.getByRole('textbox', { name: 'Definition', exact: true });
    await def.fill('A teacher who holds a chair.');
    await page
      .getByRole('textbox', { name: 'Add a source to the definition' })
      .fill('UNI:handbook');
    await page.getByRole('textbox', { name: 'Add a source to the definition' }).press('Enter');
    await page
      .getByRole('group', { name: /^Definition/ })
      .getByRole('button', { name: 'Preview change' })
      .click();
    await expect(preview(page)).toContainText('Add a definition to Professor');
    await expect(preview(page)).toContainText('A teacher who holds a chair.');
    await commit(page);
    await expect(
      page.getByRole('status').filter({ hasText: 'Add a definition to Professor' }),
    ).toBeVisible();
    await expect(def).toHaveValue('A teacher who holds a chair.');

    // a synonym: add, change its kind, remove
    const syn = page.getByRole('region', { name: /^Synonyms/ });
    await page.getByRole('textbox', { name: 'New synonym', exact: true }).fill('full professor');
    await page
      .getByRole('form', { name: 'Add a synonym' })
      .getByRole('button', { name: 'Add' })
      .click();
    await expect(preview(page)).toContainText('hasExactSynonym');
    await commit(page);
    await expect(syn).toContainText('full professor');
    await syn.getByRole('button', { name: 'Edit the synonym full professor' }).click();
    await syn
      .getByRole('combobox', { name: 'Kind', exact: true })
      .selectOption({ label: 'Related' });
    await syn.getByRole('button', { name: 'Preview change' }).click();
    await expect(preview(page).locator('.axioms.del')).toContainText('full professor');
    await expect(preview(page).locator('.axioms.ins')).toContainText('hasRelatedSynonym');
    await commit(page);
    await expect(syn.locator('li .k', { hasText: 'Related' })).toBeVisible();
    await syn.getByRole('button', { name: 'Remove the synonym full professor' }).click();
    await commit(page);
    await expect(syn).not.toContainText('full professor');

    // an annotation through the editor's rows
    const ann = page.getByRole('region', { name: /^Annotations/ });
    await ann.getByRole('button', { name: /Add annotation/ }).click();
    await ann.getByRole('combobox').first().fill('rdfs:comment');
    await ann.getByRole('textbox', { name: 'Value' }).fill('Holds a chair.');
    await ann.getByRole('button', { name: 'Preview' }).click();
    await expect(preview(page)).toContainText('Holds a chair.');
    await commit(page);
    await expect(ann).toContainText('Holds a chair.');

    // a parent: add Person, then remove it
    const tree = page.getByRole('complementary', { name: 'Hierarchy' });
    await tree.getByRole('combobox', { name: 'Add a parent class' }).fill('Pers');
    await tree
      .getByRole('option', { name: /Person/ })
      .first()
      .click();
    await expect(preview(page)).toContainText('SubClassOf');
    await commit(page);
    await expect(tree.getByRole('button', { name: 'Remove the parent Person' })).toBeVisible();
    await tree.getByRole('button', { name: 'Remove the parent Person' }).click();
    await commit(page);
    await expect(tree.getByRole('button', { name: 'Remove the parent Person' })).toHaveCount(0);

    // an axiom in Manchester syntax, then its removal
    await page.locator('#browse-add-axiom .cm-content').click();
    await page.keyboard.type('teaches some Course');
    await page
      .getByRole('form', { name: 'Add an axiom' })
      .getByRole('button', { name: 'Add' })
      .click();
    await expect(preview(page)).toBeVisible();
    // the mock already holds this axiom, so the edit changes nothing and the dialog says so
    await preview(page).getByRole('button', { name: 'Cancel' }).click();
    await page.locator('#browse-add-axiom .cm-content').click();
    await page.keyboard.press('Control+a');
    await page.keyboard.type('Agent');
    await page
      .getByRole('form', { name: 'Add an axiom' })
      .getByRole('button', { name: 'Add' })
      .click();
    await commit(page);
    const logic = page.getByRole('region', { name: 'Axioms' });
    await expect(logic.getByRole('link', { name: 'Agent', exact: true })).toBeVisible();
    await logic.getByRole('button', { name: /Remove the axiom SubClassOf\(.*Agent\)/ }).click();
    await commit(page);
    await expect(logic.getByRole('link', { name: 'Agent', exact: true })).toHaveCount(0);

    // undo takes back the axiom's removal, redo removes it again
    await bar(page).getByRole('button', { name: 'Undo' }).click();
    await expect(preview(page)).toContainText('Undo:');
    await commit(page);
    await expect(logic.getByRole('link', { name: 'Agent', exact: true })).toBeVisible();
    await bar(page).getByRole('button', { name: 'Redo' }).click();
    await commit(page);
    await expect(logic.getByRole('link', { name: 'Agent', exact: true })).toHaveCount(0);

    // Done leaves edit mode and the page reads as before, with the new definition
    await bar(page).getByRole('button', { name: /^Done/ }).click();
    await expect(page).not.toHaveURL(/edit=1/);
    await expect(page.locator('.lede')).toContainText('A teacher who holds a chair.');
    await expect(page.locator('.lede')).toContainText('UNI:handbook');
    return branch;
  });
});

test('SKOS edits: a label, a definition and a broader concept', async ({ page, request }) => {
  await onSession(request, 'thes', async () => {
    await open(page, term('thes', TH + 'fish', '&edit=1'));
    const branch = await startSession(page, 'skos');
    // an alternative label through the thesaurus editor's labels
    const add = page.getByRole('form', { name: 'Add a label' });
    await add.getByRole('combobox', { name: 'Kind', exact: true }).selectOption('alt');
    await add.getByRole('textbox', { name: 'Label' }).fill('Fishes');
    await add.getByRole('combobox', { name: 'Language' }).fill('en');
    await add.getByRole('button', { name: 'Add' }).click();
    await expect(preview(page)).toContainText('altLabel');
    await commit(page);
    await expect(page.getByRole('region', { name: 'Labels', exact: true })).toContainText('Fishes');

    await page
      .getByRole('textbox', { name: 'Definition', exact: true })
      .fill('A cold-blooded animal of the water.');
    await page
      .getByRole('group', { name: /^Definition/ })
      .getByRole('button', { name: 'Preview change' })
      .click();
    await expect(preview(page)).toContainText('skos/core#definition');
    await commit(page);

    // a second broader concept through the picker, then its removal
    const tree = page.getByRole('complementary', { name: 'Hierarchy' });
    await tree.getByRole('button', { name: 'Add broader' }).click();
    const picker = tree.getByRole('group', { name: 'Broader picker' });
    await picker.getByRole('searchbox').fill('Marine');
    await picker.getByRole('button', { name: /Marine animals/ }).click();
    await expect(preview(page)).toContainText('broader');
    await commit(page);
    const broader = tree.locator('[data-section="broader"]');
    await expect(broader).toContainText('Marine animals');
    await broader.getByRole('button', { name: /Remove.*Marine animals/ }).click();
    await commit(page);
    await expect(broader).not.toContainText('Marine animals');

    // a narrower concept writes both directions of the link
    await tree.getByRole('button', { name: 'Add narrower' }).click();
    const down = tree.getByRole('group', { name: 'Narrower picker' });
    await down.getByRole('searchbox').fill('Insects');
    await down.getByRole('button', { name: /Insects/ }).click();
    await expect(preview(page)).toContainText('narrower');
    await expect(preview(page)).toContainText('broader');
    await commit(page);
    await expect(tree.locator('[data-section="narrower"]')).toContainText('Insects');
    return branch;
  });
});

test('SKOS label edits wait for the labels of the commit the page shows', async ({
  page,
  request,
}) => {
  await onSession(request, 'thes', async () => {
    await open(page, term('thes', TH + 'fish', '&edit=1'));
    const branch = await startSession(page, 'skos');
    const add = page.getByRole('form', { name: 'Add a label' });
    await add.getByRole('combobox', { name: 'Kind', exact: true }).selectOption('hidden');
    await add.getByRole('textbox', { name: 'Label' }).fill('fishes');
    await add.getByRole('combobox', { name: 'Language' }).fill('en');
    // after this commit the labels are read again, and that read is held back
    const held: (() => Promise<void>)[] = [];
    await page.route('**/sparql**', (route) => {
      const body = route.request().postData() ?? '';
      if (body.includes(TH + 'fish') && body.includes('prefLabel'))
        held.push(() => route.continue());
      else void route.continue();
    });
    await add.getByRole('button', { name: 'Add' }).click();
    await commit(page);
    await expect.poll(() => held.length).toBeGreaterThan(0);
    await add.getByRole('textbox', { name: 'Label' }).fill('fishies');
    await add.getByRole('combobox', { name: 'Language' }).fill('en');
    await add.getByRole('button', { name: 'Add' }).click();
    await expect(page.getByText('Reading the labels again')).toBeVisible();
    await expect(preview(page)).toHaveCount(0);
    // the draft is still there, and once the labels arrive the edit goes through
    await expect(add.getByRole('textbox', { name: 'Label' })).toHaveValue('fishies');
    // so is a second preferred label offered as the replacement of the first
    await add.getByRole('combobox', { name: 'Kind', exact: true }).selectOption('pref');
    await page.getByRole('button', { name: /^Make it preferred and keep/ }).click();
    await expect(preview(page)).toHaveCount(0);
    await expect(add.getByRole('textbox', { name: 'Label' })).toHaveValue('fishies');
    await add.getByRole('combobox', { name: 'Kind', exact: true }).selectOption('hidden');
    for (const go of held.splice(0)) await go().catch(() => {});
    await page.unroute('**/sparql**');
    await expect(async () => {
      await add.getByRole('button', { name: 'Add' }).click();
      await expect(preview(page)).toBeVisible({ timeout: 1000 });
    }).toPass();
    await preview(page).getByRole('button', { name: 'Cancel' }).click();
    return branch;
  });
});

test('a refused write and a stale read are explained in the preview', async ({ page }) => {
  await open(page, term('uni', UNI + 'Student', '&edit=1'));
  await expect(bar(page)).toBeVisible();
  await page.route('**/$/ontology/uni/axioms?**', (route) =>
    route.fulfill({ status: 403, json: { error: 'write access to <urn:g> is needed' } }),
  );
  await page.getByRole('textbox', { name: 'New synonym', exact: true }).fill('pupil');
  await page
    .getByRole('form', { name: 'Add a synonym' })
    .getByRole('button', { name: 'Add' })
    .click();
  await expect(preview(page).getByRole('alert')).toContainText(
    'You can read uni but not write to it. write access to <urn:g> is needed',
  );
  await expect(preview(page).getByRole('button', { name: 'Commit' })).toBeDisabled();
  await preview(page).getByRole('button', { name: 'Cancel' }).click();
  await page.unroute('**/$/ontology/uni/axioms?**');
  await page.route('**/$/ontology/uni/axioms?**', (route) =>
    route.fulfill({
      status: 412,
      json: { error: 'If-Match: the representation is at commit 9 now' },
    }),
  );
  await page
    .getByRole('form', { name: 'Add a synonym' })
    .getByRole('button', { name: 'Add' })
    .click();
  await expect(preview(page).locator('[data-outcome]')).toHaveAttribute(
    'data-outcome',
    'precondition-failed',
  );
  await preview(page).getByRole('button', { name: 'Cancel' }).click();
  // the form keeps its text for the next try
  await expect(page.getByRole('textbox', { name: 'New synonym', exact: true })).toHaveValue(
    'pupil',
  );
});

test('the keyboard alone enters, edits, previews and leaves edit mode', async ({ page }) => {
  await open(page, term('uni', UNI + 'Student'));
  await page.locator('h1.term-title').click();
  await page.keyboard.press('e');
  await expect(bar(page)).toBeVisible();
  await expect(page).toHaveURL(/edit=1/);

  // S focuses the add field, and letters typed there stay text
  await page.keyboard.press('s');
  const field = page.getByRole('textbox', { name: 'New synonym', exact: true });
  await expect(field).toBeFocused();
  await page.keyboard.type('pesa');
  await expect(field).toHaveValue('pesa');
  await expect(page.locator('#browse-add-parent')).not.toBeFocused();

  // Mod+S previews the form that holds focus, and the browser's own save is prevented
  const prevented = await page.evaluate(() => {
    const e = new KeyboardEvent('keydown', {
      key: 's',
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    document.getElementById('browse-add-synonym')!.dispatchEvent(e);
    return e.defaultPrevented;
  });
  expect(prevented).toBe(true);
  await expect(preview(page)).toContainText('pesa');
  // with the dialog open the single letters do nothing
  await page.keyboard.press('p');
  await expect(page.locator('#browse-add-parent')).not.toBeFocused();
  await page.keyboard.press('Escape');
  await expect(preview(page)).toBeHidden();

  // P and A reach the parent and axiom fields; Esc in a field leaves the mode alone
  await page.locator('h1.term-title').click();
  await page.keyboard.press('p');
  await expect(page.locator('#browse-add-parent')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(bar(page)).toBeVisible();
  await page.locator('h1.term-title').click();
  await page.keyboard.press('a');
  await expect(page.locator('#browse-add-axiom .cm-content')).toBeFocused();
  await page.keyboard.type('e');
  await expect(bar(page)).toBeVisible();

  // Esc outside the fields leaves edit mode, after asking about the unsaved text
  await page.locator('h1.term-title').click();
  page.once('dialog', (d) => void d.accept());
  await page.keyboard.press('Escape');
  await expect(bar(page)).toHaveCount(0);
  await expect(page).not.toHaveURL(/edit=1/);

  // the palette has the keyboard while it is open
  await page.keyboard.press('Control+k');
  await page.keyboard.press('e');
  await expect(bar(page)).toHaveCount(0);
});

test('drafts count what is typed: a pending source, an annotation form and an untagged synonym', async ({
  page,
}) => {
  await open(page, term('uni', UNI + 'Student', '&edit=1'));
  // a source typed and not added goes into a preview by keyboard
  await page
    .getByRole('textbox', { name: 'Definition', exact: true })
    .fill('A person who studies.');
  const src = page.getByRole('textbox', { name: 'Add a source to the definition' });
  await src.fill('PMID:42');
  await src.press('Control+s');
  await expect(preview(page)).toContainText('Add a definition to Student');
  await expect(preview(page)).toContainText('PMID:42');
  await preview(page).getByRole('button', { name: 'Cancel' }).click();
  await expect(preview(page)).toBeHidden();
  await page
    .getByRole('group', { name: /^Definition/ })
    .getByRole('button', { name: 'Cancel' })
    .click();

  // an annotation typed and not previewed makes Done ask first
  const anns = page.getByRole('region', { name: /^Annotations/ });
  await anns.getByRole('button', { name: 'Add annotation' }).click();
  await anns.getByRole('form').locator('textarea').fill('A note not previewed');
  let asked = '';
  page.once('dialog', (d) => {
    asked = d.message();
    void d.dismiss();
  });
  await bar(page).getByRole('button', { name: /^Done/ }).click();
  await expect.poll(() => asked).toContain('discard');
  await expect(bar(page)).toBeVisible();
  // Mod+S in that form previews it and not another form's change
  await anns.getByRole('form').locator('textarea').press('Control+s');
  await expect(preview(page)).toContainText('A note not previewed');
  await preview(page).getByRole('button', { name: 'Cancel' }).click();
  // the form handed its rows to the preview and closed
  await expect(anns.getByRole('form')).toHaveCount(0);

  // a cleared language adds an untagged synonym
  const add = page.getByRole('form', { name: 'Add a synonym' });
  await page.getByRole('textbox', { name: 'New synonym', exact: true }).fill('pupil');
  const lang = page.getByRole('textbox', { name: 'Language of the new synonym' });
  await expect(lang).toHaveValue('en');
  await lang.fill('');
  await expect(lang).toHaveValue('');
  await add.getByRole('button', { name: 'Add' }).click();
  await expect(preview(page)).toContainText('"pupil"');
  await expect(preview(page)).not.toContainText('"pupil"@');
  await preview(page).getByRole('button', { name: 'Cancel' }).click();
});

test('the palette enters edit mode and focuses the add fields', async ({ page }) => {
  await open(page, term('uni', UNI + 'Student'));
  const list = page.getByRole('listbox', { name: 'Search results' });
  await page.keyboard.press('Control+k');
  await page.keyboard.type('>add synonym');
  await list.getByRole('option', { name: /^Add synonym/ }).click();
  await expect(bar(page)).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'New synonym', exact: true })).toBeFocused();

  await page.keyboard.press('Control+k');
  await page.keyboard.type('>leave edit');
  await list.getByRole('option', { name: /^Leave edit mode/ }).click();
  await expect(bar(page)).toHaveCount(0);

  await page.keyboard.press('Control+k');
  await page.keyboard.type('>edit this page');
  await list.getByRole('option', { name: /^Edit this page/ }).click();
  await expect(bar(page)).toBeVisible();
  await page.keyboard.press('Control+k');
  await page.keyboard.type('>full editor');
  await list.getByRole('option', { name: /^Open in the full editor/ }).click();
  await expect(page).toHaveURL(/\/ui\/workbench\/ontology\?ds=uni&iri=/);
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

  test('the forms stack and the edit bar sits at the bottom', async ({ page }) => {
    await open(page, term('uni', UNI + 'Professor', '&edit=1'));
    const bottom = bar(page);
    await expect(bottom).toBeVisible();
    const box = await bottom.boundingBox();
    expect(box && box.y + box.height).toBeGreaterThan(800);
    await expect(bottom.getByRole('button', { name: 'Undo' })).toBeVisible();
    await expectNoSidewaysScroll(page, 'edit mode on a phone');
    // the preview is reachable by touch
    await page.getByRole('textbox', { name: 'New synonym', exact: true }).fill('prof');
    await page
      .getByRole('form', { name: 'Add a synonym' })
      .getByRole('button', { name: 'Add' })
      .tap();
    await expect(preview(page)).toBeVisible();
    await expect(preview(page).getByRole('button', { name: 'Commit' })).toBeInViewport();
    await expectNoSidewaysScroll(page, 'the preview on a phone');
    await preview(page).getByRole('button', { name: 'Cancel' }).tap();
    page.once('dialog', (d) => void d.accept());
    await bottom.getByRole('button', { name: 'Done' }).tap();
    await expect(bar(page)).toHaveCount(0);
  });

  test('a concept’s label table scrolls inside the page', async ({ page }) => {
    await open(page, term('thes', TH + 'fish', '&edit=1'));
    await expect(page.getByRole('form', { name: 'Add a label' })).toBeVisible();
    // a thesaurus with labels in four scripts makes the table wider than the phone, as
    // UNESCO's does; a wide cell stands in for them
    await page
      .locator('.labels-scroll td')
      .first()
      .evaluate((td) =>
        td.insertAdjacentHTML(
          'beforeend',
          '<span style="display:inline-block;width:640px"></span>',
        ),
      );
    await expectNoSidewaysScroll(page, 'a concept in edit mode on a phone');
    expect(
      await page.locator('.labels-scroll').evaluate((e) => e.scrollWidth > e.clientWidth),
    ).toBe(true);
  });
});
