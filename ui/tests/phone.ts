// Shared by the mock and the end-to-end suites: a phone-sized browser context, and the
// check that a page never scrolls sideways on it. Wide content (result tables, code, plans)
// has to scroll inside its own container; the page itself, `main` (the layout's scroll
// container) and any open dialog must fit the screen.

import { devices, expect, type Page } from '@playwright/test';

// The narrowest phone in common use (320 CSS pixels): what fits here fits on the larger
// ones. The browser stays the project's (Chromium): only the viewport, touch and the
// mobile meta viewport handling are taken over.
const { defaultBrowserType: _, ...iPhoneSE } = devices['iPhone SE'];
export const phone = iPhoneSE;

/** Selects the whole text of the focused CodeMirror editor, whichever its platform keys. */
export async function selectAll(page: Page) {
  // with an iPhone user agent CodeMirror may take the Mac bindings, where Ctrl+A only
  // moves to the line start
  await page.keyboard.press('Control+a');
  await page.keyboard.press('Meta+a');
}

/**
 * The containers that scroll sideways on `page`, and the elements that make them: those that
 * stick out of the screen (or of their dialog) while their parent does not, and that no
 * scrolling or clipping box of their own holds.
 */
function overflow() {
  const vw = document.documentElement.clientWidth;
  const main = document.querySelector('main');
  const pageLevel = (e: Element | null) =>
    e === null || e === main || e === document.body || e === document.documentElement;

  const describe = (el: Element) => {
    const parts: string[] = [];
    for (let e: Element | null = el; e && e !== document.body && parts.length < 5;) {
      let s = e.tagName.toLowerCase();
      if (e.id) s += `#${e.id}`;
      const cls = [...e.classList].filter((c) => !c.startsWith('svelte-')).slice(0, 3);
      if (cls.length) s += `.${cls.join('.')}`;
      parts.unshift(s);
      e = e.parentElement;
    }
    return parts.join(' > ');
  };

  const scrolling: string[] = [];
  const containers: Element[] = [
    document.documentElement,
    document.body,
    ...(main ? [main] : []),
    ...document.querySelectorAll('dialog[open]'),
  ];
  for (const c of containers) {
    // scrollWidth rounds; a pixel of slack keeps fractional layouts from counting
    if (c.scrollWidth > c.clientWidth + 1)
      scrolling.push(`${describe(c)} is ${c.scrollWidth}px wide in ${c.clientWidth}px`);
  }
  for (const d of document.querySelectorAll('dialog[open]')) {
    const r = d.getBoundingClientRect();
    if (r.left < -1 || r.right > vw + 1)
      scrolling.push(`${describe(d)} spans ${Math.round(r.left)}..${Math.round(r.right)}px`);
  }

  /** The box that holds `el` sideways: a scroller or clipper, a dialog, or the page. */
  const holder = (el: Element): Element | null => {
    for (let e = el.parentElement; e; e = e.parentElement) {
      if (pageLevel(e) || e.tagName === 'DIALOG') return e;
      const cs = getComputedStyle(e);
      if (cs.overflowX !== 'visible' || cs.position === 'fixed') return e;
    }
    return null;
  };
  const culprits: string[] = [];
  for (const el of document.querySelectorAll('body *')) {
    const cs = getComputedStyle(el);
    if (cs.display === 'none' || (cs.position === 'fixed' && el.tagName !== 'DIALOG')) continue;
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue;
    const h = holder(el);
    let [lo, hi] = [0, vw];
    if (h?.tagName === 'DIALOG') {
      const d = h.getBoundingClientRect();
      [lo, hi] = [d.left, d.right];
    } else if (!pageLevel(h)) continue;
    const out = (b: DOMRect) => b.left < lo - 1 || b.right > hi + 1;
    if (out(r) && !out(el.parentElement!.getBoundingClientRect()))
      culprits.push(`${describe(el)} spans ${Math.round(r.left)}..${Math.round(r.right)}px`);
  }
  return { scrolling, culprits };
}

/** Fails, naming the elements responsible, if the page or an open dialog scrolls sideways. */
export async function expectNoSidewaysScroll(page: Page, where: string) {
  // a dialog slides in from the side: measure where it comes to rest
  await page.evaluate(() =>
    Promise.all(
      document
        .getAnimations()
        .filter((a) => a.effect?.getTiming().iterations !== Infinity)
        .map((a) => a.finished.catch(() => {})),
    ),
  );
  const { scrolling, culprits } = await page.evaluate(overflow);
  expect(
    scrolling,
    `${where} scrolls sideways at ${page.viewportSize()?.width}px; it sticks out:\n  ${culprits.join('\n  ') || '(nothing found)'}`,
  ).toEqual([]);
}
