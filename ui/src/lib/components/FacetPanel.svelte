<script lang="ts">
  /**
   * The facet panel (docs/specs/U11-linked-views.md, section 2.3): histograms and bar lists
   * of the literal properties of a set, counted on the server for every entity of the set.
   * Dragging across a histogram, moving its two handles or clicking the values of a bar list
   * brushes the facet. Every other facet then counts the brushed set, and the drawn entities
   * outside the brushes are reported through `onmatch` so that the canvas dims them. **Apply
   * to query** hands the brushes to the page, which writes them as triples and filters.
   */
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import type { FacetAnswer, FacetSetSpec, FacetSpec, FacetsAnswer, Term } from '$lib/api';
  import {
    BRUSH_PAUSE_MS,
    FIRST_FACETS,
    KIND_LABEL,
    MAX_FACETS,
    binLabel,
    binsFor,
    binsBrush,
    brushText,
    brushedBins,
    facetsRequest,
    frameOf,
    keptBy,
    toggleValue,
    valuesQuery,
    type Brush,
  } from '$lib/facets';
  import { fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { displayTerm, toSparql, type PrefixMap } from '$lib/rdf';
  import { isAbort } from '$lib/supersede';
  import Icon from './Icon.svelte';

  let {
    ds,
    within,
    setLabel,
    at = '',
    prefixes = {},
    drawn = null,
    reasoning = null,
    onmatch,
    onapply,
    applyLabel = 'Apply to query',
    onclose,
    header,
  }: {
    /** The dataset, or `ds@branch`. */
    ds: string;
    /** The set the facets count; null while the page has none. */
    within: FacetSetSpec | null;
    /** What the set is, for the title: "people", "values of ?person". */
    setLabel: string;
    at?: string;
    prefixes?: PrefixMap;
    /** The term keys of the drawn entities, whose values decide what the canvas dims. */
    drawn?: string[] | null;
    /** False to count the asserted data alone, as a result read with `reasoning=false`. */
    reasoning?: boolean | null;
    /** The drawn entities every brush keeps, or null when nothing is brushed. */
    onmatch?: (ids: Set<string> | null, text: string) => void;
    onapply?: (brushes: Brush[]) => void;
    applyLabel?: string;
    onclose?: () => void;
    /** Controls above the facets, such as the choice of the set. */
    header?: import('svelte').Snippet;
  } = $props();

  let candidates = $state<NonNullable<FacetsAnswer['candidates']>>([]);
  /** The facets shown, with the frames (edges or bars) of their first answer. */
  let specs = $state<FacetSpec[]>([]);
  let answer = $state<FacetsAnswer | null>(null);
  /** The brushes the answer counted, so that a count line never shows a stale count. */
  let answered = $state('');
  /** Each facet's counts without brushes, drawn faintly behind the brushed counts. */
  let ghosts = $state<Record<string, FacetAnswer>>({});
  let brushes = $state<Record<string, Brush>>({});
  let loading = $state(false);
  let error = $state<string | null>(null);
  let live = $state('');
  let tables = $state<Record<string, boolean>>({});
  let controller: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const brushList = $derived(Object.values(brushes));
  const fresh = $derived(answered === JSON.stringify(brushList));
  const short = (iri: string) => shortLabel(iri, prefixes);
  const termLabel = (t: Term, label?: string) => label ?? displayTerm(t, prefixes);

  // ------------------------------------------------------------------ requests ------

  async function load(req: api.FacetsRequest): Promise<FacetsAnswer | null> {
    controller?.abort();
    const c = new AbortController();
    controller = c;
    loading = true;
    try {
      const a = await api.graphFacets(ds, req, c.signal, { reasoning: reasoning ?? undefined });
      if (controller !== c) return null;
      error = null;
      return a;
    } catch (e) {
      if (isAbort(e)) return null;
      if (controller === c) error = api.errorMessage(e);
      return null;
    } finally {
      if (controller === c) loading = false;
    }
  }

  /** A new set: its candidates, then the first facets. */
  async function open(w: FacetSetSpec) {
    answer = null;
    ghosts = {};
    brushes = {};
    specs = [];
    candidates = [];
    const first = await load(facetsRequest(w, [], [], { candidates: true, at }));
    if (!first) return;
    candidates = first.candidates ?? [];
    answer = first;
    if (!candidates.length) return;
    const bins = binsFor(first.size);
    specs = candidates
      .slice(0, FIRST_FACETS)
      .map((c) => ({ property: c.property, kind: c.kind, bins }));
    await count();
  }

  /** Count the shown facets under the brushes. */
  async function count() {
    if (!within || !specs.length) return;
    const used = brushList;
    const req = facetsRequest(within, specs, used, { at });
    const a = await load(req);
    if (!a) return;
    answer = a;
    answered = JSON.stringify(used);
    // keep the frame of each facet, so its bins stay while others are brushed
    specs = specs.map((s) => {
      if (s.edges || s.bars) return s;
      const f = a.facets.find((x) => x.property === s.property);
      return f && !f.error ? frameOf(f) : s;
    });
    // a facet that no other brush filters shows the counts of the whole set, which stay
    // behind it as a ghost once other facets are brushed
    const next = { ...ghosts };
    for (const f of a.facets)
      if (!f.error && !used.some((b) => b.property !== f.property)) next[f.property] = f;
    ghosts = next;
    live = used.length
      ? `${used.map((b) => brushText(b, short(b.property), (t) => termLabel(t))).join(', ')}: ${fmtInt(a.matching)} of ${fmtInt(a.size)} match`
      : `${fmtInt(a.size)} ${setLabel}`;
  }

  function later() {
    clearTimeout(timer);
    timer = setTimeout(() => void count(), BRUSH_PAUSE_MS);
  }

  $effect(() => {
    const w = within;
    void at;
    void ds;
    untrack(() => {
      clearTimeout(timer);
      if (w) void open(w);
      else {
        controller?.abort();
        answer = null;
        specs = [];
        candidates = [];
        brushes = {};
      }
    });
  });

  $effect(() => () => {
    clearTimeout(timer);
    controller?.abort();
    dimController?.abort();
  });

  function setBrush(property: string, b: Brush | null) {
    const next = { ...brushes };
    if (b) next[property] = b;
    else delete next[property];
    brushes = next;
    later();
  }

  function clearAll() {
    if (!brushList.length) return;
    brushes = {};
    later();
  }

  function addFacet(property: string) {
    if (!property || specs.some((s) => s.property === property) || specs.length >= MAX_FACETS)
      return;
    const c = candidates.find((x) => x.property === property);
    specs = [...specs, { property, kind: c?.kind, bins: binsFor(answer?.size ?? 0) }];
    void count();
  }

  function removeFacet(property: string) {
    specs = specs.filter((s) => s.property !== property);
    if (brushes[property]) setBrush(property, null);
  }

  const shown = $derived(
    specs.map((s) => ({
      spec: s,
      facet: answer?.facets.find((f) => f.property === s.property) ?? null,
    })),
  );
  const addable = $derived(candidates.filter((c) => !specs.some((s) => s.property === c.property)));

  // ----------------------------------------------------- dimming the drawn entities ----

  /** Values of the brushed properties for the drawn entities, per property and `at`. */
  let valueCache = new Map<string, Map<string, Term[]>>();
  let cacheKey = '';
  let dimController: AbortController | null = null;

  async function valuesOf(property: string, ids: string[], signal: AbortSignal) {
    const key = `${ds}|${at}`;
    if (key !== cacheKey) {
      valueCache = new Map();
      cacheKey = key;
    }
    let m = valueCache.get(property);
    if (!m) {
      m = new Map();
      valueCache.set(property, m);
    }
    const missing = ids.filter((id) => id.startsWith('<') && !m!.has(id));
    for (let i = 0; i < missing.length; i += 200) {
      const batch = missing.slice(i, i + 200);
      const rows = await api.select(ds, valuesQuery(property, batch), {
        at: at || undefined,
        reasoning: reasoning ?? undefined,
        signal,
      });
      for (const id of batch) m.set(id, []);
      for (const r of rows) if (r.s && r.v) m.get(toSparql(r.s))?.push(r.v);
    }
    return m;
  }

  $effect(() => {
    const list = brushList;
    const ids = drawn;
    untrack(() => {
      dimController?.abort();
      if (!onmatch) return;
      if (!list.length || !ids) {
        onmatch(null, '');
        return;
      }
      const c = new AbortController();
      dimController = c;
      void (async () => {
        try {
          for (const b of list) await valuesOf(b.property, ids, c.signal);
          if (c.signal.aborted) return;
          const kept = keptBy(ids, list, valueCache);
          onmatch(kept, list.map((b) => brushText(b, short(b.property))).join(', '));
        } catch (e) {
          if (!isAbort(e)) error = api.errorMessage(e);
        }
      })();
    });
  });

  // ---------------------------------------------------------------- histograms ------

  /** The bin under the keyboard cursor and the anchor of a Shift extension, per facet. */
  let cursor = $state<Record<string, number>>({});
  let anchorBin: Record<string, number> = {};
  let dragging: { property: string; from: number } | null = null;

  function binAt(e: PointerEvent, el: HTMLElement, n: number): number {
    const r = el.getBoundingClientRect();
    const x = Math.min(Math.max(e.clientX - r.left, 0), r.width - 1);
    return Math.min(n - 1, Math.max(0, Math.floor((x / r.width) * n)));
  }

  function brushBins(f: FacetAnswer, i: number, j: number) {
    setBrush(f.property, binsBrush(f, i, j));
  }

  function histDown(e: PointerEvent, f: FacetAnswer) {
    const n = f.bins?.length ?? 0;
    if (!n || e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture?.(e.pointerId);
    const i = binAt(e, el, n);
    dragging = { property: f.property, from: i };
    cursor = { ...cursor, [f.property]: i };
    brushBins(f, i, i);
  }

  function histMove(e: PointerEvent, f: FacetAnswer) {
    if (!dragging || dragging.property !== f.property) return;
    const n = f.bins?.length ?? 0;
    const i = binAt(e, e.currentTarget as HTMLElement, n);
    const cur = brushedBins(f, brushes[f.property]);
    const [a, z] = dragging.from <= i ? [dragging.from, i] : [i, dragging.from];
    if (!cur || cur[0] !== a || cur[1] !== z) brushBins(f, a, z);
    cursor = { ...cursor, [f.property]: i };
  }

  function histUp() {
    dragging = null;
  }

  function histKey(e: KeyboardEvent, f: FacetAnswer) {
    const n = f.bins?.length ?? 0;
    if (!n) return;
    const p = f.property;
    const at_ = cursor[p] ?? 0;
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
      e.preventDefault();
      const next = Math.min(n - 1, Math.max(0, at_ + (e.key === 'ArrowLeft' ? -1 : 1)));
      if (e.shiftKey) {
        const from = anchorBin[p] ?? at_;
        anchorBin[p] = from;
        brushBins(f, from, next);
      } else anchorBin[p] = next;
      cursor = { ...cursor, [p]: next };
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      const next = e.key === 'Home' ? 0 : n - 1;
      anchorBin[p] = next;
      cursor = { ...cursor, [p]: next };
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      const cur = brushedBins(f, brushes[p]);
      anchorBin[p] = at_;
      if (cur && cur[0] === at_ && cur[1] === at_) setBrush(p, null);
      else brushBins(f, at_, at_);
    } else if (e.key === 'Escape' && brushes[p]) {
      e.preventDefault();
      setBrush(p, null);
    }
  }

  /** The two ends of a histogram's brush, as edge indexes from 0 to the bin count. */
  function ends(f: FacetAnswer): [number, number] {
    const n = f.bins?.length ?? 0;
    const cur = brushedBins(f, brushes[f.property]);
    return cur ? [cur[0], cur[1] + 1] : [0, n];
  }

  function setEnd(f: FacetAnswer, which: 0 | 1, edge: number) {
    const n = f.bins?.length ?? 0;
    const [a, z] = ends(f);
    let lo = which === 0 ? edge : a;
    let hi = which === 1 ? edge : z;
    lo = Math.max(0, Math.min(lo, n - 1));
    hi = Math.min(n, Math.max(hi, lo + 1));
    if (lo === 0 && hi === n) setBrush(f.property, null);
    else brushBins(f, lo, hi - 1);
  }

  function handleKey(e: KeyboardEvent, f: FacetAnswer, which: 0 | 1) {
    const n = f.bins?.length ?? 0;
    const cur = ends(f)[which];
    let next: number | null = null;
    if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') next = cur - 1;
    else if (e.key === 'ArrowRight' || e.key === 'ArrowUp') next = cur + 1;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = n;
    else if (e.key === 'Escape' && brushes[f.property]) {
      e.preventDefault();
      setBrush(f.property, null);
      return;
    }
    if (next === null) return;
    e.preventDefault();
    setEnd(f, which, next);
  }

  let handleDrag: { property: string; which: 0 | 1; track: HTMLElement } | null = null;

  function handleDown(e: PointerEvent, f: FacetAnswer, which: 0 | 1) {
    const el = e.currentTarget as HTMLElement;
    const track = el.parentElement as HTMLElement;
    el.setPointerCapture?.(e.pointerId);
    handleDrag = { property: f.property, which, track };
    e.stopPropagation();
  }

  function handleMove(e: PointerEvent, f: FacetAnswer) {
    if (!handleDrag || handleDrag.property !== f.property) return;
    const n = f.bins?.length ?? 0;
    const r = handleDrag.track.getBoundingClientRect();
    const edge = Math.round(((e.clientX - r.left) / Math.max(r.width, 1)) * n);
    const [a, z] = ends(f);
    if ((handleDrag.which === 0 ? a : z) !== edge) setEnd(f, handleDrag.which, edge);
  }

  function edgeText(f: FacetAnswer, edge: number): string {
    const bins = f.bins ?? [];
    if (!bins.length) return '';
    return edge >= bins.length ? String(bins[bins.length - 1].to) : String(bins[edge].from);
  }

  function valueText(f: FacetAnswer): string {
    const name = short(f.property);
    const b = brushes[f.property];
    const n = b ? (answer?.matching ?? 0) : (answer?.size ?? 0);
    return b
      ? `${brushText(b, name)}, ${fmtInt(n)} entities`
      : `${name}, every value, ${fmtInt(n)} entities`;
  }

  const maxOf = (f: FacetAnswer | null | undefined, g: FacetAnswer | undefined) =>
    Math.max(
      1,
      ...(f?.bins ?? []).map((b) => b.count),
      ...(g?.bins ?? []).map((b) => b.count),
      ...(f?.values ?? []).map((v) => v.count),
      ...(g?.values ?? []).map((v) => v.count),
    );

  const pressed = (f: FacetAnswer, v: Term) => {
    const b = brushes[f.property];
    return !!b && b.kind === 'values' && b.values.some((x) => toSparql(x) === toSparql(v));
  };
</script>

<section class="facets" aria-label="Facets" data-testid="facet-panel">
  <div class="fhead">
    <h2>
      Facets
      {#if answer}<span class="faint">· {fmtInt(answer.size)} {setLabel}</span>{/if}
    </h2>
    <span class="spacer"></span>
    {#if addable.length && specs.length < MAX_FACETS}
      <label class="add">
        <span class="sr-only">Add a facet</span>
        <select
          aria-label="Add a facet"
          value=""
          onchange={(e) => {
            addFacet((e.currentTarget as HTMLSelectElement).value);
            (e.currentTarget as HTMLSelectElement).value = '';
          }}
        >
          <option value="">+ facet</option>
          {#each addable as c (c.property)}
            <option value={c.property}
              >{short(c.property)} ({KIND_LABEL[c.kind]}, {Math.round(c.share * 100)}%)</option
            >
          {/each}
        </select>
      </label>
    {/if}
    {#if onclose}
      <button class="btn sm ghost icon" aria-label="Close the facets" onclick={onclose}
        ><Icon name="x" size={14} /></button
      >
    {/if}
  </div>
  {#if header}<div class="fsub">{@render header()}</div>{/if}

  <div class="fbody">
    {#if error}
      <div class="error-box" role="alert">{error}</div>
    {/if}
    {#if !within}
      <p class="faint empty">Choose a set to facet.</p>
    {:else if answer && !candidates.length && !specs.length}
      <p class="faint empty">These entities have no literal properties to facet.</p>
    {:else if !answer && loading}
      <p class="faint empty">Counting…</p>
    {/if}

    {#each shown as { spec, facet: f } (spec.property)}
      {@const g = ghosts[spec.property]}
      {@const max = maxOf(f, g)}
      <div
        class="facet"
        data-property={spec.property}
        data-testid="facet"
        class:brushed={!!brushes[spec.property]}
      >
        <div class="frow">
          <span class="fname" title={spec.property}>{short(spec.property)}</span>
          {#if f?.kind}<span class="faint kind">{KIND_LABEL[f.kind]}</span>{/if}
          <span class="spacer"></span>
          {#if f?.bins?.length}
            <button
              class="btn sm ghost"
              aria-pressed={!!tables[spec.property]}
              onclick={() => (tables = { ...tables, [spec.property]: !tables[spec.property] })}
              >Table</button
            >
          {/if}
          {#if brushes[spec.property]}
            <button class="btn sm ghost" onclick={() => setBrush(spec.property, null)}>Clear</button
            >
          {/if}
          <button
            class="btn sm ghost icon"
            aria-label={`Remove the facet ${short(spec.property)}`}
            onclick={() => removeFacet(spec.property)}><Icon name="x" size={12} /></button
          >
        </div>
        {#if !f}
          <p class="faint small">Counting…</p>
        {:else if f.error}
          <p class="small warn-text" role="status">
            This facet is over the {f.error.budget} budget: {f.error.message}
          </p>
        {:else if f.bins}
          {@const cur = brushedBins(f, brushes[f.property])}
          {@const [lo, hi] = ends(f)}
          {#if !f.bins.length}
            <p class="faint small">No values of this kind.</p>
          {:else}
            <!-- the histogram is a composite widget whose keys its label names -->
            <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
            <div
              class="hist"
              role="group"
              tabindex="0"
              aria-label={`${short(f.property)} histogram, ${f.bins.length} bins. Arrows move, Shift with arrows brushes, Enter brushes a bin.`}
              onpointerdown={(e) => histDown(e, f)}
              onpointermove={(e) => histMove(e, f)}
              onpointerup={histUp}
              onpointercancel={histUp}
              onkeydown={(e) => histKey(e, f)}
            >
              {#each f.bins as b, i (i)}
                {@const gb = g?.bins?.[i]}
                <div
                  class="bin"
                  class:in={!!cur && i >= cur[0] && i <= cur[1]}
                  class:focus={cursor[f.property] === i}
                  title={`${binLabel(f, i)}: ${fmtInt(b.count)}`}
                >
                  {#if gb}<span class="ghost" style:height={`${(gb.count / max) * 100}%`}
                    ></span>{/if}
                  <span class="bar" style:height={`${(b.count / max) * 100}%`}></span>
                </div>
              {/each}
            </div>
            <div class="track" onpointermove={(e) => handleMove(e, f)} role="presentation">
              <span
                class="range"
                style:left={`${(lo / f.bins.length) * 100}%`}
                style:right={`${100 - (hi / f.bins.length) * 100}%`}
              ></span>
              {#each [0, 1] as const as which (which)}
                {@const edge = which === 0 ? lo : hi}
                <span
                  class="handle"
                  role="slider"
                  tabindex="0"
                  aria-label={`${short(f.property)} ${which === 0 ? 'from' : 'to'}`}
                  aria-valuemin={0}
                  aria-valuemax={f.bins.length}
                  aria-valuenow={edge}
                  aria-valuetext={valueText(f)}
                  aria-orientation="horizontal"
                  style:left={`${(edge / f.bins.length) * 100}%`}
                  onpointerdown={(e) => handleDown(e, f, which)}
                  onpointerup={() => (handleDrag = null)}
                  onpointercancel={() => (handleDrag = null)}
                  onkeydown={(e) => handleKey(e, f, which)}
                ></span>
              {/each}
            </div>
            <div class="axis faint">
              <span>{edgeText(f, 0)}</span>
              <span class="small brushtext"
                >{cur
                  ? `${binLabel(f, cur[0]).split('–')[0]}–${binLabel(f, cur[1]).split('–').at(-1)}`
                  : ''}{cur && f.missing ? ' · ' : ''}{f.missing
                  ? `${fmtInt(f.missing)} without`
                  : ''}</span
              >
              <span>{edgeText(f, f.bins.length)}</span>
            </div>
            {#if tables[f.property]}
              <table class="bins">
                <caption class="sr-only">{short(f.property)} bins</caption>
                <thead><tr><th scope="col">Range</th><th scope="col">Entities</th></tr></thead>
                <tbody>
                  {#each f.bins as b, i (i)}
                    <tr class:in={!!cur && i >= cur[0] && i <= cur[1]}
                      ><td>{binLabel(f, i)}</td><td class="num">{fmtInt(b.count)}</td></tr
                    >
                  {/each}
                  {#if f.missing}<tr
                      ><td class="faint">No value</td><td class="num">{fmtInt(f.missing)}</td></tr
                    >{/if}
                </tbody>
              </table>
            {/if}
          {/if}
        {:else if f.values}
          {#if !f.values.length}
            <p class="faint small">No values.</p>
          {:else}
            <ul class="bars">
              {#each f.values as v (toSparql(v.value))}
                <li>
                  <button
                    class="val"
                    aria-pressed={pressed(f, v.value)}
                    onclick={() =>
                      setBrush(f.property, toggleValue(brushes[f.property], f.property, v.value))}
                  >
                    <span class="vlabel" title={toSparql(v.value)}
                      >{termLabel(v.value, v.label)}</span
                    >
                    <span class="vbar"><span style:width={`${(v.count / max) * 100}%`}></span></span
                    >
                    <span class="num">{fmtInt(v.count)}</span>
                  </button>
                </li>
              {/each}
            </ul>
            {#if f.other || f.missing}
              <p class="faint small">
                {#if f.other}{fmtInt(f.other)} with other values{/if}{#if f.other && f.missing}
                  ·
                {/if}{#if f.missing}{fmtInt(f.missing)} without{/if}
              </p>
            {/if}
          {/if}
        {/if}
      </div>
    {/each}
  </div>

  {#if answer && specs.length}
    <div class="fcount" data-testid="facet-count">
      {#if fresh}
        <span
          ><strong>{fmtInt(brushList.length ? answer.matching : answer.size)}</strong>
          {#if brushList.length}of {fmtInt(answer.size)} match{:else}{setLabel}{/if}</span
        >
      {:else}
        <span class="faint">Counting…</span>
      {/if}
      <span class="spacer"></span>
      {#if onapply}
        <button
          class="btn sm"
          disabled={!brushList.length}
          title="Add the brushes to the query as triples and filters"
          onclick={() => onapply?.(brushList)}>{applyLabel}</button
        >
      {/if}
      <button class="btn sm ghost" disabled={!brushList.length} onclick={clearAll}>Clear</button>
    </div>
  {/if}
  <p class="sr-only" aria-live="polite">{live}</p>
</section>

<style>
  .facets {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    background: var(--surface);
  }
  .fhead {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px 8px;
    border-bottom: 1px solid var(--border);
  }
  .fhead h2 {
    font-size: var(--fs-md);
    margin: 0;
    font-weight: 600;
  }
  .fsub {
    padding: 6px 12px;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .add select {
    font-size: var(--fs-sm);
    max-width: 180px;
  }
  .fbody {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 6px 12px 10px;
  }
  .empty {
    padding: 12px 0;
  }
  .facet {
    padding: 8px 0 10px;
    border-bottom: 1px solid var(--border);
  }
  .facet:last-child {
    border-bottom: 0;
  }
  .frow {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
    font-size: var(--fs-sm);
  }
  .fname {
    font-family: var(--font-mono);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .kind {
    font-size: var(--fs-xs);
  }
  .hist {
    display: flex;
    align-items: flex-end;
    gap: 1px;
    height: 64px;
    cursor: crosshair;
    touch-action: none;
    border-radius: var(--r-sm);
    padding: 2px;
  }
  .hist:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  .bin {
    position: relative;
    flex: 1;
    height: 100%;
    display: flex;
    align-items: flex-end;
  }
  .bin .ghost,
  .bin .bar {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    border-radius: 2px 2px 0 0;
  }
  .bin .ghost {
    background: var(--border);
  }
  .bin .bar {
    background: var(--text);
    opacity: 0.38;
    min-height: 1px;
  }
  .brushed .bin .bar {
    opacity: 0.22;
  }
  .bin.in .bar {
    background: var(--spark);
    opacity: 1;
  }
  .bin.in {
    /* the brush is shading as well as colour */
    background: var(--spark-soft);
  }
  .hist:focus-visible .bin.focus {
    box-shadow: inset 0 -3px 0 var(--focus);
  }
  .track {
    position: relative;
    height: 18px;
    margin: 2px 2px 0;
    touch-action: none;
  }
  .track::before {
    content: '';
    position: absolute;
    left: 0;
    right: 0;
    top: 8px;
    height: 2px;
    background: var(--border);
  }
  .track .range {
    position: absolute;
    top: 7px;
    height: 4px;
    background: var(--spark);
    border-radius: 2px;
  }
  .handle {
    position: absolute;
    top: 1px;
    width: 14px;
    height: 16px;
    margin-left: -7px;
    border-radius: 4px;
    background: var(--surface);
    border: 2px solid var(--spark);
    cursor: ew-resize;
    touch-action: none;
  }
  .handle:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  .axis {
    display: flex;
    justify-content: space-between;
    font-size: var(--fs-xs);
    font-variant-numeric: tabular-nums;
    margin-top: 2px;
  }
  .brushtext {
    color: var(--text);
  }
  table.bins {
    width: 100%;
    margin-top: 6px;
    font-size: var(--fs-xs);
    border-collapse: collapse;
  }
  table.bins th,
  table.bins td {
    text-align: left;
    padding: 2px 4px;
    border-bottom: 1px solid var(--border);
  }
  table.bins tr.in td {
    background: var(--spark-soft);
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  ul.bars {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .val {
    width: 100%;
    display: grid;
    grid-template-columns: minmax(0, 9fr) minmax(40px, 6fr) auto;
    align-items: center;
    gap: 8px;
    padding: 3px 6px;
    border: 1px solid transparent;
    border-radius: var(--r-sm);
    background: none;
    color: inherit;
    font: inherit;
    font-size: var(--fs-sm);
    text-align: left;
    cursor: pointer;
  }
  .val:hover {
    background: var(--hover);
  }
  .val[aria-pressed='true'] {
    background: var(--spark-soft);
    border-color: var(--spark);
  }
  .vlabel {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .vbar {
    height: 8px;
    background: var(--border);
    border-radius: 2px;
    overflow: hidden;
  }
  .vbar span {
    display: block;
    height: 100%;
    background: var(--text);
    opacity: 0.45;
  }
  .val[aria-pressed='true'] .vbar span {
    background: var(--spark);
    opacity: 1;
  }
  .fcount {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    padding: 8px 12px;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .warn-text {
    color: var(--warn);
  }
  .small {
    font-size: var(--fs-xs);
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  @media (max-width: 760px) {
    .handle {
      /* a 44 px target around the visible handle */
      width: 22px;
      height: 22px;
      margin-left: -11px;
      top: -2px;
    }
    .handle::after {
      content: '';
      position: absolute;
      inset: -11px;
    }
    /* room at both ends for the wider handles, with the bars above them in line */
    .track {
      height: 26px;
      margin: 8px 11px 0;
    }
    .hist,
    .axis {
      margin-left: 9px;
      margin-right: 9px;
    }
    .val {
      min-height: 44px;
    }
    .fhead :global(.btn),
    .fcount :global(.btn),
    .frow :global(.btn) {
      min-height: 44px;
      min-width: 44px;
    }
    .add select {
      min-height: 44px;
    }
  }
</style>
