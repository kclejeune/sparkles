// Everything the measurements know about the pages' structure, in one place, so that the
// changed UI needs only this file adapted (ROADMAP item 725). Each function works on the
// baseline UI of 4ab93ecb. The comments say what the changed UI must offer in its place.

import type { Page } from '@playwright/test';

/** The ontology tree's rows: `role="treeitem"` elements with the entity's IRI in `data-id`. */
export const TREE_ROW = '[role="tree"] li[role="treeitem"]';
export const rowOf = (iri: string) => `[role="tree"] li[role="treeitem"][data-id="${iri}"]`;

/**
 * The twisty of a row with children. A virtual tree keeps the aria-label "Expand NAME" and
 * `data-id` on its rows; the selector may stay.
 */
export const twistyOf = (iri: string) => `${rowOf(iri)} > .node .twisty`;
export const FIRST_EXPANDABLE = `${TREE_ROW} > .node .twisty`;

/** The "Show more" button of a long level (the "Showing 1,000 of N" row). */
export const SHOW_MORE = '[role="tree"] .more-row button';

/**
 * The number of rows the tree holds, whether or not they are in the DOM. The baseline draws
 * every row, so it counts the elements. A virtual tree should expose the count as
 * `aria-rowcount` on the tree, which is read first.
 */
export async function loadedRows(page: Page): Promise<number> {
  return page.evaluate(() => {
    const tree = document.querySelector('[role="tree"]');
    const n = Number(tree?.getAttribute('aria-rowcount'));
    return Number.isFinite(n) && n > 0 ? n : document.querySelectorAll('[role="tree"] li').length;
  });
}

/** The rows in the DOM, for the cost of the tree. */
export async function rowsInDom(page: Page): Promise<number> {
  return page.evaluate(() => document.querySelectorAll('[role="tree"] li').length);
}

/** The pages' own scrolling element of the tree: the nearest scrollable ancestor of the tree. */
export const SCROLLER_JS = `(() => {
  const tree = document.querySelector('[role="tree"]');
  for (let e = tree; e; e = e.parentElement) {
    const o = getComputedStyle(e).overflowY;
    if ((o === 'auto' || o === 'scroll') && e.scrollHeight > e.clientHeight) return e;
  }
  return document.scrollingElement;
})()`;

// --- Explore ---------------------------------------------------------------------------
// The measurements need a handle on the drawn graph. The baseline draws with Cytoscape,
// which registers its instance on the container element (`_cyreg`), so the handle is built
// from that. The changed UI draws past a size with sigma, which keeps no such handle: it
// must provide `window.__perfHook` with the same members, set by the page when it draws
// (a few lines in the Explore page, next to its node and edge records).

export type PerfHook = {
  /** Every node of the explored graph, drawn or hidden by a filter. */
  nodeCount(): number;
  /** The nodes drawn now (equals nodeCount when no filter or budget hides any). */
  drawnCount(): number;
  /** The ids of the nodes, in a stable order. */
  nodeIds(): string[];
  /** Expands a node as a double click does; resolves when its neighbours are drawn. */
  expand(id: string): Promise<void>;
  /** The renderer in use, 'cytoscape' or 'webgl'. */
  renderer(): string;
  /** The camera, to check that the synthetic pan and zoom moved it (optional). */
  viewport?(): unknown;
};

/** Installs `window.__perfHook` from the Cytoscape instance, when the page has not set one. */
export const INSTALL_HOOK_JS = `(() => {
  if (window.__perfHook) return true;
  const host = [...document.querySelectorAll('div')].find((d) => d._cyreg && d._cyreg.cy);
  if (!host) return false;
  const cy = host._cyreg.cy;
  window.__perfHook = {
    nodeCount: () => cy.nodes().length,
    drawnCount: () => cy.nodes().length,
    nodeIds: () => cy.nodes().map((n) => n.id()),
    expand: (id) => {
      const before = cy.nodes().length;
      cy.getElementById(id).emit('dbltap');
      return new Promise((ok) => {
        const t0 = performance.now();
        const tick = () => {
          if (cy.nodes().length > before || performance.now() - t0 > 8000) ok();
          else requestAnimationFrame(tick);
        };
        tick();
      });
    },
    renderer: () => 'cytoscape',
    viewport: () => ({ pan: cy.pan(), zoom: cy.zoom() }),
  };
  return true;
})()`;

/** The element the graph is drawn in, for the synthetic pan and zoom: the largest canvas. */
export const GRAPH_CANVAS_JS = `(() => {
  let best = null, area = 0;
  for (const c of document.querySelectorAll('main canvas, .wrap canvas, .lg-host canvas, canvas')) {
    const r = c.getBoundingClientRect();
    if (r.width * r.height > area) { area = r.width * r.height; best = c; }
  }
  if (!best) return null;
  const r = best.getBoundingClientRect();
  return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
})()`;

export const exploreUrl = (base: string, ds: string, iri: string) =>
  `${base}/ui/explore?ds=${ds}&iri=${encodeURIComponent(iri)}`;
export const ontologyUrl = (base: string, ds: string, q = '') =>
  `${base}/ui/workbench/ontology?ds=${ds}${q}`;
export const thesaurusUrl = (base: string, ds: string, q = '') =>
  `${base}/ui/workbench/thesaurus?ds=${ds}${q}`;
