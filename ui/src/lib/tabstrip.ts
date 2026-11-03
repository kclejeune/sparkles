// Every tab strip (`.tabs`) is one row that scrolls sideways inside itself (ROADMAP item
// 633). This watches the document for strips: each gets `data-more-start` and
// `data-more-end` while tabs are off screen on that side, which app.css draws as a fade,
// and keeps its selected tab scrolled into view.

const SEEN = new WeakSet<Element>();

function edges(el: HTMLElement) {
  const start = el.scrollLeft > 1;
  const end = el.scrollLeft + el.clientWidth < el.scrollWidth - 1;
  el.toggleAttribute('data-more-start', start);
  el.toggleAttribute('data-more-end', end);
}

function reveal(el: HTMLElement) {
  const sel = el.querySelector<HTMLElement>('[aria-selected="true"], [aria-current="page"]');
  if (!sel || el.scrollWidth <= el.clientWidth) return;
  const a = sel.offsetLeft - el.offsetLeft;
  const b = a + sel.offsetWidth;
  if (a < el.scrollLeft) el.scrollLeft = a - 16;
  else if (b > el.scrollLeft + el.clientWidth) el.scrollLeft = b - el.clientWidth + 16;
}

function attach(el: HTMLElement, resize: ResizeObserver, sel: MutationObserver) {
  if (SEEN.has(el)) return;
  SEEN.add(el);
  el.addEventListener('scroll', () => edges(el), { passive: true });
  resize.observe(el);
  sel.observe(el, {
    subtree: true,
    attributes: true,
    attributeFilter: ['aria-selected', 'aria-current'],
  });
  reveal(el);
  edges(el);
}

/** Watch `root` for tab strips; returns the function that stops. */
export function watchTabStrips(root: HTMLElement = document.body): () => void {
  const resize = new ResizeObserver((es) => {
    for (const e of es) edges(e.target as HTMLElement);
  });
  const sel = new MutationObserver((ms) => {
    for (const m of ms) {
      const strip = (m.target as HTMLElement).closest<HTMLElement>('.tabs');
      if (strip) (reveal(strip), edges(strip));
    }
  });
  const scan = () =>
    root.querySelectorAll<HTMLElement>('.tabs').forEach((t) => attach(t, resize, sel));
  // at most one scan a frame, however busy the document is
  let queued = 0;
  const added = new MutationObserver(() => {
    if (!queued) queued = requestAnimationFrame(() => ((queued = 0), scan()));
  });
  added.observe(root, { childList: true, subtree: true });
  scan();
  return () => {
    cancelAnimationFrame(queued);
    added.disconnect();
    sel.disconnect();
    resize.disconnect();
  };
}
