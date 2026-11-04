<script lang="ts">
  import { displayName } from '$lib/browse';
  import { WELL_KNOWN, type PrefixMap } from '$lib/rdf';
  // The landing page's search: the entity search of the ontology as a list in the page,
  // by label, synonym, short form, IRI and definition, with kind chips and pages of 20.
  // While it is mounted, `/` focuses it in place of opening the command palette.
  import { iriForm } from '$lib/iri-form.svelte';
  import * as api from '$lib/api';
  import { usePageSearch } from '$lib/launcher.svelte';
  import { kindWord } from '$lib/browse';
  import { searchEntities, type EntityHit, type EntitySearchKind } from '$lib/entity-search';
  import PageSearch from '$components/PageSearch.svelte';

  let {
    target,
    prefixes = WELL_KNOWN,
    graph,
    href,
    chips,
    placeholder = 'Search by label, synonym, ID or definition',
  }: {
    prefixes?: PrefixMap;
    target: string;
    graph: string | null;
    href: (iri: string) => string;
    /** The kind chips to offer, after All. */
    chips: { label: string; kind: EntitySearchKind }[];
    placeholder?: string;
  } = $props();

  const PAGE = 20;
  let q = $state('');
  let kind = $state<EntitySearchKind | null>(null);
  let hits = $state<EntityHit[]>([]);
  let next = $state<number | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let asked = $state('');
  let run = 0;
  let field: HTMLInputElement | undefined = $state();
  usePageSearch(() => {
    if (!field) return false;
    field.focus();
    field.select();
  });

  async function search(offset = 0) {
    const text = q.trim();
    const mine = ++run;
    if (!text) {
      hits = [];
      next = null;
      asked = '';
      error = null;
      return;
    }
    busy = true;
    try {
      const r = await searchEntities(target, text, {
        limit: PAGE,
        offset,
        kind: kind ? [kind] : undefined,
        graph,
      });
      if (mine !== run) return;
      hits = offset ? [...hits, ...r.results] : r.results;
      next = r.next;
      asked = text;
      error = null;
    } catch (e) {
      if (mine === run) error = api.errorMessage(e);
    } finally {
      if (mine === run) busy = false;
    }
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    void q;
    void kind;
    void target;
    void graph;
    void iriForm.languages;
    clearTimeout(timer);
    timer = setTimeout(() => void search(0), 180);
    return () => clearTimeout(timer);
  });

  /** The matched text with its highlighted ranges. */
  function parts(h: EntityHit) {
    const t = h.match.text;
    const out: { text: string; hit: boolean }[] = [];
    let at = 0;
    for (const [s, e] of h.match.highlight) {
      if (s > at) out.push({ text: t.slice(at, s), hit: false });
      out.push({ text: t.slice(s, e), hit: true });
      at = e;
    }
    if (at < t.length) out.push({ text: t.slice(at), hit: false });
    return out;
  }
  const FIELD: Record<string, string> = {
    synonym: 'synonym',
    shortForm: 'ID',
    iri: 'IRI',
    definition: 'definition',
  };
</script>

<section class="search" aria-label="Search the ontology">
  <PageSearch bind:value={q} bind:field {placeholder} label="Search" />
  <div class="chips" role="radiogroup" aria-label="Kind">
    <button
      type="button"
      class="chip"
      role="radio"
      aria-checked={kind === null}
      onclick={() => (kind = null)}>All</button
    >
    {#each chips as c (c.kind)}
      <button
        type="button"
        class="chip"
        role="radio"
        aria-checked={kind === c.kind}
        onclick={() => (kind = c.kind)}>{c.label}</button
      >
    {/each}
  </div>
  {#if error}
    <p class="err">{error}</p>
  {:else if asked && !hits.length && !busy}
    <p class="muted">Nothing matches “{asked}”.</p>
  {/if}
  {#if hits.length}
    <ul class="hits">
      {#each hits as h (h.iri)}
        <li>
          <a href={href(h.iri)} class:dep={h.deprecated}
            >{displayName(h.iri, { [h.iri]: h }, prefixes)}</a
          >
          {#if h.shortForm && h.shortForm !== h.label}<span class="sfx">{h.shortForm}</span>{/if}
          <span class="badge iri">{kindWord(h.kinds)}</span>
          {#if h.deprecated}<span class="badge danger">deprecated</span>{/if}
          {#if h.match.field !== 'label'}
            <span class="via"
              >{FIELD[h.match.field] ?? h.match.field}: {#each parts(h) as p, i (i)}{#if p.hit}<mark
                    >{p.text}</mark
                  >{:else}{p.text}{/if}{/each}</span
            >
          {/if}
        </li>
      {/each}
    </ul>
    {#if next !== null}
      <button class="more-btn" type="button" disabled={busy} onclick={() => search(hits.length)}
        >{busy ? 'Reading…' : 'Show more'}</button
      >
    {/if}
  {/if}
</section>

<style>
  .search {
    display: grid;
    gap: var(--sp-3);
    min-width: 0;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    height: var(--ctl-h-sm);
    display: inline-flex;
    align-items: center;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: var(--r-pill);
    font-size: var(--fs-sm);
    color: var(--text-2);
    background: var(--surface);
    cursor: pointer;
  }
  .chip[aria-checked='true'] {
    border-color: var(--spark);
    background: var(--spark-soft);
    color: var(--spark-ink);
    font-weight: 600;
  }
  .hits {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .hits li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px var(--sp-2);
    padding: 6px 0;
    border-top: 1px solid var(--border);
    min-width: 0;
  }
  .hits li:first-child {
    border-top: 0;
  }
  .hits a {
    color: var(--iri);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .dep {
    text-decoration: line-through;
  }
  .sfx {
    font: 11px var(--font-mono);
    color: var(--text-3);
  }
  .via {
    width: 100%;
    color: var(--text-2);
    font-size: var(--fs-sm);
    overflow-wrap: anywhere;
  }
  mark {
    background: var(--spark-soft);
    color: inherit;
    border-radius: 2px;
  }
  .err {
    color: var(--danger);
  }
</style>
