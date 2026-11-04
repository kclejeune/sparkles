// The command palette (ROADMAP item 631): the top bar's search with nothing typed, its
// modes, each source and command, the recent picks, the keyboard and the phone.

import { expect, test, type Locator, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

const LARGE = { width: 1440, height: 900 };
const MEDIUM = { width: 900, height: 900 };
const PHONE = { width: 390, height: 844 };

const search = (page: Page) => page.locator('.scope-search input[role="combobox"]');
const list = (page: Page) => page.getByRole('listbox', { name: 'Search results' });
/** The titles of the rows, without their shortcuts and tags. */
const titles = (page: Page, within?: Locator) =>
  (within ?? list(page)).getByRole('option').locator('.title');
/** A mode chip above the results. */
const chip = (page: Page, name: string) =>
  page.getByRole('toolbar', { name: 'Show' }).getByRole('button', { name: new RegExp(`^${name}`) });
const group = (page: Page, name: string | RegExp) => list(page).getByRole('group', { name });
const option = (page: Page, name: string | RegExp) =>
  list(page).getByRole('option', {
    name: typeof name === 'string' ? new RegExp(`^${name}`) : name,
  });

async function onDataset(page: Page, ds: string) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
}

async function visit(page: Page, url: string) {
  await page.goto(url);
  // the first visit of a page compiles it in the dev server
  await expect(page.locator('main h1')).toBeAttached({ timeout: 15_000 });
}

/** Opens the palette with Ctrl+K and types `text`. */
async function palette(page: Page, text = '') {
  await page.keyboard.press('Control+k');
  await expect(search(page)).toBeFocused();
  if (text) await page.keyboard.type(text);
}

