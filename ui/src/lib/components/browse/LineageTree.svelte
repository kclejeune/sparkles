<script lang="ts">
  // The hierarchy around an entity as a lineage spine: its ancestors from a root down one
  // line without indentation, the entity on the line with its other direct parents named
  // under it, its children branching off, and its siblings folded into one row that opens a
  // page at a time. Every level past the first is read when it is opened, one page of the
  // hierarchy route at a time, so the tree costs the same on a large ontology as on a small
  // one.
  import {
    countOf,
    fetchHierarchy,
    lineage,
    treeWords,
    type BrowseSelection,
    type Hierarchy,
    type TreeKind,
    type TreeNode,
  } from '$lib/browse';
  import * as api from '$lib/api';

  let {
    target,
    sel,
    kind,
    iri,
    title,
    answer,
    href,
    langs = [],
    page = 50,
  }: {
    target: string;
    sel: BrowseSelection;
    kind: TreeKind;
    iri: string;
    /** The entity's name on the line. */
    title: string;
    /** The hierarchy route's answer for the entity, with its ancestors. */
    answer: Hierarchy;
    href: (iri: string) => string;
    langs?: readonly string[];
    page?: number;
  } = $props();

  type Level = {
    items: TreeNode[];
    total: number;
    next: number | null;
    loading: boolean;
    error: string | null;
  };

  const words = $derived(treeWords(kind));
  const lin = $derived(lineage(iri, answer.ancestors));
  const parent = $derived(lin.spine.at(-1) ?? null);
  /** The levels read so far, by the IRI of their node; the entity's own is the answer's. */
  let levels = $state<Record<string, Level>>({});
  let openRows = $state<Record<string, boolean>>({});
  let siblingsOpen = $state(false);

  $effect(() => {
    // a new entity starts with its own children and nothing else open
    levels = {
      [iri]: {
        items: answer.children,
        total: answer.total,
        next: answer.next,
        loading: false,
        error: null,
      },
    };
    openRows = {};
    siblingsOpen = false;
  });

  async function load(of: string, more = false) {
    const was = levels[of];
    if (was?.loading) return;
    const offset = more && was ? was.items.length : 0;
    levels[of] = {
      items: was?.items ?? [],
      total: was?.total ?? 0,
      next: was?.next ?? null,
      loading: true,
      error: null,
    };
    try {
      const h = await fetchHierarchy(target, sel, {
        kind,
        iri: of,
        limit: page,
        offset,
        lang: langs,
      });
      const items = more ? [...(was?.items ?? []), ...h.children] : h.children;
      levels[of] = { items, total: h.total, next: h.next, loading: false, error: null };
    } catch (e) {
      levels[of] = { ...levels[of], loading: false, error: api.errorMessage(e) };
    }
  }

  function toggle(n: TreeNode) {
    openRows[n.iri] = !openRows[n.iri];
    if (openRows[n.iri] && !levels[n.iri]) void load(n.iri);
  }

  function openSiblings() {
    siblingsOpen = !siblingsOpen;
    if (siblingsOpen && parent && !levels[parent.iri]) void load(parent.iri);
  }

  const nodeName = (n: TreeNode) => n.label ?? n.shortForm ?? n.iri;
</script>

