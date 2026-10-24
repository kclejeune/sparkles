<script lang="ts">
  // The embedding map (docs/specs/U11-linked-views.md, section 2.4): a vector index's
  // entities projected to two dimensions by the server, drawn over a density shading of
  // every entity, coloured by class or by a predicate. A lasso or a box selects points into
  // the shared selection, which Use in query writes as a VALUES block and Open as graph
  // draws in Explore. The URL keeps the dataset, the index, the method and the colouring
  // (`?ds=&index=&method=&colour=`).
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import {
    byDistance,
    categories,
    densityCounts,
    keepSelected,
    keyOf,
    lassoed,
    boxed,
    NONE,
    numericBuckets,
    positions,
    VALUES_CAP,
    valuesQuery,
    type Category,
    type Points,
    type Projection,
    type Pt,
  } from '$lib/embedding';
  import * as ex from '$lib/explore';
  import { fmtInt, fmtMs } from '$lib/format';
  import { OTHER } from '$lib/lens';
  import { displayIri } from '$lib/rdf';
  import { selection, SELECTION_CAP } from '$lib/selection.svelte';
  import { LatestRun } from '$lib/supersede';
  import { fmtScore, resolvePredicate, vectorSearch } from '$lib/vectors';
  import EmbeddingMap from '$components/EmbeddingMap.svelte';
  import SelectionBar from '$components/SelectionBar.svelte';

  const SOURCE = 'the embedding map';
  /** The points one answer draws at most, the server's own cap. */
  const LIMIT = 50_000;
  /** The rows of the list of points in view. */
  const LIST_ROWS = 200;

  type Method = 'pca' | 'neighbours';
  const params = page.url.searchParams;

  {
    const urlDs = params.get('ds');
    if (urlDs && urlDs !== app.current) app.setDataset(urlDs);
  }
  const ds = $derived(app.current);
  const prefixes = $derived(app.prefixes(ds));

  // --- the dataset's indexes -----------------------------------------------------------
  let vstatus = $state<api.VectorStatus | null>(null);
  let statusError = $state<{ title: string; hint?: string; forbidden?: boolean } | null>(null);
  const statusRuns = new LatestRun();
  let indexName = $state(params.get('index') ?? '');
  let method = $state<Method>(params.get('method') === 'neighbours' ? 'neighbours' : 'pca');
  let colourText = $state(params.get('colour') ?? '');
  /** `class`, or the predicate typed in. */
  let colourBy = $state<'class' | 'predicate'>(params.get('colour') ? 'predicate' : 'class');

  async function loadStatus(name: string) {
    const owns = statusRuns.claim('status');
    try {
      const s = await api.vectorStatus(name);
      if (!owns()) return;
      vstatus = s;
      statusError = null;
      if (!s.indexes.some((i) => i.name === indexName)) indexName = s.indexes[0]?.name ?? '';
    } catch (e) {
      if (!owns()) return;
      const none = e instanceof api.ApiError && (e.status === 404 || e.status === 501);
      vstatus = none
        ? { budgetBytes: 0, usedBytes: 0, generation: '', indexes: [], predicates: [] }
        : null;
      statusError = none ? null : failure(e);
    }
  }

  let lastDs: string | null = null;
  $effect(() => {
    const name = ds;
    if (!name) return;
    untrack(() => {
      if (lastDs != null && lastDs !== name) {
        indexName = '';
        proj = null;
      }
      lastDs = name;
      vstatus = null;
      void loadStatus(name);
    });
  });

  const indexes = $derived(vstatus?.indexes ?? []);
  const chosen = $derived(indexes.find((i) => i.name === indexName));

  // --- the projection ------------------------------------------------------------------
  // raw: the columns are large and replaced whole, so they need no deep proxies
  let proj = $state.raw<Projection | null>(null);
  /** The whole projection's bounds, which frame the camera across region answers. */
  let bounds = $state<[number, number, number, number] | null>(null);
  let loading = $state(false);
  let error = $state<{ title: string; hint?: string; forbidden?: boolean } | null>(null);
  let task = $state<api.Task | null>(null);
  const runs = new LatestRun();
  let ctl: AbortController | null = null;
  let polling: ReturnType<typeof setTimeout> | undefined;
  /** The region the last answer covers, when it is not the whole projection. */
  let region = $state<[number, number, number, number] | null>(null);

  const colourIri = $derived(
    colourBy === 'predicate' && colourText.trim() ? resolvePredicate(colourText, prefixes) : null,
  );

  function syncUrl() {
    const p = new URLSearchParams();
    if (ds) p.set('ds', ds);
    if (indexName) p.set('index', indexName);
    if (method !== 'pca') p.set('method', method);
    if (colourIri) p.set('colour', colourIri);
    goto(`${resolve('/embedding')}?${p}`, { replaceState: true, keepFocus: true, noScroll: true });
  }

  function failure(e: unknown) {
    if (e instanceof api.ApiError && e.status === 403)
      return {
        title: 'Embedding maps need access to every graph',
        hint: 'The map places every entity in the index, so it needs read access to the whole dataset.',
        forbidden: true,
      };
    return { title: api.errorMessage(e) };
  }

  /** Fetch the kept projection, or a region of it; compute one when none is kept. */
  async function load(opts: { bbox?: [number, number, number, number]; compute?: boolean } = {}) {
    const name = ds;
    const ix = indexName;
    if (!name || !ix) return;
    const owns = runs.claim('load');
    ctl?.abort();
    const c = (ctl = new AbortController());
    loading = true;
    if (!opts.bbox) error = null;
    try {
      const p = await api.vectorProjection(name, ix, {
        method,
        bbox: opts.bbox,
        limit: LIMIT,
        colour: colourIri ?? undefined,
        signal: c.signal,
      });
      if (!owns()) return;
      proj = p;
      region = opts.bbox ?? null;
      if (!opts.bbox) bounds = p.head.bounds;
      error = null;
    } catch (e) {
      if (!owns() || (e instanceof DOMException && e.name === 'AbortError')) return;
      if (e instanceof api.ApiError && e.status === 404 && opts.compute !== false && !opts.bbox) {
        void compute();
        return;
      }
      error = failure(e);
    } finally {
      if (owns()) loading = false;
    }
  }

  /** Compute the projection, in the request or as a task to follow. */
  async function compute() {
    const name = ds;
    const ix = indexName;
    if (!name || !ix) return;
    const owns = runs.claim('load');
    loading = true;
    error = null;
    try {
      const r = await api.computeProjection(name, ix, method);
      if (!owns()) return;
      if ('task' in r) {
        task = r.task;
        follow(r.task.id);
        return;
      }
      await load({ compute: false });
    } catch (e) {
      if (!owns()) return;
      error = failure(e);
      loading = false;
    }
  }

  function follow(id: string) {
    clearTimeout(polling);
    polling = setTimeout(async () => {
      try {
        const t = await api.getTask(id);
        if (task?.id !== id) return;
        task = t;
        if (t.state === 'done') {
          task = null;
          await load({ compute: false });
        } else if (t.state === 'failed' || t.state === 'cancelled') {
          task = null;
          loading = false;
          if (t.state === 'failed') error = { title: t.message ?? 'The projection failed' };
        } else follow(id);
      } catch (e) {
        if (task?.id !== id) return;
        task = null;
        loading = false;
        error = failure(e);
      }
    }, 500);
  }

  async function cancel() {
    const t = task;
    if (!t) return;
    try {
      await api.cancelTask(t.id);
    } catch (e) {
      toasts.error('Could not cancel the projection', e);
    }
  }

  // a new index, method or colouring loads again
  $effect(() => {
    void indexName;
    void method;
    void colourIri;
    if (!ds || !indexName) return;
    untrack(() => {
      task = null;
      clearTimeout(polling);
      // positions of another index or layout mean nothing here
      selectedAt.clear();
      selectRun++;
      syncUrl();
      void load();
    });
  });

  /** Aborted when the page goes. */
  const pageAbort = new AbortController();

  onMount(() => () => {
    pageAbort.abort();
    ctl?.abort();
    clearTimeout(polling);
    clearTimeout(hoverTimer);
  });

  // --- colours -------------------------------------------------------------------------
  const legend = $derived.by((): { list: Category[]; colourIndex: Uint32Array; title: string } => {
    const empty = { list: [], colourIndex: new Uint32Array(0), title: '' };
    if (!proj) return empty;
    const { head, points } = proj;
    const n = points.x.length;
    const out = new Uint32Array(n).fill(OTHER);
    if (head.colour && points.colours) {
      const values = head.colour.values;
      const buckets = numericBuckets(values);
      if (buckets) {
        const bins = values.map((v) => buckets.of(v.value));
        const counts = new Array(buckets.breaks.length + 1).fill(0);
        values.forEach((v, i) => (counts[bins[i]] += v.count));
        const edges = [-Infinity, ...buckets.breaks, Infinity];
        const list: Category[] = counts.map((count, k) => ({
          key: String(k),
          label:
            k === 0
              ? `below ${edges[1]}`
              : k === counts.length - 1
                ? `${edges[k]} and above`
                : `${edges[k]} to below ${edges[k + 1]}`,
          count,
          colour: k,
        }));
        for (let i = 0; i < n; i++) {
          const c = points.colours[i];
          if (c !== NONE) out[i] = bins[c];
        }
        return { list, colourIndex: out, title: displayIri(head.colour.predicate, prefixes) };
      }
      const cat = categories(
        values.map((v) => ({
          key: String(v.value.value),
          label:
            v.value.type === 'uri' ? displayIri(v.value.value, prefixes) : String(v.value.value),
          count: v.count,
        })),
        OTHER,
      );
      for (let i = 0; i < n; i++) {
        const c = points.colours[i];
        if (c !== NONE) out[i] = cat.index[c];
      }
      return {
        list: cat.list,
        colourIndex: out,
        title: displayIri(head.colour.predicate, prefixes),
      };
    }
    const cat = categories(
      head.classes.map((c) => ({ key: c.iri, label: displayIri(c.iri, prefixes), count: c.count })),
      OTHER,
    );
    for (let i = 0; i < n; i++) {
      const c = points.classes[i];
      if (c !== NONE) out[i] = cat.index[c];
    }
    return { list: cat.list, colourIndex: out, title: 'class' };
  });

  // --- the density ---------------------------------------------------------------------
  const density = $derived(proj ? densityCounts(proj.head.density) : null);

  // --- selection -----------------------------------------------------------------------
  let mode = $state<'pan' | 'lasso' | 'box'>('pan');
  let map = $state<EmbeddingMap>();

  const selectedPoints = $derived.by((): ReadonlySet<number> | null => {
    if (!proj || !selection.active) return null;
    const out = new Set<number>();
    const ids = selection.ids;
    proj.points.entities.forEach((e, i) => {
      if (ids.has(keyOf(e))) out.add(i);
    });
    return out;
  });

  type Shape = { kind: 'lasso'; poly: Pt[] } | { kind: 'box'; a: Pt; b: Pt };

  /**
   * The positions of the entities the map selected, kept apart from the drawn points, so
   * that the order of Use in query sees entities fetched for a sampled region, or left
   * out of the points drawn after a pan.
   */
  const selectedAt = new Map<string, Pt>();
  // the positions follow the shared selection: entities it left out at its cap, cleared or
  // took out elsewhere lose theirs
  $effect(() => {
    const ids = selection.ids;
    untrack(() => {
      keepSelected(selectedAt, ids);
    });
  });
  /** Counts the map's selections, so that a region's late answer cannot overwrite a newer one. */
  let selectRun = 0;

  /**
   * Change the shared selection. A selection above its cap keeps the entities nearest the
   * centre of what was drawn, so the selection keeps its core.
   */
  function applySelection(keys: string[], how: 'set' | 'add' | 'remove', from: Points) {
    const at = positions(from);
    if (how === 'set') selectedAt.clear();
    for (const k of keys) {
      const p = at.get(k);
      if (how === 'remove') selectedAt.delete(k);
      else if (p) selectedAt.set(k, p);
    }
    const list = keys.length > SELECTION_CAP ? byDistance(keys, at).ordered : keys;
    if (how === 'add') selection.add(list, SOURCE);
    else if (how === 'remove') selection.remove(list);
    else selection.set(list, SOURCE, { keepEmpty: true });
  }

  /**
   * Select the points of a lasso or a box. In a sampled view the drawn points are only a
   * part of the region, so the shape's region is fetched in full and selected again,
   * unless the selection, the index or the page changed meanwhile.
   */
  async function onselect(found: number[], how: 'set' | 'add' | 'remove', shape: Shape) {
    const p = proj;
    if (!p) return;
    const run = ++selectRun;
    applySelection(
      found.map((i) => keyOf(p.points.entities[i])),
      how,
      p.points,
    );
    if (!p.head.region.sampled || !ds || !indexName) return;
    const mine = selection.ids;
    const context = `${ds}\n${indexName}\n${method}`;
    const pts = shape.kind === 'lasso' ? shape.poly : [shape.a, shape.b];
    const bbox: [number, number, number, number] = [
      Math.min(...pts.map((q) => q.x)),
      Math.min(...pts.map((q) => q.y)),
      Math.max(...pts.map((q) => q.x)),
      Math.max(...pts.map((q) => q.y)),
    ];
    try {
      const full = await api.vectorProjection(ds, indexName, {
        method,
        bbox,
        limit: LIMIT,
        signal: pageAbort.signal,
      });
      if (
        run !== selectRun ||
        selection.ids !== mine ||
        context !== `${ds}\n${indexName}\n${method}`
      )
        return;
      const inside =
        shape.kind === 'lasso'
          ? lassoed(full.points, shape.poly)
          : boxed(full.points, shape.a, shape.b);
      const keys = inside.map((i) => keyOf(full.points.entities[i]));
      applySelection(keys, how, full.points);
      if (full.head.region.sampled)
        toasts.push(
          'info',
          `Selected ${fmtInt(keys.length)} of the region's ${fmtInt(full.head.region.total)} entities`,
          'Zoom in to select every entity of a dense region.',
        );
    } catch (e) {
      if (pageAbort.signal.aborted || run !== selectRun) return;
      toasts.error('Could not select the whole region', e);
    }
  }

  function useSelection(ids: string[]) {
    if (!proj) return;
    // the positions the map selected, then those of the points drawn now
    const at = positions(proj.points);
    for (const [k, p] of selectedAt) at.set(k, p);
    const { ordered } = byDistance(ids, at);
    const kept = ordered.slice(0, VALUES_CAP);
    if (ordered.length > VALUES_CAP)
      toasts.push(
        'info',
        `The query takes the ${fmtInt(VALUES_CAP)} entities nearest the selection's centre`,
        `${fmtInt(ordered.length - VALUES_CAP)} more were left out.`,
      );
    app.pendingQuery = { query: valuesQuery(kept), title: 'Embedding map' };
    goto(resolve('/query'));
  }

  function openAsGraph() {
    if (!ds) return;
    goto(`${resolve('/explore')}?ds=${encodeURIComponent(ds)}&selection=1`);
  }

  // --- hover and click -----------------------------------------------------------------
  type Hover = {
    i: number;
    at: { x: number; y: number };
    label?: string;
    neighbours?: { label: string; score: number }[] | null;
  };
  let hover = $state<Hover | null>(null);
  let hoverTimer: ReturnType<typeof setTimeout> | undefined;
  const hoverRuns = new LatestRun();

  function onhover(i: number | null, at: { x: number; y: number } | null) {
    clearTimeout(hoverTimer);
    if (i == null || !at || !proj) {
      hover = null;
      return;
    }
    hover = { i, at };
    const entity = proj.points.entities[i];
    const name = ds;
    const ix = chosen;
    if (!name || !ix || entity.startsWith('_:')) return;
    const owns = hoverRuns.claim('hover');
    hoverTimer = setTimeout(async () => {
      try {
        const [l, r] = await Promise.all([
          ex.labels(name, [entity]),
          vectorSearch(
            name,
            { predicate: ix.predicate, query: { entity }, k: 5, metric: ix.metric, skipSelf: true },
            entity,
          ).catch(() => null),
        ]);
        const iris = r?.hits.flatMap((h) => (h.term.type === 'uri' ? [h.term.value] : [])) ?? [];
        const nl = await ex.labels(name, iris).catch(() => new Map<string, string>());
        if (!owns() || hover?.i !== i) return;
        hover = {
          ...hover,
          label: l.get(entity),
          neighbours: r
            ? r.hits.map((h) => ({
                label:
                  h.term.type === 'uri'
                    ? (nl.get(h.term.value) ?? displayIri(h.term.value, prefixes))
                    : String(h.term.value),
                score: h.score,
              }))
            : null,
        };
      } catch {
        /* the tooltip keeps the IRI */
      }
    }, 400);
  }

  function exploreHref(entity: string) {
    return `${resolve('/explore')}?ds=${encodeURIComponent(ds ?? '')}&iri=${encodeURIComponent(entity)}`;
  }

  function onopen(i: number) {
    const e = proj?.points.entities[i];
    if (e && !e.startsWith('_:')) goto(exploreHref(e));
  }

  function className(i: number): string {
    if (!proj) return '';
    const c = proj.points.classes[i];
    return c === NONE ? '' : displayIri(proj.head.classes[c]?.iri ?? '', prefixes);
  }

  // --- the view ------------------------------------------------------------------------
  let view = $state<[number, number, number, number] | null>(null);

  /** After the camera stops, fetch the region in view when the sample hides some of it. */
  function onview(b: [number, number, number, number]) {
    view = b;
    if (!proj || !bounds) return;
    const [x0, y0, x1, y1] = bounds;
    const whole = (x1 - x0) * (y1 - y0);
    const area = (b[2] - b[0]) * (b[3] - b[1]);
    const zoomedIn = whole > 0 && area < whole * 0.8;
    if (proj.head.entities <= LIMIT) return;
    if (!zoomedIn) {
      if (region) void load({ compute: false });
      return;
    }
    // a region already covering the view with every point needs nothing
    if (
      region &&
      !proj.head.region.sampled &&
      b[0] >= region[0] &&
      b[1] >= region[1] &&
      b[2] <= region[2] &&
      b[3] <= region[3]
    )
      return;
    const pw = (b[2] - b[0]) * 0.25;
    const ph = (b[3] - b[1]) * 0.25;
    void load({ bbox: [b[0] - pw, b[1] - ph, b[2] + pw, b[3] + ph], compute: false });
  }

  let showList = $state(false);
  const inView = $derived.by(() => {
    if (!proj || !showList) return [];
    const out: number[] = [];
    const p = proj.points;
    for (let i = 0; i < p.x.length && out.length < LIST_ROWS; i++) {
      if (
        !view ||
        (p.x[i] >= view[0] && p.x[i] <= view[2] && p.y[i] >= view[1] && p.y[i] <= view[3])
      )
        out.push(i);
    }
    return out;
  });

  const head = $derived(proj?.head ?? null);
  const datasetHref = $derived(
    ds ? resolve('/datasets/[name]', { name: ds }) : resolve('/datasets'),
  );
  const pct = (x: number) => `${(x * 100).toFixed(x < 0.1 ? 1 : 0)}%`;

  function onkey(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    if (t?.closest?.('input, textarea, select, [contenteditable]')) return;
    if (e.key === 'l' && !e.metaKey && !e.ctrlKey) mode = mode === 'lasso' ? 'pan' : 'lasso';
    else if (e.key === 'b' && !e.metaKey && !e.ctrlKey) mode = mode === 'box' ? 'pan' : 'box';
  }