test.describe('at 1440 pixels', () => {
  test.use({ viewport: LARGE });

  test('opens on every page with its commands and the pages', async ({ page }) => {
    test.setTimeout(90_000);
    await onDataset(page, 'uni');
    const pages: [string, string | null][] = [
      ['/ui/query', 'Run query'],
      ['/ui/explore?ds=uni', null],
      ['/ui/workbench/ontology?ds=uni', 'New class…'],
      ['/ui/workbench/thesaurus?ds=thes', 'New concept…'],
      ['/ui/similar?ds=uni', null],
      ['/ui/memory?ds=uni', null],
      ['/ui/similar/map?ds=uni', null],
      ['/ui/datasets', 'Refresh the dataset list'],
      ['/ui/datasets/uni', 'Show settings'],
      ['/ui/admin/backups', null],
      ['/ui/admin/server', null],
      ['/ui/admin/tokens', null],
    ];
    for (const [url, command] of pages) {
      await visit(page, url);
      await palette(page);
      await expect(group(page, 'Go to'), url).toBeVisible();
      if (command)
        await expect(group(page, 'This page').getByRole('option', { name: command })).toBeVisible();
      else await expect(group(page, 'This page')).toHaveCount(0);
      await page.keyboard.press('Escape');
      await expect(list(page)).toHaveCount(0);
    }
  });

  test('a page takes its commands and branches with it when the shell navigates away', async ({
    page,
  }) => {
    await onDataset(page, 'foaf');
    await visit(page, '/ui/workbench/ontology?ds=foaf');
    await palette(page, '#');
    await expect(group(page, 'Branches of foaf')).toBeVisible();
    await page.keyboard.press('Escape');
    await page.keyboard.press('Escape');
    // a client-side navigation: the layout and its palette stay, the page goes
    await page
      .getByRole('navigation', { name: 'Pages' })
      .getByRole('link', { name: 'Admin', exact: true })
      .click();
    await expect(page).toHaveURL(/\/ui\/admin\/server$/);
    await palette(page, '#');
    await expect(group(page, 'Datasets')).toBeVisible();
    await expect(group(page, /^Branches/)).toHaveCount(0);
    await page.keyboard.press('Escape');
    await page.keyboard.press('Escape');
    await palette(page, '>new class');
    await expect(option(page, 'New class')).toHaveCount(0);
  });

  test('New dataset opens the dialog on the Datasets page too', async ({ page }) => {
    await visit(page, '/ui/datasets');
    await palette(page, '>new dataset');
    await expect(titles(page)).toHaveText(['New dataset…']);
    await page.keyboard.press('Enter');
    await expect(page.getByRole('dialog', { name: /dataset/i })).toBeVisible();
    await expect(page).toHaveURL(/\/ui\/datasets$/);
  });

  test('`>form` lists only Format query and the dataset settings', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/query');
    await palette(page, '>form');
    await expect(chip(page, 'Commands')).toHaveAttribute('aria-pressed', 'true');
    await expect(titles(page)).toHaveText(['Format query', 'Open dataset settings']);
    await expect(option(page, 'Format query').locator('mark')).toHaveText('Form');
    await expect(option(page, 'Format query').locator('kbd')).toHaveText(['Shift', 'Alt', 'F']);
    await expect(option(page, 'Format query')).toHaveAccessibleName(
      'Format query , shortcut Shift Alt F',
    );
  });

  test('runs and formats a query and gives the focus back to the editor', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/query');
    const editor = page.locator('.cm-content');
    await editor.click();
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.press('Delete');
    const original = 'SELECT   *  WHERE { ?s ?p ?o }   LIMIT 3';
    await page.keyboard.insertText(original);
    await palette(page, '>format');
    await page.keyboard.press('Enter');
    // the mock's formatter collapses runs of spaces
    await expect
      .poll(() => editor.locator('.cm-line').first().innerText())
      .toBe('SELECT * WHERE { ?s ?p ?o } LIMIT 3');
    await expect(editor).toBeFocused();
    await palette(page, '>run');
    await expect(option(page, 'Run query')).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Enter');
    await expect(page.getByRole('tab', { name: /^Table/ })).toBeVisible();
  });

  test('switches the theme', async ({ page }) => {
    await visit(page, '/ui/admin/server');
    await palette(page, '>dark');
    await expect(titles(page)).toHaveText(['Theme: dark']);
    await page.keyboard.press('Enter');
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
    await palette(page, '>theme');
    await expect(option(page, 'Theme: dark')).toHaveCount(0);
    await option(page, 'Theme: light').click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  });

  test('`#` lists the datasets and switches to one', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/explore?ds=uni');
    await palette(page, '>switch');
    await page.keyboard.press('Enter');
    await expect(search(page)).toHaveValue('#');
    await expect(group(page, 'Datasets')).toBeVisible();
    await page.keyboard.type('org');
    await expect(option(page, 'org')).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/explore\?ds=org$/);
    await expect(page.locator('.ds-button')).toContainText('org');
  });

  test('`#` switches the branch of the ontology editor', async ({ page }) => {
    await onDataset(page, 'foaf');
    await visit(page, '/ui/workbench/ontology?ds=foaf');
    await palette(page, '#');
    await expect(group(page, 'Branches of foaf')).toBeVisible();
    await option(page, 'dev').click();
    await expect(page).toHaveURL(/[?&]branch=dev/);
  });

  test('`?` lists the saved queries and opens one', async ({ page }) => {
    const put = await page.request.put('/$/queries/uni/people-by-name', {
      data: { query: 'SELECT ?p WHERE { ?p ?q ?o } LIMIT 5', description: 'Everyone by name' },
    });
    expect(put.ok()).toBe(true);
    await onDataset(page, 'uni');
    await visit(page, '/ui/admin/server');
    await palette(page, '?');
    await expect(group(page, 'Saved queries in uni')).toBeVisible();
    await expect(option(page, 'people-by-name')).toContainText('Everyone by name');
    await page.keyboard.type('peop');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/query/);
    await expect(page).not.toHaveURL(/saved=/);
    await expect(page.getByRole('tab', { name: /people-by-name/ })).toBeVisible();
    await expect(page.locator('.cm-content')).toContainText('LIMIT 5');
  });

  test('entity mode searches entities only', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/admin/server');
    await palette(page, '@');
    await expect(list(page)).toContainText('Type to search the entities of uni.');
    await page.keyboard.type('stud');
    await expect(group(page, 'Entities in uni')).toBeVisible();
    await expect(group(page, 'Go to')).toHaveCount(0);
  });

  test('starts a session and opens the new class dialog', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/workbench/ontology?ds=uni');
    await palette(page, '>start');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/[?&]branch=/);
    await palette(page, '>new class');
    await page.keyboard.press('Enter');
    const dialog = page.getByRole('dialog', { name: 'New class' });
    await expect(dialog).toBeVisible();
  });

  test('opens the settings, the server pages and the new dataset dialog', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/query');
    await palette(page, '>settings');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/datasets\/uni\?tab=settings$/);
    await palette(page, '>tokens');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/admin\/tokens$/);
    await palette(page, '>new dataset');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/datasets$/);
    await expect(page.getByRole('dialog', { name: /dataset/i })).toBeVisible();
  });

  test('`/` filters the ontology tree, and opens the palette elsewhere', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/workbench/ontology?ds=uni');
    await expect(page.getByRole('searchbox', { name: 'Filter' })).toBeVisible();
    await page.locator('main h1').click();
    await page.keyboard.press('/');
    await expect(page.getByRole('searchbox', { name: 'Filter' })).toBeFocused();
    await expect(list(page)).toHaveCount(0);
    // Ctrl+K is still the palette
    await page.locator('main h1').click();
    await palette(page, '>search');
    await expect(option(page, 'Search this page')).toBeVisible();
    await page.keyboard.press('Escape');

    await visit(page, '/ui/workbench/thesaurus?ds=thes');
    await page.locator('main h1').click();
    await page.keyboard.press('/');
    await expect(page.getByRole('searchbox', { name: 'Search labels' })).toBeFocused();

    await visit(page, '/ui/admin/server');
    await page.keyboard.press('/');
    await expect(search(page)).toBeFocused();
  });

  test('puts recent picks first, across pages and reloads', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/datasets');
    await palette(page, '>server');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/admin\/server$/);
    await palette(page, 'thes');
    await option(page, 'Workbench › Thesaurus').click();
    await expect(page).toHaveURL(/\/ui\/workbench\/thesaurus/);
    await page.reload();
    await expect(page.locator('main h1')).toBeAttached();
    await palette(page);
    const recent = group(page, 'Recent');
    await expect(titles(page, recent)).toHaveText(['Workbench › Thesaurus', 'Open server status']);
    await expect(list(page).getByRole('group').first()).toHaveAccessibleName('Recent');
    // a row under Recent shows only there
    await expect(
      group(page, 'Go to').getByRole('option', { name: /^Workbench › Thesaurus/ }),
    ).toHaveCount(0);
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/workbench\/thesaurus/);
  });

  test('an item stored twice shows once among the recent picks', async ({ page }) => {
    await onDataset(page, 'uni');
    // a page picked once with a dataset recorded and once without
    await page.addInitScript(() => {
      const pick = {
        provider: 'pages',
        id: '/admin/server',
        title: 'Admin › Server',
        href: '/ui/admin/server',
      };
      localStorage.setItem(
        'sparkles.launcher.recent',
        JSON.stringify([
          { ...pick, ds: null, at: 2 },
          { ...pick, ds: 'uni', at: 1 },
        ]),
      );
    });
    await visit(page, '/ui/datasets');
    await palette(page);
    await expect(titles(page, group(page, 'Recent'))).toHaveText(['Admin › Server']);
  });

  test('works when the browser blocks storage', async ({ page }) => {
    await page.addInitScript(() => {
      const fail = () => {
        throw new DOMException('blocked', 'SecurityError');
      };
      Object.defineProperty(window, 'localStorage', { get: fail });
    });
    await visit(page, '/ui/admin/server');
    await palette(page, '>tokens');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/ui\/admin\/tokens$/);
    // the pick is remembered for this visit only
    await palette(page);
    await expect(titles(page, group(page, 'Recent'))).toHaveText(['Open access tokens']);
    await page.keyboard.press('Escape');
    await page.reload();
    await expect(page.locator('main h1')).toBeAttached();
    await palette(page);
    await expect(group(page, 'Recent')).toHaveCount(0);
    await expect(group(page, 'Go to')).toBeVisible();
  });

  test('moves by keys: arrows, Tab between groups, Escape out of a mode', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/query');
    const refresh = page.locator('.cm-content');
    await refresh.click();
    await palette(page);
    const options = list(page).getByRole('option');
    await expect(options.first()).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('ArrowDown');
    await expect(options.nth(1)).toHaveAttribute('aria-selected', 'true');
    // Tab: the first row of the next group, Shift+Tab back
    await page.keyboard.press('Tab');
    await expect(group(page, 'Go to').getByRole('option').first()).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(search(page)).toBeFocused();
    await page.keyboard.press('Shift+Tab');
    await expect(group(page, 'This page').getByRole('option').first()).toHaveAttribute(
      'aria-selected',
      'true',
    );
    await page.keyboard.press('ArrowUp');
    await expect(options.last()).toHaveAttribute('aria-selected', 'true');
    // a mode with nothing after it: Escape leaves the mode, then closes
    await page.keyboard.type('>');
    await page.keyboard.press('Escape');
    await expect(search(page)).toHaveValue('');
    await expect(list(page)).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(list(page)).toHaveCount(0);
    await expect(refresh).toBeFocused();
    // Ctrl+K again closes it
    await palette(page);
    await page.keyboard.press('Control+k');
    await expect(list(page)).toHaveCount(0);
    await expect(refresh).toBeFocused();
  });

  test('the chips choose a mode', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/admin/server');
    await palette(page, 'or');
    await chip(page, 'Datasets').click();
    await expect(search(page)).toHaveValue('#or');
    await expect(search(page)).toBeFocused();
    // other suites may have added datasets to the shared mock
    await expect(titles(page).first()).toHaveText('org');
    await chip(page, 'All').click();
    await expect(search(page)).toHaveValue('or');
  });
});

