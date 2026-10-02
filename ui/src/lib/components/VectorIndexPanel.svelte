<script lang="ts">
  // The dataset page's vector indexes (`/$/vector/{ds}`): a card per index with its
  // predicate, dimension, metric, model, HNSW settings, build state, sizes and exact
  // threshold, a recall measurement, and the packed predicates that have no index.
  // Admins of the dataset create, edit, rebuild and drop indexes (builds are
  // `vector-index` tasks); the others see the cards without those actions.
  import { onMount } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtBytes, fmtInt, fmtMs, fmtRelative, fmtTime } from '$lib/format';
  import { displayIri, sparqlIri, type PrefixMap } from '$lib/rdf';
  import { load as loadStored, save as saveStored } from '$lib/storage';
  import { LatestRun } from '$lib/supersede';
  import {
    fmtRecall,
    indexConfig,
    indexForm,
    metricInfo,
    METRICS,
    needsBuild,
    overlayShare,
    parseVector,
    recallKey,
    resolvePredicate,
    VECTOR_DATATYPE,
    type IndexForm,
    type IndexFormErrors,
    type RememberedRecall,
  } from '$lib/vectors';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';

  let {
    name,
    prefixes,
    predicates = [],
    canAdmin = false,
    readOnly = false,
    busy = false,
    refreshKey = 0,
    onstart,
    onstarted,
    onchanged,
  }: {
    name: string;
    prefixes: PrefixMap;
    /** The dataset's predicates (most used first), offered when creating an index. */
    predicates?: string[];
    /** The caller may create, edit, rebuild and drop indexes (admin on the dataset). */
    canAdmin?: boolean;
    /** The server is read-only: the admin actions are disabled. */
    readOnly?: boolean;
    /** Another action is starting; disables the task buttons. */
    busy?: boolean;
    /** Bump to reload the status (a task finished, data changed). */
    refreshKey?: number;
    /** Start a task with a toast (the page's task runner). */
    onstart: (label: string, fn: () => Promise<api.Task>) => Promise<void>;
    /** A task was started from the index dialog. */
    onstarted: (t: api.Task) => void;
    /** Something changed on the server: refresh the dataset. */
    onchanged: () => void;
  } = $props();

  type Loaded = { kind: 'ok'; status: api.VectorStatus } | { kind: 'unsupported' };

  let loaded = $state<Loaded | null>(null);
  let error = $state<string | null>(null);
  let now = $state(Date.now());
  const runs = new LatestRun();

  async function load() {
    const owns = runs.claim('vector');
    try {
      const s = await api.vectorStatus(name);
      if (!owns()) return;
      loaded = { kind: 'ok', status: s };
      error = null;
    } catch (e) {
      if (!owns()) return;
      if (e instanceof api.ApiError && (e.status === 404 || e.status === 501) && !loaded)
        loaded = { kind: 'unsupported' };
      else error = api.errorMessage(e);
    }
  }

  $effect(() => {
    void name;
    void refreshKey;
    void load();
  });

  const status = $derived(loaded?.kind === 'ok' ? loaded.status : null);
  const indexes = $derived(status?.indexes ?? []);
  const building = $derived(indexes.some((i) => i.state === 'building'));

  onMount(() => {
    const t = setInterval(() => {
      now = Date.now();
      // follow a build
      if (building) void load();
    }, 1500);
    return () => clearInterval(t);
  });

  const stateClass = (s: api.VectorIndexState) =>
    s === 'ready' ? 'ok' : s === 'building' ? 'warn' : 'danger';
  const actionsOff = $derived(readOnly || !canAdmin);
  const offTitle = $derived(readOnly ? 'The server is read-only' : undefined);
  const searchHref = (index: string) =>
    `${resolve('/similar')}?ds=${encodeURIComponent(name)}&index=${encodeURIComponent(index)}`;

  // --- recall --------------------------------------------------------------------------
  const RECALL_KS = [1, 10, 50, 100];
  let recallK = $state<Record<string, number>>({});
  let recallEf = $state<Record<string, string>>({});
  let measured = $state<Record<string, RememberedRecall>>({});
  let measuring = $state<Record<string, boolean>>({});
  let recallError = $state<Record<string, string | null>>({});

  // the last measurement of each index, kept in this browser
  const recalls = $derived(
    Object.fromEntries(
      indexes.map((i) => [
        i.name,
        measured[`${name}/${i.name}`] ??
          loadStored<RememberedRecall | null>(recallKey(name, i.name), null),
      ]),
    ) as Record<string, RememberedRecall | null>,
  );

  async function measure(ix: api.VectorIndexStatus) {
    const efText = (recallEf[ix.name] ?? '').trim();
    const ef = efText ? Number(efText) : undefined;
    if (ef != null && !(Number.isInteger(ef) && ef >= 1 && ef <= 4096)) {
      recallError[ix.name] = 'ef is a whole number from 1 to 4096.';
      return;
    }
    measuring[ix.name] = true;
    recallError[ix.name] = null;
    try {
      const r = await api.vectorRecall(name, ix.name, { k: recallK[ix.name] ?? 10, ef });
      const kept: RememberedRecall = { ...r, at: new Date().toISOString() };
      measured[`${name}/${ix.name}`] = kept;
      saveStored(recallKey(name, ix.name), kept);
    } catch (e) {
      recallError[ix.name] = api.errorMessage(e);
    } finally {
      measuring[ix.name] = false;
    }
  }

  // --- rebuild and drop ----------------------------------------------------------------
  async function rebuild(ix: api.VectorIndexStatus) {
    await onstart(`Vector index ${ix.name} rebuild`, () => api.rebuildVectorIndex(name, ix.name));
    void load();
  }

  let dropTarget = $state<api.VectorIndexStatus | null>(null);
  let dropOpen = $state(false);
  let dropping = $state(false);

  async function drop() {
    if (!dropTarget) return;
    dropping = true;
    try {
      await api.dropVectorIndex(name, dropTarget.name);
      toasts.push('success', `Vector index ${dropTarget.name} dropped`, 'Its files were deleted.');
      dropOpen = false;
      onchanged();
      void load();
    } catch (e) {
      toasts.error('Could not drop the vector index', e);
    } finally {
      dropping = false;
    }
  }

  // --- create and edit -----------------------------------------------------------------
  let formOpen = $state(false);
  let editing = $state<api.VectorIndexStatus | null>(null);
  let form = $state<IndexForm>(indexForm());
  let touched = $state(false);
  let saving = $state(false);
  let formError = $state<string | null>(null);
  let detecting = $state(false);
  let detectNote = $state<string | null>(null);

  function openCreate(predicate?: string, dimension?: number) {
    editing = null;
    form = indexForm(undefined, prefixes);
    if (predicate) {
      form.predicate = displayIri(predicate, prefixes);
      form.name = suggestName(predicate);
    }
    if (dimension) form.dimension = String(dimension);
    touched = false;
    formError = null;
    detectNote = null;
    formOpen = true;
  }

  function openEdit(ix: api.VectorIndexStatus) {
    editing = ix;
    form = indexForm(ix, prefixes);
    touched = false;
    formError = null;
    detectNote = null;
    formOpen = true;
  }

  /** `ex:embedding` → `embedding`, unless an index already has that name. */
  function suggestName(iri: string): string {
    const local = (/[^#/:]+$/.exec(iri)?.[0] ?? 'vectors').replace(/[^A-Za-z0-9_.-]/g, '_');
    let n = local.replace(/^\./, '_').slice(0, 60) || 'vectors';
    for (let i = 2; indexes.some((x) => x.name === n); i++) n = `${local.slice(0, 58)}-${i}`;
    return n;
  }

  const predicateIri = $derived(resolvePredicate(form.predicate, prefixes));
  /** An empty name takes the predicate's local name. */
  const namePlaceholder = $derived(predicateIri ? suggestName(predicateIri) : 'embedding');
  const indexName = $derived(editing?.name ?? (form.name.trim() || namePlaceholder));
  const checked = $derived(indexConfig({ ...form, name: indexName }, prefixes));
  const errors = $derived<IndexFormErrors>(touched ? (checked.errors ?? {}) : {});
  const rebuilds = $derived(!!editing && !!checked.config && needsBuild(editing, checked.config));

  /** Packed predicates first, then the dataset's predicates that look like embeddings. */
  const suggestions = $derived.by(() => {
    const packed = (status?.predicates ?? []).map((p) => p.predicate);
    const looks = predicates.filter((p) => /embed|vector|emb$/i.test(p));
    const taken = new Set(indexes.map((i) => i.predicate));
    return [...new Set([...packed, ...looks])].filter((p) => !taken.has(p)).slice(0, 12);
  });

  // a new predicate: read the dimension of one of its vectors
  const detectRuns = new LatestRun();
  async function detect(iri: string) {
    const owns = detectRuns.claim('dim');
    detecting = true;
    detectNote = null;
    try {
      const rows = await api.select(
        name,
        `SELECT ?o WHERE { ?s ${sparqlIri(iri)} ?o . FILTER(DATATYPE(?o) = <${VECTOR_DATATYPE}>) } LIMIT 1`,
      );
      if (!owns()) return;
      const o = rows[0]?.o;
      const v = o?.type === 'literal' ? parseVector(o.value) : null;
      if (v) {
        form.dimension = String(v.length);
        detectNote = `A vector of this predicate has ${v.length} dimensions.`;
      } else detectNote = 'No vector literal of this predicate was found.';
    } catch {
      if (owns()) detectNote = null;
    } finally {
      if (owns()) detecting = false;
    }
  }

  $effect(() => {
    if (!formOpen || editing || !predicateIri || form.dimension.trim()) return;
    const iri = predicateIri;
    const t = setTimeout(() => void detect(iri), 300);
    return () => clearTimeout(t);
  });

  async function save(e: Event) {
    e.preventDefault();
    touched = true;
    if (!checked.config) return;
    saving = true;
    formError = null;
    try {
      const r = await api.putVectorIndex(name, indexName, checked.config);
      toasts.push(
        'info',
        editing ? `Vector index ${indexName} saved` : `Vector index ${indexName} created`,
        r.index.state === 'building' ? `Building the index (task ${r.task.id})` : undefined,
      );
      onstarted(r.task);
      formOpen = false;
      void load();
    } catch (err) {
      formError =
        err instanceof api.ApiError && err.status === 409
          ? `${err.message}. A predicate has at most one index.`
          : api.errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<section class="panel" aria-labelledby="vector-indexes">
  <div class="panel-head">
    <h2 id="vector-indexes">Vector indexes</h2>
    {#if status}<span class="faint">{indexes.length}</span>{/if}
    <span class="spacer"></span>
    {#if status && canAdmin}
      <button
        class="btn sm"
        onclick={() => openCreate()}
        disabled={readOnly}
        title={offTitle ?? 'Index the vectors of a predicate'}
        ><Icon name="plus" size={13} /> New index…</button
      >
    {/if}
  </div>
  <div class="panel-body vec">
    {#if error && !loaded}
      <div class="error-box">
        <strong>Could not load the vector indexes.</strong>
        <span class="muted">{error}</span>
      </div>
    {:else if !loaded}
      <div class="row faint"><span class="spinner"></span> Loading…</div>
    {:else if loaded.kind === 'unsupported'}
      <p class="faint">This server has no vector indexes.</p>
    {:else if status}
      <div
        class="mem"
        title="Packed vectors and graphs of every dataset against the server's budget"
      >
        <div class="bar">
          <span
            style:width="{Math.min(
              100,
              (status.usedBytes / Math.max(1, status.budgetBytes)) * 100,
            )}%"
          ></span>
        </div>
        <span class="faint small"
          >{fmtBytes(status.usedBytes)} of the {fmtBytes(status.budgetBytes)} vector budget · generation
          <span class="mono">{status.generation}</span></span
        >
      </div>
      {#if !indexes.length}
        <p class="faint">
          No vector index. Searches with <span class="mono">spk:vectorSearch</span> scan the vectors exactly.
          An index builds an HNSW graph over one predicate's vectors, which makes searches over large
          sets fast.
        </p>
      {/if}
      <div class="cards">
        {#each indexes as ix (ix.name)}
          {@const recall = recalls[ix.name]}
          {@const share = overlayShare(ix)}
          {@const skipped = (
            [
              ['malformed', ix.skipped.malformed],
              ['of another dimension', ix.skipped.wrongDimension],
              ['zero vectors (no cosine)', ix.skipped.zeroNorm],
            ] as const
          ).filter(([, n]) => n > 0)}
          <article class="vcard" aria-label="Vector index {ix.name}">
            <header class="vhead">
              <h3 class="mono">{ix.name}</h3>
              <span class="badge {stateClass(ix.state)}" title={ix.message ?? undefined}>
                <Icon name={ix.state === 'ready' ? 'check' : 'alert'} size={12} />
                {ix.state}{ix.state === 'building' && ix.progress != null
                  ? ` ${Math.round(ix.progress * 100)}%`
                  : ''}
              </span>
            </header>
            <div class="pred t-iri mono" title={ix.predicate}>
              {displayIri(ix.predicate, prefixes)}
            </div>
            <div class="chips">
              <span class="chip">{fmtInt(ix.dimension)} dimensions</span>
              <span
                class="chip"
                title="{metricInfo(ix.metric).label}: {metricInfo(ix.metric).better} is closer"
                >{ix.metric}</span
              >
              {#if ix.model}<span class="chip model" title="Embedding model">{ix.model}</span>{/if}
            </div>
            {#if ix.state === 'building'}
              <div
                class="progress"
                role="progressbar"
                aria-label="Build progress"
                aria-valuenow={Math.round((ix.progress ?? 0) * 100)}
              >
                <span style:width="{(ix.progress ?? 0) * 100}%"></span>
              </div>
            {/if}
            {#if ix.message && ix.state !== 'ready'}<p class="small msg">{ix.message}</p>{/if}
            <dl class="facts">
              <div>
                <dt>Rows</dt>
                <dd>{fmtInt(ix.rows)}</dd>
              </div>
              <div>
                <dt title="Packed vectors and the graph's links">Memory</dt>
                <dd>{fmtBytes(ix.memory.segmentBytes + ix.memory.hnswBytes)}</dd>
              </div>
              <div>
                <dt title="Searches over at most this many rows are exact">Exact below</dt>
                <dd>{fmtInt(ix.exactThreshold)}</dd>
              </div>
            </dl>
            <div class="kv">
              <span class="faint">HNSW</span>
              <span>
                {#if ix.hnsw}
                  M {ix.hnsw.m} · efConstruction {ix.hnsw.efConstruction} · efSearch {ix.hnsw
                    .efSearch}
                  {#if ix.state === 'ready'}<span class="faint"
                      >· {fmtInt(ix.hnsw.nodes)} nodes, {ix.hnsw.layers}
                      {ix.hnsw.layers === 1 ? 'layer' : 'layers'}</span
                    >{/if}
                {:else}
                  <span class="faint">no graph, searched exactly</span>
                {/if}
              </span>
              <span class="faint">Size</span>
              <span>
                vectors {fmtBytes(ix.memory.segmentBytes)}
                {ix.hnsw ? `· graph ${fmtBytes(ix.memory.hnswBytes)}` : ''}
                <span class="faint"
                  >· {ix.memory.residency === 'mmap' ? 'mapped from the file' : 'in memory'}</span
                >
                {#if ix.files}<span class="faint">· file {fmtBytes(ix.files.bytes)}</span>{/if}
              </span>
              {#if ix.overlay.inserts || ix.overlay.deletes}
                <span class="faint">Since the build</span>
                <span>
                  +{fmtInt(ix.overlay.inserts)} −{fmtInt(ix.overlay.deletes)}
                  <span class="faint">searched exactly</span>
                  {#if share > 0.1}<span class="warn small"
                      >· compacting the dataset folds them into the index</span
                    >{/if}
                </span>
              {/if}
              {#if skipped.length}
                <span class="faint">Skipped</span>
                <span
                  >{#each skipped as [what, n] (what)}<span class="chip warn"
                      >{fmtInt(n)} {what}</span
                    >{/each}</span
                >
              {/if}
              <span class="faint">Last build</span>
              <span>
                {#if ix.lastBuild}
                  <span title={fmtTime(ix.lastBuild.at)}>{fmtRelative(ix.lastBuild.at, now)}</span>
                  <span class="faint"
                    >· {fmtInt(ix.lastBuild.rows)} rows in {fmtMs(ix.lastBuild.ms)}</span
                  >
                {:else if ix.files?.opened}
                  <span class="faint">read from its file, not built in this process</span>
                {:else}
                  <span class="faint">—</span>
                {/if}
              </span>
              <span class="faint">Recall</span>
              <span class="recall">
                {#if recall}
                  <strong>{fmtRecall(recall.recall)}</strong>
                  <span class="faint"
                    >recall@{recall.k}{recall.ef
                      ? ` at ef ${recall.ef}`
                      : ', searched exactly because of the exact threshold'}, {fmtInt(
                      recall.samples,
                    )} samples · graph {fmtMs(recall.hnswMs)}, exact {fmtMs(recall.exactMs)} per search
                    ·
                    <span title={fmtTime(recall.at)}>{fmtRelative(recall.at, now)}</span></span
                  >
                {:else}
                  <span class="faint">not measured</span>
                {/if}
              </span>
            </div>
            {#if ix.hnsw}
              <div class="measure">
                <label class="ctl">
                  <span class="faint">k</span>
                  <select
                    class="select sm"
                    value={recallK[ix.name] ?? 10}
                    onchange={(e) => (recallK[ix.name] = Number(e.currentTarget.value))}
                    aria-label="Recall k"
                  >
                    {#each RECALL_KS as k (k)}<option value={k}>{k}</option>{/each}
                  </select>
                </label>
                <label class="ctl">
                  <span class="faint">ef</span>
                  <input
                    class="input sm"
                    inputmode="numeric"
                    placeholder={String(ix.hnsw.efSearch)}
                    bind:value={recallEf[ix.name]}
                    aria-label="Recall ef"
                  />
                </label>
                <button
                  class="btn sm"
                  onclick={() => measure(ix)}
                  disabled={measuring[ix.name] || ix.state !== 'ready'}
                  title="Search with stored vectors through the graph and exactly, and compare"
                >
                  {#if measuring[ix.name]}<span class="spinner"></span>{:else}<Icon
                      name="target"
                      size={13}
                    />{/if}
                  Measure recall
                </button>
              </div>
              {#if recallError[ix.name]}<div class="error-box small">
                  {recallError[ix.name]}
                </div>{/if}
            {/if}
            <div class="row actions">
              <a class="btn sm" href={searchHref(ix.name)}
                ><Icon name="search" size={13} /> Search</a
              >
              <span class="spacer"></span>
              {#if canAdmin}
                <button
                  class="btn sm"
                  onclick={() => ((dropTarget = ix), (dropOpen = true))}
                  disabled={actionsOff}
                  title={offTitle ?? 'Drop the index and its files'}
                  ><Icon name="trash" size={13} /> Drop</button
                >
                <button
                  class="btn sm"
                  onclick={() => openEdit(ix)}
                  disabled={actionsOff || busy}
                  title={offTitle}><Icon name="filter" size={13} /> Edit…</button
                >
                <button
                  class="btn sm"
                  onclick={() => rebuild(ix)}
                  disabled={actionsOff || busy || ix.state === 'building'}
                  title={offTitle ?? 'Build the index again from the data'}
                  ><Icon name="refresh" size={13} /> Rebuild</button
                >
              {/if}
            </div>
          </article>
        {/each}
      </div>
      {#if status.predicates.length}
        <div class="packed">
          <h3 class="sub">Packed without an index</h3>
          <p class="faint small">
            These predicates were searched, and their vectors are packed for exact scans.
          </p>
          <ul>
            {#each status.predicates as p (p.predicate)}
              <li>
                <span class="t-iri mono pname" title={p.predicate}
                  >{displayIri(p.predicate, prefixes)}</span
                >
                <span class="faint small"
                  >{p.dimensions.map((d) => `${fmtInt(d.vectors)} × ${d.dimension}`).join(', ')} · {fmtBytes(
                    p.bytes,
                  )}{p.malformed ? ` · ${fmtInt(p.malformed)} malformed` : ''}</span
                >
                {#if canAdmin}
                  <button
                    class="btn sm ghost"
                    onclick={() => openCreate(p.predicate, p.dimensions[0]?.dimension)}
                    disabled={readOnly}
                    title={offTitle}><Icon name="plus" size={13} /> Index…</button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    {/if}
  </div>
</section>

<Modal
  bind:open={formOpen}
  title={editing ? `Edit vector index ${editing.name}` : 'New vector index'}
  width={540}
>
  <form id="vector-index-form" class="form" onsubmit={save} novalidate>
    {#if !editing}
      <label class="field">
        Name
        <input
          class="input mono"
          bind:value={form.name}
          placeholder={namePlaceholder}
          autocomplete="off"
          spellcheck="false"
          aria-invalid={!!errors.name}
        />
        {#if errors.name}<span class="bad">{errors.name}</span>{/if}
      </label>
    {/if}
    <label class="field">
      Predicate <span class="faint">(an IRI or a prefixed name)</span>
      <input
        class="input mono"
        bind:value={form.predicate}
        list="vector-predicates"
        placeholder="ex:embedding"
        autocomplete="off"
        spellcheck="false"
        aria-invalid={!!errors.predicate}
      />
      <datalist id="vector-predicates">
        {#each suggestions as p (p)}<option value={displayIri(p, prefixes)}></option>{/each}
      </datalist>
      {#if errors.predicate}<span class="bad">{errors.predicate}</span>{/if}
    </label>
    <div class="grid2">
      <label class="field">
        Dimension
        <input
          class="input"
          inputmode="numeric"
          bind:value={form.dimension}
          placeholder={detecting ? 'detecting…' : '384'}
          aria-invalid={!!errors.dimension}
        />
        {#if errors.dimension}<span class="bad">{errors.dimension}</span>{/if}
      </label>
      <label class="field">
        Metric
        <select class="select" bind:value={form.metric}>
          {#each METRICS as m (m)}<option value={m}>{metricInfo(m).label}</option>{/each}
        </select>
      </label>
    </div>
    {#if detectNote}<p class="faint small">{detectNote}</p>{/if}
    <label class="field">
      Model <span class="faint">(a label, optional)</span>
      <input
        class="input"
        bind:value={form.model}
        placeholder="all-MiniLM-L6-v2"
        autocomplete="off"
        aria-invalid={!!errors.model}
      />
      {#if errors.model}<span class="bad">{errors.model}</span>{/if}
    </label>
    <label class="check">
      <input type="checkbox" bind:checked={form.hnsw} />
      <span
        >Build an HNSW graph <span class="faint"
          >(approximate search; without it the index keeps the packed vectors and searches exactly)</span
        ></span
      >
    </label>
    {#if form.hnsw}
      <div class="grid3">
        <label
          class="field"
          title="Links per node. More links raise recall, memory and build time."
        >
          M
          <input class="input" inputmode="numeric" bind:value={form.m} aria-invalid={!!errors.m} />
          {#if errors.m}<span class="bad">{errors.m}</span>{/if}
        </label>
        <label
          class="field"
          title="Candidates kept while a node is inserted. More raise recall and build time."
        >
          efConstruction
          <input
            class="input"
            inputmode="numeric"
            bind:value={form.efConstruction}
            aria-invalid={!!errors.efConstruction}
          />
          {#if errors.efConstruction}<span class="bad">{errors.efConstruction}</span>{/if}
        </label>
        <label
          class="field"
          title="Candidates kept by a search. A query can set its own with ef:N."
        >
          efSearch
          <input
            class="input"
            inputmode="numeric"
            bind:value={form.efSearch}
            aria-invalid={!!errors.efSearch}
          />
          {#if errors.efSearch}<span class="bad">{errors.efSearch}</span>{/if}
        </label>
      </div>
    {/if}
    <label class="field">
      Exact threshold <span class="faint">(searches over at most this many rows are exact)</span>
      <input
        class="input"
        inputmode="numeric"
        bind:value={form.exactThreshold}
        aria-invalid={!!errors.exactThreshold}
      />
      {#if errors.exactThreshold}<span class="bad">{errors.exactThreshold}</span>{/if}
    </label>
    <p class="faint small">
      {#if !editing}
        The index is built by a background task. Searches scan exactly until the graph is ready.
      {:else if rebuilds}
        These changes build the index again. Searches scan exactly until the new graph is ready.
      {:else}
        The build is kept: only efSearch, the exact threshold and the model changed.
      {/if}
    </p>
    {#if formError}<div class="error-box">{formError}</div>{/if}
  </form>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (formOpen = false)}>Cancel</button>
    <button
      class="btn primary"
      type="submit"
      form="vector-index-form"
      disabled={saving || (touched && !checked.config)}
    >
      {#if saving}<span class="spinner"></span>{/if}
      {editing ? (rebuilds ? 'Save and rebuild' : 'Save') : 'Create'}
    </button>
  {/snippet}
</Modal>

<Modal bind:open={dropOpen} title="Drop vector index {dropTarget?.name ?? ''}?">
  <p>
    This drops the index <span class="mono">{dropTarget?.name}</span> of
    <span class="mono">{name}</span> and deletes its files. Searches of
    <span class="mono">{dropTarget ? displayIri(dropTarget.predicate, prefixes) : ''}</span> keep working,
    by scanning the vectors exactly. The data is not changed.
  </p>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (dropOpen = false)}>Cancel</button>
    <button class="btn danger" type="button" onclick={drop} disabled={dropping}>
      {#if dropping}<span class="spinner"></span>{:else}<Icon name="trash" size={13} />{/if}
      Drop index
    </button>
  {/snippet}
</Modal>

<style>
  .vec {
    display: grid;
    gap: 12px;
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .vec > p {
    margin: 0;
  }
  .mem {
    display: grid;
    gap: 4px;
  }
  .bar,
  .progress {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar span,
  .progress span {
    display: block;
    height: 100%;
    background: var(--iri);
    transition: width 0.3s;
  }
  .progress span {
    background: var(--spark);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 340px), 1fr));
    gap: 10px;
  }
  .vcard {
    display: grid;
    gap: 8px;
    align-content: start;
    min-width: 0;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .vhead {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .vhead h3 {
    margin: 0;
    font-size: var(--fs-md);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .pred,
  .pname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    display: inline-block;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-right: 4px;
    padding: 1px 7px;
    border-radius: 10px;
    font-size: 11px;
    background: color-mix(in srgb, var(--iri) 10%, transparent);
  }
  .chips .chip {
    margin: 0;
  }
  .chip.model {
    background: color-mix(in srgb, var(--literal) 12%, transparent);
  }
  .chip.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .msg {
    margin: 0;
    color: var(--text-2);
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    margin: 0;
  }
  .facts dt {
    color: var(--text-2);
    font-size: var(--fs-xs);
  }
  .facts dd {
    margin: 2px 0 0;
    font-size: var(--fs-md);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .kv {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 5px 12px;
    align-items: baseline;
  }
  .kv > span {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .warn {
    color: var(--warn);
  }
  .measure {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
  }
  .ctl {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .select.sm,
  .input.sm {
    height: 24px;
    font-size: var(--fs-sm);
  }
  .input.sm {
    width: 64px;
  }
  .small {
    font-size: 11px;
  }
  .actions {
    flex-wrap: wrap;
  }
  a.btn {
    text-decoration: none;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .packed {
    display: grid;
    gap: 4px;
  }
  .packed .sub {
    margin: 0;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
  }
  .packed p {
    margin: 0;
  }
  .packed ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 2px;
  }
  .packed li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 10px;
    min-width: 0;
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .form p {
    margin: 0;
  }
  .grid2 {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
  }
  .grid3 {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 10px;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    font-size: var(--fs-sm);
  }
  .check input {
    margin-top: 2px;
    accent-color: var(--iri);
  }
  .bad {
    color: var(--danger);
    font-size: var(--fs-xs);
    font-weight: 400;
  }
  @media (max-width: 420px) {
    .grid3 {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
