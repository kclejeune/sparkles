<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { receiptSummary } from '$lib/commits';
  import { fmtBytes, fmtCompact, fmtInt, fmtMs, fmtRelative, fmtTime } from '$lib/format';
  import { displayIri, localName, WELL_KNOWN } from '$lib/rdf';
  import { load, save } from '$lib/storage';
  import BackupsPanel from '$components/BackupsPanel.svelte';
  import CloneDialog from '$components/CloneDialog.svelte';
  import DatasetDialogs from '$components/DatasetDialogs.svelte';
  import FullTextPanel from '$components/FullTextPanel.svelte';
  import HistoryPanel from '$components/HistoryPanel.svelte';
  import Icon from '$components/Icon.svelte';
  import ReasoningPanel from '$components/ReasoningPanel.svelte';
  import TaskList from '$components/TaskList.svelte';
  import TermView from '$components/TermView.svelte';

  const name = $derived(page.params.name ?? '');
  const info = $derived(app.datasets.find((d) => d.name === name));
  const prefixes = $derived(app.prefixes(name));

  let stats = $state<api.DatasetStats | null>(null);
  let statsError = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  let taskKick = $state(0);
  /** Bumped after anything that may have changed the dataset (panels reload). */
  let refreshKick = $state(0);
  let deleteTarget = $state<string | null>(null);
  let cloneOpen = $state(false);
  /** The last clone of this dataset that finished, for an "Open" link. */
  let cloned = $state<string | null>(null);
  $effect(() => {
    void name;
    cloned = null;
  });

  async function loadStats() {
    loading = true;
    try {
      stats = await api.datasetStats(name);
      statsError = null;
    } catch (e) {
      statsError = e as Error;
    } finally {
      loading = false;
    }
  }

  // only a new name resets the page: reloading the prefixes (refreshAll) must not
  // unmount the panels and their open dialogs
  $effect(() => {
    if (!name) return;
    const ds = name;
    untrack(() => {
      stats = null;
      void app.loadPrefixes(ds);
      void loadStats();
    });
  });

  function refreshAll() {
    refreshKick++;
    void loadStats();
    void app.refreshDatasets();
    void app.loadPrefixes(name, true);
  }

  // --- actions -----------------------------------------------------------
  let acting = $state<string | null>(null);

  async function startTask(label: string, fn: () => Promise<api.Task>) {
    acting = label;
    try {
      const t = await fn();
      toasts.push('info', `${label} started`, t?.id ? `Task ${t.id}` : undefined);
      taskKick++;
    } catch (e) {
      toasts.error(`${label} failed`, e);
    } finally {
      acting = null;
    }
  }

  // read-only servers disable the write actions
  let readOnly = $state(false);
  $effect(() => {
    api.serverInfo().then(
      (s) => (readOnly = s?.readOnly === true),
      () => {},
    );
  });

  // upload
  let files = $state<File[]>([]);
  let graph = $state('');
  let dragging = $state(false);
  let uploading = $state(false);
  let progress = $state(0);
  let uploadError = $state<string | null>(null);
  /** What the last upload committed ("commit 42 · +120 −0"). */
  let uploadReceipt = $state<api.Receipt | null>(null);
  $effect(() => {
    void name;
    uploadReceipt = null;
  });
  let uploadCtl: AbortController | null = null;
  let fileInput: HTMLInputElement | undefined = $state();

  const ACCEPT = '.ttl,.nt,.nq,.trig,.rdf,.owl,.xml,.jsonld,.n3,.gz,.zst,.br,.lz4';

  function addFiles(list: FileList | null | undefined) {
    if (!list) return;
    const incoming = [...list].filter(
      (f) => !files.some((x) => x.name === f.name && x.size === f.size),
    );
    files = [...files, ...incoming];
  }

  async function doUpload() {
    if (!files.length) return;
    uploading = true;
    uploadError = null;
    progress = 0;
    uploadCtl = new AbortController();
    try {
      const res = await api.upload(name, files, {
        graph: graph.trim() || undefined,
        onProgress: (p) => (progress = p.total ? p.loaded / p.total : 0),
        signal: uploadCtl.signal,
      });
      const n =
        typeof res === 'object' ? (res.quadCount ?? res.tripleCount ?? res.count) : undefined;
      const receipt = typeof res === 'object' ? res.receipt : undefined;
      uploadReceipt = receipt ?? null;
      toasts.push(
        'success',
        `Uploaded ${files.length} file${files.length === 1 ? '' : 's'}`,
        receipt ? receiptSummary(receipt) : n != null ? `${fmtInt(n)} quads added` : undefined,
      );
      files = [];
      refreshAll();
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError'))
        uploadError = api.errorMessage(e);
    } finally {
      uploading = false;
      uploadCtl = null;
    }
  }

  // result cache
  let clearingCache = $state(false);
  async function clearCache() {
    clearingCache = true;
    try {
      const r = await api.clearResultCache(name);
      toasts.push(
        'success',
        'Result cache cleared',
        r ? `${fmtInt(r.cleared)} entries, ${fmtBytes(r.bytes)}` : undefined,
      );
      void loadStats();
    } catch (e) {
      toasts.error('Could not clear the result cache', e);
    } finally {
      clearingCache = false;
    }
  }

  // SHACL validation
  const DEFAULT_SHAPES = `@prefix sh:   <http://www.w3.org/ns/shacl#> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix ex:   <http://example.org/> .

# Every ex:Person (subclasses included) has exactly one string name
# and exactly one integer age between 0 and 150.
ex:PersonShape a sh:NodeShape ;
  sh:targetClass ex:Person ;
  sh:property [
    sh:path foaf:name ;
    sh:minCount 1 ; sh:maxCount 1 ;
    sh:datatype xsd:string ;
  ] ;
  sh:property [
    sh:path foaf:age ;
    sh:minCount 1 ; sh:maxCount 1 ;
    sh:datatype xsd:integer ;
    sh:minInclusive 0 ; sh:maxInclusive 150 ;
  ] .`;
  const shapesKey = $derived(`sparkles.shacl.${name}`);
  let shapes = $state(DEFAULT_SHAPES);
  let shaclGraph = $state('default');
  let useInferences = $state(true);
  let validating = $state(false);
  let downloadingReport = $state(false);
  let report = $state<api.ShaclReport | null>(null);
  let reportMs = $state(0);
  let shaclError = $state<string | null>(null);
  let resultsShown = $state(50);
  let shaclCtl: AbortController | null = null;

  $effect(() => {
    // per-dataset shapes draft
    shapes = load(shapesKey, DEFAULT_SHAPES);
    shaclGraph = 'default';
    report = null;
    shaclError = null;
  });

  // dataset prefixes plus those declared in the shapes graph, for the results table
  const reportPrefixes = $derived.by(() => {
    const p: Record<string, string> = { ...prefixes };
    for (const m of shapes.matchAll(/@prefix\s+([A-Za-z][\w.-]*|):\s*<([^>\s]*)>/gi))
      p[m[1]] ??= m[2];
    return p;
  });
  // Focus nodes: prefixed name if possible, else namespace + rest (e.g. "ex:person/7"),
  // so the distinguishing tail of the IRI stays visible in a narrow column.
  const nsList = $derived(
    Object.entries(reportPrefixes)
      .filter(([, ns]) => ns)
      .sort((a, b) => b[1].length - a[1].length),
  );
  function focusLabel(iri: string): string {
    const short = displayIri(iri, reportPrefixes);
    if (short !== iri) return short;
    const hit = nsList.find(([, ns]) => iri.startsWith(ns) && iri.length > ns.length);
    return hit ? `${hit[0]}:${iri.slice(hit[1].length)}` : iri;
  }
  const namedGraphs = $derived(
    (stats?.graphs ?? []).flatMap((g) => (g.name == null ? [] : [g.name])),
  );
  const shaclOpts = () => ({
    graph: shaclGraph,
    reasoning: info?.reasoning ? useInferences : undefined,
  });

  async function validate() {
    shaclCtl?.abort();
    shaclCtl = new AbortController();
    validating = true;
    shaclError = null;
    save(shapesKey, shapes);
    const t0 = performance.now();
    try {
      report = await api.shacl(name, shapes, { ...shaclOpts(), signal: shaclCtl.signal });
      reportMs = performance.now() - t0;
      resultsShown = 50;
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) {
        shaclError = api.errorMessage(e);
        report = null;
      }
    } finally {
      validating = false;
      shaclCtl = null;
    }
  }

  async function downloadReport() {
    downloadingReport = true;
    try {
      const blob = await api.shaclRaw(name, shapes, 'text/turtle', shaclOpts());
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${name.replace(/[^\w.-]+/g, '_')}-shacl-report.ttl`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) {
      toasts.error('Could not download the report', e);
    } finally {
      downloadingReport = false;
    }
  }

  const SH = WELL_KNOWN.sh;
  const termText = (t: api.Term): string => (t.type === 'triple' ? 'triple term' : t.value);
  const shName = (t: api.Term): string =>
    t.type !== 'uri'
      ? termText(t)
      : t.value.startsWith(SH)
        ? t.value.slice(SH.length)
        : localName(t.value);
  const componentName = (t: api.Term) => shName(t).replace(/ConstraintComponent$/, '');
  const severityClass = (t: api.Term) => {
    const s = shName(t);
    return s === 'Violation' ? 'danger' : s === 'Warning' ? 'warn' : 'iri';
  };
  const severityCounts = $derived.by(() => {
    const m = new Map<string, number>();
    for (const r of report?.results ?? [])
      m.set(shName(r.severity), (m.get(shName(r.severity)) ?? 0) + 1);
    return [...m]
      .map(([k, n]) => `${fmtInt(n)} ${k.toLowerCase()}${n === 1 ? '' : 's'}`)
      .join(', ');
  });

  // --- derived visuals ------------------------------------------------------
  const maxPred = $derived(Math.max(1, ...(stats?.predicates.map((p) => p.count) ?? [1])));
  const maxClass = $derived(Math.max(1, ...(stats?.classes.map((c) => c.instances) ?? [1])));
  const rate = (c: { hits: number; misses: number } | undefined) =>
    c && c.hits + c.misses > 0 ? `${((c.hits / (c.hits + c.misses)) * 100).toFixed(1)}%` : '—';
  const delta = $derived.by(() => {
    if (!stats) return null;
    const total = Math.max(stats.baseQuads + stats.deltaInserts, 1);
    return {
      base: ((stats.baseQuads - stats.deltaDeletes) / total) * 100,
      del: (stats.deltaDeletes / total) * 100,
      ins: (stats.deltaInserts / total) * 100,
      dirty: stats.deltaInserts + stats.deltaDeletes > 0,
    };
  });
  let predShown = $state(15);
  let classShown = $state(15);

  function queryPredicate(iri: string) {
    const q = `SELECT ?s ?o\nWHERE {\n  ?s <${iri}> ?o\n}\nLIMIT 100`;
    openInQuery(q);
  }
  function queryClass(iri: string) {
    openInQuery(`SELECT ?s\nWHERE {\n  ?s a <${iri}>\n}\nLIMIT 100`);
  }
  function openInQuery(q: string) {
    app.pendingQuery = { query: q };
    app.setDataset(name);
    goto(resolve('/query'));
  }
  const explore = (iri: string) =>
    `${resolve('/explore')}?ds=${encodeURIComponent(name)}&iri=${encodeURIComponent(iri)}`;
</script>

<svelte:head><title>{name} | Sparkles</title></svelte:head>

<div class="page">
  <nav class="crumbs">
    <a href={resolve('/datasets')}>Datasets</a><Icon name="chevron" size={12} /><span>{name}</span>
  </nav>

  <header class="head">
    <div class="title">
      <h1 class="mono">{name}</h1>
      <div class="row meta">
        {#if info}<span class="badge">{info.type === 'mem' ? 'in-memory' : 'persistent'}</span>{/if}
        {#if info?.reasoning}
          <span
            class="badge {info.reasoning.stale === false ? 'ok' : 'warn'}"
            title={info.reasoning.stale === false
              ? 'Inferences are up to date'
              : info.reasoning.stale
                ? 'Inferences are stale: re-run reasoning'
                : 'Freshness of the inferences is unknown'}
            >{info.reasoning.profile} reasoning{info.reasoning.stale === false
              ? ''
              : info.reasoning.stale
                ? ' · stale'
                : ''}</span
          >
        {/if}
        {#if info?.head != null}
          <span
            class="badge iri"
            title="Head commit{info.modified ? `, made ${fmtTime(info.modified)}` : ''}"
            >commit {info.head}</span
          >
          {#if info.modified}<span class="faint">modified {fmtRelative(info.modified)}</span>{/if}
        {/if}
        <span class="mono faint">{info?.endpoints?.query ?? `/${name}/sparql`}</span>
      </div>
      {#if info?.origin}
        {@const o = info.origin}
        <div class="faint origin">
          cloned from
          {#if app.datasets.some((d) => d.name === o.source.name)}<a
              class="mono"
              href={resolve('/datasets/[name]', { name: o.source.name })}>/{o.source.name}</a
            >{:else}<span class="mono">{o.source.name}</span>{/if}
          at commit {o.forkedFrom.seq}, {new Date(o.clonedAt).toLocaleString()}{o.inferences ===
          'drop'
            ? ', without inferences'
            : ''}
        </div>
      {/if}
    </div>
    <span class="spacer"></span>
    <button class="btn" onclick={refreshAll} disabled={loading}>
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={14} />{/if} Refresh
    </button>
    <button
      class="btn"
      onclick={() => {
        app.setDataset(name);
        goto(resolve('/query'));
      }}><Icon name="query" size={14} /> Query</button
    >
    <a class="btn" href={explore('')}><Icon name="explore" size={14} /> Explore</a>
    {#if auth.can(name, 'admin')}
      <button
        class="btn"
        onclick={() => (cloneOpen = true)}
        disabled={readOnly || !info}
        title={readOnly
          ? 'The server is read-only'
          : 'Copy this dataset into a new, independent dataset'}
        ><Icon name="copy" size={14} /> Clone</button
      >
      <button class="btn danger" onclick={() => (deleteTarget = name)}
        ><Icon name="trash" size={14} /> Delete</button
      >
    {/if}
  </header>

  {#if cloned}
    <div class="cloned row">
      <Icon name="check" size={14} />
      <span>Cloned into <span class="mono">/{cloned}</span>.</span>
      <a class="btn sm" href={resolve('/datasets/[name]', { name: cloned })}>Open</a>
      <span class="spacer"></span>
      <button class="btn ghost icon sm" aria-label="Dismiss" onclick={() => (cloned = null)}
        ><Icon name="x" size={12} /></button
      >
    </div>
  {/if}

  {#if statsError}
    <div class="error-box">
      <strong
        >{statsError instanceof api.ApiError && statsError.status === 404
          ? `No dataset named “${name}”.`
          : 'Could not load statistics.'}</strong
      >
      <span class="muted">{api.errorMessage(statsError)}</span>
    </div>
  {/if}

  {#if stats}
    <!-- headline numbers -->
    <dl class="figures panel">
      <div>
        <dt>Quads</dt>
        <dd>{fmtInt(stats.quads)}</dd>
      </div>
      <div>
        <dt>Terms</dt>
        <dd>{fmtInt(stats.terms)}</dd>
      </div>
      <div>
        <dt>Graphs</dt>
        <dd>{fmtInt(stats.graphs.length)}</dd>
      </div>
      <div>
        <dt>Predicates</dt>
        <dd>{fmtInt(stats.predicates.length)}{stats.predicates.length >= 100 ? '+' : ''}</dd>
      </div>
      <div>
        <dt>Classes</dt>
        <dd>{fmtInt(stats.classes.length)}{stats.classes.length >= 100 ? '+' : ''}</dd>
      </div>
      <div>
        <dt>On disk</dt>
        <dd>{info?.type === 'mem' ? 'in memory' : fmtBytes(stats.diskBytes)}</dd>
      </div>
    </dl>

    <div class="cols">
      <div class="col">
        <!-- storage: base vs delta -->
        <section class="panel">
          <div class="panel-head">
            <h2>Storage</h2>
            <span class="spacer"></span>
            {#if auth.can(name, 'admin')}
              <button
                class="btn sm"
                onclick={() => startTask('Compaction', () => api.compact(name))}
                disabled={acting != null || !delta?.dirty}
                title={delta?.dirty
                  ? 'Merge pending updates into a freshly sorted base index'
                  : 'Nothing to compact'}
              >
                <Icon name="layers" size={13} /> Compact
              </button>
              <button
                class="btn sm"
                onclick={() => startTask('Dump', () => api.backup(name))}
                disabled={acting != null}
                title="Write a zstd-compressed N-Quads dump (.nq.zst) into the server's backup directory"
              >
                <Icon name="download" size={13} /> Dump
              </button>
            {/if}
          </div>
          <div class="panel-body storage">
            {#if delta}
              <div
                class="stack"
                role="img"
                aria-label="Base {stats.baseQuads} quads, {stats.deltaInserts} inserted, {stats.deltaDeletes} deleted"
              >
                <span class="base" style:width="{delta.base}%"></span>
                <span class="del" style:width="{delta.del}%"></span>
                <span class="ins" style:width="{delta.ins}%"></span>
              </div>
              <div class="stack-legend">
                <span
                  ><i class="base"></i>Base index <strong>{fmtInt(stats.baseQuads)}</strong></span
                >
                <span
                  ><i class="ins"></i>Inserted since compaction
                  <strong>+{fmtInt(stats.deltaInserts)}</strong></span
                >
                <span
                  ><i class="del"></i>Deleted <strong>−{fmtInt(stats.deltaDeletes)}</strong></span
                >
              </div>
              <p class="faint note">
                {delta.dirty
                  ? 'Updates live in a delta alongside the sorted base permutations. Compacting rebuilds the base and clears the delta.'
                  : 'Everything is in the sorted base index. Nothing to compact.'}
              </p>
            {/if}
            <div class="caches">
              {#if stats.resultCache}
                {@const rc = stats.resultCache}
                <div class="cache">
                  <span class="cname">Result cache</span>
                  {#if rc.enabled}
                    <span><strong>{fmtInt(rc.entries)}</strong> entries, {fmtBytes(rc.bytes)}</span>
                    <span
                      ><span class="faint">hit rate</span> <strong>{rate(rc)}</strong>
                      <span class="faint">({fmtInt(rc.hits)} hits, {fmtInt(rc.misses)} misses)</span
                      ></span
                    >
                  {:else}
                    <span class="faint"
                      >disabled (<span class="mono">--result-cache-mb 0</span>)</span
                    >
                  {/if}
                  <span class="spacer"></span>
                  {#if auth.can(name, 'admin')}
                    <button
                      class="btn sm"
                      onclick={clearCache}
                      disabled={clearingCache || !rc.enabled || rc.entries === 0}
                      title="Drop cached query results for this dataset"
                    >
                      {#if clearingCache}<span class="spinner"></span>{:else}<Icon
                          name="trash"
                          size={12}
                        />{/if} Clear cache
                    </button>
                  {/if}
                </div>
              {/if}
              <div class="cache">
                <span class="cname">Block cache</span>
                <span
                  ><strong>{fmtInt(stats.cache.entries)}</strong> blocks, {fmtBytes(
                    stats.cache.bytes,
                  )}</span
                >
                <span
                  ><span class="faint">hit rate</span> <strong>{rate(stats.cache)}</strong>
                  <span class="faint"
                    >({fmtInt(stats.cache.hits)} hits, {fmtInt(stats.cache.misses)} misses)</span
                  ></span
                >
              </div>
            </div>
          </div>
        </section>

        <!-- commit history -->
        <HistoryPanel {name} {info} refreshKey={refreshKick} />

        <!-- SHACL validation -->
        <section class="panel">
          <div class="panel-head">
            <h2>Validate (SHACL)</h2>
            <span class="spacer"></span>
            {#if report}
              <span class="badge {report.conforms ? 'ok' : 'danger'}">
                <Icon name={report.conforms ? 'check' : 'alert'} size={12} />
                {report.conforms ? 'Conforms' : 'Does not conform'}
              </span>
            {/if}
          </div>
          <div class="panel-body shacl">
            <textarea
              class="textarea mono"
              rows="12"
              bind:value={shapes}
              spellcheck="false"
              aria-label="Shapes graph (Turtle)"></textarea>
            <div class="row shacl-opts">
              <label class="inline">
                <span class="faint">Data graph</span>
                <select class="select" bind:value={shaclGraph} aria-label="Data graph">
                  <option value="default">Default graph</option>
                  <option value="union">Union of all graphs</option>
                  {#each namedGraphs as g (g)}
                    <option value={g}>{displayIri(g, prefixes)}</option>
                  {/each}
                </select>
              </label>
              {#if info?.reasoning}
                <label
                  class="inline check"
                  title="Validate the data together with the materialized inferences"
                >
                  <input type="checkbox" bind:checked={useInferences} /> Use inferences
                </label>
              {/if}
              <span class="spacer"></span>
              <button
                class="btn"
                onclick={downloadReport}
                disabled={downloadingReport || !shapes.trim()}
                title="Validate and download the report as Turtle"
              >
                {#if downloadingReport}<span class="spinner"></span>{:else}<Icon
                    name="download"
                    size={13}
                  />{/if} Report (.ttl)
              </button>
              {#if validating}
                <button class="btn" onclick={() => shaclCtl?.abort()}>Cancel</button>
              {/if}
              <button
                class="btn primary"
                onclick={validate}
                disabled={validating || !shapes.trim()}
              >
                {#if validating}<span class="spinner"></span>{:else}<Icon
                    name="check"
                    size={14}
                  />{/if} Validate
              </button>
            </div>
            {#if shaclError}<div class="error-box">
                <strong>Validation failed.</strong>
                {shaclError}
              </div>{/if}
            {#if report}
              <p class="faint note">
                {#if report.conforms}
                  The data graph conforms to the shapes ({fmtMs(reportMs)}).
                {:else}
                  {fmtInt(report.results.length)} result{report.results.length === 1 ? '' : 's'}: {severityCounts}
                  ({fmtMs(reportMs)}).
                {/if}
              </p>
            {/if}
          </div>
          {#if report && report.results.length}
            <div class="shacl-results">
              <table class="data">
                <thead>
                  <tr
                    ><th>Focus node</th><th>Path</th><th>Value</th><th>Constraint</th><th
                      >Severity</th
                    ><th>Message</th></tr
                  >
                </thead>
                <tbody>
                  {#each report.results.slice(0, resultsShown) as r, i (i)}
                    <tr>
                      <td class="mono cell">
                        {#if r.focusNode.type === 'uri'}
                          {@const iri = r.focusNode.value}
                          <a
                            class="focus t-iri"
                            href={explore(iri)}
                            title="{iri}  (open in Explore)">{focusLabel(iri)}</a
                          >
                        {:else}
                          <TermView term={r.focusNode} prefixes={reportPrefixes} />
                        {/if}
                      </td>
                      <td class="mono cell">
                        {#if r.resultPath?.type === 'path'}<span class="t-iri"
                            >{r.resultPath.value}</span
                          >{:else}<TermView term={r.resultPath} prefixes={reportPrefixes} />{/if}
                      </td>
                      <td class="mono cell"
                        ><TermView term={r.value} prefixes={reportPrefixes} /></td
                      >
                      <td class="cell" title={termText(r.sourceConstraintComponent)}
                        >{componentName(r.sourceConstraintComponent)}</td
                      >
                      <td
                        ><span class="badge {severityClass(r.severity)}">{shName(r.severity)}</span
                        ></td
                      >
                      <td class="cell msg" title={r.messages.join('\n')}>{r.messages[0] ?? ''}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
              {#if report.results.length > resultsShown}
                <button class="btn ghost sm more" onclick={() => (resultsShown += 100)}
                  >Show more ({fmtInt(report.results.length - resultsShown)} hidden)</button
                >
              {/if}
            </div>
          {/if}
        </section>

        <!-- predicates -->
        <section class="panel">
          <div class="panel-head">
            <h2>Top predicates</h2>
            <span class="faint">by triple count</span>
          </div>
          {#if stats.predicates.length === 0}
            <p class="empty">No triples yet. Upload some data to get started.</p>
          {:else}
            <table class="data bars">
              <thead>
                <tr
                  ><th>Predicate</th><th class="num">Triples</th><th
                    class="num"
                    title="Distinct subjects">Subj.</th
                  ><th class="num" title="Distinct objects">Obj.</th></tr
                >
              </thead>
              <tbody>
                {#each stats.predicates.slice(0, predShown) as p (p.iri)}
                  <tr>
                    <td class="bar-cell">
                      <span class="bar" style:width="{(p.count / maxPred) * 100}%"></span>
                      <button
                        class="linkish t-iri mono"
                        title="{p.iri}  (click to query)"
                        onclick={() => queryPredicate(p.iri)}>{displayIri(p.iri, prefixes)}</button
                      >
                    </td>
                    <td class="num">{fmtInt(p.count)}</td>
                    <td class="num faint">{fmtCompact(p.distinctSubjects)}</td>
                    <td class="num faint">{fmtCompact(p.distinctObjects)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
            {#if stats.predicates.length > predShown}
              <button class="btn ghost sm more" onclick={() => (predShown += 25)}
                >Show more ({stats.predicates.length - predShown} hidden)</button
              >
            {/if}
          {/if}
        </section>

        <!-- classes -->
        <section class="panel">
          <div class="panel-head">
            <h2>Top classes</h2>
            <span class="faint">by rdf:type instances</span>
          </div>
          {#if stats.classes.length === 0}
            <p class="empty">No typed resources.</p>
          {:else}
            <table class="data bars">
              <thead><tr><th>Class</th><th class="num">Instances</th><th></th></tr></thead>
              <tbody>
                {#each stats.classes.slice(0, classShown) as c (c.iri)}
                  <tr>
                    <td class="bar-cell">
                      <span class="bar cls" style:width="{(c.instances / maxClass) * 100}%"></span>
                      <button
                        class="linkish t-iri mono"
                        title="{c.iri}  (click to list instances)"
                        onclick={() => queryClass(c.iri)}>{displayIri(c.iri, prefixes)}</button
                      >
                    </td>
                    <td class="num">{fmtInt(c.instances)}</td>
                    <td class="num"
                      ><a class="faint" href={explore(c.iri)} title="Open in Explore"
                        ><Icon name="explore" size={13} /></a
                      ></td
                    >
                  </tr>
                {/each}
              </tbody>
            </table>
            {#if stats.classes.length > classShown}
              <button class="btn ghost sm more" onclick={() => (classShown += 25)}
                >Show more ({stats.classes.length - classShown} hidden)</button
              >
            {/if}
          {/if}
        </section>
      </div>

      <div class="col">
        <!-- upload -->
        <section class="panel" hidden={!auth.can(name, 'write')}>
          <div class="panel-head"><h2>Upload data</h2></div>
          <div class="panel-body upload">
            <div
              class="drop"
              class:over={dragging}
              role="button"
              tabindex="0"
              aria-label="Drop RDF files here or press to choose files"
              ondragover={(e) => {
                e.preventDefault();
                dragging = true;
              }}
              ondragleave={() => (dragging = false)}
              ondrop={(e) => {
                e.preventDefault();
                dragging = false;
                addFiles(e.dataTransfer?.files);
              }}
              onclick={() => fileInput?.click()}
              onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && fileInput?.click()}
            >
              <Icon name="upload" size={20} />
              <span><strong>Drop RDF files</strong> or click to choose</span>
              <span class="faint"
                >Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD. Format comes from the file
                extension.</span
              >
            </div>
            <input
              bind:this={fileInput}
              type="file"
              multiple
              accept={ACCEPT}
              hidden
              onchange={(e) => addFiles(e.currentTarget.files)}
            />
            {#if files.length}
              <ul class="files">
                {#each files as f, i (f.name + f.size)}
                  <li>
                    <span class="mono">{f.name}</span>
                    <span class="faint">{fmtBytes(f.size)}</span>
                    <button
                      class="btn ghost icon sm"
                      aria-label="Remove {f.name}"
                      disabled={uploading}
                      onclick={() => (files = files.filter((_, j) => j !== i))}
                    >
                      <Icon name="x" size={12} />
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
            <label class="field">
              Target graph <span class="faint"
                >(optional; default graph when empty, ignored for quad formats)</span
              >
              <input
                class="input mono"
                bind:value={graph}
                placeholder="http://example.org/graph/…"
              />
            </label>
            {#if uploading}
              <div class="progress"><span style:width="{Math.round(progress * 100)}%"></span></div>
              <div class="row faint">
                <span class="spinner"></span>
                {progress < 1
                  ? `Sending ${Math.round(progress * 100)}%`
                  : 'Parsing and indexing on the server…'}
                <span class="spacer"></span>
                <button class="btn sm" onclick={() => uploadCtl?.abort()}>Cancel</button>
              </div>
            {/if}
            {#if uploadError}<div class="error-box">
                <strong>Upload failed.</strong>
                {uploadError}
              </div>{/if}
            {#if uploadReceipt && !uploading}
              <p class="receipt faint" class:none={!uploadReceipt.committed}>
                <Icon name={uploadReceipt.committed ? 'check' : 'info'} size={13} />
                Last upload: {receiptSummary(uploadReceipt)}{uploadReceipt.committed
                  ? ` (${uploadReceipt.commit.quads.toLocaleString('en-US')} quads after)`
                  : ''}
              </p>
            {/if}
            <div class="row">
              <span class="spacer"></span>
              <button class="btn primary" disabled={!files.length || uploading} onclick={doUpload}>
                <Icon name="upload" size={14} /> Upload {files.length
                  ? `${files.length} file${files.length === 1 ? '' : 's'}`
                  : ''}
              </button>
            </div>
          </div>
        </section>

        <!-- reasoning -->
        <ReasoningPanel
          {name}
          {info}
          {prefixes}
          {explore}
          readOnly={readOnly || !auth.can(name, 'admin')}
          busy={acting != null}
          onstart={startTask}
          onchanged={refreshAll}
        />

        <!-- full-text search -->
        <FullTextPanel
          {name}
          {prefixes}
          predicates={stats.predicates.map((p) => p.iri)}
          readOnly={readOnly || !auth.can(name, 'admin')}
          busy={acting != null}
          refreshKey={refreshKick}
          onstart={startTask}
          onstarted={() => taskKick++}
          onchanged={refreshAll}
        />

        <!-- backups in repositories -->
        <BackupsPanel
          {name}
          {info}
          {readOnly}
          refreshKey={refreshKick}
          onstarted={() => taskKick++}
          onchanged={refreshAll}
        />

        <!-- graphs -->
        <section class="panel">
          <div class="panel-head">
            <h2>Graphs</h2>
            <span class="faint">{stats.graphs.length}</span>
          </div>
          <table class="data">
            <thead><tr><th>Graph</th><th class="num">Quads</th></tr></thead>
            <tbody>
              {#each stats.graphs as g (g.name ?? '')}
                <tr>
                  <td class="mono gname" title={g.name ?? 'Default graph'}>
                    {#if g.name == null}<span class="muted">default graph</span>{:else}<span
                        class="t-iri">{displayIri(g.name, prefixes)}</span
                      >{/if}
                  </td>
                  <td class="num">{fmtInt(g.quads)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </section>

        <!-- tasks -->
        <section class="panel">
          <div class="panel-head"><h2>Tasks</h2></div>
          <div class="panel-body">
            <TaskList
              dataset={name}
              refreshKey={taskKick}
              ondone={(t) => {
                if (t.state === 'failed') toasts.push('error', `${t.kind} failed`, t.message);
                else if (t.state === 'cancelled')
                  toasts.push('info', `${t.kind} cancelled`, t.message);
                else if (t.kind === 'clone' && t.target) {
                  toasts.push('success', `Cloned into /${t.target}`, t.message);
                  cloned = t.target;
                } else if (t.kind === 'text-rebuild')
                  toasts.push('success', 'Full-text index built', t.message);
                else toasts.push('success', `${t.kind} finished`, t.message);
                refreshAll();
              }}
            />
          </div>
        </section>
      </div>
    </div>
  {:else if !statsError}
    <div class="empty"><span class="spinner"></span> Loading statistics…</div>
  {/if}
</div>

<DatasetDialogs bind:deleteTarget ondeleted={() => goto(resolve('/datasets'))} />
<CloneDialog
  bind:open={cloneOpen}
  source={name}
  hasInferences={!!info?.reasoning}
  onstarted={() => taskKick++}
/>

<style>
  .page {
    padding: 18px 28px 40px;
    display: grid;
    gap: 16px;
    max-width: 1320px;
    width: 100%;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .crumbs a {
    color: var(--text-2);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .title {
    display: grid;
    gap: 6px;
  }
  .title h1 {
    font-family: var(--font-mono);
    font-size: 22px;
    letter-spacing: -0.03em;
  }
  .meta {
    font-size: var(--fs-sm);
  }
  .origin {
    font-size: var(--fs-sm);
  }
  .cloned {
    padding: 8px 12px;
    border: 1px solid color-mix(in srgb, var(--ok) 35%, transparent);
    background: var(--ok-soft);
    border-radius: var(--r);
    font-size: var(--fs-sm);
  }
  a.btn {
    text-decoration: none;
  }
  .figures {
    display: grid;
    grid-template-columns: repeat(6, 1fr);
    margin: 0;
  }
  .figures > div {
    padding: 12px 16px;
    border-right: 1px solid var(--border);
  }
  .figures > div:last-child {
    border-right: 0;
  }
  dt {
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  dd {
    margin: 2px 0 0;
    font-size: 20px;
    font-weight: 600;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1.25fr) minmax(0, 1fr);
    gap: 16px;
    align-items: start;
  }
  .col {
    display: grid;
    gap: 16px;
    min-width: 0;
  }
  /* grid items default to their min-content width; keep wide tables inside the track */
  .col > section {
    min-width: 0;
  }
  .storage {
    display: grid;
    gap: 10px;
  }
  .stack {
    display: flex;
    height: 12px;
    border-radius: 6px;
    overflow: hidden;
    background: var(--surface-3);
  }
  .stack span {
    min-width: 0;
  }
  .stack .base,
  .stack-legend i.base {
    background: var(--iri);
  }
  .stack .ins,
  .stack-legend i.ins {
    background: var(--ok);
  }
  .stack .del,
  .stack-legend i.del {
    background: var(--danger);
  }
  .stack-legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 18px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .stack-legend i {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    margin-right: 6px;
  }
  .stack-legend strong {
    color: var(--text);
    font-variant-numeric: tabular-nums;
    margin-left: 4px;
  }
  .note {
    font-size: var(--fs-sm);
  }
  .caches {
    display: grid;
    gap: 6px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .cache {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 18px;
    min-height: 26px;
  }
  .cache .cname {
    color: var(--text-2);
    min-width: 88px;
  }
  .cache strong {
    font-variant-numeric: tabular-nums;
  }
  .shacl {
    display: grid;
    gap: 10px;
  }
  .shacl textarea {
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.5;
    resize: vertical;
  }
  .shacl-opts {
    flex-wrap: wrap;
    gap: 8px;
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .inline .select {
    max-width: 260px;
  }
  .inline.check input {
    accent-color: var(--iri);
    margin: 0;
  }
  .shacl-results {
    border-top: 1px solid var(--border);
    overflow-x: auto;
  }
  .shacl-results table {
    table-layout: fixed;
    min-width: 560px;
  }
  .shacl-results th:nth-child(1) {
    width: 22%;
  }
  .shacl-results th:nth-child(2),
  .shacl-results th:nth-child(3) {
    width: 14%;
  }
  .shacl-results th:nth-child(4) {
    width: 15%;
  }
  .shacl-results th:nth-child(5) {
    width: 11%;
  }
  .shacl-results .cell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .shacl-results .focus {
    color: var(--iri);
    text-decoration: none;
  }
  .shacl-results .focus:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .shacl-results .msg {
    color: var(--text-2);
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  table.bars td {
    height: 30px;
  }
  .bar-cell {
    position: relative;
    max-width: 0;
    width: 70%;
  }
  .bar {
    position: absolute;
    left: 6px;
    top: 5px;
    bottom: 5px;
    border-radius: 3px;
    background: color-mix(in srgb, var(--iri) 13%, transparent);
    pointer-events: none;
  }
  .bar.cls {
    background: color-mix(in srgb, var(--literal) 14%, transparent);
  }
  .linkish {
    all: unset;
    position: relative;
    cursor: pointer;
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--iri);
  }
  .linkish:hover {
    text-decoration: underline;
  }
  .linkish:focus-visible {
    box-shadow: var(--focus);
  }
  .more {
    margin: 4px 8px 8px;
  }
  .upload {
    display: grid;
    gap: 12px;
  }
  .drop {
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 20px 12px;
    border: 1.5px dashed var(--border-strong);
    border-radius: var(--r);
    text-align: center;
    color: var(--text-2);
    cursor: pointer;
    font-size: var(--fs-sm);
  }
  .drop:hover,
  .drop.over {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 6%, transparent);
    color: var(--text);
  }
  .files {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .files li {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 3px 4px 3px 8px;
    border-radius: 4px;
    background: var(--surface-2);
    font-size: var(--fs-sm);
  }
  .files li .mono {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .receipt {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--ok);
  }
  .receipt.none {
    color: var(--text-2);
  }
  .progress {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .progress span {
    display: block;
    height: 100%;
    background: var(--spark);
    transition: width 0.2s;
  }
  .gname {
    max-width: 0;
    width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  @media (max-width: 1100px) {
    .cols {
      grid-template-columns: 1fr;
    }
    .figures {
      grid-template-columns: repeat(3, 1fr);
    }
    .figures > div:nth-child(3) {
      border-right: 0;
    }
    .figures > div:nth-child(-n + 3) {
      border-bottom: 1px solid var(--border);
    }
  }
  @media (max-width: 760px) {
    .page {
      padding: 14px 16px 32px;
    }
    .figures {
      grid-template-columns: repeat(2, 1fr);
    }
    .figures > div {
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
    .figures > div:nth-child(odd) {
      border-right: 1px solid var(--border);
    }
    .figures > div:nth-last-child(-n + 2) {
      border-bottom: 0;
    }
  }
</style>
