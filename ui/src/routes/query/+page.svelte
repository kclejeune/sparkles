<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { EXAMPLES } from '$lib/examples';
  import { fmtInt, fmtMs, formatSse } from '$lib/format';
  import { triplesToGraph, type Triple } from '$lib/graph';
  import { addMissingPrefixes, queryKind, RDF_TYPE } from '$lib/rdf';
  import { load, save } from '$lib/storage';
  import GraphView from '$components/GraphView.svelte';
  import Icon from '$components/Icon.svelte';
  import PlanView from '$components/PlanView.svelte';
  import ResultTable from '$components/ResultTable.svelte';
  import SparqlEditor from '$components/SparqlEditor.svelte';

  type QTab = { id: string; title: string; query: string };
  type View = 'table' | 'graph' | 'plan' | 'raw' | 'explain';
  type Outcome = {
    status: 'running' | 'done' | 'error';
    ds: string;
    kind: string;
    result?: api.SparklesResult;
    explain?: api.ExplainResult;
    updated?: { ms: number; result: api.UpdateResult | null };
    /** Whether materialized inferences were included (null: dataset has none). */
    reasoning?: boolean | null;
    error?: api.ApiError | Error;
    view: View;
    startedAt: number;
    elapsed?: number;
    controller?: AbortController;
  };

  const DEFAULT_QUERY = EXAMPLES[0].query;
  const STORE_KEY = 'sparkles.queryTabs';
  const LIMITS = [1_000, 10_000, 100_000, 1_000_000];

  const saved = load<{
    tabs: QTab[];
    active: string;
    limit?: number;
    editorH?: number;
    inferences?: boolean;
  }>(STORE_KEY, {
    tabs: [],
    active: '',
  });
  let tabs = $state<QTab[]>(
    Array.isArray(saved.tabs) && saved.tabs.length
      ? saved.tabs
      : [{ id: newId(), title: 'Query 1', query: DEFAULT_QUERY }],
  );
  let activeId = $state(tabs.some((t) => t.id === saved.active) ? saved.active : tabs[0].id);
  let limit = $state(LIMITS.includes(saved.limit ?? 0) ? saved.limit! : 10_000);
  let editorH = $state(Math.min(Math.max(saved.editorH ?? 260, 120), 900));
  /** Include materialized inferences (the server's `reasoning=` parameter). */
  let inferences = $state(saved.inferences !== false);
  let outcomes = $state<Record<string, Outcome>>({});
  let examplesOpen = $state(false);
  let renaming = $state<string | null>(null);
  let editor: SparqlEditor | undefined = $state();

  // graph options
  let hideTypes = $state(false);
  let hideLiterals = $state(false);
  let maxNodes = $state(400);
  let gCols = $state<{ s: string; p: string; o: string }>({ s: '', p: '', o: '' });

  const active = $derived(tabs.find((t) => t.id === activeId) ?? tabs[0]);
  const outcome = $derived(outcomes[activeId]);
  const ds = $derived(app.current);
  const prefixes = $derived(app.prefixes(outcome?.ds ?? ds));
  const kind = $derived(queryKind(active.query));
  const reasoningInfo = $derived(app.datasets.find((d) => d.name === ds)?.reasoning ?? null);
  /** `reasoning=` for a dataset: only sent when it has materialized inferences. */
  const reasoningFor = (name: string | null | undefined) =>
    app.datasets.find((d) => d.name === name)?.reasoning ? inferences : undefined;

  $effect(() => {
    save(STORE_KEY, { tabs, active: activeId, limit, editorH, inferences });
  });

  $effect(() => {
    if (ds) {
      void app.loadPrefixes(ds);
      void app.loadVocab(ds);
    }
  });

  function newId() {
    return Math.random().toString(36).slice(2, 10);
  }

  function addTab(query = '', title?: string) {
    const n = tabs.length + 1;
    const t = { id: newId(), title: title ?? `Query ${n}`, query };
    tabs.push(t);
    activeId = t.id;
    queueMicrotask(() => editor?.focus());
  }

  function closeTab(id: string) {
    const i = tabs.findIndex((t) => t.id === id);
    if (i < 0) return;
    outcomes[id]?.controller?.abort();
    delete outcomes[id];
    editor?.forget(id);
    tabs.splice(i, 1);
    if (!tabs.length) tabs.push({ id: newId(), title: 'Query 1', query: DEFAULT_QUERY });
    if (activeId === id) activeId = tabs[Math.min(i, tabs.length - 1)].id;
  }

  function setQuery(q: string) {
    const t = tabs.find((x) => x.id === activeId);
    if (t) t.query = q;
  }

  function openExample(i: number) {
    examplesOpen = false;
    const ex = EXAMPLES[i];
    const cur = active;
    if (!cur.query.trim() || cur.query === DEFAULT_QUERY) {
      cur.query = ex.query;
      cur.title = ex.title;
    } else addTab(ex.query, ex.title);
  }

  function defaultView(r: api.SparklesResult): View {
    if (r.queryType === 'CONSTRUCT' || r.queryType === 'DESCRIBE') return 'graph';
    return 'table';
  }

  async function run() {
    const tabId = activeId;
    const dsName = ds;
    if (!dsName) {
      toasts.push('error', 'Choose a dataset first', 'Create one on the Datasets page.');
      return;
    }
    outcomes[tabId]?.controller?.abort();
    await app.loadPrefixes(dsName);
    let text = active.query;
    const fixed = addMissingPrefixes(text, app.prefixes(dsName));
    if (fixed !== text) {
      setQuery(fixed);
      text = fixed;
    }
    editor?.showError(undefined);
    const k = queryKind(text) ?? 'SELECT';
    const controller = new AbortController();
    const prevView = outcomes[tabId]?.view;
    const prevKind = outcomes[tabId]?.result?.queryType;
    const started = performance.now();
    const reasoning = reasoningFor(dsName);
    outcomes[tabId] = {
      status: 'running',
      ds: dsName,
      kind: k,
      view: prevView ?? 'table',
      startedAt: started,
      controller,
      reasoning,
    };
    try {
      if (k === 'UPDATE') {
        const result = await api.update(dsName, text, controller.signal);
        const ms = performance.now() - started;
        outcomes[tabId] = {
          status: 'done',
          ds: dsName,
          kind: k,
          updated: { ms, result },
          view: 'table',
          startedAt: started,
          elapsed: ms,
        };
        toasts.push(
          'success',
          'Update applied',
          result
            ? `${dsName}: +${fmtInt(result.inserted)} / −${fmtInt(result.deleted)} quads in ${fmtMs(ms)}`
            : `${dsName} in ${fmtMs(ms)}`,
        );
        delete app.vocab[dsName];
        void app.refreshDatasets();
      } else {
        const result = await api.query(dsName, text, {
          send: limit,
          reasoning,
          signal: controller.signal,
        });
        const elapsed = performance.now() - started;
        // Keep the user's chosen view only when re-running the same kind of query.
        const sameKind = prevKind === result.queryType;
        const keep =
          sameKind &&
          prevView &&
          prevView !== 'explain' &&
          (prevView !== 'graph' || result.queryType !== 'ASK');
        outcomes[tabId] = {
          status: 'done',
          ds: dsName,
          kind: result.queryType,
          result,
          view: keep ? prevView! : defaultView(result),
          startedAt: started,
          elapsed,
          reasoning,
        };
        autoPickColumns(result);
      }
    } catch (e) {
      if (e instanceof DOMException && e.name === 'AbortError') {
        delete outcomes[tabId];
        return;
      }
      outcomes[tabId] = {
        status: 'error',
        ds: dsName,
        kind: k,
        error: e as Error,
        view: 'table',
        startedAt: started,
      };
      if (e instanceof api.ApiError && e.line && tabId === activeId)
        editor?.showError(e.line, e.column);
    }
  }

  async function runExplain() {
    const tabId = activeId;
    const dsName = ds;
    if (!dsName) return;
    await app.loadPrefixes(dsName);
    const text = addMissingPrefixes(active.query, app.prefixes(dsName));
    if (text !== active.query) setQuery(text);
    if (queryKind(text) === 'UPDATE') {
      toasts.push('info', 'Explain works for queries only', 'Updates have no query plan.');
      return;
    }
    editor?.showError(undefined);
    const started = performance.now();
    // Keep a previous query result (its Table/Plan tabs stay usable), but not an update
    // confirmation or an error, which would otherwise take precedence over the plan.
    const prev = outcomes[tabId]?.result ? outcomes[tabId] : undefined;
    const base = {
      ...(prev ?? { ds: dsName, kind: 'EXPLAIN', startedAt: started }),
      updated: undefined,
      error: undefined,
    };
    outcomes[tabId] = { ...base, status: 'running', view: 'explain' } as Outcome;
    try {
      const ex = await api.explain(dsName, text, { reasoning: reasoningFor(dsName) });
      outcomes[tabId] = { ...base, status: 'done', explain: ex, view: 'explain' } as Outcome;
    } catch (e) {
      outcomes[tabId] = {
        status: 'error',
        ds: dsName,
        kind: 'EXPLAIN',
        error: e as Error,
        view: 'explain',
        startedAt: started,
      };
      if (e instanceof api.ApiError && e.line) editor?.showError(e.line, e.column);
    }
  }

  function cancel() {
    outcome?.controller?.abort();
  }

  function setView(v: View) {
    if (outcomes[activeId]) outcomes[activeId].view = v;
  }

  function openIri(iri: string) {
    const dsName = outcome?.ds ?? ds ?? '';
    goto(`${resolve('/explore')}?ds=${encodeURIComponent(dsName)}&iri=${encodeURIComponent(iri)}`);
  }

  // --- graph --------------------------------------------------------------

  function autoPickColumns(r: api.SparklesResult) {
    const vars = r.vars ?? [];
    if (r.queryType !== 'SELECT' || vars.length < 2) return;
    const find = (re: RegExp) => vars.find((v) => re.test(v)) ?? '';
    let s = find(/^(s|subj|subject|from|source|a|x)$/i);
    let p = find(/^(p|pred|predicate|property|rel|relation|edge)$/i);
    let o = find(/^(o|obj|object|to|target|b|y|value)$/i);
    if (!(s && o)) {
      if (vars.length === 3) [s, p, o] = vars;
      else if (vars.length >= 2) {
        s = vars[0];
        o = vars[1];
        p = '';
      }
    }
    gCols = { s, p, o };
  }

  const graphTriples = $derived.by((): Triple[] => {
    const r = outcome?.result;
    if (!r) return [];
    if (r.triples) return r.triples;
    if (r.queryType !== 'SELECT' || !r.vars || !r.rows) return [];
    const si = r.vars.indexOf(gCols.s);
    const pi = r.vars.indexOf(gCols.p);
    const oi = r.vars.indexOf(gCols.o);
    if (si < 0 || oi < 0) return [];
    const out: Triple[] = [];
    const label: api.Term = { type: 'uri', value: `urn:var:${gCols.o}` };
    for (const row of r.rows) {
      const s = row[si];
      const o = row[oi];
      const p = pi >= 0 ? row[pi] : label;
      if (s && o && p) out.push([s, p, o]);
    }
    return out;
  });

  const graph = $derived(
    outcome?.view === 'graph'
      ? triplesToGraph(graphTriples, prefixes, { hideTypes, hideLiterals, maxNodes })
      : { nodes: [], edges: [], truncated: false, totalNodes: 0 },
  );
  const hasTypeEdges = $derived(
    graphTriples.some(([, p]) => p.type === 'uri' && p.value === RDF_TYPE),
  );

  // --- raw / downloads -------------------------------------------------------

  const rawJson = $derived.by(() => {
    const r = outcome?.result;
    if (!r || outcome?.view !== 'raw') return '';
    const cap = 500;
    const trimmed = {
      ...r,
      rows: r.rows && r.rows.length > cap ? r.rows.slice(0, cap) : r.rows,
      triples: r.triples && r.triples.length > cap ? r.triples.slice(0, cap) : r.triples,
    };
    return JSON.stringify(trimmed, null, 2);
  });

  const SELECT_DL = [
    { label: 'CSV', accept: 'text/csv', ext: 'csv' },
    { label: 'TSV', accept: 'text/tab-separated-values', ext: 'tsv' },
    { label: 'JSON', accept: 'application/sparql-results+json', ext: 'srj' },
    { label: 'XML', accept: 'application/sparql-results+xml', ext: 'srx' },
  ];
  const GRAPH_DL = [
    { label: 'Turtle', accept: 'text/turtle', ext: 'ttl' },
    { label: 'N-Triples', accept: 'application/n-triples', ext: 'nt' },
    { label: 'JSON-LD', accept: 'application/ld+json', ext: 'jsonld' },
    { label: 'RDF/XML', accept: 'application/rdf+xml', ext: 'rdf' },
  ];
  let downloading = $state<string | null>(null);

  async function download(fmt: { label: string; accept: string; ext: string }) {
    const dsName = outcome?.ds ?? ds;
    if (!dsName) return;
    downloading = fmt.label;
    try {
      const reasoning = outcome?.reasoning ?? reasoningFor(dsName);
      const blob = await api.queryRaw(dsName, active.query, fmt.accept, {
        reasoning: reasoning ?? undefined,
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${(active.title || 'results').replace(/[^\w.-]+/g, '_')}.${fmt.ext}`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 5000);
    } catch (e) {
      toasts.error(`Download as ${fmt.label} failed`, e);
    } finally {
      downloading = null;
    }
  }

  async function copyRaw() {
    try {
      await navigator.clipboard.writeText(
        JSON.stringify(outcome?.result ?? outcome?.explain, null, 2),
      );
      toasts.push('success', 'Copied JSON', undefined, 1500);
    } catch (e) {
      toasts.error('Could not copy', e);
    }
  }

  // --- split resize -------------------------------------------------------

  function startSplit(e: PointerEvent) {
    e.preventDefault();
    const y0 = e.clientY;
    const h0 = editorH;
    const move = (ev: PointerEvent) =>
      (editorH = Math.min(Math.max(h0 + ev.clientY - y0, 100), window.innerHeight - 200));
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  // --- timing bar ---------------------------------------------------------
  const timing = $derived(outcome?.result?.meta?.timing);
  const phases = $derived(
    timing
      ? [
          { key: 'parse', ms: timing.parseMs, color: 'var(--text-3)' },
          { key: 'plan', ms: timing.planMs, color: 'var(--bnode)' },
          { key: 'exec', ms: timing.execMs, color: 'var(--spark)' },
          { key: 'serialize', ms: timing.serializeMs, color: 'var(--literal)' },
        ]
      : [],
  );
  const phaseTotal = $derived(
    Math.max(
      phases.reduce((a, p) => a + (p.ms || 0), 0),
      1e-9,
    ),
  );

  // Elapsed ticker while running
  let now = $state(performance.now());
  $effect(() => {
    if (outcome?.status !== 'running') return;
    const t = setInterval(() => (now = performance.now()), 100);
    return () => clearInterval(t);
  });

  onMount(() => {
    if (app.pendingQuery) {
      const { query, title } = app.pendingQuery;
      app.pendingQuery = null;
      addTab(query, title);
    }
    const onKey = (e: KeyboardEvent) => {
      if (
        (e.metaKey || e.ctrlKey) &&
        e.key === 'Enter' &&
        !(e.target as HTMLElement)?.closest?.('.cm-editor')
      ) {
        e.preventDefault();
        run();
      }
    };
    const onDoc = (e: MouseEvent) => {
      if (examplesOpen && !(e.target as HTMLElement).closest('.examples')) examplesOpen = false;
    };
    window.addEventListener('keydown', onKey);
    document.addEventListener('mousedown', onDoc);
    return () => {
      window.removeEventListener('keydown', onKey);
      document.removeEventListener('mousedown', onDoc);
    };
  });

  const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);
  const completion = {
    prefixes: () => app.prefixes(app.current),
    vocab: () => (app.current ? (app.vocab[app.current] ?? []) : []),
  };
</script>

<svelte:head><title>Query | Sparkles</title></svelte:head>

<div class="page" style:--editor-h="{editorH}px">
  <!-- query tabs -->
  <div class="qtabs" role="tablist" aria-label="Query tabs">
    {#each tabs as t (t.id)}
      <div class="qtab" class:active={t.id === activeId} role="presentation">
        {#if renaming === t.id}
          <input
            class="rename"
            value={t.title}
            onblur={(e) => {
              t.title = e.currentTarget.value.trim() || t.title;
              renaming = null;
            }}
            onkeydown={(e) => {
              if (e.key === 'Enter') e.currentTarget.blur();
              if (e.key === 'Escape') renaming = null;
            }}
            {@attach (el) => el.select()}
          />
        {:else}
          <button
            class="qtab-btn"
            role="tab"
            aria-selected={t.id === activeId}
            onclick={() => (activeId = t.id)}
            ondblclick={() => (renaming = t.id)}
            title="Double-click to rename"
          >
            {#if outcomes[t.id]?.status === 'running'}<span class="spinner"></span>{/if}
            {#if outcomes[t.id]?.status === 'error'}<span class="err-dot"></span>{/if}
            {t.title}
          </button>
        {/if}
        <button class="qtab-x" aria-label="Close {t.title}" onclick={() => closeTab(t.id)}
          ><Icon name="x" size={12} /></button
        >
      </div>
    {/each}
    <button
      class="btn ghost icon sm add"
      aria-label="New query tab"
      title="New query tab"
      onclick={() => addTab()}
    >
      <Icon name="plus" size={14} />
    </button>
  </div>

  <!-- toolbar -->
  <div class="toolbar">
    <span class="kind" data-kind={kind ?? ''}>{kind ?? 'SPARQL'}</span>
    <span class="target faint">
      {kind === 'UPDATE' ? 'POST' : 'POST'}
      <span class="mono">/{ds ?? '…'}/{kind === 'UPDATE' ? 'update' : 'sparql'}</span>
    </span>
    <span class="spacer"></span>
    <div class="examples">
      <button
        class="btn sm"
        aria-haspopup="menu"
        aria-expanded={examplesOpen}
        onclick={() => (examplesOpen = !examplesOpen)}
      >
        Examples <Icon name="chevronDown" size={13} />
      </button>
      {#if examplesOpen}
        <div class="menu" role="menu">
          {#each EXAMPLES as ex, i (ex.title)}
            <button role="menuitem" class="menu-item" onclick={() => openExample(i)}>
              <span>{ex.title}</span>
              <span class="faint">{ex.description}</span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
    {#if reasoningInfo && kind !== 'UPDATE'}
      <label
        class="limit inf"
        title="Include the {fmtInt(
          reasoningInfo.inferred,
        )} triples inferred by {reasoningInfo.profile} reasoning (reasoning={inferences})"
      >
        <input type="checkbox" bind:checked={inferences} />
        <span>Use inferences</span>
      </label>
    {/if}
    <label class="limit" title="Maximum rows the server sends to the browser (send=)">
      <span class="faint">Show</span>
      <select class="select sm" bind:value={limit}>
        {#each LIMITS as l (l)}<option value={l}>{fmtInt(l)} rows</option>{/each}
      </select>
    </label>
    <button
      class="btn sm"
      onclick={runExplain}
      disabled={!ds || kind === 'UPDATE'}
      title="Show algebra and plan without running"
    >
      <Icon name="plan" size={14} /> Explain
    </button>
    {#if outcome?.status === 'running' && outcome.controller}
      <button class="btn sm danger" onclick={cancel}><Icon name="x" size={14} /> Cancel</button>
    {:else}
      <button class="btn sm primary" onclick={run} disabled={!ds}>
        <Icon name="play" size={12} />
        {kind === 'UPDATE' ? 'Run update' : 'Run'}
        <span class="kbd">{isMac ? '⌘' : 'Ctrl'}↵</span>
      </button>
    {/if}
  </div>

  <div class="editor-wrap">
    <SparqlEditor
      bind:this={editor}
      docId={active.id}
      value={active.query}
      onchange={setQuery}
      onrun={run}
      {completion}
    />
  </div>

  <div
    class="splitter"
    role="separator"
    aria-orientation="horizontal"
    aria-label="Resize editor"
    onpointerdown={startSplit}
  ></div>

  <!-- results -->
  <section class="results" aria-label="Results">
    {#if !outcome}
      <div class="empty">
        <Icon name="query" size={22} />
        <p>Run a query to see results here.</p>
        <p class="faint">
          {isMac ? '⌘' : 'Ctrl'}+Enter runs the query. Type a prefix like
          <span class="mono">foaf:</span> for completions; missing PREFIX lines are added for you.
        </p>
      </div>
    {:else}
      {#if outcome.result || outcome.explain || outcome.status === 'running'}
        <div class="rbar">
          <div class="tabs" role="tablist">
            {#if outcome.result || (outcome.status === 'running' && outcome.view !== 'explain')}
              <button
                class="tab"
                role="tab"
                aria-selected={outcome.view === 'table'}
                onclick={() => setView('table')}
              >
                <Icon name="table" size={14} /> Table
                {#if outcome.result?.meta}<span class="count"
                    >{fmtInt(outcome.result.meta.totalRows)}</span
                  >{/if}
              </button>
              {#if outcome.result?.queryType !== 'ASK'}
                <button
                  class="tab"
                  role="tab"
                  aria-selected={outcome.view === 'graph'}
                  onclick={() => setView('graph')}
                >
                  <Icon name="graph" size={14} /> Graph
                </button>
              {/if}
              <button
                class="tab"
                role="tab"
                aria-selected={outcome.view === 'plan'}
                onclick={() => setView('plan')}
              >
                <Icon name="plan" size={14} /> Plan
              </button>
              <button
                class="tab"
                role="tab"
                aria-selected={outcome.view === 'raw'}
                onclick={() => setView('raw')}
              >
                <Icon name="code" size={14} /> Raw
              </button>
            {/if}
            {#if outcome.explain || outcome.view === 'explain'}
              <button
                class="tab"
                role="tab"
                aria-selected={outcome.view === 'explain'}
                onclick={() => setView('explain')}
              >
                <Icon name="zap" size={14} /> Explain
              </button>
            {/if}
          </div>
          {#if outcome.reasoning === false && outcome.view !== 'explain'}
            <span
              class="badge"
              title="Ran with reasoning=false: materialized inferences were excluded"
              >no inferences</span
            >
          {/if}
          <span class="spacer"></span>
          {#if outcome.status === 'running'}
            <span class="row faint"
              ><span class="spinner"></span> Running {fmtMs(now - outcome.startedAt)}</span
            >
          {:else if timing && outcome.view !== 'explain'}
            <div class="timing" title="Server-side timing">
              <div class="tbar">
                {#each phases as ph (ph.key)}
                  <span
                    style:width="{(ph.ms / phaseTotal) * 100}%"
                    style:background={ph.color}
                    title="{ph.key} {fmtMs(ph.ms)}"
                  ></span>
                {/each}
              </div>
              <div class="tlegend">
                {#each phases as ph (ph.key)}
                  <span><i style:background={ph.color}></i>{ph.key} {fmtMs(ph.ms)}</span>
                {/each}
                <strong>{fmtMs(timing.totalMs)}</strong>
              </div>
            </div>
          {/if}
        </div>
      {/if}

      {#if outcome.result && outcome.result.meta.sentRows < outcome.result.meta.totalRows && outcome.view !== 'explain'}
        <div class="notice">
          <Icon name="info" size={14} />
          Showing the first {fmtInt(outcome.result.meta.sentRows)} of {fmtInt(
            outcome.result.meta.totalRows,
          )} results. Raise the row limit or download the full result from Raw.
        </div>
      {/if}

      <div class="rbody">
        {#if outcome.status === 'error' && outcome.error}
          {@const err = outcome.error}
          <div class="pad">
            <div class="error-box">
              <strong>{err.message}</strong>
              {#if err instanceof api.ApiError && err.line}
                <span class="muted"
                  >at line {err.line}{#if err.column}, column {err.column}{/if}</span
                >
                <button
                  class="btn sm ghost"
                  onclick={() =>
                    editor?.showError((err as api.ApiError).line, (err as api.ApiError).column)}
                >
                  Show in editor
                </button>
              {/if}
              {#if err instanceof api.ApiError && err.detail}<pre>{err.detail}</pre>{/if}
            </div>
          </div>
        {:else if outcome.updated}
          <div class="empty">
            <Icon name="check" size={22} />
            <p>Update applied to <strong>{outcome.ds}</strong> in {fmtMs(outcome.updated.ms)}.</p>
            {#if outcome.updated.result}
              {@const u = outcome.updated.result}
              <p class="faint">
                {fmtInt(u.inserted)} quad{u.inserted === 1 ? '' : 's'} inserted, {fmtInt(u.deleted)} deleted
                ({u.operations} operation{u.operations === 1 ? '' : 's'}).
              </p>
            {/if}
          </div>
        {:else if outcome.view === 'explain'}
          {#if outcome.explain}
            <div class="explain">
              <div class="algebra">
                <div class="sub-head">Algebra <span class="faint">(SSE)</span></div>
                <pre class="mono">{formatSse(outcome.explain.algebra)}</pre>
              </div>
              <div class="explain-plan">
                <PlanView plan={outcome.explain.plan} executed={false} {prefixes} />
              </div>
            </div>
          {:else}
            <div class="empty"><span class="spinner"></span></div>
          {/if}
        {:else if outcome.result}
          {@const r = outcome.result}
          {#if outcome.view === 'table'}
            {#if r.queryType === 'ASK'}
              <div class="ask">
                <span class="ask-val" class:yes={r.boolean}>{r.boolean ? 'true' : 'false'}</span>
                <span class="faint">ASK result</span>
              </div>
            {:else if r.triples}
              {#if r.triples.length}
                <ResultTable
                  vars={['subject', 'predicate', 'object']}
                  rows={r.triples}
                  {prefixes}
                  onopen={openIri}
                />
              {:else}
                <div class="empty">The query matched no triples.</div>
              {/if}
            {:else if r.rows && r.rows.length}
              <ResultTable vars={r.vars ?? []} rows={r.rows} {prefixes} onopen={openIri} />
            {:else}
              <div class="empty">
                <p>No results.</p>
                <p class="faint">
                  The pattern matched nothing in <strong>{outcome.ds}</strong>. Check IRIs and
                  prefixes, or try the Explain view.
                </p>
              </div>
            {/if}
          {:else if outcome.view === 'graph'}
            <div class="graph-view">
              <div class="gopts">
                {#if r.queryType === 'SELECT'}
                  {#each ['s', 'p', 'o'] as const as role (role)}
                    <label class="row">
                      <span class="faint"
                        >{role === 's' ? 'Subject' : role === 'p' ? 'Predicate' : 'Object'}</span
                      >
                      <select class="select sm" bind:value={gCols[role]}>
                        <option value="">{role === 'p' ? '(column name)' : '—'}</option>
                        {#each r.vars ?? [] as v (v)}<option value={v}>?{v}</option>{/each}
                      </select>
                    </label>
                  {/each}
                {/if}
                <label class="row check" class:disabled={!hasTypeEdges}>
                  <input type="checkbox" bind:checked={hideTypes} disabled={!hasTypeEdges} /> Hide rdf:type
                </label>
                <label class="row check"
                  ><input type="checkbox" bind:checked={hideLiterals} /> Hide literals</label
                >
                <label class="row">
                  <span class="faint">Max nodes</span>
                  <select class="select sm" bind:value={maxNodes}>
                    {#each [100, 250, 400, 1000, 2500] as n (n)}<option value={n}>{n}</option
                      >{/each}
                  </select>
                </label>
                <span class="spacer"></span>
                <span class="faint">{graph.nodes.length} nodes, {graph.edges.length} edges</span>
              </div>
              {#if graph.truncated}
                <div class="notice warn">
                  <Icon name="alert" size={14} />
                  Showing {graph.nodes.length} of {fmtInt(graph.totalNodes)} nodes. Large graphs are hard
                  to read; add a LIMIT or raise Max nodes.
                </div>
              {/if}
              <div class="gcanvas">
                <GraphView
                  nodes={graph.nodes}
                  edges={graph.edges}
                  onexpand={(id) => id.startsWith('<') && openIri(id.slice(1, -1))}
                  emptyText={r.queryType === 'SELECT'
                    ? 'Pick subject and object columns to draw a graph'
                    : 'No triples to draw'}
                />
              </div>
              <div class="ghint faint">
                Double-click an IRI node to open it in Explore. Hover highlights its neighbourhood.
              </div>
            </div>
          {:else if outcome.view === 'plan'}
            {#if r.meta.plan}
              <PlanView plan={r.meta.plan} {prefixes} />
            {:else}
              <div class="empty">The server did not return a plan for this query.</div>
            {/if}
          {:else if outcome.view === 'raw'}
            <div class="raw">
              <div class="dl">
                <span class="faint">Download full result</span>
                {#each r.queryType === 'CONSTRUCT' || r.queryType === 'DESCRIBE' ? GRAPH_DL : SELECT_DL as f (f.label)}
                  <button class="btn sm" onclick={() => download(f)} disabled={downloading != null}>
                    {#if downloading === f.label}<span class="spinner"></span>{:else}<Icon
                        name="download"
                        size={13}
                      />{/if}
                    {f.label}
                  </button>
                {/each}
                <span class="spacer"></span>
                <button class="btn sm ghost" onclick={copyRaw}
                  ><Icon name="copy" size={13} /> Copy JSON</button
                >
              </div>
              {#if (r.rows?.length ?? 0) > 500 || (r.triples?.length ?? 0) > 500}
                <div class="notice">
                  Pretty-printing the first 500 rows. Downloads contain everything.
                </div>
              {/if}
              <pre class="json mono">{rawJson}</pre>
            </div>
          {/if}
        {:else}
          <div class="empty"><span class="spinner"></span></div>
        {/if}
      </div>
    {/if}
  </section>
</div>

<style>
  .page {
    flex: 1;
    display: grid;
    grid-template-rows: auto auto var(--editor-h) 7px minmax(0, 1fr);
    /* without an explicit column, wide content (long editor lines, the plan table)
       stretches the grid past the viewport and the toolbar's Run button is clipped */
    grid-template-columns: minmax(0, 1fr);
    min-height: 0;
    height: 100%;
  }
  .qtabs {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    padding: 8px 12px 0;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }
  .qtab {
    display: flex;
    align-items: center;
    height: 30px;
    border: 1px solid transparent;
    border-bottom: 0;
    border-radius: var(--r) var(--r) 0 0;
    color: var(--text-2);
    max-width: 220px;
  }
  .qtab:hover {
    background: var(--hover);
  }
  .qtab.active {
    background: var(--surface);
    border-color: var(--border);
    color: var(--text);
    margin-bottom: -1px;
    height: 31px;
  }
  .qtab-btn {
    all: unset;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 4px 0 12px;
    height: 100%;
    font-weight: 500;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .qtab-btn:focus-visible {
    box-shadow: var(--focus);
  }
  .qtab-x {
    all: unset;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    margin-right: 6px;
    border-radius: 4px;
    color: var(--text-3);
    cursor: pointer;
    opacity: 0;
  }
  .qtab:hover .qtab-x,
  .qtab.active .qtab-x,
  .qtab-x:focus-visible {
    opacity: 1;
  }
  .qtab-x:hover {
    background: var(--active);
    color: var(--text);
  }
  .rename {
    width: 140px;
    margin: 0 6px 0 8px;
    height: 22px;
    font: inherit;
    border: 1px solid var(--iri);
    border-radius: 4px;
    background: var(--surface);
    color: var(--text);
    padding: 0 4px;
  }
  .err-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--danger);
  }
  .add {
    margin: 0 0 3px 4px;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    min-width: 0;
    flex-wrap: wrap;
  }
  .kind {
    font-size: var(--fs-xs);
    font-weight: 700;
    letter-spacing: 0.02em;
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--surface-3);
    color: var(--text-2);
  }
  .kind[data-kind='UPDATE'] {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .kind[data-kind='CONSTRUCT'],
  .kind[data-kind='DESCRIBE'] {
    background: color-mix(in srgb, var(--literal) 14%, transparent);
    color: var(--literal);
  }
  .target {
    font-size: var(--fs-sm);
  }
  .limit {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
  }
  .inf {
    cursor: pointer;
    white-space: nowrap;
  }
  .select.sm {
    height: 24px;
    font-size: var(--fs-sm);
  }
  .examples {
    position: relative;
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 30;
    width: 300px;
    padding: 4px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r);
    box-shadow: var(--shadow-pop);
    display: grid;
  }
  .menu-item {
    all: unset;
    display: grid;
    gap: 1px;
    padding: 6px 8px;
    border-radius: 4px;
    cursor: pointer;
    font-weight: 500;
  }
  .menu-item span:last-child {
    font-size: var(--fs-sm);
    font-weight: 400;
  }
  .menu-item:hover,
  .menu-item:focus-visible {
    background: var(--hover);
  }
  .editor-wrap {
    min-height: 0;
    background: var(--surface);
  }
  .splitter {
    cursor: row-resize;
    background: var(--surface-2);
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    position: relative;
  }
  .splitter::after {
    content: '';
    position: absolute;
    left: 50%;
    top: 2px;
    width: 32px;
    height: 2px;
    margin-left: -16px;
    border-radius: 1px;
    background: var(--border-strong);
  }
  .splitter:hover {
    background: var(--surface-3);
  }
  .results {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--surface);
  }
  .rbar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border);
    min-height: 38px;
    flex-wrap: wrap;
  }
  .rbody {
    flex: 1;
    min-height: 0;
    position: relative;
    display: flex;
    flex-direction: column;
  }
  .rbody > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .pad {
    padding: 14px;
    flex: none !important;
  }
  .notice {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
    flex: none !important;
  }
  .notice.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 8%, var(--surface));
  }
  .timing {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
  .tbar {
    display: flex;
    width: 120px;
    height: 6px;
    border-radius: 3px;
    overflow: hidden;
    background: var(--surface-3);
  }
  .tbar span {
    min-width: 2px;
  }
  .tlegend {
    display: flex;
    gap: 10px;
    align-items: center;
    font-variant-numeric: tabular-nums;
  }
  .tlegend i {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 2px;
    margin-right: 4px;
  }
  .tlegend strong {
    color: var(--text);
    font-size: var(--fs-sm);
  }
  .ask {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
  }
  .ask-val {
    font-family: var(--font-mono);
    font-size: 40px;
    font-weight: 600;
    color: var(--danger);
  }
  .ask-val.yes {
    color: var(--ok);
  }
  .graph-view {
    display: flex;
    flex-direction: column;
  }
  .gopts {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 14px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .check {
    gap: 5px;
    cursor: pointer;
  }
  .check.disabled {
    opacity: 0.5;
  }
  .gcanvas {
    flex: 1;
    min-height: 0;
  }
  .ghint {
    padding: 4px 12px;
    font-size: var(--fs-xs);
    border-top: 1px solid var(--border);
  }
  .raw {
    display: flex;
    flex-direction: column;
  }
  .dl {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
    font-size: var(--fs-sm);
  }
  .json {
    flex: 1;
    margin: 0;
    padding: 12px 14px;
    overflow: auto;
    font-size: 12px;
    line-height: 1.5;
    color: var(--text-2);
    min-height: 0;
  }
  .explain {
    display: grid;
    grid-template-columns: minmax(260px, 34%) 1fr;
    min-height: 0;
  }
  .algebra {
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
    min-height: 0;
  }
  .algebra pre {
    flex: 1;
    margin: 0;
    padding: 12px;
    overflow: auto;
    font-size: 12px;
    color: var(--text);
  }
  .sub-head {
    padding: 8px 12px;
    font-weight: 600;
    border-bottom: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .explain-plan {
    min-height: 0;
    min-width: 0;
  }
  @media (max-width: 900px) {
    .explain {
      grid-template-columns: 1fr;
      grid-template-rows: 200px 1fr;
    }
    .tlegend span {
      display: none;
    }
  }
</style>
