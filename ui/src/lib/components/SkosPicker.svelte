<script lang="ts">
  // The concept picker of the thesaurus editor (docs/specs/U12-ontology-workbench/SPEC.md,
  // "SKOS and SKOS-XL thesaurus editor"): a label search in any language, in the dataset
  // the editor shows or in another one the caller can read, in one scheme or in all, or an
  // IRI typed as it is. Mapping relations link concepts across schemes and datasets.
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app } from '$lib/app.svelte';
  import { iriName, validIri } from '$lib/ontology';
  import { displayEntity, type PrefixMap } from '$lib/rdf';
  import { iriForm } from '$lib/iri-form.svelte';
  import {
    ANY_GRAPH,
    labelMap,
    labelsQuery,
    schemesQuery,
    searchHits,
    searchQuery,
    type SearchHit,
    type Label,
  } from '$lib/skos';

  let {
    target,
    dataset,
    graph,
    prefixes,
    langs,
    label = 'Concept',
    exclude = [],
    onpick,
  }: {
    /** The target the editor reads (`name` or `name@branch`). */
    target: string;
    /** The dataset's name, for the dataset select. */
    dataset: string;
    graph: string | null;
    prefixes: PrefixMap;
    langs: readonly string[];
    label?: string;
    exclude?: string[];
    /** The IRI picked, with its name when a search found it. */
    onpick: (iri: string, name?: string) => void;
  } = $props();

  let ds = $state('');
  $effect(() => {
    void dataset;
    untrack(() => (ds = dataset));
  });
  const here = $derived(ds === dataset);
  const on = $derived(here ? target : ds);
  const g = $derived(here ? graph : ANY_GRAPH);

  let schemeLabels = $state<{ iri: string; labels: Label[] }[]>([]);
  const schemes = $derived(
    schemeLabels.map(({ iri, labels }) => ({
      iri,
      name: displayEntity(
        iri,
        prefixes,
        labels.map((l) => ({ '@value': l.value, '@language': l.lang })),
        langs,
      ),
    })),
  );
  let scheme = $state('');
  $effect(() => {
    const t = on;
    const gg = g;
    let current = true;
    scheme = '';
    schemeLabels = [];
    void (async () => {
      try {
        const rows = await api.select(t, schemesQuery(gg), { nocache: true });
        const iris = rows.flatMap((r) => (r.s?.type === 'uri' ? [r.s.value] : []));
        const ls = iris.length
          ? labelMap(await api.select(t, labelsQuery(iris, gg), { nocache: true }))
          : new Map<string, Label[]>();
        if (current) schemeLabels = iris.map((iri) => ({ iri, labels: ls.get(iri) ?? [] }));
      } catch {
        if (current) schemeLabels = [];
      }
    })();
    return () => {
      current = false;
    };
  });

  let text = $state('');
  let hits = $state<(SearchHit & { name: string })[]>([]);
  let searching = $state(false);
  let error = $state<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  $effect(() => {
    const q = text.trim();
    const t = on;
    const gg = g;
    const sc = scheme || null;
    void iriForm.mode;
    void langs;
    clearTimeout(timer);
    if (q.length < 2 || validIri(q)) {
      hits = [];
      return;
    }
    const my = ++seq;
    timer = setTimeout(async () => {
      searching = true;
      error = null;
      try {
        const rows = await api.select(t, searchQuery(q, { scheme: sc, graph: gg }), {
          nocache: true,
        });
        const hs = searchHits(rows, q).filter((h) => !exclude.includes(h.iri));
        const iris = [...new Set(hs.map((h) => h.iri))];
        const ls = iris.length
          ? labelMap(await api.select(t, labelsQuery(iris, gg), { nocache: true }))
          : new Map<string, Label[]>();
        if (my !== seq) return;
        const seen = new Set<string>();
        hits = hs
          .filter((h) => (seen.has(h.iri) ? false : (seen.add(h.iri), true)))
          .map((h) => ({
            ...h,
            name: displayEntity(
              h.iri,
              prefixes,
              ls.get(h.iri)?.map((l) => ({ '@value': l.value, '@language': l.lang })),
              langs,
            ),
          }));
      } catch (e) {
        if (my === seq) error = api.errorMessage(e);
      } finally {
        if (my === seq) searching = false;
      }
    }, 250);
  });

  function pickTyped() {
    const q = text.trim();
    const c = q.indexOf(':');
    const expanded =
      c > 0 && prefixes[q.slice(0, c)] !== undefined && !q.slice(c + 1).startsWith('//')
        ? prefixes[q.slice(0, c)] + q.slice(c + 1)
        : q.replace(/^<|>$/g, '');
    if (validIri(expanded)) {
      onpick(expanded);
      text = '';
    }
  }
</script>

<div class="picker" role="group" aria-label="{label} picker">
  <div class="row">
    <select class="select sm" bind:value={ds} aria-label="Dataset">
      {#each app.datasets as d (d.name)}<option value={d.name}>{d.name}</option>{/each}
      {#if !app.datasets.some((d) => d.name === dataset)}<option value={dataset}>{dataset}</option
        >{/if}
    </select>
    <select class="select sm" bind:value={scheme} aria-label="Scheme">
      <option value="">all schemes</option>
      {#each schemes as s (s.iri)}<option value={s.iri}>{s.name}</option>{/each}
    </select>
  </div>
  <form
    class="row"
    onsubmit={(e) => {
      e.preventDefault();
      if (hits.length === 1) {
        onpick(hits[0].iri, hits[0].name);
        text = '';
      } else pickTyped();
    }}
  >
    <input
      class="input sm grow"
      type="search"
      bind:value={text}
      placeholder="Search by label, or an IRI"
      aria-label="Search {label.toLowerCase()}s"
    />
    <button
      class="btn sm"
      disabled={!validIri(text.trim().replace(/^<|>$/g, '')) && !text.includes(':')}>Add IRI</button
    >
  </form>
  {#if error}<p class="error-box small" role="alert">{error}</p>{/if}
  {#if searching && !hits.length}<p class="faint small">
      <span class="spinner"></span> Searching…
    </p>{/if}
  {#if hits.length}
    <ul class="hits" aria-label="Matches">
      {#each hits as h (h.iri)}
        <li>
          <button
            class="hit"
            onclick={() => {
              onpick(h.iri, h.name);
              text = '';
            }}
          >
            <span class="name">{h.name}</span>
            {#if h.label.value !== h.name}<span class="faint small"
                >{h.kind} “{h.label.value}”{h.label.lang ? `@${h.label.lang}` : ''}</span
              >{/if}
            <span class="mono faint xs">{iriName(h.iri, prefixes)}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .picker {
    display: grid;
    gap: 6px;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    min-width: 0;
  }
  .row {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .grow {
    flex: 1 1 12em;
    min-width: 0;
  }
  .hits {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 200px;
    overflow: auto;
  }
  .hit {
    display: flex;
    gap: 8px;
    align-items: baseline;
    flex-wrap: wrap;
    width: 100%;
    border: 0;
    background: none;
    padding: 3px 6px;
    font: inherit;
    text-align: left;
    color: var(--text);
    cursor: pointer;
    border-radius: var(--r-sm);
  }
  .hit:hover {
    background: var(--hover);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .xs {
    font-size: var(--fs-xs);
  }
</style>
