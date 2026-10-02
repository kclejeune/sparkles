<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { receiptSummary } from '$lib/commits';
  import { atLabel, normalizeAt, validAt } from '$lib/history';
  import { EXAMPLES } from '$lib/examples';
  import { fmtInt, fmtMs, formatSse } from '$lib/format';
  import { geoColumns } from '$lib/geo';
  import { triplesToGraph, type Triple } from '$lib/graph';
  import { applyMissingPrefixes, queryKind, RDF_TYPE } from '$lib/rdf';
  import { formatEditor } from '$lib/fmt-edit';
  import { formatAny } from '$lib/fmt-wasm';
  import { LatestRun } from '$lib/supersede';
  import { load, save } from '$lib/storage';
  import GraphView from '$components/GraphView.svelte';
  import Icon from '$components/Icon.svelte';
  import PlanView from '$components/PlanView.svelte';
  import ResultMap from '$components/ResultMap.svelte';
  import Modal from '$components/Modal.svelte';
  import ResultTable from '$components/ResultTable.svelte';
  import SparqlEditor from '$components/SparqlEditor.svelte';
  import {
    buildDefinition,
    formDefaults,
    inputType,
    isRequired,
    PARAM_TYPES,
    placeholder,
    queryVariables,
    runValues,
    saveProblems,
    type FormValue,
    type ParamRow,
  } from '$lib/stored-queries';

  /** A query tab; `stored` names the saved query it was opened from. */
  type QTab = { id: string; title: string; query: string; stored?: string };
  type View = 'table' | 'graph' | 'map' | 'plan' | 'raw' | 'explain';
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
  const FORMAT_ON_RUN_KEY = 'sparkles.formatOnRun';
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
  let savedOpen = $state(false);
  let formatMenuOpen = $state(false);
  let formatting = $state(false);
  /** Format the query before each Run (per viewer, off by default). */
  let formatOnRun = $state(load<boolean>(FORMAT_ON_RUN_KEY, false) === true);
  $effect(() => save(FORMAT_ON_RUN_KEY, formatOnRun));
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
  /** The `at` field: a past state to read, or empty for the head. */
  const atOk = $derived(validAt(app.queryAt));
  /** Named snapshots of the dataset, offered in the `at` field. */
  let snapshotNames = $state<string[]>([]);
  $effect(() => {
    const name = ds;
    snapshotNames = [];
    if (!name) return;
    api
      .snapshots(name)
      .then((l) => {
        if (ds === name) snapshotNames = l.snapshots.map((s) => s.ref);
      })
      .catch(() => {});
  });
  /** `reasoning=` for a dataset: only sent when it has materialized inferences. */
  const reasoningFor = (name: string | null | undefined) =>
    app.datasets.find((d) => d.name === name)?.reasoning ? inferences : undefined;

  $effect(() => {
    save(STORE_KEY, { tabs, active: activeId, limit, editorH, inferences });
  });

  // --- saved (stored) queries ------------------------------------------------------

  /** The dataset's stored queries (`/$/queries/{ds}`), without their text. */
  let savedList = $state<api.StoredQuery[]>([]);
  let savedError = $state<string | null>(null);
  /** Full definitions by `ds/name`. */
  let storedDefs = $state<Record<string, api.StoredQuery>>({});
  /** The parameter form of each tab opened from a stored query. */
  let forms = $state<Record<string, Record<string, FormValue>>>({});
  const canAdmin = $derived(!!ds && auth.can(ds, 'admin'));

  async function loadSaved(name: string) {
    try {
      const list = await api.storedQueries(name);
      if (ds === name) {
        savedList = list;
        savedError = null;
      }
    } catch (e) {
      if (ds === name) {
        savedList = [];
        // servers without stored queries answer 404
        savedError = e instanceof api.ApiError && e.status === 404 ? null : api.errorMessage(e);
      }
    }
  }
  $effect(() => {
    const name = ds;
    savedList = [];
    if (name) void loadSaved(name);
  });

  /** The definition behind the active tab, when it was opened from a stored query. */
  const activeStored = $derived(
    ds && active.stored ? storedDefs[`${ds}/${active.stored}`] : undefined,
  );
  $effect(() => {
    const name = ds;
    const stored = active.stored;
    const tabId = active.id;
    if (!name || !stored || storedDefs[`${name}/${stored}`]) return;
    api.storedQuery(name, stored).then(
      (d) => {
        storedDefs[`${name}/${stored}`] = d;
        forms[tabId] ??= formDefaults(d.parameters);
      },
      () => {},
    );
  });

  // a tab whose definition is known gets its parameter form
  $effect(() => {
    const d = activeStored;
    if (d && !forms[activeId]) forms[activeId] = formDefaults(d.parameters);
  });

  async function openSaved(item: api.StoredQuery) {
    savedOpen = false;
    if (!ds) return;
    try {
      const d = await api.storedQuery(ds, item.name);
      storedDefs[`${ds}/${item.name}`] = d;
      const cur = active;
      if (!cur.query.trim() || cur.query === DEFAULT_QUERY) {
        cur.query = d.query ?? '';
        cur.title = item.name;
        cur.stored = item.name;
      } else {
        addTab(d.query ?? '', item.name);
        active.stored = item.name;
      }
      forms[activeId] = formDefaults(d.parameters);
    } catch (e) {
      toasts.error(`Could not open the saved query ${item.name}`, e);
    }
  }

  function runSaved() {
    const d = activeStored;
    if (!d) return;
    const { values, missing } = runValues(d.parameters, forms[activeId] ?? {});
    if (missing.length) {
      toasts.push('error', 'Fill in the required parameters', missing.join(', '));
      return;
    }
    void run({ name: d.name, kind: d.kind ?? 'SELECT', values });
  }

  async function deleteSaved() {
    const d = activeStored;
    if (!ds || !d) return;
    if (!confirm(`Delete the saved query ${d.name} of /${ds}? Its versions go with it.`)) return;
    try {
      await api.deleteStoredQuery(ds, d.name);
      delete storedDefs[`${ds}/${d.name}`];
      for (const t of tabs) if (t.stored === d.name) t.stored = undefined;
      toasts.push('success', `Deleted ${d.name}`);
      void loadSaved(ds);
    } catch (e) {
      toasts.error('Could not delete the saved query', e);
    }
  }

  // the save dialog
  let saveOpen = $state(false);
  let saveName = $state('');
  let saveDescription = $state('');
  let saveRows = $state<(ParamRow & { use: boolean })[]>([]);
  let saving = $state(false);
  const saveIssues = $derived(
    saveProblems(
      saveName,
      saveRows.filter((r) => r.use),
    ),
  );

  function openSave() {
    savedOpen = false;
    const d = activeStored;
    saveName = active.stored ?? '';
    saveDescription = d?.description ?? '';
    saveRows = queryVariables(active.query).map((v) => {
      const p = d?.parameters?.[v];
      return {
        name: v,
        use: !!p,
        type: p?.type ?? 'string',
        default: p?.default == null ? '' : String(p.default),
        description: p?.description ?? '',
      };
    });
    saveOpen = true;
  }

  async function saveStored() {
    if (!ds || saveIssues.length) return;
    saving = true;
    const def = buildDefinition(
      active.query,
      saveDescription,
      saveRows.filter((r) => r.use),
    );
    try {
      const r = await api.putStoredQuery(ds, saveName, def);
      storedDefs[`${ds}/${saveName}`] = r;
      active.stored = saveName;
      forms[activeId] = formDefaults(r.parameters);
      saveOpen = false;
      toasts.push(
        'success',
        r.changed ? `Saved ${saveName} (version ${r.version.version})` : `${saveName} is unchanged`,
      );
      void loadSaved(ds);
    } catch (e) {
      if (e instanceof api.ApiError && e.status === 403)
        toasts.push('error', 'Saving a query needs admin access', e.message);
      else toasts.error('Could not save the query', e);
    } finally {
      saving = false;
    }
  }

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

  /**
   * Format the editor's query, in the browser or through the server (the Format button and
   * Shift+Alt+F).
   * `quiet` (format on run) reports nothing: the run reports a syntax error itself.
   */
  async function formatQuery(quiet = false) {
    const ed = editor;
    if (!ed || formatting || !ed.snapshot().text.trim()) return;
    formatting = true;
    try {
      await formatEditor(ed, (req) => formatAny(req));
      ed.showError(undefined);
    } catch (e) {
      if (quiet) return;
      if (e instanceof api.ApiError && e.status === 400 && e.code === 'syntax') {
        ed.showError(e.line, e.column);
        toasts.push(
          'error',
          e.line != null
            ? `Can't format: syntax error at line ${e.line}`
            : "Can't format: syntax error",
          e.message,
        );
      } else if (e instanceof api.ApiError && e.status === 422) {
        toasts.push(
          'error',
          'The formatter could not format this query safely; it was left unchanged',
        );
      } else {
        toasts.error('Formatting failed', e);
      }
    } finally {
      formatting = false;
    }
  }

  // Latest Run/Explain per tab: a superseded execution must not touch the tab's outcome.
  const runs = new LatestRun();
  const claim = (tabId: string) => runs.claim(tabId);

  /** Add missing prefixes to the captured tab's text (only if the user has not edited it
   * in the meantime) and return the text to execute. */
  const withPrefixes = (tab: QTab, text: string, dsName: string) =>
    applyMissingPrefixes(tab, text, app.prefixes(dsName));

  /** Run the editor's query, or (`stored`) a stored query with parameter values. */
  async function run(stored?: { name: string; kind: string; values: Record<string, string> }) {
    const tabId = activeId;
    const dsName = ds;
    if (!dsName) {
      toasts.push('error', 'Choose a dataset first', 'Create one on the Datasets page.');
      return;
    }
    // capture the tab and its text before awaiting: the user may switch tabs meanwhile
    const tab = active;
    // format first when asked to; on any failure the query runs as written
    if (formatOnRun && !stored && tabId === activeId) await formatQuery(true);
    const original = tab.query;
    const owns = claim(tabId);
    outcomes[tabId]?.controller?.abort();
    await app.loadPrefixes(dsName);
    if (!owns()) return;
    const text = stored ? original : withPrefixes(tab, original, dsName);
    if (tabId === activeId) editor?.showError(undefined);
    const k = stored?.kind ?? queryKind(text) ?? 'SELECT';
    let at: string | undefined;
    try {
      at = normalizeAt(app.queryAt) ?? undefined;
    } catch (e) {
      toasts.push('error', 'Invalid At', (e as Error).message);
      return;
    }
    if (k === 'UPDATE' && at) {
      toasts.push('error', 'Updates always apply to the head', 'Clear the At field to run one.');
      return;
    }
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
        // the update was applied either way; only the outcome display is ownership-bound
        delete app.vocab[dsName];
        void app.refreshDatasets();
        if (!owns()) return;
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
          result?.receipt && !result.receipt.committed
            ? 'Update applied, no change'
            : 'Update applied',
          result?.receipt
            ? `${dsName}: ${receiptSummary(result.receipt)} in ${fmtMs(ms)}`
            : result
              ? `${dsName}: +${fmtInt(result.inserted)} / −${fmtInt(result.deleted)} quads in ${fmtMs(ms)}`
              : `${dsName} in ${fmtMs(ms)}`,
        );
      } else {
        const opts = { send: limit, reasoning, at, signal: controller.signal };
        const result = stored
          ? await api.runStoredQuery(dsName, stored.name, stored.values, opts)
          : await api.query(dsName, text, opts);
        const elapsed = performance.now() - started;
        if (!owns()) return;
        // Keep the user's chosen view only when re-running the same kind of query.
        const sameKind = prevKind === result.queryType;
        const keep =
          sameKind &&
          prevView &&
          prevView !== 'explain' &&
          (prevView !== 'graph' || result.queryType !== 'ASK') &&
          (prevView !== 'map' || geoColumns(result.vars ?? [], result.rows ?? []).length > 0);
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
      if (!owns()) return;
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
    const tab = active;
    const original = tab.query;
    const owns = claim(tabId);
    outcomes[tabId]?.controller?.abort();
    await app.loadPrefixes(dsName);
    if (!owns()) return;
    const text = withPrefixes(tab, original, dsName);
    if (queryKind(text) === 'UPDATE') {
      toasts.push('info', 'Explain works for queries only', 'Updates have no query plan.');
      return;
    }
    if (tabId === activeId) editor?.showError(undefined);
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
      if (!owns()) return;
      outcomes[tabId] = { ...base, status: 'done', explain: ex, view: 'explain' } as Outcome;
    } catch (e) {
      if (!owns()) return;
      outcomes[tabId] = {
        status: 'error',
        ds: dsName,
        kind: 'EXPLAIN',
        error: e as Error,
        view: 'explain',
        startedAt: started,
      };
      if (e instanceof api.ApiError && e.line && tabId === activeId)
        editor?.showError(e.line, e.column);
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

  /** The result's columns holding geometry literals (the Map view draws them). */
  const mapColumns = $derived(
    outcome?.result?.vars && outcome.result.rows
      ? geoColumns(outcome.result.vars, outcome.result.rows)
      : [],
  );

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
        void run();
      }
    };
    const onDoc = (e: MouseEvent) => {
      if (examplesOpen && !(e.target as HTMLElement).closest('.examples')) examplesOpen = false;
      if (savedOpen && !(e.target as HTMLElement).closest('.saved')) savedOpen = false;
      if (formatMenuOpen && !(e.target as HTMLElement).closest('.format-group'))
        formatMenuOpen = false;
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
    <div class="saved">
      <button
        class="btn sm"
        aria-haspopup="menu"
        aria-expanded={savedOpen}
        disabled={!ds}
        onclick={() => (savedOpen = !savedOpen)}
      >
        Saved <Icon name="chevronDown" size={13} />
      </button>
      {#if savedOpen}
        <div class="menu" role="menu">
          {#each savedList as q (q.name)}
            <button role="menuitem" class="menu-item" onclick={() => openSaved(q)}>
              <span
                >{q.name}
                <span class="faint mono small"
                  >{q.kind ?? ''}{Object.keys(q.parameters ?? {}).length
                    ? ` · ${Object.keys(q.parameters ?? {})
                        .map((n) => `?${n}`)
                        .join(' ')}`
                    : ''}</span
                ></span
              >
              <span class="faint">{q.description ?? ''}</span>
            </button>
          {:else}
            <p class="faint small menu-note">
              {savedError ?? `/${ds} has no saved queries.`}
            </p>
          {/each}
          <button
            role="menuitem"
            class="menu-item"
            onclick={openSave}
            disabled={!canAdmin || !active.query.trim() || queryKind(active.query) === 'UPDATE'}
            title={canAdmin ? undefined : `Requires admin access to /${ds}`}
          >
            <span><Icon name="plus" size={12} /> Save this query…</span>
            <span class="faint">A named query with typed parameters, for HTTP, MCP and the CLI</span
            >
          </button>
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
    <label
      class="at"
      title="Read a past state (at=): a commit number, commit:N, time:<RFC 3339> or snapshot:NAME. Empty reads the head; updates always apply to the head."
    >
      <span class="faint">At</span>
      <input
        class="input sm mono"
        class:invalid={!atOk}
        placeholder="head"
        list="at-options"
        size="12"
        aria-invalid={!atOk}
        bind:value={app.queryAt}
      />
      <datalist id="at-options">
        {#each snapshotNames as s (s)}<option value={s}></option>{/each}
      </datalist>
      {#if app.queryAt}
        <button
          class="btn ghost icon sm"
          aria-label="Read the head"
          title="Read the head"
          onclick={() => (app.queryAt = '')}><Icon name="x" size={11} /></button
        >
      {/if}
    </label>
    <label class="limit" title="Maximum rows the server sends to the browser (send=)">
      <span class="faint">Show</span>
      <select class="select sm" bind:value={limit}>
        {#each LIMITS as l (l)}<option value={l}>{fmtInt(l)} rows</option>{/each}
      </select>
    </label>
    <div class="format-group">
      <button
        class="btn sm format"
        onclick={() => formatQuery()}
        disabled={!active.query.trim() || formatting}
        title="Format (Shift+Alt+F)"
      >
        <Icon name="wand" size={14} /> Format
      </button>
      <button
        class="btn sm format-more"
        aria-label="Format options"
        aria-haspopup="true"
        aria-expanded={formatMenuOpen}
        onclick={() => (formatMenuOpen = !formatMenuOpen)}
      >
        <Icon name="chevronDown" size={13} />
      </button>
      {#if formatMenuOpen}
        <div class="menu format-menu">
          <label class="menu-item check">
            <input type="checkbox" bind:checked={formatOnRun} />
            <span>Format on run</span>
          </label>
        </div>
      {/if}
    </div>
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
      <button
        class="btn sm primary"
        onclick={() => run()}
        disabled={!ds || (kind === 'UPDATE' && !auth.can(ds, 'write'))}
        title={kind === 'UPDATE' && ds && !auth.can(ds, 'write')
          ? `Requires write access to /${ds}`
          : undefined}
      >
        <Icon name="play" size={12} />
        {kind === 'UPDATE' ? 'Run update' : 'Run'}
        <span class="kbd">{isMac ? '⌘' : 'Ctrl'}↵</span>
      </button>
    {/if}
    {#if activeStored}
      {@const form = forms[activeId] ?? {}}
      <div class="params" aria-label="Saved query parameters">
        <span class="small" title={activeStored.description ?? ''}
          ><Icon name="archive" size={12} /> <strong>{activeStored.name}</strong>
          <span class="faint">v{activeStored.version.version}</span></span
        >
        {#each Object.entries(activeStored.parameters ?? {}) as [pname, p] (pname)}
          <label class="param small" title={p.description ?? p.type}>
            <span class="mono">?{pname}{isRequired(p) ? '*' : ''}</span>
            {#if p.enum?.length}
              <select class="select sm" bind:value={form[pname]}>
                {#if !isRequired(p)}<option value="">—</option>{/if}
                {#each p.enum as v (String(v))}<option value={String(v)}>{String(v)}</option>{/each}
              </select>
            {:else if inputType(p.type) === 'checkbox'}
              <input
                type="checkbox"
                checked={form[pname] === true}
                onchange={(e) => (form[pname] = e.currentTarget.checked)}
              />
            {:else}
              <input
                class="input sm"
                type={inputType(p.type)}
                step={p.type === 'integer' ? 1 : 'any'}
                placeholder={placeholder(p.type)}
                value={String(form[pname] ?? '')}
                oninput={(e) => (form[pname] = e.currentTarget.value)}
                onkeydown={(e) => e.key === 'Enter' && runSaved()}
              />
            {/if}
          </label>
        {/each}
        <span class="spacer"></span>
        <button
          class="btn sm primary"
          onclick={runSaved}
          disabled={outcome?.status === 'running'}
          title="Run the stored version with these values (/{ds}/queries/{activeStored.name})"
        >
          <Icon name="play" size={12} /> Run saved query
        </button>
        {#if canAdmin}
          <button
            class="btn ghost icon sm"
            aria-label="Delete the saved query"
            title="Delete the saved query"
            onclick={deleteSaved}><Icon name="trash" size={13} /></button
          >
        {/if}
        <button
          class="btn ghost icon sm"
          aria-label="Detach the tab from the saved query"
          title="Detach the tab from the saved query"
          onclick={() => (active.stored = undefined)}><Icon name="x" size={12} /></button
        >
      </div>
    {/if}
  </div>

  <div class="editor-wrap">
    <SparqlEditor
      bind:this={editor}
      docId={active.id}
      value={active.query}
      onchange={setQuery}
      onrun={() => run()}
      onformat={() => void formatQuery()}
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
              {#if mapColumns.length}
                <button
                  class="tab"
                  role="tab"
                  aria-selected={outcome.view === 'map'}
                  onclick={() => setView('map')}
                >
                  <Icon name="map" size={14} /> Map
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
          {#if outcome.result?.at?.historical && outcome.view !== 'explain'}
            {@const at = outcome.result.at}
            <span
              class="badge commit past"
              title="A past state of {outcome.ds}, read with at={at.selector}{at.datetime
                ? ` (${at.datetime})`
                : ''}">{atLabel(at)}</span
            >
          {:else if outcome.result?.meta.commit != null && outcome.view !== 'explain'}
            <span
              class="badge commit"
              title="The result was read at commit {outcome.result.meta
                .commit} of {outcome.ds}{outcome.result.meta.datasetId
                ? ` (dataset id ${outcome.result.meta.datasetId})`
                : ''}">commit {outcome.result.meta.commit}</span
            >
          {/if}
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

      {#if outcome.result?.inferences && outcome.view !== 'explain'}
        {@const inf = outcome.result.inferences}
        <div class="notice warn">
          <Icon name="alert" size={14} />
          {inf.commitsSince != null
            ? `Includes inferences that are ${fmtInt(inf.commitsSince)} commit${inf.commitsSince === 1 ? '' : 's'} out of date.`
            : inf.stale
              ? 'Includes inferences that are out of date.'
              : 'Includes inferences whose freshness is unknown.'}
          <a href={resolve('/datasets/[name]', { name: outcome.ds })}>Re-run reasoning</a>
        </div>
      {/if}

      <div class="rbody">
        {#if outcome.status === 'error' && outcome.error}
          {@const err = outcome.error}
          <div class="pad">
            <div class="error-box">
              {#if err instanceof api.ApiError && err.budget}
                <strong>{api.budgetHint(err.budget)}</strong>
                <div class="muted">{err.message}</div>
              {:else}
                <strong>{err.message}</strong>
              {/if}
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
              {#if err instanceof api.ApiError && err.requestId}
                <div class="faint rid">Request <span class="mono">{err.requestId}</span></div>
              {/if}
            </div>
          </div>
        {:else if outcome.updated}
          <div class="empty">
            <Icon name="check" size={22} />
            <p>Update applied to <strong>{outcome.ds}</strong> in {fmtMs(outcome.updated.ms)}.</p>
            {#if outcome.updated.result?.receipt}
              {@const r = outcome.updated.result.receipt}
              {#if r.committed}
                <p class="receipt">
                  <span class="badge iri" title="{r.commit.ref} · {r.commit.timestamp}"
                    >{receiptSummary(r)}</span
                  >
                  <span class="faint">{fmtInt(r.commit.quads)} quads after</span>
                </p>
              {:else}
                <p class="faint">{receiptSummary(r)}.</p>
              {/if}
            {/if}
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
          {:else if outcome.view === 'map'}
            <ResultMap
              vars={r.vars ?? []}
              rows={r.rows ?? []}
              columns={mapColumns}
              {prefixes}
              onopen={openIri}
            />
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

<Modal bind:open={saveOpen} title="Save as a stored query" width={560}>
  <p class="muted small">
    The editor's query is stored on the server for /{ds}. Runs bind each parameter to one value of
    its type, so a value never changes the query. It is also an MCP tool.
  </p>
  <div class="save-form">
    <label class="field">
      <span class="small">Name</span>
      <input class="input" bind:value={saveName} placeholder="people-by-age" />
    </label>
    <label class="field">
      <span class="small">Description</span>
      <input class="input" bind:value={saveDescription} placeholder="What the query answers" />
    </label>
    {#if saveRows.length}
      <div class="small">Parameters</div>
      <div class="param-rows">
        {#each saveRows as r (r.name)}
          <label class="row small">
            <input type="checkbox" bind:checked={r.use} />
            <span class="mono">?{r.name}</span>
          </label>
          <select class="select sm" bind:value={r.type} disabled={!r.use}>
            {#each PARAM_TYPES as t (t)}<option value={t}>{t}</option>{/each}
          </select>
          <input
            class="input sm"
            bind:value={r.default}
            disabled={!r.use}
            placeholder="default (empty: required)"
          />
        {/each}
      </div>
    {/if}
    {#each saveIssues as issue (issue)}<p class="err-text small">{issue}</p>{/each}
  </div>
  {#snippet actions()}
    <button class="btn" onclick={() => (saveOpen = false)}>Cancel</button>
    <button class="btn primary" onclick={saveStored} disabled={saving || saveIssues.length > 0}>
      Save
    </button>
  {/snippet}
</Modal>

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
  .rid {
    margin-top: 6px;
    font-size: var(--fs-xs);
  }
  .rid .mono {
    user-select: all;
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
  .limit,
  .at {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
  }
  .at input {
    width: 9.5em;
  }
  .at input.invalid {
    border-color: var(--danger);
  }
  .badge.past {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .inf {
    cursor: pointer;
    white-space: nowrap;
  }
  .select.sm {
    height: 24px;
    font-size: var(--fs-sm);
  }
  .examples,
  .saved {
    position: relative;
  }
  .menu-note {
    margin: 4px 8px;
  }
  .menu-item:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .params {
    flex-basis: 100%;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    padding-top: 6px;
    border-top: 1px dashed var(--border);
    min-width: 0;
  }
  .param {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .param .input {
    width: 140px;
    max-width: 40vw;
  }
  .save-form {
    display: grid;
    gap: 10px;
  }
  .field {
    display: grid;
    gap: 4px;
  }
  .param-rows {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr);
    gap: 6px 8px;
    align-items: center;
  }
  .err-text {
    color: var(--danger);
    margin: 0;
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
  .format-group {
    position: relative;
    display: flex;
  }
  .format-group .format {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
  }
  .format-group .format-more {
    border-top-left-radius: 0;
    border-bottom-left-radius: 0;
    border-left: none;
    padding: 0 4px;
  }
  .format-menu {
    width: auto;
    white-space: nowrap;
  }
  .menu-item.check {
    display: flex;
    align-items: center;
    gap: 8px;
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
  .badge.commit {
    font-family: var(--font-mono);
    font-weight: 500;
    margin-right: 10px;
  }
  .receipt {
    display: flex;
    align-items: center;
    gap: 8px;
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
  /* a phone: the toolbar wraps and its buttons land anywhere, so the menus hang from the
     toolbar's right edge instead of their button's, which could put them off screen */
  @media (max-width: 600px) {
    /* the wrapped toolbar and the editor would leave the results a sliver of a short
       screen: they get a fixed share of it, and the page scrolls down to them */
    .page {
      height: auto;
      grid-template-rows: auto auto min(var(--editor-h), 40vh) 7px max(320px, 70vh);
    }
    .toolbar {
      position: relative;
    }
    .examples,
    .saved,
    .format-group {
      position: static;
    }
    .menu {
      right: 12px;
      max-width: calc(100% - 24px);
    }
  }
  @media (max-width: 900px) {
    .explain {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: 200px minmax(0, 1fr);
    }
    .tlegend span {
      display: none;
    }
  }
</style>
