<script lang="ts">
  // Similar: nearest neighbours by `spk:vectorSearch` (docs/API.md "Vector similarity").
  // Pick a vector index (or a predicate without one), then search from an entity's own
  // vector or from a pasted vector. The URL keeps the dataset, the index or predicate,
  // the entity and the controls (`?ds=&index=&predicate=&iri=&k=&metric=&ef=&exact=`).
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import type { Term } from '$lib/api';
  import { app } from '$lib/app.svelte';
  import * as ex from '$lib/explore';
  import { fmtBytes, fmtInt, fmtMs } from '$lib/format';
  import { shortLabel } from '$lib/graph';
  import { displayIri, sparqlIri } from '$lib/rdf';
  import { LatestRun } from '$lib/supersede';
  import {
    abbreviateVector,
    buildVectorSearch,
    fmtScore,
    MAX_EF,
    metricInfo,
    METRICS,
    parseVector,
    parseVectorInput,
    resolvePredicate,
    scoreBars,
    searchModeText,
    VECTOR_DATATYPE,
    vectorErrorHint,
    vectorSearch,
    type Metric,
    type VectorSearchQuery,
    type VectorSearchResult,
  } from '$lib/vectors';
  import Icon from '$components/Icon.svelte';
  import TermView from '$components/TermView.svelte';

  const KS = [5, 10, 20, 50, 100];

  const ds = $derived(app.current);
  const prefixes = $derived(app.prefixes(ds));
  const params = page.url.searchParams;

  // --- the dataset's indexes ---------------------------------------------------------
  let vstatus = $state<api.VectorStatus | null>(null);
  let vstatusError = $state<string | null>(null);
  const statusRuns = new LatestRun();

  async function loadStatus(name: string) {
    const owns = statusRuns.claim('status');
    try {
      const s = await api.vectorStatus(name);
      if (!owns()) return;
      vstatus = s;
      vstatusError = null;
    } catch (e) {
      if (!owns()) return;
      // a server without vector indexes still searches predicates
      const none = e instanceof api.ApiError && (e.status === 404 || e.status === 501);
      vstatus = none
        ? { budgetBytes: 0, usedBytes: 0, generation: '', indexes: [], predicates: [] }
        : null;
      vstatusError = none ? null : api.errorMessage(e);
    }
  }

  // the target: `index:NAME`, `pred:IRI` (packed without an index) or `other`
  let target = $state(
    params.get('index')
      ? `index:${params.get('index')}`
      : params.get('predicate')
        ? `pred:${params.get('predicate')}`
        : '',
  );
  let otherPredicate = $state('');

  // a dataset from the URL (a link from the explorer or the dataset page)
  {
    const urlDs = params.get('ds');
    if (urlDs && urlDs !== app.current) app.setDataset(urlDs);
  }
  let lastDs: string | null = null;
  $effect(() => {
    const name = ds;
    if (!name) return;
    untrack(() => {
      if (lastDs != null && lastDs !== name) {
        target = '';
        result = null;
        error = null;
      }
      lastDs = name;
      vstatus = null;
      void loadStatus(name);
    });
  });

  const indexes = $derived(vstatus?.indexes ?? []);
  const packed = $derived(
    (vstatus?.predicates ?? []).filter((p) => !indexes.some((i) => i.predicate === p.predicate)),
  );

  // with nothing chosen, the first index; a predicate from the URL is searched through
  // its index when it has one, and is typed in as another predicate when it is not listed
  $effect(() => {
    if (!vstatus) return;
    const t = target;
    untrack(() => {
      if (t.startsWith('pred:')) {
        const iri = t.slice(5);
        const ix = indexes.find((i) => i.predicate === iri);
        if (ix) target = `index:${ix.name}`;
        else if (!packed.some((p) => p.predicate === iri)) {
          otherPredicate = displayIri(iri, prefixes);
          target = 'other';
        }
      } else if (t.startsWith('index:') && !indexes.some((i) => `index:${i.name}` === t)) {
        target = '';
      }
      if (target) return;
      if (indexes.length) target = `index:${indexes[0].name}`;
      else if (packed.length) target = `pred:${packed[0].predicate}`;
      else target = 'other';
    });
  });

  const chosenIndex = $derived(
    target.startsWith('index:') ? indexes.find((i) => `index:${i.name}` === target) : undefined,
  );
  const predicate = $derived.by((): string | null => {
    if (chosenIndex) return chosenIndex.predicate;
    if (target.startsWith('pred:')) return target.slice(5);
    if (target === 'other') return resolvePredicate(otherPredicate, prefixes);
    return null;
  });
  const packedDims = $derived(
    packed.find((p) => p.predicate === predicate)?.dimensions.map((d) => d.dimension) ?? [],
  );
  /** The dimension a query must have, when an index fixes it. */
  const wantDim = $derived(chosenIndex?.dimension ?? null);

  // --- controls ----------------------------------------------------------------------
  const urlK = Number(params.get('k'));
  let k = $state(KS.includes(urlK) ? urlK : 10);
  const urlMetric = params.get('metric') as Metric | null;
  let metric = $state<Metric | null>(urlMetric && METRICS.includes(urlMetric) ? urlMetric : null);
  let efText = $state(params.get('ef') ?? '');
  let exact = $state(params.get('exact') === 'true');

  // the index's metric unless one was picked
  const usedMetric = $derived<Metric>(metric ?? chosenIndex?.metric ?? 'cosine');
  const hnsw = $derived(chosenIndex?.hnsw ?? null);
  const efValue = $derived.by(() => {
    const t = efText.trim();
    if (!t) return { ok: true as const, ef: undefined };
    const n = Number(t);
    return Number.isInteger(n) && n >= 1 && n <= MAX_EF
      ? { ok: true as const, ef: n }
      : { ok: false as const, ef: undefined };
  });

  // --- the query: an entity or a vector ----------------------------------------------
  let mode = $state<'entity' | 'vector'>(
    params.get('iri') || !params.has('vector') ? 'entity' : 'vector',
  );
  let entityText = $state(params.get('iri') ?? '');
  let vectorText = $state('');

  const entity = $derived(resolvePredicate(entityText, prefixes));
  const parsed = $derived(mode === 'vector' ? parseVectorInput(vectorText) : null);

  // completion of the entity input, by label or IRI
  let suggestions = $state<ex.SearchHit[]>([]);
  const suggestRuns = new LatestRun();
  let suggestTimer: ReturnType<typeof setTimeout> | undefined;
  function suggest() {
    clearTimeout(suggestTimer);
    const text = entityText.trim();
    if (!ds || text.length < 2 || /^<|^https?:/.test(text)) {
      suggestions = [];
      return;
    }
    const name = ds;
    suggestTimer = setTimeout(async () => {
      const owns = suggestRuns.claim('suggest');
      try {
        const hits = await ex.search(name, text);
        if (owns()) suggestions = hits;
      } catch {
        if (owns()) suggestions = [];
      }
    }, 250);
  }

  // the entity's vectors under the predicate: how many, and of which dimension
  type EntityVectors = { iri: string; predicate: string; values: string[] };
  let entityVectors = $state<EntityVectors | null>(null);
  let vectorChoice = $state(0);
  const entityRuns = new LatestRun();
  $effect(() => {
    const iri = mode === 'entity' ? entity : null;
    const p = predicate;
    const name = ds;
    if (!iri || !p || !name) {
      entityVectors = null;
      return;
    }
    const owns = entityRuns.claim('entity');
    const t = setTimeout(async () => {
      try {
        const rows = await api.select(
          name,
          `SELECT ?v WHERE { ${sparqlIri(iri)} ${sparqlIri(p)} ?v . FILTER(DATATYPE(?v) = <${VECTOR_DATATYPE}>) } LIMIT 20`,
        );
        if (!owns()) return;
        entityVectors = {
          iri,
          predicate: p,
          values: rows.flatMap((r) => (r.v?.type === 'literal' ? [r.v.value] : [])),
        };
        vectorChoice = 0;
      } catch {
        if (owns()) entityVectors = null;
      }
    }, 250);
    return () => clearTimeout(t);
  });
  const ownVectors = $derived(
    entityVectors && entityVectors.iri === entity && entityVectors.predicate === predicate
      ? entityVectors.values
      : null,
  );
  const ownDims = $derived(
    ownVectors?.map((v) => parseVector(v)?.length ?? null).filter((n) => n != null) ?? [],
  );

  /** What is wrong with the query, in words; null when it can run. */
  const problem = $derived.by((): string | null => {
    if (!ds) return 'Pick a dataset.';
    if (!predicate) return target === 'other' ? 'Type the embedding predicate.' : 'Pick an index.';
    if (!efValue.ok) return `ef is a whole number from 1 to ${MAX_EF}.`;
    if (mode === 'entity') {
      if (!entity)
        return entityText.trim() ? 'Not an IRI or a known prefixed name.' : 'Type an entity.';
      if (ownVectors && ownVectors.length === 0)
        return 'This entity has no vector under the predicate.';
      const d = ownDims[ownVectors && ownVectors.length > 1 ? vectorChoice : 0];
      if (wantDim != null && d != null && d !== wantDim)
        return `The entity's vector has ${d} dimensions, and the index holds ${wantDim}.`;
      return null;
    }
    if (!parsed?.ok) return parsed?.message ?? 'Paste a vector.';
    if (wantDim != null && parsed.values.length !== wantDim)
      return `The vector has ${parsed.values.length} dimensions, and the index holds ${wantDim}.`;
    return null;
  });

  /** The search the form describes, and the entity left out of the results. */
  const search = $derived.by((): { q: VectorSearchQuery; self: string | null } | null => {
    if (problem || !predicate) return null;
    const common = {
      predicate,
      k,
      metric: usedMetric,
      ...(hnsw && efValue.ef != null ? { ef: efValue.ef } : {}),
      ...(hnsw && exact ? { exact: true } : {}),
    };
    if (mode === 'entity' && entity) {
      // an entity with several vectors is searched by the one picked
      const several = ownVectors && ownVectors.length > 1;
      return {
        q: {
          ...common,
          query: several ? { lexical: ownVectors[vectorChoice] } : { entity },
          skipSelf: true,
        },
        self: entity,
      };
    }
    if (parsed?.ok) return { q: { ...common, query: { lexical: parsed.lexical } }, self: null };
    return null;
  });

  // --- running -----------------------------------------------------------------------
  let result = $state<VectorSearchResult | null>(null);
  /** The search that produced `result`. */
  let ran = $state<{ q: VectorSearchQuery; self: string | null } | null>(null);
  let labels = $state<Map<string, string>>(new Map());
  let error = $state<{ title: string; hint?: string } | null>(null);
  let searching = $state(false);
  let elapsed = $state(0);
  const runs = new LatestRun();
  let ctl: AbortController | null = null;

  function syncUrl() {
    const p = new URLSearchParams();
    if (ds) p.set('ds', ds);
    if (chosenIndex) p.set('index', chosenIndex.name);
    else if (predicate) p.set('predicate', predicate);
    if (mode === 'entity' && entity) p.set('iri', entity);
    if (k !== 10) p.set('k', String(k));
    if (metric) p.set('metric', metric);
    if (efValue.ef != null) p.set('ef', String(efValue.ef));
    if (exact) p.set('exact', 'true');
    goto(`${resolve('/similar')}?${p}`, { replaceState: true, keepFocus: true, noScroll: true });
  }

  async function run() {
    const s = search;
    const name = ds;
    if (!s || !name) return;
    const owns = runs.claim('search');
    ctl?.abort();
    const c = (ctl = new AbortController());
    searching = true;
    error = null;
    syncUrl();
    const t0 = performance.now();
    try {
      const r = await vectorSearch(name, s.q, s.self, c.signal);
      if (!owns()) return;
      elapsed = performance.now() - t0;
      result = r;
      ran = s;
      const iris = r.hits.flatMap((h) => (h.term.type === 'uri' ? [h.term.value] : []));
      const l = await ex.labels(name, iris).catch(() => new Map<string, string>());
      if (owns()) labels = l;
    } catch (e) {
      if (!owns() || (e instanceof DOMException && e.name === 'AbortError')) return;
      result = null;
      error = vectorErrorHint(e);
    } finally {
      if (owns()) searching = false;
    }
  }

  // an entity from the URL searches as soon as the form can run
  let autoRun = $state(!!params.get('iri'));
  $effect(() => {
    if (!autoRun || !search) return;
    // wait for the entity's vectors, so several of them are told apart
    if (mode === 'entity' && ownVectors == null) return;
    autoRun = false;
    untrack(() => void run());
  });

  onMount(() => () => {
    ctl?.abort();
    clearTimeout(suggestTimer);
  });

  function searchFrom(iri: string) {
    mode = 'entity';
    entityText = iri;
    autoRun = true;
  }

  function openInQuery() {
    const s = ran ?? search;
    if (!s) return;
    app.pendingQuery = { query: buildVectorSearch(s.q), title: 'Similar' };
    goto(resolve('/query'));
  }

  const info = $derived(metricInfo(ran?.q.metric ?? usedMetric));
  const bars = $derived(
    result
      ? scoreBars(
          result.hits.map((h) => h.score),
          ran?.q.metric ?? usedMetric,
        )
      : [],
  );
  const timing = $derived(
    result
      ? `${fmtMs(elapsed)}${result.ms != null ? ` (${fmtMs(result.ms)} on the server)` : ''}`
      : '',
  );
  const exploreHref = (iri: string) =>
    `${resolve('/explore')}?ds=${encodeURIComponent(ds ?? '')}&iri=${encodeURIComponent(iri)}`;
  const datasetHref = $derived(
    ds ? resolve('/datasets/[name]', { name: ds }) : resolve('/datasets'),
  );
  const vectorLabel = (t: Term | undefined) =>
    t?.type === 'literal' ? abbreviateVector(t.value, 8) : '';
