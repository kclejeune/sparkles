<script lang="ts">
  // The path finder of Explore (docs/specs/U09-path-finder.md §2): two entities, a mode
  // and filters, one `SERVICE path:search` query at Explore's state, and the paths found
  // as a listbox that the graph follows.
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { onDestroy, untrack } from 'svelte';
  import * as api from '$lib/api';
  import type { Term } from '$lib/api';
  import { app } from '$lib/app.svelte';
  import * as ex from '$lib/explore';
  import { fmtInt } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { displayIri, termKey } from '$lib/rdf';
  import { datasetKindUrl, readKind } from '$lib/settings';
  import * as pf from '$lib/paths';
  import EntityPicker, { type Picked } from './EntityPicker.svelte';
  import Icon from './Icon.svelte';

  let {
    ds,
    prefixes,
    at = '',
    branch = 'main',
    from = $bindable(null),
    to = $bindable(null),
    highlight = $bindable(null),
    onresult,
    onadd,
    onclear,
    onclose,
    onfit,
  }: {
    ds: string;
    prefixes: Record<string, string>;
    /** The state of the page (`at=`), empty for the head. */
    at?: string;
    /** The branch the page reads, for the header. */
    branch?: string;
    from?: Picked | null;
    to?: Picked | null;
    /** The path the graph highlights, null for all. */
    highlight?: number | null;
    /** The paths found, with the labels of their nodes, or null when cleared. */
    onresult?: (r: { paths: pf.FoundPath[]; labels: Map<string, string> } | null) => void;
    /** Keeps the paths in the page's graph; without it there is no **Add to graph**. */
    onadd?: () => void;
    onclear?: () => void;
    onclose?: () => void;
    onfit?: (path: pf.FoundPath) => void;
  } = $props();

  let mode = $state<pf.PathMode>('shortest');
  let k = $state(3);
  let minLength = $state(1);
  let maxLength = $state(6);
  let direction = $state<pf.PathDirection>('both');
  let maxDegree = $state<number | null>(1000);
  let follow = $state<'except' | 'only'>('except');
  let excluded = $state<string[]>([...pf.DEFAULT_EXCLUDED]);
  let only = $state<string[]>([]);
  /** The person changed the skipped predicates, so the dataset's defaults leave them. */
  let excludedTouched = false;
  // the dataset's defaults (the `paths` settings kind), else the built-in ones
  $effect(() => {
    const name = ds;
    const c = new AbortController();
    readKind(datasetKindUrl(name, 'paths'), c.signal)
      .then((k) => {
        const list = k.effective?.excludedPredicates;
        if (!excludedTouched && Array.isArray(list) && list.every((p) => typeof p === 'string'))
          excluded = list as string[];
      })
      .catch(() => {
        // a server without the kind: the built-in defaults stay
      });
    return () => c.abort();
  });
  /** False once the server refused the Phase 1 parameters. */
  let phase1 = $state(true);

  // --- weights from reifiers (U09 Phase 3) ------------------------------------------------

  /** The reifier property whose values weigh the edges, or null to count edges. */
  let weight = $state<string | null>(null);
  /** The weight properties of the dataset at the page's state, once read. */
  let weights = $state<pf.WeightProperty[] | null>(null);
  let weightsErr = $state('');
  let weightsFor = $state('');
  let weightCtl: AbortController | undefined;
  function loadWeights() {
    const key = `${ds}|${at}`;
    if (weightsFor === key || !ds) return;
    weightsFor = key;
    weights = null;
    weightsErr = '';
    const c = new AbortController();
    weightCtl?.abort();
    weightCtl = c;
    pf.weightProperties(ds, { at: at || undefined, signal: c.signal })
      .then((w) => {
        if (weightCtl !== c) return;
        weights = w;
        // a property the state no longer has is dropped
        if (weight && !w.some((x) => x.iri === weight)) weight = null;
      })
      .catch((e) => {
        if (weightCtl !== c || (e instanceof DOMException && e.name === 'AbortError')) return;
        weights = [];
        weightsErr = 'The weight properties could not be read';
      });
  }
  onDestroy(() => weightCtl?.abort());

  let running = $state(false);
  let failure = $state<pf.SearchFailure | null>(null);
  /** The paths found, with the state, the settings and the query that found them. */
  let result = $state<
    (pf.PathResult & { at: string; settings: pf.PathSettings; query: string }) | null
  >(null);
  let labels = $state(new Map<string, string>());
  let announce = $state('');
  let ctl: AbortController | undefined;
  /** Stops the search in flight, so that nothing it returns is shown. */
  function cancel() {
    ctl?.abort();
    ctl = undefined;
  }
  onDestroy(cancel);

  let fromPicker: EntityPicker | undefined = $state();
  let list: HTMLDivElement | undefined = $state();
  /** The filters are folded on a phone. */
  const wide = typeof matchMedia === 'function' ? !matchMedia('(max-width: 600px)').matches : true;
  let filtersOpen = $state(wide);
  // the weight picker is filled when it is first used, since listing the weight properties
  // reads every reifier of the dataset, and again at a new state
  $effect(() => {
    void ds;
    void at;
    untrack(() => {
      if (weightsFor) {
        weightsFor = '';
        weights = null;
      }
    });
  });
  /** On a phone, after a search the sheet shows the first path only until it is swiped up. */
  let sheetFull = $state(true);

  const settings = $derived.by((): pf.PathSettings | null => {
    if (!from || !to) return null;
    return {
      source: from.term,
      target: to.term,
      mode,
      k,
      minLength,
      maxLength,
      direction,
      maxDegree: phase1 ? maxDegree : null,
      follow: phase1 ? follow : 'only',
      predicates: phase1 && follow === 'except' ? excluded : only,
      weight,
    };
  });
  const blocked = $derived(
    !from || !to ? 'Choose two entities' : settings ? pf.invalidReason(settings) : null,
  );
  /** Explore moved to another state since the search. */
  const stale = $derived(!!result && result.at !== at);

  export function focus() {
    fromPicker?.focus();
  }

  function swap() {
    if (to?.term.type === 'literal') return;
    [from, to] = [to, from];
  }

  async function run() {
    if (!settings || blocked || !ds) return;
    ctl?.abort();
    const c = (ctl = new AbortController());
    const s = $state.snapshot(settings) as pf.PathSettings;
    // the state and the query of this search, which the slider may move away from
    const runAt = at;
    const runPhase1 = phase1;
    const live = () => ctl === c && !c.signal.aborted;
    running = true;
    failure = null;
    announce = '';
    try {
      const r = await pf.findPaths(ds, s, {
        at: runAt || undefined,
        phase1: runPhase1,
        signal: c.signal,
      });
      if (!live()) return;
      // the labels of the nodes, from the lookup Explore already uses
      const iris = new Set<string>();
      for (const p of r.paths) for (const t of p.nodes) if (t.type === 'uri') iris.add(t.value);
      for (const p of [from, to]) if (p?.term.type === 'uri') iris.add(p.term.value);
      let found = new Map<string, string>();
      try {
        found = await ex.labels(ds, [...iris]);
      } catch {
        // labels are a nicety: the paths stand without them
      }
      if (!live()) return;
      labels = found;
      result = { ...r, at: runAt, settings: s, query: pf.pathQuery(s, runPhase1) };
      highlight = null;
      sheetFull = wide || r.paths.length <= 1;
      announce = pf.announcement(r.paths);
      onresult?.({ paths: r.paths, labels: found });
    } catch (e) {
      if (!live() || (e instanceof DOMException && e.name === 'AbortError')) return;
      failure = pf.classifyFailure(e);
      if (failure.kind === 'unsupported') {
        phase1 = false;
        follow = 'only';
      }
      announce = failure.message;
    } finally {
      if (ctl === c) running = false;
    }
  }

  function clear() {
    cancel();
    running = false;
    result = null;
    failure = null;
    highlight = null;
    announce = '';
    onresult?.(null);
    onclear?.();
  }

  function labelOf(t: Term): string {
    if (t.type === 'uri') return labels.get(t.value) ?? shortLabel(t.value, prefixes);
    if (t.type === 'literal')
      return `“${t.value.length > 40 ? t.value.slice(0, 37) + '…' : t.value}”`;
    return termKey(t);
  }

  /** The query page opens with the query that found the paths shown, at its state. */
  function openInEditor() {
    if (!result) return;
    app.pendingQuery = { query: result.query, title: 'Paths' };
    app.queryAt = result.at;
    void goto(resolve('/query'));
  }

  function choose(n: number | null) {
    highlight = n;
    if (n != null) {
      const p = result?.paths.find((x) => x.n === n);
      if (p) onfit?.(p);
      list?.querySelector<HTMLElement>(`[data-path="${n}"]`)?.scrollIntoView({ block: 'nearest' });
    }
  }

  function onListKey(e: KeyboardEvent) {
    const paths = result?.paths ?? [];
    if (!paths.length) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const cur = highlight ?? 0;
      const next =
        e.key === 'ArrowDown' ? Math.min(paths.length, cur + 1) : Math.max(1, cur - 1 || 1);
      choose(next);
    } else if (e.key === 'Home') {
      e.preventDefault();
      choose(1);
    } else if (e.key === 'End') {
      e.preventDefault();
      choose(paths.length);
    }
  }

  /** The panel's own keys: Escape clears the highlight, then closes the panel. */
  function onPanelKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      if (highlight != null) highlight = null;
      else onclose?.();
      return;
    }
    const inField = (e.target as HTMLElement)?.closest?.('input, select, textarea');
    if (inField || e.metaKey || e.ctrlKey || e.altKey) return;
    if (/^[1-9]$/.test(e.key) && result && Number(e.key) <= result.paths.length) {
      e.preventDefault();
      choose(Number(e.key));
    } else if ((e.key === 'a' || e.key === 'A') && result) {
      e.preventDefault();
      choose(null);
    }
  }

  // --- the predicate picker: the schema's predicates under the caller's view -------------

  let schemaPreds = $state<{ iri: string; label?: string }[]>([]);
  let schemaLoaded = false;
  async function loadPredicates() {
    if (schemaLoaded || !ds) return;
    schemaLoaded = true;
    try {
      const s = await ex.loadSchema(ds, { at: at || undefined });
      schemaPreds = s.properties.map((p) => ({ iri: p.iri, label: p.label }));
    } catch {
      // the picker still takes typed IRIs
    }
  }
  let predText = $state('');
  function expandName(q: string): string | null {
    const t = q.trim().replace(/^<(.*)>$/, '$1');
    if (/^(https?|urn):/i.test(t)) return t;
    const pn = /^([A-Za-z][\w.-]*)?:(\S*)$/.exec(t);
    if (pn && prefixes[pn[1] ?? ''] != null) return prefixes[pn[1] ?? ''] + pn[2];
    const byLabel = schemaPreds.find(
      (p) => p.label?.toLowerCase() === t.toLowerCase() || shortLabel(p.iri, prefixes) === t,
    );
    return byLabel?.iri ?? null;
  }
  let predErr = $state('');
  function addPredicate() {
    const iri = expandName(predText);
    if (!iri) {
      predErr = predText.trim() ? 'Type an IRI, a prefixed name or a predicate of the schema' : '';
      return;
    }
    predErr = '';
    predText = '';
    if (follow === 'except' && phase1) {
      excludedTouched = true;
      if (!excluded.includes(iri)) excluded = [...excluded, iri];
    } else if (!only.includes(iri)) only = [...only, iri];
  }
  function removePredicate(iri: string) {
    if (follow === 'except' && phase1) {
      excludedTouched = true;
      excluded = excluded.filter((p) => p !== iri);
    } else only = only.filter((p) => p !== iri);
  }
  const shownPreds = $derived(follow === 'except' && phase1 ? excluded : only);

  const lengthText = (n: number) => `${n} ${n === 1 ? 'edge' : 'edges'}`;
  const hubText = (n: number | null) => (n == null ? 'off' : fmtInt(n));

  // --- the phone sheet: a swipe on the handle shows or folds the list ---------------------

  let swipeY: number | null = null;
  function onHandleDown(e: PointerEvent) {
    swipeY = e.clientY;
  }
  function onHandleUp(e: PointerEvent) {
    if (swipeY == null) return;
    const dy = e.clientY - swipeY;
    swipeY = null;
    if (dy < -24) sheetFull = true;
    else if (dy > 24) sheetFull = false;
    else sheetFull = !sheetFull;
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section class="paths" class:folded={!sheetFull} aria-label="Paths" onkeydown={onPanelKey}>
  <div class="head">
    <h2><Icon name="route" size={15} /> Paths</h2>
    <button
      class="btn sm icon ghost"
      aria-label="Close paths"
      title="Close (Escape)"
      onclick={onclose}><Icon name="x" size={14} /></button
    >
  </div>
  <form
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      void run();
    }}
  >
    <div class="ends">
      <EntityPicker
        bind:this={fromPicker}
        id="path-from"
        label="From"
        {ds}
        {prefixes}
        bind:value={from}
      />
      <button
        type="button"
        class="btn sm icon swap"
        aria-label="Swap from and to"
        title={to?.term.type === 'literal' ? 'A literal cannot start a path' : 'Swap from and to'}
        disabled={to?.term.type === 'literal'}
        onclick={swap}><Icon name="swap" size={14} /></button
      >
      <EntityPicker id="path-to" label="To" {ds} {prefixes} bind:value={to} literals />
    </div>
    <fieldset class="modes">
      <legend>Mode</legend>
      <label><input type="radio" bind:group={mode} value="shortest" /> shortest</label>
      <label><input type="radio" bind:group={mode} value="allShortest" /> all shortest</label>
      <label><input type="radio" bind:group={mode} value="kShortest" /> k shortest</label>
      {#if mode === 'kShortest'}
        <label class="k"
          >k <select bind:value={k} aria-label="Paths to find">
            {#each Array.from({ length: 19 }, (_, i) => i + 2) as n (n)}<option value={n}
                >{n}</option
              >{/each}
          </select></label
        >
      {/if}
    </fieldset>
    <details class="filters" bind:open={filtersOpen}>
      <summary>Filters</summary>
      <div class="frow">
        <label
          >Length {#if mode === 'kShortest'}<select
              bind:value={minLength}
              aria-label="Fewest edges"
            >
              {#each Array.from({ length: pf.MAX_LENGTH }, (_, i) => i + 1) as n (n)}<option
                  value={n}>{n}</option
                >{/each}
            </select> to{:else}up to{/if}
          <select bind:value={maxLength} aria-label="Most edges">
            {#each Array.from({ length: pf.MAX_LENGTH }, (_, i) => i + 1) as n (n)}<option value={n}
                >{n}</option
              >{/each}
          </select> edges</label
        >
        <label
          >Direction <select bind:value={direction}>
            <option value="both">both</option>
            <option value="forward">forward</option>
            <option value="backward">backward</option>
          </select></label
        >
        <label title={phase1 ? 'Paths do not pass through a node with more edges' : ''}
          >Skip hubs over <select
            bind:value={maxDegree}
            disabled={!phase1}
            aria-describedby={phase1 ? undefined : 'path-phase1'}
          >
            {#each pf.HUB_LIMITS as n (n ?? 'off')}<option value={n}>{hubText(n)}</option>{/each}
          </select></label
        >
        <label
          title="Add up a property of each edge's reifier instead of counting edges. An edge without one weighs 1."
          >Weight <select
            bind:value={weight}
            aria-describedby="path-weight-note"
            onfocus={loadWeights}
            onpointerdown={loadWeights}
          >
            <option value={null}>none, count edges</option>
            {#each weights ?? [] as w (w.iri)}<option value={w.iri}
                >{displayIri(w.iri, prefixes)}</option
              >{/each}
          </select></label
        >
      </div>
      <p class="faint small" id="path-weight-note">
        {#if weights == null && weightsFor}
          Reading the weight properties…
        {:else if weights == null}
          Choose a property of the reifiers to add up instead of counting edges.
        {:else if weightsErr}
          {weightsErr}.
        {:else if !weights.length}
          No reifier has a numeric property to weigh edges by.
        {:else if weight}
          Paths are ordered by the sum of {displayIri(weight, prefixes)} on their edges' reifiers, and
          an edge without one weighs 1.
        {:else}
          {weights.length}
          {weights.length === 1 ? 'property' : 'properties'} on reifiers can weigh the edges.
        {/if}
      </p>
      <fieldset class="follow">
        <legend class="sr-only">Predicates</legend>
        <label
          ><input type="radio" bind:group={follow} value="except" disabled={!phase1} /> every predicate
          except</label
        >
        <label><input type="radio" bind:group={follow} value="only" /> only</label>
      </fieldset>
      {#if !phase1}
        <p class="faint small" id="path-phase1">
          This server cannot skip predicates or hubs, so the search follows the predicates listed,
          or every predicate when the list is empty.
        </p>
      {/if}
      <ul
        class="preds"
        aria-label={follow === 'except' && phase1 ? 'Skipped predicates' : 'Predicates to follow'}
      >
        {#each shownPreds as p (p)}
          <li class="chip" title={p}>
            <span class="mono">{displayIri(p, prefixes)}</span>
            <button
              type="button"
              class="chip-x"
              aria-label="Remove {displayIri(p, prefixes)}"
              onclick={() => removePredicate(p)}>×</button
            >
          </li>
        {/each}
        <li class="add">
          <input
            class="pred-input"
            list="path-preds"
            placeholder="+ predicate"
            aria-label="Add a predicate"
            bind:value={predText}
            onfocus={loadPredicates}
            onkeydown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                e.stopPropagation();
                addPredicate();
              }
            }}
          />
          <datalist id="path-preds">
            {#each schemaPreds as p (p.iri)}<option value={displayIri(p.iri, prefixes)}
                >{p.label ?? ''}</option
              >{/each}
          </datalist>
        </li>
      </ul>
      {#if predErr}<p class="warn small" role="alert">{predErr}</p>{/if}
    </details>
    <div class="go">
      <button class="btn primary sm" type="submit" disabled={!!blocked || running}
        >{#if running}<span class="spinner"></span>{/if} Find paths</button
      >
      {#if blocked && from && to}<span class="faint small">{blocked}</span>{/if}
      {#if result}
        <span class="faint small at"
          >at commit {result.commit ?? '?'} · {branch}{result.at ? ` (${result.at})` : ''}</span
        >
      {/if}
    </div>
    {#if stale}
      <button type="button" class="btn sm" onclick={run}
        >Search again at {at ? `commit ${at}` : 'the head'}</button
      >
    {/if}
  </form>

  <div class="sr-only" aria-live="polite">{announce}</div>

  {#if failure}
    <div class="msg" role="alert">
      <Icon name="alert" size={14} />
      <span>{failure.message}</span>
    </div>
  {/if}

  {#if result}
    {@const s = result.settings}
    <div class="results">
      <button
        type="button"
        class="handle"
        aria-label={sheetFull ? 'Fold the paths' : 'Show every path'}
        aria-expanded={sheetFull}
        onpointerdown={onHandleDown}
        onpointerup={onHandleUp}
        onkeydown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            sheetFull = !sheetFull;
          }
        }}
      ></button>
      <p class="summary">
        {result.paths.length}
        {result.paths.length === 1 ? 'path' : 'paths'} · {result.ms < 1000
          ? `${Math.max(1, Math.round(result.ms))} ms`
          : `${(result.ms / 1000).toFixed(1)} s`}{#if result.visited != null}
          · {fmtInt(result.visited)} nodes visited{/if}{#if result.skippedHubs}
          · {fmtInt(result.skippedHubs)} hubs skipped{/if}
      </p>
      {#if result.paths.length === 0}
        <p class="empty">
          No path of up to {lengthText(s.maxLength)}.
          {#if s.maxLength < pf.MAX_LENGTH}
            <button
              type="button"
              class="btn sm"
              onclick={() => {
                maxLength = pf.MAX_LENGTH;
                void run();
              }}>Allow {pf.MAX_LENGTH}</button
            >
          {/if}
        </p>
      {:else}
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <div
          class="list"
          role="listbox"
          tabindex="0"
          aria-label="Paths found"
          aria-activedescendant={highlight != null ? `path-opt-${highlight}` : undefined}
          bind:this={list}
          onkeydown={onListKey}
        >
          {#each result.paths as p (p.n)}
            <div
              id="path-opt-{p.n}"
              data-path={p.n}
              role="option"
              tabindex="-1"
              aria-selected={highlight === p.n}
              aria-label={pf.describePath(p, labelOf)}
              class="row"
              class:on={highlight === p.n}
              onclick={() => choose(highlight === p.n ? null : p.n)}
              onkeydown={() => {}}
            >
              <span class="num">{p.n}</span>
              <span class="len"
                >length {p.length}{#if p.cost != null}<span class="cost">
                    · cost {pf.fmtCost(p.cost)}</span
                  >{/if}</span
              >
              <span class="steps">
                {#each p.edges as e, i (i)}
                  {#if i === 0}<span class="node">{labelOf(p.nodes[0])}</span>{/if}
                  <span class="pred"
                    >{e.forward ? '' : '←'}{shortLabel(e.p, prefixes)}{e.forward ? '→' : ''}</span
                  >
                  <span class="node">{labelOf(p.nodes[i + 1])}</span>
                {/each}
              </span>
            </div>
          {/each}
        </div>
      {/if}
      <div class="actions">
        {#if result.paths.length > 1}
          <button
            type="button"
            class="btn sm ghost"
            aria-pressed={highlight == null}
            onclick={() => choose(null)}>All</button
          >
        {/if}
        <button type="button" class="btn sm" onclick={openInEditor}
          ><Icon name="query" size={13} /> Open in editor</button
        >
        {#if result.paths.length && onadd}
          <button type="button" class="btn sm" onclick={onadd}
            ><Icon name="plus" size={13} /> Add to graph</button
          >
        {/if}
        <button type="button" class="btn sm ghost" onclick={clear}
          ><Icon name="trash" size={13} /> Clear</button
        >
      </div>
      <p class="faint small foot">
        Visit limit {fmtInt(pf.MAX_VISITED)} · timeout {pf.TIMEOUT_S} s · keys 1–9 pick a path, A shows
        all
      </p>
    </div>
  {/if}
</section>

<style>
  .paths {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: auto;
    background: var(--surface);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px 6px;
  }
  .head h2 {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-lg);
    margin: 0;
  }
  .form {
    display: grid;
    gap: 10px;
    padding: 4px 16px 12px;
    border-bottom: 1px solid var(--border);
  }
  .ends {
    display: flex;
    align-items: flex-end;
    gap: 6px;
    flex-wrap: wrap;
  }
  .swap {
    margin-bottom: 3px;
  }
  fieldset {
    border: 0;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    align-items: center;
  }
  legend {
    font-size: var(--fs-xs);
    color: var(--text-2);
    padding: 0;
    margin-bottom: 3px;
    width: 100%;
  }
  .modes label,
  .follow label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .filters summary {
    cursor: pointer;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .filters[open] summary {
    margin-bottom: 8px;
  }
  .frow {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    font-size: var(--fs-sm);
    margin-bottom: 8px;
  }
  .preds {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    max-width: 100%;
    padding: 1px 3px 1px 8px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: var(--fs-xs);
  }
  .chip .mono {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-x {
    border: 0;
    background: transparent;
    color: var(--text-2);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 0 4px;
  }
  .pred-input {
    width: 150px;
    max-width: 100%;
    font-size: var(--fs-xs);
    padding: 3px 6px;
    border: 1px dashed var(--border-strong);
    border-radius: 999px;
    background: transparent;
    color: var(--text);
  }
  .go {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }
  .at {
    margin-left: auto;
  }
  .msg {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    margin: 10px 16px 0;
    padding: 8px 10px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--warn, #c80) 12%, var(--surface));
    font-size: var(--fs-sm);
  }
  .results {
    display: grid;
    gap: 6px;
    padding: 8px 16px 14px;
  }
  .handle {
    display: none;
  }
  .summary {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .empty {
    margin: 0;
    font-size: var(--fs-sm);
  }
  .list {
    display: grid;
    gap: 2px;
    max-height: 340px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 2px;
  }
  .list:focus-visible {
    outline: 2px solid var(--spark);
    outline-offset: 1px;
  }
  .row {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr);
    gap: 8px;
    align-items: baseline;
    padding: 6px 8px;
    border-radius: 4px;
    cursor: pointer;
    font-size: var(--fs-sm);
  }
  .row:hover {
    background: color-mix(in srgb, var(--spark) 7%, var(--surface));
  }
  .row.on {
    background: color-mix(in srgb, var(--spark) 16%, var(--surface));
  }
  .num {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--spark);
  }
  .len {
    color: var(--text-3);
    font-size: var(--fs-xs);
    white-space: nowrap;
  }
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 4px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .pred {
    color: var(--text-3);
    font-size: var(--fs-xs);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .foot {
    margin: 0;
  }
  .warn {
    color: var(--removed, #b33);
    margin: 4px 0 0;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  /* a phone: a sheet whose list folds to its first path after a search */
  @media (max-width: 600px) {
    .handle {
      display: block;
      justify-self: center;
      width: 44px;
      height: 18px;
      border: 0;
      background: transparent;
      cursor: grab;
      touch-action: none;
      position: relative;
    }
    .handle::after {
      content: '';
      position: absolute;
      left: 6px;
      right: 6px;
      top: 7px;
      height: 4px;
      border-radius: 2px;
      background: var(--border-strong);
    }
    .folded .form {
      display: none;
    }
    .folded .list {
      max-height: 44px;
      overflow: hidden;
    }
    .folded .foot {
      display: none;
    }
  }
</style>
