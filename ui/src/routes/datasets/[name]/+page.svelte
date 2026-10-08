<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { page } from '$app/state';
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { branchOption, branchParam, MAIN, onBranch } from '$lib/branches';
  import { receiptSummary } from '$lib/commits';
  import { normalizeAt, validAt } from '$lib/history';
  import { formatEditor } from '$lib/fmt-edit';
  import { formatAny } from '$lib/fmt-wasm';
  import { formatFailure } from '$lib/fmt-view';
  import { compactionSummary, lastCompaction } from '$lib/compaction';
  import { fmtBytes, fmtCompact, fmtInt, fmtMs, fmtRelative, fmtTime } from '$lib/format';
  import { displayIri, localName, WELL_KNOWN } from '$lib/rdf';
  import {
    DEFAULT_SHACLC,
    readShaclSyntax,
    shaclSyntaxKey,
    shapesLabel,
    shapesPrefixes,
    type ShaclSyntax,
  } from '$lib/shacl';
  import { readLang, validateLangKey, type ValidateLang } from '$lib/shex';
  import { load, save } from '$lib/storage';
  import { cloneDetail, cloneMethodText, originSummary } from '$lib/clone';
  import { tableKind, tableProblem, UPLOAD_ACCEPT } from '$lib/upload';
  import BackupsPanel from '$components/BackupsPanel.svelte';
  import BranchesPanel from '$components/BranchesPanel.svelte';
  import CloneDialog from '$components/CloneDialog.svelte';
  import DatasetDialogs from '$components/DatasetDialogs.svelte';
  import DescribePanel from '$components/DescribePanel.svelte';
  import FullTextPanel from '$components/FullTextPanel.svelte';
  import HistoryPanel from '$components/HistoryPanel.svelte';
  import SnapshotsPanel from '$components/SnapshotsPanel.svelte';
  import Icon from '$components/Icon.svelte';
  import ReasoningPanel from '$components/ReasoningPanel.svelte';
  import ShexPanel from '$components/ShexPanel.svelte';
  import SpatialIndexPanel from '$components/SpatialIndexPanel.svelte';
  import TaskList from '$components/TaskList.svelte';
  import TermView from '$components/TermView.svelte';
  import TurtleEditor from '$components/TurtleEditor.svelte';
  import VectorIndexPanel from '$components/VectorIndexPanel.svelte';
  import WriteValidationPanel from '$components/WriteValidationPanel.svelte';

  const name = $derived(page.params.name ?? '');
  const info = $derived(app.datasets.find((d) => d.name === name));
  /** The branch the page shows (`?branch=`): null for `main`. */
  const branch = $derived(branchParam(page.url.searchParams.get('branch')));
  /** The dataset on that branch, as the API client takes it (`name@branch`). */
  const target = $derived(onBranch(name, branch));
  const prefixes = $derived(app.prefixes(target));

  // Every dataset can offer branches; older servers may answer 404 or 501.
  let branchList = $state<api.Branch[] | null>(null);
  let branchesError = $state<api.ApiError | Error | null>(null);
  let branchesLoading = $state(false);
  const inMemory = $derived(info?.type === 'mem');
  const hasBranches = $derived(branchList != null);
  /** Servers without branch support answer 404 or 501. */
  const branchesUnsupported = $derived(
    branchesError instanceof api.ApiError &&
      (branchesError.status === 404 || branchesError.status === 501),
  );
  /** The branch the page shows, once the list has it. */
  const shown = $derived(branchList?.find((b) => b.name === (branch ?? MAIN)));

  async function loadBranches() {
    const ds = name;
    branchesLoading = true;
    try {
      const l = await api.branches(ds);
      if (ds !== name) return;
      branchList = l.branches;
      branchesError = null;
    } catch (e) {
      if (ds !== name) return;
      branchList = null;
      branchesError = e as Error;
    } finally {
      if (ds === name) branchesLoading = false;
    }
  }

  $effect(() => {
    if (!name) return;
    untrack(() => {
      branchList = null;
      void loadBranches();
    });
  });

  /** Show another branch. The choice lives in the URL, so a link opens the same branch. */
  function selectBranch(b: string) {
    const path = resolve('/datasets/[name]', { name });
    goto(b === MAIN ? path : `${path}?branch=${encodeURIComponent(b)}`, {
      keepFocus: true,
      noScroll: true,
    });
  }

  let stats = $state<api.DatasetStats | null>(null);
  let statsError = $state<api.ApiError | Error | null>(null);
  /** The state the figures describe (`at=`): empty for the head. */
  let statsAt = $state('');
  const statsAtOk = $derived(validAt(statsAt));
  let loading = $state(false);
  let taskKick = $state(0);
  /** Bumped after anything that may have changed the dataset (panels reload). */
  let refreshKick = $state(0);
  let deleteTarget = $state<string | null>(null);
  let cloneOpen = $state(false);
  /** The last clone of this dataset that finished, for an "Open" link. */
  let cloned = $state<string | null>(null);
  /** How that clone was made (from its task's detail), when the server says. */
  let clonedHow = $state<string | null>(null);
  $effect(() => {
    void name;
    cloned = null;
    clonedHow = null;
  });

  async function loadStats() {
    loading = true;
    try {
      const at = statsAtOk ? (normalizeAt(statsAt) ?? undefined) : undefined;
      stats = await api.datasetStats(target, at);
      statsError = null;
    } catch (e) {
      statsError = e as Error;
    } finally {
      loading = false;
    }
  }

  // only a new name or branch resets the page: reloading the prefixes (refreshAll) must
  // not unmount the panels and their open dialogs
  $effect(() => {
    if (!name) return;
    const ds = target;
    untrack(() => {
      stats = null;
      statsAt = '';
      void app.loadPrefixes(ds);
      void loadStats();
    });
  });

  function refreshAll() {
    refreshKick++;
    void loadStats();
    void loadBranches();
    void app.refreshDatasets();
    void app.loadPrefixes(target, true);
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

  /** Turn the dataset's automatic compaction off or on, keeping its other settings. */
  async function toggleAutoCompaction(c: api.CompactionStatus) {
    acting = 'Automatic compaction';
    try {
      await api.setCompaction(target, { ...c.own, enabled: !c.policy.enabled });
      await loadStats();
    } catch (e) {
      toasts.error('Changing automatic compaction failed', e);
    } finally {
      acting = null;
    }
  }

  // read-only servers disable the write actions
  let readOnly = $state(false);
  $effect(() => {
    api.cachedServerInfo().then(
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

  // CSV and TSV files: the default mapping's base IRI and key column, or a CSVW mapping
  // or CONSTRUCT template file (the base IRI is kept per dataset)
  let tableBase = $state('');
  let tableKey = $state('');
  let tableMapping = $state<File | null>(null);
  let mappingInput: HTMLInputElement | undefined = $state();
  const tableBaseKey = $derived(`sparkles.upload.base.${name}`);
  $effect(() => {
    tableBase = load(tableBaseKey, '');
    tableKey = '';
    tableMapping = null;
  });
  const hasTables = $derived(files.some((f) => tableKind(f.name)));
  const tableIssue = $derived(
    tableProblem(files, { base: tableBase, key: tableKey, mapping: tableMapping }),
  );

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
      if (hasTables) save(tableBaseKey, tableBase.trim());
      const res = await api.upload(target, files, {
        graph: graph.trim() || undefined,
        tables: hasTables ? { base: tableBase, key: tableKey, mapping: tableMapping } : undefined,
        onProgress: (p) => (progress = p.total ? p.loaded / p.total : 0),
        signal: uploadCtl.signal,
      });
      const n =
        typeof res === 'object' ? (res.quadCount ?? res.tripleCount ?? res.count) : undefined;
      const receipt = typeof res === 'object' ? res.receipt : undefined;
      uploadReceipt = receipt ?? null;
      const tables = typeof res === 'object' ? (res.tables ?? []) : [];
      const rows = tables.reduce((a, t) => a + (t.rows ?? 0), 0);
      const tableNote = tables.length
        ? ` · ${fmtInt(rows)} row${rows === 1 ? '' : 's'} from ${tables.length} table${tables.length === 1 ? '' : 's'}`
        : '';
      const summary = receipt
        ? receiptSummary(receipt)
        : n != null
          ? `${fmtInt(n)} quads added`
          : undefined;
      toasts.push(
        'success',
        `Uploaded ${files.length} file${files.length === 1 ? '' : 's'}`,
        summary != null ? summary + tableNote : tableNote.slice(3) || undefined,
      );
      files = [];
      tableMapping = null;
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
      const r = await api.clearResultCache(target);
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

  // `#validate` (the schema browser's "Open in shapes editor") scrolls to the Validate panel
  // once it is on the page
  let validateSection = $state<HTMLElement>();
  $effect(() => {
    if (validateSection && page.url.hash === '#validate')
      validateSection.scrollIntoView({ block: 'start' });
  });

  // the Validate panel's language, per dataset (ShEx only where the server offers it)
  let validateLang = $state<ValidateLang>('shacl');
  let shexConforms = $state<boolean | null>(null);
  $effect(() => {
    validateLang = readLang(load<unknown>(validateLangKey(name), null));
  });
  const lang = $derived<ValidateLang>(info?.endpoints?.shex ? validateLang : 'shacl');
  function setLang(l: ValidateLang) {
    validateLang = l;
    save(validateLangKey(name), l);
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
  // the shapes editor's syntax: Turtle or SHACLC (per dataset)
  let shapesSyntax = $state<ShaclSyntax>('turtle');
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
    shapesSyntax = readShaclSyntax(load<unknown>(shaclSyntaxKey(name), null));
    shapes = load(shapesKey, shapesSyntax === 'shaclc' ? DEFAULT_SHACLC : DEFAULT_SHAPES);
    shaclGraph = 'default';
    report = null;
    shaclError = null;
  });

  // dataset prefixes plus those declared in the shapes graph, for the results table
  const reportPrefixes = $derived.by(() => {
    const p: Record<string, string> = { ...prefixes };
    for (const [k, ns] of Object.entries(shapesPrefixes(shapes, shapesSyntax))) p[k] ??= ns;
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
    syntax: shapesSyntax,
  });

  /** Switch the editor's syntax; an untouched example becomes the other syntax's example. */
  function setShapesSyntax(s: ShaclSyntax) {
    const untouched = !shapes.trim() || shapes === DEFAULT_SHAPES || shapes === DEFAULT_SHACLC;
    shapesSyntax = s;
    save(shaclSyntaxKey(name), s);
    if (untouched) shapes = s === 'shaclc' ? DEFAULT_SHACLC : DEFAULT_SHAPES;
  }

  async function validate() {
    shaclCtl?.abort();
    shaclCtl = new AbortController();
    validating = true;
    shaclError = null;
    save(shapesKey, shapes);
    const t0 = performance.now();
    try {
      report = await api.shacl(target, shapes, { ...shaclOpts(), signal: shaclCtl.signal });
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

  let shapesEditor = $state<TurtleEditor>();
  let formattingShapes = $state(false);

  /** Format the shapes graph, in the browser or on the server (Format and Shift+Alt+F). */
  async function formatShapes() {
    const ed = shapesEditor;
    // the formatter has no SHACLC
    if (!ed || formattingShapes || shapesSyntax !== 'turtle' || !ed.snapshot().text.trim()) return;
    formattingShapes = true;
    try {
      await formatEditor(ed, (req) => formatAny({ ...req, language: 'turtle' }));
      ed.showError(undefined);
    } catch (e) {
      const failure = formatFailure(e, 'these shapes');
      if (!failure) {
        toasts.error('Formatting failed', e);
        return;
      }
      if (failure.line != null) ed.showError(failure.line, failure.column);
      toasts.push('error', failure.title, failure.detail);
    } finally {
      formattingShapes = false;
    }
  }

  async function downloadReport() {
    downloadingReport = true;
    try {
      const blob = await api.shaclRaw(target, shapes, 'text/turtle', shaclOpts());
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
    openQuery();
  }
  /** The query page, on this dataset and branch. */
  function openQuery() {
    app.setDataset(name);
    app.queryBranch = branch ?? '';
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
      <div class="name row">
        <h1 class="mono">{name}</h1>
        {#if hasBranches && branchList}
          <label class="branch-pick" title="The branch this page shows">
            <Icon name="branch" size={14} />
            <select
              class="select sm mono"
              aria-label="Branch"
              value={branch ?? MAIN}
              onchange={(e) => selectBranch(e.currentTarget.value)}
            >
              {#each branchList as b (b.name)}
                <option value={b.name}>{branchOption(b)}</option>
              {/each}
              {#if !shown}<option value={branch}>{branch}</option>{/if}
            </select>
          </label>
        {/if}
      </div>
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
        {#if branch}
          {#if shown}
            <span
              class="badge iri"
              title="Head commit of {shown.name}, made {fmtTime(shown.modified)}"
              >commit {shown.head}</span
            >
            {#if shown.protected}<span class="badge" title="Takes changes through merges only"
                ><Icon name="lock" size={10} /> protected</span
              >{/if}
            <span class="faint">modified {fmtRelative(shown.modified)}</span>
          {/if}
          <span class="mono faint">/{name}@{branch}/sparql</span>
        {:else}
          {#if info?.head != null}
            <span
              class="badge iri"
              title="Head commit{info.modified ? `, made ${fmtTime(info.modified)}` : ''}"
              >commit {info.head}</span
            >
            {#if info.modified}<span class="faint">modified {fmtRelative(info.modified)}</span>{/if}
          {/if}
          <span class="mono faint">{info?.endpoints?.query ?? `/${name}/sparql`}</span>
        {/if}
      </div>
      {#if info?.origin}
        {@const o = info.origin}
        <div class="faint origin">
          cloned from
          {#if app.datasets.some((d) => d.name === o.source.name)}<a
              class="mono"
              href={resolve('/datasets/[name]', { name: o.source.name })}>/{o.source.name}</a
            >{:else}<span class="mono">{o.source.name}</span>{/if}
          at commit {o.forkedFrom.seq}, {new Date(o.clonedAt).toLocaleString()}{originSummary(o)}
        </div>
      {/if}
    </div>
    <span class="spacer"></span>
    <button class="btn" onclick={refreshAll} disabled={loading}>
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={14} />{/if} Refresh
    </button>
    <button class="btn" onclick={openQuery}><Icon name="query" size={14} /> Query</button>
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
      <span
        >Cloned into <span class="mono">/{cloned}</span>.{#if clonedHow}
          <span class="faint">The clone {clonedHow}.</span>{/if}</span
      >
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
        >{statsError instanceof api.ApiError && statsError.code === 'no-such-branch'
          ? `No branch named “${branch}”.`
          : statsError instanceof api.ApiError && statsError.status === 404
            ? `No dataset named “${name}”.`
            : 'Could not load statistics.'}</strong
      >
      <span class="muted">{api.errorMessage(statsError)}</span>
      {#if branch}
        <button class="btn sm" onclick={() => selectBranch(MAIN)}>Show main</button>
      {/if}
    </div>
  {/if}

  {#if stats}
    <!-- the state the figures describe -->
    <form
      class="stats-at row"
      onsubmit={(e) => {
        e.preventDefault();
        void loadStats();
      }}
    >
      <label
        title="Show the figures of a past state: a commit number, commit:N, time:<RFC 3339> or snapshot:NAME"
      >
        <span class="faint">State at</span>
        <input
          class="input sm mono"
          class:invalid={!statsAtOk}
          placeholder="head"
          size="12"
          bind:value={statsAt}
        />
      </label>
      <button class="btn sm" disabled={!statsAtOk || loading}>Show</button>
      {#if stats.at}
        <span class="badge past" title="These figures describe a past state ({stats.at})"
          >commit {stats.commit}</span
        >
        <button
          type="button"
          class="btn ghost sm"
          onclick={() => {
            statsAt = '';
            void loadStats();
          }}>Back to head</button
        >
      {/if}
    </form>
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
        <dd>
          {info?.type === 'mem'
            ? 'in memory'
            : fmtBytes(stats.diskBytes)}{#if stats.quota?.maxBytes}
            of {fmtBytes(stats.quota.maxBytes)}{/if}
        </dd>
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
                onclick={() => startTask('Compaction', () => api.compact(target))}
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
                disabled={acting != null || branch != null}
                title={branch
                  ? 'Dumps cover main. Open main to write one.'
                  : "Write a zstd-compressed N-Quads dump (.nq.zst) into the server's backup directory"}
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
              {#if stats.compaction && !stats.at}
                {@const c = stats.compaction}
                {@const sum = compactionSummary(c)}
                {@const last = lastCompaction(c)}
                <div class="cache" data-testid="auto-compaction">
                  <span class="cname">Auto-compaction</span>
                  <span><strong>{sum.label}</strong></span>
                  <span class="faint">{sum.detail}</span>
                  {#if last}<span class="faint">{last}</span>{/if}
                  <span class="spacer"></span>
                  {#if auth.can(name, 'admin') && !readOnly && c.serverEnabled}
                    <button
                      class="btn sm"
                      onclick={() => toggleAutoCompaction(c)}
                      disabled={acting != null}
                      title={c.policy.enabled
                        ? 'Stop compacting this dataset automatically'
                        : 'Compact this dataset automatically when its delta grows'}
                    >
                      {c.policy.enabled ? 'Turn off' : 'Turn on'}
                    </button>
                  {/if}
                </div>
              {/if}
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

        <!-- branches and merges -->
        {#if !branchesUnsupported}
          <BranchesPanel
            {name}
            list={branchList}
            error={branchesError}
            loading={branchesLoading}
            current={branch ?? MAIN}
            canEdit={auth.can(name, 'admin') && !readOnly}
            canMerge={auth.can(name, 'write') && !readOnly}
            onreload={loadBranches}
            onchange={refreshAll}
            onselect={selectBranch}
          />
        {/if}

        <!-- commit history -->
        <HistoryPanel {name} {branch} {info} refreshKey={refreshKick} />

        <!-- named snapshots and retention -->
        <SnapshotsPanel
          {name}
          {branch}
          canEdit={auth.can(name, 'admin') && !readOnly}
          refreshKey={refreshKick}
          onchange={() => refreshKick++}
        />

        <!-- SHACL validation -->
        <section class="panel" id="validate" bind:this={validateSection}>
          <div class="panel-head">
            <h2>Validate</h2>
            {#if info?.endpoints?.shex}
              <div class="tabs" role="tablist" aria-label="Validation language">
                {#each [['shacl', 'SHACL'], ['shex', 'ShEx']] as const as [l, title] (l)}
                  <button
                    class="tab"
                    role="tab"
                    aria-selected={lang === l}
                    onclick={() => setLang(l)}>{title}</button
                  >
                {/each}
              </div>
            {/if}
            <span class="spacer"></span>
            {#if lang === 'shex'}
              {#if shexConforms != null}
                <span class="badge {shexConforms ? 'ok' : 'danger'}">
                  <Icon name={shexConforms ? 'check' : 'alert'} size={12} />
                  {shexConforms ? 'Conforms' : 'Does not conform'}
                </span>
              {/if}
            {:else if report}
              <span class="badge {report.conforms ? 'ok' : 'danger'}">
                <Icon name={report.conforms ? 'check' : 'alert'} size={12} />
                {report.conforms ? 'Conforms' : 'Does not conform'}
              </span>
            {/if}
          </div>
          {#if lang === 'shex'}
            <ShexPanel
              {name}
              {branch}
              {info}
              {prefixes}
              {namedGraphs}
              {explore}
              bind:conforms={shexConforms}
            />
          {/if}
          <div class="panel-body shacl" hidden={lang === 'shex'}>
            <TurtleEditor
              bind:this={shapesEditor}
              value={shapes}
              onchange={(v) => (shapes = v)}
              onformat={() => void formatShapes()}
              label={shapesLabel(shapesSyntax)}
            />
            <div class="row shacl-opts">
              <label class="inline">
                <span class="faint">Syntax</span>
                <select
                  class="select"
                  value={shapesSyntax}
                  onchange={(e) => setShapesSyntax(readShaclSyntax(e.currentTarget.value))}
                  aria-label="Shapes syntax"
                >
                  <option value="turtle">Turtle</option>
                  <option value="shaclc">SHACLC</option>
                </select>
              </label>
              <button
                class="btn"
                onclick={() => formatShapes()}
                disabled={formattingShapes || !shapes.trim() || shapesSyntax !== 'turtle'}
                title={shapesSyntax === 'turtle'
                  ? 'Format (Shift+Alt+F)'
                  : 'The formatter handles Turtle shapes, not SHACLC'}
              >
                {#if formattingShapes}<span class="spinner"></span>{:else}<Icon
                    name="wand"
                    size={13}
                  />{/if} Format
              </button>
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
          {#if lang === 'shacl' && report && report.results.length}
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

        <!-- write-time validation -->
        <WriteValidationPanel
          {name}
          {branch}
          {prefixes}
          refreshKey={refreshKick}
          canEdit={auth.can(name, 'admin') && !readOnly}
        />

        <!-- how DESCRIBE describes a resource -->
        <DescribePanel {name} {branch} canEdit={auth.can(name, 'admin') && !readOnly} />

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
                >Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD, TriX, Jena's RDF Thrift, RDF
                Protobuf and RDF/JSON, and CSV or TSV tables. Format comes from the file extension.</span
              >
            </div>
            <input
              bind:this={fileInput}
              type="file"
              multiple
              accept={UPLOAD_ACCEPT}
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
            {#if hasTables}
              <fieldset class="tables">
                <legend>CSV and TSV tables</legend>
                <label class="field">
                  Base IRI <span class="faint"
                    >(the namespace of the rows and columns; optional with a mapping or template)</span
                  >
                  <input
                    class="input mono"
                    bind:value={tableBase}
                    placeholder="http://example.org/people/"
                  />
                </label>
                <label class="field">
                  Key column <span class="faint"
                    >(optional; names each row, else rows are numbered)</span
                  >
                  <input
                    class="input mono"
                    bind:value={tableKey}
                    placeholder="id"
                    disabled={tableMapping != null}
                  />
                </label>
                <div class="field">
                  <span
                    >Mapping or template <span class="faint"
                      >(optional; CSVW metadata in JSON, or a CONSTRUCT query in a .rq file)</span
                    ></span
                  >
                  <div class="row">
                    {#if tableMapping}
                      <span class="mono">{tableMapping.name}</span>
                      <button
                        class="btn ghost icon sm"
                        aria-label="Remove {tableMapping.name}"
                        disabled={uploading}
                        onclick={() => (tableMapping = null)}
                      >
                        <Icon name="x" size={12} />
                      </button>
                    {:else}
                      <button class="btn sm" onclick={() => mappingInput?.click()}>
                        Choose a file
                      </button>
                    {/if}
                  </div>
                  <input
                    bind:this={mappingInput}
                    type="file"
                    accept=".json,.jsonld,.rq,.sparql"
                    aria-label="Mapping or template file"
                    hidden
                    onchange={(e) => {
                      tableMapping = e.currentTarget.files?.[0] ?? null;
                      e.currentTarget.value = '';
                    }}
                  />
                </div>
                {#if tableIssue}<p class="faint warn">{tableIssue}</p>{/if}
              </fieldset>
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
                Last upload: {receiptSummary(uploadReceipt)}{uploadReceipt.committed &&
                uploadReceipt.commit.quads !== undefined
                  ? ` (${uploadReceipt.commit.quads.toLocaleString('en-US')} quads after)`
                  : ''}
              </p>
            {/if}
            <div class="row">
              <span class="spacer"></span>
              <button
                class="btn primary"
                disabled={!files.length || uploading || tableIssue != null}
                onclick={doUpload}
              >
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
          {branch}
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
          {branch}
          {prefixes}
          predicates={stats.predicates.map((p) => p.iri)}
          readOnly={readOnly || !auth.can(name, 'admin')}
          busy={acting != null}
          refreshKey={refreshKick}
          onstart={startTask}
          onstarted={() => taskKick++}
          onchanged={refreshAll}
        />

        <!-- spatial index -->
        <SpatialIndexPanel
          {name}
          {branch}
          {prefixes}
          readOnly={readOnly || !auth.can(name, 'admin')}
          busy={acting != null}
          refreshKey={refreshKick}
          onstart={startTask}
          onstarted={() => taskKick++}
          onchanged={refreshAll}
        />

        <!-- vector indexes -->
        <VectorIndexPanel
          {name}
          {branch}
          {prefixes}
          predicates={stats.predicates.map((p) => p.iri)}
          canAdmin={auth.can(name, 'admin')}
          {readOnly}
          busy={acting != null}
          refreshKey={refreshKick}
          onstart={startTask}
          onstarted={() => taskKick++}
          onchanged={refreshAll}
        />

        <!-- backups in repositories -->
        <BackupsPanel
          {name}
          {branch}
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
                  const d = cloneDetail(t);
                  clonedHow = d ? cloneMethodText(d) : null;
                  toasts.push(
                    'success',
                    `Cloned into /${t.target}`,
                    [t.message, clonedHow].filter(Boolean).join('; '),
                  );
                  cloned = t.target;
                } else if (t.kind === 'text-rebuild')
                  toasts.push('success', 'Full-text index built', t.message);
                else if (t.kind === 'geo-index')
                  toasts.push('success', 'Spatial index built', t.message);
                else if (t.kind === 'vector-index')
                  toasts.push('success', 'Vector index built', t.message);
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
  {branch}
  hasInferences={!!info?.reasoning}
  graphs={stats?.graphs ?? []}
  onstarted={() => taskKick++}
/>

<style>
  .page {
    padding: 18px 28px 40px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
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
  .title {
    min-width: 0;
  }
  .title h1 {
    font-family: var(--font-mono);
    font-size: 22px;
    letter-spacing: -0.03em;
    overflow-wrap: anywhere;
  }
  .name {
    flex-wrap: wrap;
    gap: 6px 12px;
  }
  .branch-pick {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
    color: var(--text-2);
  }
  .branch-pick .select {
    min-width: 0;
    max-width: 260px;
  }
  .meta {
    flex-wrap: wrap;
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
    grid-template-columns: minmax(0, 1fr);
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
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
  }
  .shacl[hidden] {
    display: none;
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
    max-width: 100%;
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
  .tables {
    display: grid;
    gap: 10px;
    margin: 0;
    padding: 8px 10px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    min-width: 0;
  }
  .tables legend {
    padding: 0 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .tables .warn {
    margin: 0;
    color: var(--warn);
    font-size: var(--fs-sm);
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
      grid-template-columns: minmax(0, 1fr);
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
  .stats-at {
    gap: 8px;
    margin: 0 0 8px;
    font-size: var(--fs-sm);
    flex-wrap: wrap;
  }
  .stats-at label {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .stats-at input.invalid {
    border-color: var(--danger);
  }
  .badge.past {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
</style>
