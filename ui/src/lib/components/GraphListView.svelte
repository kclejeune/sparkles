<script lang="ts" module>
  /** A row of the list view: a drawn node of the large view. */
  export type ListRow = {
    key: string;
    label: string;
    kind: string;
    size: number;
    group?: string;
    cls?: string;
    degree: number;
    color: string;
    openable: boolean;
    /** Marked, such as an entity of the chosen finding. */
    marked?: boolean;
    /** The tooltip of the name, the key without its prefix by default. */
    title?: string;
  };
</script>

<script lang="ts">
  /**
   * The list view of the WebGL graph (spec U06 §2.4): the drawn nodes as a table, largest
   * first, with their group, class and degree, in sync with the selection. It is the
   * accessible companion of the canvas.
   */
  import { fmtInt } from '$lib/format';
  import type { LensColumn } from '$lib/lenses.svelte';

  let {
    rows,
    selected = null,
    onselect,
    onopen,
    openLabel = 'Open',
    clsLabel = 'Class',
    plain = false,
    lensColumns = [],
    label = 'Drawn nodes',
  }: {
    rows: ListRow[];
    selected?: string | null;
    onselect?: (key: string) => void;
    onopen?: (key: string) => void;
    openLabel?: string;
    /** The heading of the class column. */
    clsLabel?: string;
    /**
     * A list of drawn resources rather than the overview's groups: no size or class column,
     * and `size` only orders the rows, such as by the size lens's value.
     */
    plain?: boolean;
    /** A column per active lens (docs/specs/U10-canvas-lenses.md §2.1). */
    lensColumns?: LensColumn[];
    label?: string;
  } = $props();

  const LIMIT = 500;
  let all = $state(false);
  const sorted = $derived(
    [...rows].sort(
      (a, b) => b.size - a.size || b.degree - a.degree || a.label.localeCompare(b.label),
    ),
  );
  const shown = $derived(all ? sorted : sorted.slice(0, LIMIT));
  let body: HTMLTableSectionElement | undefined = $state();

  $effect(() => {
    if (!selected || !body) return;
    const row = body.querySelector<HTMLElement>(`[data-key="${CSS.escape(selected)}"]`);
    row?.scrollIntoView({ block: 'nearest' });
  });
</script>

<div class="list-wrap">
  <table class="list" aria-label={label}>
    <thead>
      <tr>
        <th scope="col">Name</th>
        {#if !plain}<th scope="col" class="num">Size</th>{/if}
        {#each lensColumns as c (`${c.channel}:${c.title}`)}
          <th scope="col" class="lens-col" data-lens={c.channel}>{c.title}</th>
        {/each}
        <th scope="col" class="hide-sm">Kind</th>
        {#if !plain}<th scope="col" class="hide-sm">{clsLabel}</th>{/if}
        <th scope="col" class="num hide-sm">Links</th>
        <th scope="col"><span class="sr-only">Actions</span></th>
      </tr>
    </thead>
    <tbody bind:this={body}>
      {#each shown as r (r.key)}
        <tr
          data-key={r.key}
          class:sel={r.key === selected}
          class:marked={r.marked}
          aria-selected={r.key === selected}
        >
          <td>
            <button
              class="name"
              onclick={() => onselect?.(r.key)}
              title={r.title ?? r.key.slice(2)}
            >
              <span class="dot" style:background={r.color} aria-hidden="true"></span>
              <span class="lbl">{r.label}</span>{#if r.marked}<span class="sr-only">, marked</span
                >{/if}
            </button>
          </td>
          {#if !plain}<td class="num">{r.kind === 'member' ? '' : fmtInt(r.size)}</td>{/if}
          {#each lensColumns as c (`${c.channel}:${c.title}`)}
            <td class="lens-col" data-lens={c.channel}>{c.cells.get(r.key) ?? ''}</td>
          {/each}
          <td class="hide-sm faint">{r.kind}</td>
          {#if !plain}<td class="hide-sm faint mono small">{r.cls ?? ''}</td>{/if}
          <td class="num hide-sm">{fmtInt(r.degree)}</td>
          <td class="act">
            <button class="btn sm" onclick={() => onopen?.(r.key)}
              >{r.openable ? openLabel : 'Explore'}</button
            >
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if sorted.length > LIMIT && !all}
    <p class="faint small more">
      Showing the {fmtInt(LIMIT)} largest of {fmtInt(sorted.length)} nodes.
      <button class="btn sm ghost" onclick={() => (all = true)}>Show all</button>
    </p>
  {/if}
</div>

<style>
  .list-wrap {
    height: 100%;
    overflow: auto;
  }
  .list {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-sm);
  }
  th {
    position: sticky;
    top: 0;
    background: var(--surface-2);
    text-align: left;
    font-weight: 600;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
  }
  td {
    padding: 3px 8px;
    border-bottom: 1px solid var(--border);
    vertical-align: middle;
  }
  .lens-col {
    max-width: 22ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  tr.marked td {
    box-shadow: inset 3px 0 0 var(--spark);
  }
  tr.sel td {
    background: var(--spark-soft);
  }
  .name {
    display: flex;
    gap: 6px;
    align-items: center;
    background: none;
    border: 0;
    color: var(--text);
    cursor: pointer;
    padding: 2px 0;
    text-align: left;
    font: inherit;
    max-width: 100%;
  }
  .lbl {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40ch;
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex: none;
  }
  .act {
    text-align: right;
    white-space: nowrap;
  }
  .more {
    padding: 8px;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  @media (max-width: 640px) {
    .hide-sm {
      display: none;
    }
  }
</style>
