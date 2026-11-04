<script lang="ts">
  // The panel of a group node (ROADMAP item 725): the neighbours of a resource through one
  // predicate, a page at a time, with a text filter and a split by class. Nothing is drawn
  // until the person draws it, so a resource with a million neighbours stays a single node.
  import { untrack } from 'svelte';
  import { isAbort } from '$lib/supersede';
  import { fmtInt } from '$lib/format';
  import {
    FIRST_PAGE,
    pageOf,
    PAGE_SIZE,
    readClasses,
    readPage,
    type GroupPage,
    type GroupQuery,
    type GroupSpec,
    type Member,
  } from '$lib/explore-groups';
  import { errorMessage, type Term } from '$lib/api';
  import { termKey } from '$lib/rdf';
  import Icon from './Icon.svelte';

  let {
    ds,
    at = '',
    spec,
    predicateLabel,
    ownerLabel,
    termLabel,
    drawn,
    ondraw,
    onremove,
  }: {
    /** The dataset and branch the members are read from. */
    ds: string;
    at?: string;
    spec: GroupSpec;
    predicateLabel: string;
    ownerLabel: string;
    /** The caption of a member that has no label of its own. */
    termLabel: (t: Term) => string;
    /** Whether a member is on the graph already. */
    drawn: (t: Term) => boolean;
    /** Draw these members, linked to the owner through the predicate. */
    ondraw: (members: Member[]) => void;
    onremove: () => void;
  } = $props();

  let q = $state<GroupQuery>({ ...FIRST_PAGE });
  let page = $state<GroupPage | null>(null);
  let classes = $state<{ iri: string; count: number }[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let text = $state('');
  let ctl: AbortController | undefined;
  let classCtl: AbortController | undefined;
  let timer = 0;

  async function load() {
    ctl?.abort();
    const c = (ctl = new AbortController());
    loading = true;
    error = null;
    try {
      const p = await readPage(ds, spec, q, at || undefined, c.signal);
      if (!c.signal.aborted) page = p;
    } catch (e) {
      if (!isAbort(e) && !c.signal.aborted) error = errorMessage(e);
    } finally {
      if (!c.signal.aborted) loading = false;
    }
  }
  async function loadClasses(filter: string) {
    classCtl?.abort();
    const c = (classCtl = new AbortController());
    try {
      const r = await readClasses(ds, spec, filter, at || undefined, c.signal);
      if (!c.signal.aborted) classes = r;
    } catch {
      if (!c.signal.aborted) classes = [];
    }
  }

  // a different group starts over
  $effect(() => {
    void spec.owner;
    void spec.predicate;
    void spec.incoming;
    void ds;
    void at;
    untrack(() => {
      q = { ...FIRST_PAGE };
      text = '';
      page = null;
      void load();
      void loadClasses('');
    });
    return () => {
      ctl?.abort();
      classCtl?.abort();
      clearTimeout(timer);
    };
  });

  function setFilter(v: string) {
    text = v;
    clearTimeout(timer);
    timer = window.setTimeout(() => {
      q = { ...q, filter: v, offset: 0 };
      void load();
      void loadClasses(v);
    }, 250);
  }
  function setClass(iri: string | null) {
    q = { ...q, cls: q.cls === iri ? null : iri, offset: 0 };
    void load();
  }
  function go(offset: number | null) {
    if (offset == null) return;
    q = { ...q, offset };
    void load();
  }

  const pos = $derived(pageOf(page?.total ?? 0, q.offset));
  const fresh = $derived((page?.members ?? []).filter((m) => !drawn(m.term)));
  const label = (m: Member) => m.label ?? termLabel(m.term);
</script>

<section class="group" aria-label="Group of neighbours" data-testid="group-panel">
  <div class="ghead">
    <div class="gtitle">
      <h2>
        {fmtInt(spec.total)} via <span class="mono">{predicateLabel}</span>
      </h2>
      <p class="faint small">
        {spec.incoming ? 'Resources that point to' : 'Resources that'}
        <b>{ownerLabel}</b>{spec.incoming ? '' : ' points to'}, read {PAGE_SIZE} at a time.
      </p>
    </div>
    <button
      class="btn sm ghost icon"
      title="Remove the group from the graph"
      aria-label="Remove the group from the graph"
      onclick={onremove}><Icon name="x" size={14} /></button
    >
  </div>

  <div class="gbody">
    <label class="find"
      ><span class="sr-only">Filter the members by name</span>
      <input
        class="input"
        type="search"
        placeholder="Filter by name"
        aria-label="Filter the members by name"
        value={text}
        oninput={(e) => setFilter(e.currentTarget.value)}
      /></label
    >

    {#if classes.length}
      <div class="chips" role="group" aria-label="Split by class">
        {#each classes as c (c.iri)}
          <button
            class="chip"
            class:on={q.cls === c.iri}
            aria-pressed={q.cls === c.iri}
            title={c.iri}
            onclick={() => setClass(c.iri)}
            ><span class="cn">{termLabel({ type: 'uri', value: c.iri })}</span>
            <b>{fmtInt(c.count)}</b></button
          >
        {/each}
      </div>
    {/if}

    {#if error}<div class="error-box" role="alert">{error}</div>{/if}

    <div class="pager">
      <span data-testid="group-range" aria-live="polite">
        {#if page}
          {#if page.total}{fmtInt(pos.from)}–{fmtInt(pos.to)} of {fmtInt(page.total)}{:else}No
            members match{/if}
        {:else}Reading…{/if}
      </span>
      <span class="spacer"></span>
      {#if loading}<span class="spinner"></span>{/if}
      <button class="btn sm" disabled={pos.prev == null || loading} onclick={() => go(pos.prev)}
        >Previous</button
      >
      <button class="btn sm" disabled={pos.next == null || loading} onclick={() => go(pos.next)}
        >Next</button
      >
    </div>

    <ul class="members" aria-label="Members">
      {#each page?.members ?? [] as m (termKey(m.term))}
        {@const on = drawn(m.term)}
        <li class:on>
          <span class="mname" title={m.term.type === 'uri' ? m.term.value : label(m)}
            >{label(m)}</span
          >
          {#if on}<span class="faint small">drawn</span>{:else}
            <button class="btn sm" onclick={() => ondraw([m])}>Draw</button>
          {/if}
        </li>
      {/each}
    </ul>
  </div>

  <div class="gfoot">
    <button class="btn sm primary" disabled={!fresh.length} onclick={() => ondraw(fresh)}
      ><Icon name="expand" size={13} /> Draw this page{fresh.length
        ? ` (${fresh.length})`
        : ''}</button
    >
  </div>
</section>

<style>
  .group {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
    background: var(--surface);
  }
  .ghead {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    justify-content: space-between;
    padding: 14px 16px 10px;
    border-bottom: 1px solid var(--border);
  }
  .gtitle h2 {
    margin: 0;
    font-size: var(--fs-md);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .gtitle p {
    margin: 4px 0 0;
  }
  .gbody {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 10px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .find input {
    width: 100%;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    max-width: 100%;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface);
    color: var(--text);
    font: inherit;
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .chip:hover {
    border-color: var(--iri);
  }
  .chip.on {
    background: var(--spark-soft);
    border-color: var(--spark);
  }
  .cn {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 14ch;
  }
  .chip b {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .pager {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
  }
  .members {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .members li {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 32px;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .mname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .members li.on .mname {
    color: var(--text-3);
  }
  .gfoot {
    padding: 10px 16px;
    border-top: 1px solid var(--border);
    display: flex;
    gap: 8px;
  }
  .small {
    font-size: var(--fs-xs);
  }
  @media (max-width: 760px) {
    .members li {
      min-height: 44px;
    }
  }
</style>
