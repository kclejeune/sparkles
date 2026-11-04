// Two columns of cards whose bottom edges stay close (ROADMAP item 633). A page lays out its
// cards in a `[data-col="main"]` and a `[data-col="side"]` column, each at its home, and marks
// the cards that may move with `data-movable`, in the order they may move. When one column
// grows past the other, the first movable card whose move brings the bottoms closer goes to
// the end of the shorter column, and goes home again when that is closer. A move needs the
// gap to shrink by more than a threshold, so cards stay put while a tree opens or a graph
// lays out, and a card that holds focus never moves. While the columns stack as one (their
// `display` is `contents`) every card is at home. The side column gets `data-stick` when it
// is the shorter and fits in the scrolling box, for a sticky position.

/** A card that may move, with its height and the column it is in. */
export type Movable = { height: number; inMain: boolean; locked: boolean };

/**
 * Which movable card to move, if any, to bring the columns' heights closer: the index of the
 * first card whose move shrinks the gap by more than `threshold`, or -1.
 */
export function pickMove(
  main: number,
  side: number,
  cards: readonly Movable[],
  gap: number,
  threshold: number,
): number {
  const now = Math.abs(main - side);
  for (let i = 0; i < cards.length; i++) {
    const c = cards[i];
    if (c.locked) continue;
    const h = c.height + gap;
    const after = c.inMain ? Math.abs(main - h - (side + h)) : Math.abs(main + h - (side - h));
    if (after < now - threshold) return i;
  }
  return -1;
}

/** The content height of a column: the bottom of its last card below its top. */
function heightOf(col: HTMLElement): number {
  const last = col.lastElementChild as HTMLElement | null;
  if (!last) return 0;
  return last.getBoundingClientRect().bottom - col.getBoundingClientRect().top;
}

/** A Svelte action for the element that holds the two columns. */
export function balance(node: HTMLElement, opts: { threshold?: number } = {}) {
  const threshold = opts.threshold ?? 64;
  const main = () => node.querySelector<HTMLElement>(':scope > [data-col="main"]');
  const side = () => node.querySelector<HTMLElement>(':scope > [data-col="side"]');
  /** Where each card lives at home, recorded the first time it is seen. */
  const home = new WeakMap<HTMLElement, { col: 'main' | 'side'; next: Element | null }>();
  let frame = 0;

  const cards = () =>
    [...node.querySelectorAll<HTMLElement>('[data-col] > [data-movable]')].map((el) => {
      if (!home.has(el))
        home.set(el, {
          col: el.parentElement?.dataset.col === 'side' ? 'side' : 'main',
          next: el.nextElementSibling,
        });
      return el;
    });

  function goHome(el: HTMLElement) {
    const h = home.get(el);
    const col = h?.col === 'side' ? side() : main();
    if (!col || el.parentElement === col) return;
    const next = h?.next && h.next.parentElement === col ? h.next : null;
    col.insertBefore(el, next);
  }

  function run() {
    frame = 0;
    const m = main();
    const s = side();
    if (!m || !s) return;
    const list = cards();
    if (getComputedStyle(m).display === 'contents') {
      for (const el of list) goHome(el);
      delete s.dataset.stick;
      return;
    }
    const gap = parseFloat(getComputedStyle(m).rowGap) || 0;
    const hm = heightOf(m);
    const hs = heightOf(s);
    const i = pickMove(
      hm,
      hs,
      list.map((el) => ({
        height: el.getBoundingClientRect().height,
        inMain: el.parentElement === m,
        locked: el.contains(document.activeElement),
      })),
      gap,
      threshold,
    );
    if (i >= 0) {
      const el = list[i];
      const to = el.parentElement === m ? s : m;
      if (home.get(el)?.col === (to === m ? 'main' : 'side')) goHome(el);
      else to.appendChild(el);
      schedule();
      return;
    }
    const box = s.closest<HTMLElement>('[data-scroll], .page, main') ?? document.documentElement;
    if (hs < hm && hs < box.clientHeight - 32) s.dataset.stick = '';
    else delete s.dataset.stick;
  }

  function schedule() {
    if (!frame) frame = requestAnimationFrame(run);
  }

  const ro = new ResizeObserver(schedule);
  const watch = () => {
    ro.disconnect();
    ro.observe(node);
    for (const c of [main(), side()]) if (c) for (const el of c.children) ro.observe(el);
  };
  const mo = new MutationObserver(() => {
    watch();
    schedule();
  });
  mo.observe(node, { childList: true, subtree: false });
  for (const c of [main(), side()]) if (c) mo.observe(c, { childList: true });
  watch();
  schedule();

  return {
    destroy() {
      cancelAnimationFrame(frame);
      ro.disconnect();
      mo.disconnect();
    },
  };
}