test.describe('at 900 pixels', () => {
  test.use({ viewport: MEDIUM });

  test('the palette fits beside the rail', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/query');
    await palette(page);
    await expect(group(page, 'This page')).toBeVisible();
    await expect(option(page, 'Run query').locator('kbd')).toHaveText(['Ctrl', 'Enter']);
    await expectNoSidewaysScroll(page, 'the open palette');
  });
});

test.describe('at 390 pixels', () => {
  test.use({ viewport: PHONE, hasTouch: true, isMobile: true });

  test('every command is a tap away', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/query');
    await page.getByRole('button', { name: 'Open search' }).tap();
    await expect(search(page)).toBeFocused();
    await expect(group(page, 'This page')).toBeVisible();
    // shortcuts are for keyboards; the chips scroll and the last one can be reached
    await expect(option(page, 'Run query').locator('kbd').first()).toBeHidden();
    const saved = chip(page, 'Saved');
    await saved.scrollIntoViewIfNeeded();
    await expect(saved).toBeInViewport();
    await expectNoSidewaysScroll(page, 'the palette on a phone');
    await chip(page, 'Commands').tap();
    await expect(search(page)).toHaveValue('>');
    await option(page, 'Theme: dark').tap();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
    await expect(search(page)).toBeHidden();
  });

  test('the rows are tall enough for a finger', async ({ page }) => {
    await onDataset(page, 'uni');
    await visit(page, '/ui/admin/server');
    await page.getByRole('button', { name: 'Open search' }).tap();
    const box = await list(page).getByRole('option').first().boundingBox();
    expect(box!.height).toBeGreaterThanOrEqual(44);
  });
});

