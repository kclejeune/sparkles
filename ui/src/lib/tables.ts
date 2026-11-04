// Every data table (`table.data`) stays usable when it is wider than its box (ROADMAP item
// 633). This watches the document for them. A table whose box scrolls sideways gets
// `data-clipped` on that box, which app.css answers by pinning an actions column
// (`td.actions` as the last cell) to the right edge. On a phone a table that would scroll
// gets `data-stack` instead, which app.css draws as one card a row: the first cell as the
// card's title, each other cell under its column's name, and the actions in a row at the
// bottom. The column names come from the table's header row and are copied into each body
// cell's `data-label`, so the callers' markup stays an ordinary table.

const PHONE = '(max-width: 760px)';
const SEEN = new WeakSet<HTMLTableElement>();

/** Copy each header cell's text onto the body cells of its column. */
function label(table: HTMLTableElement) {
  const head = table.tHead?.rows[0];
  if (!head) return;
  const names = [...head.cells].flatMap((c) => Array(c.colSpan).fill(c.textContent?.trim() ?? ''));
  for (const body of table.tBodies)
    for (const row of body.rows) {
      let col = 0;
      for (const cell of row.cells) {
        const name = cell.colSpan === 1 ? names[col] : '';
        if (name) cell.dataset.label = name;
        else delete cell.dataset.label;
        col += cell.colSpan;
      }
    }
}

function fit(table: HTMLTableElement, phone: boolean) {
  const box = table.parentElement;
  if (!box) return;
  if (table.hasAttribute('data-stack')) {
    // a stacked table returns to rows once its box is as wide as the rows needed
    const needed = Number(table.dataset.stackWidth ?? 0);
    if (!phone || box.clientWidth >= needed) {
      table.removeAttribute('data-stack');
      delete table.dataset.stackWidth;
    } else return;
  }
  const over = table.scrollWidth > box.clientWidth + 1;
  if (over && phone && table.tHead) {
    table.dataset.stackWidth = String(table.scrollWidth);
    label(table);
    table.setAttribute('data-stack', '');
    box.removeAttribute('data-clipped');
    return;
  }
  box.toggleAttribute('data-clipped', box.scrollWidth > box.clientWidth + 1);
}

/** Watch `root` for data tables; returns the function that stops. */
export function watchTables(root: HTMLElement = document.body): () => void {
  const phone = window.matchMedia(PHONE);
  const tables = new Set<HTMLTableElement>();
  const refit = () => {
    for (const t of tables) {
      if (!t.isConnected) tables.delete(t);
      else fit(t, phone.matches);
    }
  };
  const resize = new ResizeObserver(refit);
  // rows a table gains while stacked need their labels
  const rows = new MutationObserver((ms) => {
    for (const m of ms) {
      const t = (m.target as HTMLElement).closest<HTMLTableElement>('table.data');
      if (t?.hasAttribute('data-stack')) label(t);
    }
  });
  const scan = () => {
    root.querySelectorAll<HTMLTableElement>('table.data').forEach((t) => {
      if (SEEN.has(t)) return;
      SEEN.add(t);
      tables.add(t);
      resize.observe(t);
      if (t.parentElement) resize.observe(t.parentElement);
      rows.observe(t, { childList: true, subtree: true });
      fit(t, phone.matches);
    });
  };
  let queued = 0;
  const added = new MutationObserver(() => {
    if (!queued) queued = requestAnimationFrame(() => ((queued = 0), scan()));
  });
  added.observe(root, { childList: true, subtree: true });
  phone.addEventListener('change', refit);
  scan();
  return () => {
    cancelAnimationFrame(queued);
    added.disconnect();
    rows.disconnect();
    resize.disconnect();
    phone.removeEventListener('change', refit);
  };
}