{#snippet rows(of: string, skip: string | null, path: string[])}
  {@const lv = levels[of]}
  {#if lv}
    <ul class="kids" role="group">
      {#each lv.items.filter((n) => n.iri !== skip) as n (n.iri)}
        {@const loop = path.includes(n.iri)}
        <li>
          <div class="row">
            {#if n.children && !loop}
              <button
                class="tw"
                type="button"
                aria-expanded={!!openRows[n.iri]}
                aria-label="{openRows[n.iri] ? 'Close' : 'Open'} {nodeName(n)}"
                onclick={() => toggle(n)}><span class:open={openRows[n.iri]}>›</span></button
              >
            {:else}
              <span class="tw" aria-hidden="true"></span>
            {/if}
            <a href={href(n.iri)} class:dep={n.deprecated} title={n.iri}
              ><span class="l">{nodeName(n)}</span>{#if loop}<span
                  class="cyc"
                  title="This entity is also an ancestor of this branch">cycle</span
                >{:else if n.children}<span class="n">{n.children.toLocaleString('en-US')}</span
                >{/if}</a
            >
          </div>
          {#if openRows[n.iri] && !loop}{@render rows(n.iri, null, [...path, n.iri])}{/if}
        </li>
      {/each}
      {#if lv.loading}
        <li class="faint">Loading…</li>
      {:else if lv.error}
        <li class="err">{lv.error}</li>
      {:else if lv.next !== null}
        <li>
          <button class="more-btn" type="button" onclick={() => load(of, true)}
            >Show {Math.min(page, lv.total - lv.items.length).toLocaleString('en-US')} more</button
          >
        </li>
      {/if}
    </ul>
  {/if}
{/snippet}

<ol class="spine" aria-label="Lineage">
  {#each lin.spine as n (n.iri)}
    <li>
      <a href={href(n.iri)} class:dep={n.deprecated} title={n.iri}
        ><span class="l">{nodeName(n)}</span>{#if n.children}<span class="n"
            >{n.children.toLocaleString('en-US')}</span
          >{/if}</a
      >
    </li>
  {/each}
  <li class="here" aria-current="page">
    <div class="me"><span class="l">{title}</span></div>
    {#if lin.otherParents.length}
      <p class="alsop">
        Also under {#each lin.otherParents as p, i (p.iri)}{#if i > 0},
          {/if}<a href={href(p.iri)}>{nodeName(p)}</a>{/each}
      </p>
    {/if}
    {#if answer.total === 0}
      <p class="nokids">No {words.children}</p>
    {:else}
      {@render rows(iri, null, [...lin.spine.map((n) => n.iri), iri])}
    {/if}
  </li>
  {#if parent && parent.children > 1}
    <li class="sibs">
      <button class="sib-btn" type="button" aria-expanded={siblingsOpen} onclick={openSiblings}
        >{countOf(parent.children - 1, `other ${words.child}`, `other ${words.children}`)} of {nodeName(
          parent,
        )}</button
      >
      {#if siblingsOpen}{@render rows(
          parent.iri,
          iri,
          lin.spine.map((n) => n.iri),
        )}{/if}
    </li>
  {/if}
</ol>
{#if answer.ancestors?.truncated}
  <p class="faint trunc">The ancestors were cut short at 2,000 entities.</p>
{/if}

<style>
  .spine {
    list-style: none;
    margin: 0;
    padding: 0;
    position: relative;
  }
  .spine > li {
    position: relative;
    padding: 3px 0 3px 18px;
    min-width: 0;
  }
  .spine > li::before {
    content: '';
    position: absolute;
    left: 4px;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--border-strong);
  }
  .spine > li:first-child::before {
    top: 12px;
  }
  .spine > li:last-child::before {
    bottom: auto;
    height: 12px;
  }
  .spine > li::after {
    content: '';
    position: absolute;
    left: 1px;
    top: 10px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--surface);
    border: 1px solid var(--border-strong);
  }
  a {
    color: var(--text);
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    flex: 1;
    text-decoration: none;
    border-radius: var(--r-sm);
  }
  a:hover .l {
    text-decoration: underline;
  }
  .l {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .dep .l {
    color: var(--text-3);
    text-decoration: line-through;
  }
  .n {
    margin-left: auto;
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
    flex: none;
  }
  .here {
    padding-top: 6px;
    padding-bottom: 8px;
  }
  .spine > .here::after {
    background: var(--spark);
    border-color: var(--spark);
    width: 9px;
    height: 9px;
    left: 0;
    top: 11px;
    box-shadow: 0 0 0 3px var(--spark-soft);
  }
  .me {
    display: flex;
    align-items: baseline;
    gap: 6px;
    font-weight: 600;
    background: var(--spark-soft);
    margin-left: -6px;
    padding: 2px 6px;
    border-radius: var(--r-sm);
    min-width: 0;
  }
  .me .l {
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .alsop {
    font-size: var(--fs-xs);
    color: var(--text-3);
    margin: 2px 0 0;
  }
  .alsop a {
    display: inline;
    color: var(--text-2);
  }
  .sibs {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .spine > .sibs::after {
    border-style: dashed;
  }
  .sib-btn {
    background: none;
    border: 0;
    padding: 0;
    color: var(--text-2);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .sib-btn:hover {
    color: var(--text);
    text-decoration: underline;
  }
  .kids {
    list-style: none;
    margin: 4px 0 0;
    padding: 0 0 0 10px;
    border-left: 1px dashed var(--border-strong);
    font-size: var(--fs);
  }
  .kids .kids {
    margin-left: 6px;
  }
  .kids li {
    padding: 2px 0 2px 4px;
    min-width: 0;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 2px;
    min-width: 0;
  }
  .tw {
    flex: none;
    width: 16px;
    height: 18px;
    display: inline-grid;
    place-items: center;
    color: var(--text-3);
    background: none;
    border: 0;
    padding: 0;
    cursor: pointer;
    border-radius: var(--r-sm);
  }
  .tw span {
    display: inline-block;
    transition: transform 0.12s;
  }
  .tw span.open {
    transform: rotate(90deg);
  }
  .cyc {
    margin-left: auto;
    font-size: var(--fs-xs);
    color: var(--text-3);
    font-style: italic;
    flex: none;
  }
  .nokids {
    font-size: var(--fs-sm);
    color: var(--text-3);
    margin: 4px 0 0 6px;
  }
  .err {
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  .trunc {
    font-size: var(--fs-xs);
    margin-top: var(--sp-2);
  }
  @media (prefers-reduced-motion: reduce) {
    .tw span {
      transition: none;
    }
  }
</style>