</script>

<svelte:head><title>Embedding map | Sparkles</title></svelte:head>
<svelte:window onkeydown={onkey} />

<div class="page page-container">
  <header class="head">
    <div>
      <h1>Embedding map</h1>
      <p class="muted">
        A vector index projected to two dimensions
        {#if ds}in <span class="mono">{ds}</span>{/if}
      </p>
    </div>
  </header>

  {#if app.queryAt}
    <p class="note faint">
      The map shows the latest data, not the earlier state selected on the Query page.
    </p>
  {/if}

  {#if !ds}
    <div class="empty">Pick a dataset.</div>
  {:else if statusError}
    <div class="error-box" data-state={statusError.forbidden ? 'forbidden' : 'error'}>
      <strong>{statusError.title}</strong>
      {#if statusError.hint}<p>{statusError.hint}</p>{/if}
    </div>
  {:else if vstatus && !indexes.length}
    <div class="empty" data-state="no-index">
      This dataset has no vector index to map. <a href={datasetHref}
        >Create one on the dataset page.</a
      >
    </div>
  {:else}
    <div class="toolbar" role="toolbar" aria-label="Embedding map">
      <label class="field">
        Index
        <select class="select" bind:value={indexName} aria-label="Index">
          {#each indexes as i (i.name)}
            <option value={i.name}>{i.name} ({i.dimension} dims)</option>
          {/each}
        </select>
      </label>
      <label class="field">
        Layout
        <select class="select" bind:value={method} aria-label="Layout">
          <option value="pca">PCA</option>
          <option value="neighbours">Neighbours</option>
        </select>
      </label>
      <label class="field">
        Colour by
        <select class="select" bind:value={colourBy} aria-label="Colour by">
          <option value="class">Class</option>
          <option value="predicate">Predicate</option>
        </select>
      </label>
      {#if colourBy === 'predicate'}
        <input
          class="input colour"
          aria-label="Colour predicate"
          placeholder="ex:genre"
          bind:value={colourText}
        />
      {/if}
      <span class="spacer"></span>
      <div class="seg" role="radiogroup" aria-label="Pointer">
        {#each [['pan', 'Pan'], ['lasso', 'Lasso'], ['box', 'Box']] as [m, l] (m)}
          <button
            type="button"
            role="radio"
            aria-checked={mode === m}
            class:on={mode === m}
            onclick={() => (mode = m as typeof mode)}>{l}</button
          >
        {/each}
      </div>
      <button
        class="btn sm lasso-toggle"
        aria-pressed={mode === 'lasso'}
        onclick={() => (mode = mode === 'lasso' ? 'pan' : 'lasso')}>Lasso</button
      >
      <button class="btn sm" onclick={() => map?.fit()} title="Fit the whole map">Fit</button>
      <button class="btn sm" onclick={() => map?.zoom(1.5)} aria-label="Zoom in">+</button>
      <button class="btn sm" onclick={() => map?.zoom(1 / 1.5)} aria-label="Zoom out">−</button>
      <button class="btn sm" aria-pressed={showList} onclick={() => (showList = !showList)}
        >List</button
      >
    </div>

    {#if head}
      <p class="facts faint" data-testid="facts">
        {fmtInt(head.entities)} entities from {fmtInt(head.vectors)} vectors of {head.dimension}
        dimensions{#if head.explained}, the two axes explain {pct(
            head.explained[0] + head.explained[1],
          )}
          of the variance{/if}{#if head.region.sampled}, showing {fmtInt(head.region.shown)} of {fmtInt(
            head.region.total,
          )} in view{/if}{#if head.builtMs != null}, built in {fmtMs(head.builtMs)}{/if}.
        {#if head.stale}
          <span class="badge spark" data-testid="stale">Stale</span>
          The data changed since the projection was built.
          <button class="btn sm" onclick={() => void compute()}>Recompute</button>
        {:else}
          <button class="btn sm link" onclick={() => void compute()}>Recompute</button>
        {/if}
      </p>
    {/if}

    {#if task}
      <div class="progress" role="status" aria-live="polite" data-testid="progress">
        <span>Projecting {chosen?.name}{task.message ? `: ${task.message}` : ''}</span>
        <progress max="1" value={task.progress ?? 0}></progress>
        <button class="btn sm" onclick={() => void cancel()}>Cancel</button>
      </div>
    {/if}

    {#if error}
      <div class="error-box" data-state={error.forbidden ? 'forbidden' : 'error'}>
        <strong>{error.title}</strong>
        {#if error.hint}<p>{error.hint}</p>{/if}
      </div>
    {/if}

    <SelectionBar onuse={useSelection} onopengraph={openAsGraph} />

    <div class="body">
      <div class="canvas" class:busy={loading || !!task}>
        {#if proj}
          <EmbeddingMap
            bind:this={map}
            points={proj.points}
            colourIndex={legend.colourIndex}
            {density}
            densityBox={proj.head.density.bbox}
            {bounds}
            selected={selectedPoints}
            {mode}
            label="Embedding map of {chosen?.name ?? 'the index'}"
            {onselect}
            {onhover}
            {onopen}
            {onview}
            onfail={(r) => (error = { title: r })}
          />
        {:else if loading && !task}
          <div class="empty"><span class="spinner"></span> Loading the projection</div>
        {/if}
        {#if hover && proj}
          <div class="tip" style:left="{hover.at.x + 14}px" style:top="{hover.at.y + 14}px">
            <div class="tl">
              {hover.label ?? displayIri(proj.points.entities[hover.i], prefixes)}
            </div>
            {#if className(hover.i)}<div class="faint">{className(hover.i)}</div>{/if}
            {#if hover.neighbours?.length}
              <div class="nb">
                <span class="faint">Nearest</span>
                {#each hover.neighbours as n, k (k)}
                  <div>
                    <span>{n.label}</span> <span class="faint num">{fmtScore(n.score)}</span>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
      </div>
      {#if legend.list.length}
        <aside class="legend" aria-label="Legend">
          <h2>Colour by {legend.title}</h2>
          <ul>
            {#each legend.list as c (c.key + c.colour)}
              <li>
                <span
                  class="sw"
                  style:background="var(--lens-{c.colour === OTHER ? 'other' : c.colour + 1})"
                ></span>
                <span class="lab">{c.label}</span>
                <span class="faint num">{fmtInt(c.count)}</span>
              </li>
            {/each}
          </ul>
        </aside>
      {/if}
    </div>

    {#if showList && proj}
      <section class="panel list" aria-label="Points in view">
        <div class="panel-head">
          <h2>Points in view</h2>
          <span class="faint">the first {fmtInt(inView.length)}</span>
        </div>
        <table>
          <thead
            ><tr><th>Entity</th><th>Class</th><th class="num">x</th><th class="num">y</th></tr
            ></thead
          >
          <tbody>
            {#each inView as i (i)}
              {@const e = proj.points.entities[i]}
              <tr class:sel={selectedPoints?.has(i)}>
                <td
                  ><label
                    ><input
                      type="checkbox"
                      checked={selection.has(keyOf(e))}
                      onchange={() => selection.toggle([keyOf(e)], SOURCE)}
                    />
                    {#if e.startsWith('_:')}<span class="mono">{e}</span>{:else}<a
                        href={exploreHref(e)}>{displayIri(e, prefixes)}</a
                      >{/if}</label
                  ></td
                >
                <td>{className(i)}</td>
                <td class="num">{proj.points.x[i].toFixed(3)}</td>
                <td class="num">{proj.points.y[i].toFixed(3)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
    {/if}
  {/if}
</div>

<style>
  .page {
    padding: 24px 28px 40px;
    gap: 10px;
  }
  .head h1 {
    margin: 0;
  }
  .note {
    margin: 4px 0;
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 8px;
  }
  .field {
    display: grid;
    gap: 2px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .colour {
    width: 14em;
  }
  .seg {
    display: inline-flex;
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
  .lasso-toggle {
    display: none;
  }
  .facts {
    margin: 4px 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .btn.link {
    border-color: transparent;
    background: none;
  }
  .progress {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 6px 0;
  }
  .progress progress {
    flex: 1;
    max-width: 320px;
  }
  .body {
    display: grid;
    grid-template-columns: 1fr 220px;
    gap: 12px;
    margin-top: 6px;
  }
  .canvas {
    position: relative;
    height: 62vh;
    min-height: 360px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    overflow: hidden;
  }
  .canvas.busy :global(.emap) {
    opacity: 0.5;
  }
  .canvas .empty {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    height: 100%;
  }
  .tip {
    position: absolute;
    z-index: 5;
    max-width: 280px;
    padding: 6px 8px;
    background: var(--surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--r);
    font-size: var(--fs-sm);
    pointer-events: none;
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.15);
  }
  .tip .tl {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .tip .nb {
    margin-top: 4px;
  }
  .legend h2 {
    font-size: var(--fs-sm);
    margin: 0 0 6px;
  }
  .legend ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .legend li {
    display: grid;
    grid-template-columns: 12px 1fr auto;
    gap: 6px;
    align-items: center;
  }
  .legend .lab {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sw {
    width: 12px;
    height: 12px;
    border-radius: 50%;
  }
  .list {
    margin-top: 12px;
    overflow-x: auto;
  }
  .list table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-sm);
  }
  .list th,
  .list td {
    text-align: left;
    padding: 3px 8px;
    border-bottom: 1px solid var(--border);
  }
  .list .num {
    text-align: right;
  }
  .list tr.sel {
    background: color-mix(in srgb, var(--spark) 10%, transparent);
  }
  @media (max-width: 720px) {
    .page {
      padding: 16px 16px 32px;
    }
    .body {
      grid-template-columns: 1fr;
    }
    .seg {
      display: none;
    }
    .lasso-toggle {
      display: inline-flex;
    }
    .canvas {
      height: 60vh;
    }
  }
</style>