</script>

<svelte:head><title>Similar | Sparkles</title></svelte:head>

<div class="page">
  <header class="head">
    <div>
      <h1>Similar</h1>
      <p class="muted">
        Nearest neighbours by <span class="mono">spk:vectorSearch</span>
        {#if ds}in <span class="mono">{ds}</span>{/if}
      </p>
    </div>
  </header>

  <div class="layout">
    <section class="panel query" aria-label="Query">
      <div class="panel-head"><h2>Query</h2></div>
      <form
        class="panel-body form"
        onsubmit={(e) => {
          e.preventDefault();
          void run();
        }}
      >
        {#if vstatusError}
          <div class="error-box small">{vstatusError}</div>
        {/if}
        <label class="field">
          Index
          <select class="select" bind:value={target} aria-label="Index">
            {#each indexes as i (i.name)}
              <option value="index:{i.name}"
                >{i.name} · {displayIri(i.predicate, prefixes)} · {i.dimension} dims, {i.metric}{i.state !==
                'ready'
                  ? ` (${i.state})`
                  : ''}</option
              >
            {/each}
            {#each packed as p (p.predicate)}
              <option value="pred:{p.predicate}"
                >{displayIri(p.predicate, prefixes)} · no index · {p.dimensions
                  .map((d) => d.dimension)
                  .join(', ')} dims</option
              >
            {/each}
            <option value="other">Another predicate…</option>
          </select>
        </label>
        {#if target === 'other'}
          <label class="field">
            Predicate
            <input
              class="input mono"
              bind:value={otherPredicate}
              placeholder="ex:embedding"
              spellcheck="false"
              autocomplete="off"
            />
          </label>
        {/if}
        {#if chosenIndex}
          <div class="idx">
            <span class="t-iri mono pred" title={chosenIndex.predicate}
              >{displayIri(chosenIndex.predicate, prefixes)}</span
            >
            <span class="chips">
              <span class="chip">{chosenIndex.dimension} dims</span>
              <span class="chip">{chosenIndex.metric}</span>
              {#if chosenIndex.model}<span class="chip model">{chosenIndex.model}</span>{/if}
              <span
                class="badge {chosenIndex.state === 'ready'
                  ? 'ok'
                  : chosenIndex.state === 'building'
                    ? 'warn'
                    : 'danger'}">{chosenIndex.state}</span
              >
            </span>
            <span class="faint small"
              >{fmtInt(chosenIndex.rows)} rows · {chosenIndex.hnsw
                ? `HNSW M ${chosenIndex.hnsw.m}, efSearch ${chosenIndex.hnsw.efSearch}`
                : 'no graph, exact'} · {fmtBytes(
                chosenIndex.memory.segmentBytes + chosenIndex.memory.hnswBytes,
              )}</span
            >
          </div>
        {:else if packedDims.length}
          <p class="faint small">
            No index: searches scan the {packedDims.join(', ')}-dimensional vectors exactly.
          </p>
        {:else if vstatus && !indexes.length}
          <p class="faint small">
            This dataset has no vector index, so searches scan exactly. <a href={datasetHref}
              >Create one on the dataset page.</a
            >
          </p>
        {/if}

        <div class="seg" role="radiogroup" aria-label="Search from">
          <button
            type="button"
            role="radio"
            aria-checked={mode === 'entity'}
            class:on={mode === 'entity'}
            onclick={() => (mode = 'entity')}>Entity</button
          >
          <button
            type="button"
            role="radio"
            aria-checked={mode === 'vector'}
            class:on={mode === 'vector'}
            onclick={() => (mode = 'vector')}>Vector</button
          >
        </div>

        {#if mode === 'entity'}
          <label class="field">
            Entity <span class="faint">(an IRI, a prefixed name, or a label to look up)</span>
            <input
              class="input mono"
              bind:value={entityText}
              oninput={suggest}
              list="similar-entities"
              placeholder="ex:alice"
              spellcheck="false"
              autocomplete="off"
            />
            <datalist id="similar-entities">
              {#each suggestions as h (h.iri)}<option value={h.iri}
                  >{h.label ?? displayIri(h.iri, prefixes)}</option
                >{/each}
            </datalist>
          </label>
          {#if ownVectors && ownVectors.length > 1}
            <label class="field">
              Vector <span class="faint">(the entity has {ownVectors.length})</span>
              <select class="select" bind:value={vectorChoice}>
                {#each ownVectors as v, i (i)}<option value={i}>{abbreviateVector(v, 4)}</option
                  >{/each}
              </select>
            </label>
          {:else if ownVectors?.length === 1}
            <p class="faint small mono vecline">{abbreviateVector(ownVectors[0], 6)}</p>
          {/if}
        {:else}
          <label class="field">
            Vector <span class="faint">(a JSON array or a spk:vector literal)</span>
            <textarea
              class="textarea"
              rows="4"
              bind:value={vectorText}
              placeholder="[0.12, -0.03, 0.4, …]"
              spellcheck="false"
              aria-invalid={!!vectorText.trim() && !parsed?.ok}></textarea>
          </label>
          {#if parsed?.ok}
            <p class="small" class:faint={wantDim == null || parsed.values.length === wantDim}>
              {parsed.values.length} dimensions{#if wantDim != null && parsed.values.length !== wantDim}<span
                  class="bad"
                >
                  · the index holds {wantDim}</span
                >{/if}
            </p>
          {:else if vectorText.trim() && parsed}
            <p class="small bad">{parsed.message} At character {parsed.offset + 1}.</p>
          {/if}
        {/if}

        <div class="controls">
          <label class="ctl">
            <span class="faint">top</span>
            <select class="select sm" bind:value={k} aria-label="Results">
              {#each KS as n (n)}<option value={n}>{n}</option>{/each}
            </select>
          </label>
          <div class="seg" role="radiogroup" aria-label="Metric">
            {#each METRICS as m (m)}
              <button
                type="button"
                class:on={usedMetric === m}
                role="radio"
                aria-checked={usedMetric === m}
                title="{metricInfo(m).label}: {metricInfo(m).better} is closer{chosenIndex &&
                chosenIndex.metric !== m
                  ? '. Not the index’s metric, so the search is exact.'
                  : ''}"
                onclick={() => (metric = m === chosenIndex?.metric ? null : m)}>{m}</button
              >
            {/each}
          </div>
          {#if hnsw}
            <label class="ctl" title="HNSW candidates. More raise recall and latency.">
              <span class="faint">ef</span>
              <input
                class="input sm ef"
                inputmode="numeric"
                bind:value={efText}
                placeholder={String(hnsw.efSearch)}
                aria-label="ef"
                aria-invalid={!efValue.ok}
                disabled={exact}
              />
            </label>
            <label class="check" title="Search all vectors instead of the graph">
              <input type="checkbox" bind:checked={exact} /> exact
            </label>
          {/if}
        </div>
        <div class="row">
          {#if problem && (mode === 'vector' ? vectorText.trim() : entityText.trim())}
            <span class="faint small">{problem}</span>
          {/if}
          <span class="spacer"></span>
          <button class="btn primary" type="submit" disabled={!search || searching}>
            {#if searching}<span class="spinner"></span>{:else}<Icon name="search" size={14} />{/if}
            Search
          </button>
        </div>
      </form>
    </section>

    <section class="panel results" aria-label="Similar results">
      <div class="panel-head">
        <h2>Results</h2>
        {#if result}<span class="faint">{result.hits.length}</span>{/if}
        <span class="spacer"></span>
        {#if result || ran}
          <button class="btn sm" onclick={openInQuery} title="Open the SPARQL of this search"
            ><Icon name="query" size={13} /> Open in query editor</button
          >
        {/if}
      </div>
      {#if error}
        <div class="panel-body">
          <div class="error-box">
            <strong>{error.title}</strong>
            {#if error.hint}<div class="muted">{error.hint}</div>{/if}
          </div>
        </div>
      {:else if result}
        {#if result.hits.length === 0}
          <p class="panel-body faint">No other entity has a vector of this dimension.</p>
        {:else}
          <div class="table-wrap">
            <table class="data hits">
              <thead>
                <tr>
                  <th class="rank">#</th>
                  <th>Entity</th>
                  <th class="score" title="{info.label}: {info.better} is closer"
                    >{info.score} {info.better === 'lower' ? '↓' : '↑'}</th
                  >
                  <th class="vec">Matched vector</th>
                  <th class="acts"><span class="sr">Actions</span></th>
                </tr>
              </thead>
              <tbody>
                {#each result.hits as h, i (i)}
                  <tr>
                    <td class="rank faint">{i + 1}</td>
                    <td class="ent">
                      {#if h.term.type === 'uri'}
                        {@const iri = h.term.value}
                        <a class="t-iri" href={exploreHref(iri)} title={iri}
                          >{labels.get(iri) ?? shortLabel(iri, prefixes)}</a
                        >
                        <div class="iri mono faint">{displayIri(iri, prefixes)}</div>
                      {:else}
                        <TermView term={h.term} {prefixes} />
                      {/if}
                    </td>
                    <td class="score">
                      <span class="val mono">{fmtScore(h.score)}</span>
                      <span class="track"
                        ><span class="bar" style:width="{bars[i] * 100}%"></span></span
                      >
                    </td>
                    <td
                      class="vec mono faint"
                      title={h.vector?.type === 'literal' ? h.vector.value : undefined}
                      >{vectorLabel(h.vector)}</td
                    >
                    <td class="acts">
                      {#if h.term.type === 'uri'}
                        {@const iri = h.term.value}
                        <a
                          class="btn sm ghost icon"
                          href={exploreHref(iri)}
                          title="Open in Explore"
                          aria-label="Open {shortLabel(iri, prefixes)} in Explore"
                          ><Icon name="explore" size={13} /></a
                        >
                        <button
                          class="btn sm ghost icon"
                          title="Search from here"
                          aria-label="Search from {shortLabel(iri, prefixes)}"
                          onclick={() => searchFrom(iri)}><Icon name="target" size={13} /></button
                        >
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
        <p class="foot faint small">
          {info.label}{info.better === 'lower'
            ? ' distance: lower is closer'
            : ': higher is closer'}.
          {ran?.self ? 'The query entity is left out.' : ''}
          {timing}
          {#if result.mode}
            · <span class="mono" title="How the server ran the search"
              >{searchModeText(result.mode)}</span
            >
            {result.mode.scored != null
              ? `· ${fmtInt(result.mode.scored)} of ${fmtInt(result.mode.rows ?? 0)} rows scored`
              : ''}
          {/if}
        </p>
      {:else if searching}
        <div class="panel-body row faint"><span class="spinner"></span> Searching…</div>
      {:else}
        <div class="panel-body empty faint">
          <Icon name="target" size={22} />
          <p>
            Search from an entity's vector, or paste a vector. Results link to Explore, and any of
            them can be the next query.
          </p>
        </div>
      {/if}
    </section>
  </div>
</div>

<style>
  .page {
    padding: 24px 28px 40px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 18px;
    max-width: 1280px;
    width: 100%;
  }
  .head {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    flex-wrap: wrap;
  }
  .head p {
    margin-top: 4px;
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(280px, 380px) minmax(0, 1fr);
    gap: 18px;
    align-items: start;
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
    font-size: var(--fs-sm);
  }
  .form p {
    margin: 0;
  }
  .idx {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .pred {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
  }
  .chip {
    padding: 1px 7px;
    border-radius: 10px;
    font-size: 11px;
    background: color-mix(in srgb, var(--iri) 10%, transparent);
  }
  .chip.model {
    background: color-mix(in srgb, var(--literal) 12%, transparent);
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .seg {
    display: inline-flex;
    justify-self: start;
    border: 1px solid var(--border);
    border-radius: var(--r);
    overflow: hidden;
  }
  .seg button {
    all: unset;
    padding: 3px 10px;
    font-size: var(--fs-sm);
    cursor: pointer;
    color: var(--text-2);
  }
  .seg button + button {
    border-left: 1px solid var(--border);
  }
  .seg button.on {
    background: color-mix(in srgb, var(--iri) 12%, transparent);
    color: var(--iri);
    font-weight: 600;
  }
  .seg button:focus-visible {
    box-shadow: var(--focus);
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
  }
  .ctl {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .select.sm,
  .input.sm {
    height: 26px;
    font-size: var(--fs-sm);
  }
  .ef {
    width: 64px;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--text-2);
    cursor: pointer;
  }
  .check input {
    margin: 0;
    accent-color: var(--iri);
  }
  .vecline {
    overflow-wrap: anywhere;
  }
  .small {
    font-size: 11px;
  }
  .bad {
    color: var(--danger);
  }
  .table-wrap {
    overflow-x: auto;
  }
  .hits {
    table-layout: fixed;
    font-size: var(--fs-sm);
  }
  .hits th.rank,
  .hits td.rank {
    width: 34px;
    text-align: right;
  }
  .hits th.score {
    width: 26%;
    text-align: right;
  }
  .hits th.vec {
    width: 30%;
  }
  .hits th.acts {
    width: 64px;
  }
  .ent {
    overflow: hidden;
  }
  .ent a {
    text-decoration: none;
    font-weight: 500;
  }
  .ent a:hover {
    text-decoration: underline;
  }
  .ent .iri,
  td.vec {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
  }
  td.score {
    text-align: right;
  }
  .val {
    display: block;
  }
  .track {
    display: flex;
    justify-content: flex-end;
    height: 4px;
    margin-top: 3px;
    border-radius: 2px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: var(--literal);
    opacity: 0.7;
  }
  td.acts {
    white-space: nowrap;
    text-align: right;
  }
  a.btn {
    text-decoration: none;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .foot {
    margin: 0;
    padding: 8px 14px 12px;
  }
  .empty {
    display: grid;
    justify-items: center;
    gap: 6px;
    padding: 32px 14px;
    text-align: center;
  }
  .empty p {
    margin: 0;
    max-width: 420px;
  }
  @media (max-width: 900px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  @media (max-width: 760px) {
    .page {
      padding: 16px;
    }
    .hits th.vec,
    td.vec {
      display: none;
    }
    .hits th.score {
      width: 34%;
    }
  }
</style>