// Screenshots for review (PALETTE_SHOTS=dir): the palette at three widths in both themes.
const SHOTS = process.env.PALETTE_SHOTS;
test.describe('screenshots', () => {
  test.skip(!SHOTS, 'PALETTE_SHOTS is not set');
  for (const theme of ['light', 'dark'])
    for (const [name, viewport] of Object.entries({ large: LARGE, medium: MEDIUM, phone: PHONE }))
      test(`${name} ${theme}`, async ({ page }) => {
        await page.setViewportSize(viewport);
        await page.addInitScript((t) => localStorage.setItem('sparkles.theme', t), theme);
        await onDataset(page, 'uni');
        await visit(page, '/ui/query');
        const open = async (text: string) => {
          if (name === 'phone') {
            await page.getByRole('button', { name: 'Open search' }).click();
            await page.keyboard.type(text);
          } else await palette(page, text);
          await expect(search(page)).toHaveValue(text);
          await expect(list(page).getByRole('option').first()).toBeVisible();
          await page.waitForTimeout(300);
        };
        await open('');
        await page.screenshot({ path: `${SHOTS}/${name}-${theme}-empty.png` });
        await page.keyboard.press('Escape');
        await open('>form');
        await page.screenshot({ path: `${SHOTS}/${name}-${theme}-commands.png` });
        await page.keyboard.press('Escape');
        await page.keyboard.press('Escape');
        await open('pers');
        await page.screenshot({ path: `${SHOTS}/${name}-${theme}-search.png` });
      });
});
