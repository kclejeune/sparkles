<script lang="ts">
  import { afterNavigate, goto, replaceState } from '$app/navigation';
  import { page } from '$app/state';
  import { resolve } from '$app/paths';
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { branchOption, branchParam, MAIN, onBranch, onBranchLabel } from '$lib/branches';
  import { receiptSummary } from '$lib/commits';
  import { diffGraphHref } from '$lib/diff-graph';
  import { atLabel, normalizeAt, validAt } from '$lib/history';
  import {
    CHANGE_FILTERS,
    compareResults,
    filterDiff,
    isTooLarge,
    serverRows,
    serverSummary,
    ROW_DIFF_LIMIT,
    rowAnnouncement,
    type ChangeFilter,
    type Mark,
  } from '$lib/rowdiff';
  import { timeFailure } from '$lib/timeline';
  import { EXAMPLES } from '$lib/examples';
  import { fmtInt, fmtMs, formatSse } from '$lib/format';
  import { geoColumns } from '$lib/geo';
  import { locateEntities } from '$lib/locate';
  import { entityColumns, readAt } from '$lib/result-arcs';
  import { shortLabel, triplesToGraph, type Triple } from '$lib/graph';
  import { addMissingPrefixes, applyMissingPrefixes, queryKind, RDF_TYPE } from '$lib/rdf';
  import { formatEditor } from '$lib/fmt-edit';
  import { formatAny, lintAny } from '$lib/fmt-wasm';
  import { lintCounts, lintSummary } from '$lib/lint-view';
  import { LatestRun } from '$lib/supersede';
  import { load, save } from '$lib/storage';
  import { rejectionReport } from '$lib/write-validation';
  import GuardReportView from '$components/GuardReportView.svelte';
  import GraphView from '$components/GraphView.svelte';
  import LargeGraphView, { type LNode } from '$components/LargeGraphView.svelte';
  import { chooseRenderer, hasWebGL2, seed, type Renderer } from '$lib/overview';
  import Icon from '$components/Icon.svelte';
  import ExplainedPlan from '$components/ExplainedPlan.svelte';
  import PlanView from '$components/PlanView.svelte';
  import ResultMap from '$components/ResultMap.svelte';
  import Modal from '$components/Modal.svelte';
  import ResultTable from '$components/ResultTable.svelte';
  import TimeSlider, { type TimeChange } from '$components/TimeSlider.svelte';
  import SparqlEditor from '$components/SparqlEditor.svelte';
  import PatternPane, { type CountAnswer } from '$components/PatternPane.svelte';
  import { SchemaSource, type SchemaApi } from '$lib/pattern-schema';
  import QuestionHeader from '$components/QuestionHeader.svelte';
  import AskBar from '$components/AskBar.svelte';
  import AnswerSummary from '$components/AnswerSummary.svelte';
  import GlobalAnswer from '$components/GlobalAnswer.svelte';
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
  import * as askApi from '$lib/ask-api';
  import {
    answeredBy,
    askedFromRecords,
    askFailure,
    FIRST_STEP,
    followUpContext,
    graphColumns,
    loadAskPrefs,
    nextStep,
    routingLines,
    saveAskPrefs,
    type AskPrefs,
    type Turn,
    applyProposals,
    emptyExplanation,
    hasQuestion,
    isEmptyResult,
    isNotAQuery,
    loadAsked,
    nameFromQuestion,
    proposeParameters,
    questionTitle,
    rememberAsked,
    UPDATE_REFUSAL,
    type Asked,
    type Proposal,
    type TabCheck,
    type TabQuestion,
  } from '$lib/ask';
  import { askPayload, decodeHandoff, type Handoff } from '$lib/handoff';
  import { parseCompact } from '$lib/compact';
  import TermView from '$components/TermView.svelte';
  import GraphListView, { type ListRow } from '$components/GraphListView.svelte';
  import LensMenu from '$components/LensMenu.svelte';
  import LensLegend from '$components/LensLegend.svelte';
  import { describe as lensDescribe, entryIds, OTHER } from '$lib/lens';
  import { Lenses, resultState } from '$lib/lenses.svelte';

  /**
   * A query tab; `stored` names the saved query it was opened from, and `ask` holds the
   * question it answers when it came from a handoff link or the Asked history.
   */
  type QTab = { id: string; title: string; query: string; stored?: string; ask?: TabQuestion };
  /** The diagnosis of an empty result, fetched after the result is shown. */
  type EmptyDiagnosis =
    | { state: 'loading' }
    | { state: 'done'; d: askApi.Diagnosis }
    | { state: 'error' };
  type View = 'table' | 'graph' | 'map' | 'plan' | 'pattern' | 'raw' | 'explain';
  type Outcome = {
    status: 'running' | 'done' | 'error';
    ds: string;
    /** The branch it ran on (null: `main`). */
    branch: string | null;
    kind: string;
    result?: api.SparklesResult;
    /** In compare mode: the same query read at the left handle's commit. */
    base?: api.SparklesResult;
    /** Where `base` was read, `ds@branch` and the past state, for the maps of removed rows. */
    baseFrom?: { ds: string; at: string };
    explain?: api.ExplainResult;
    updated?: { ms: number; result: api.UpdateResult | null };
    /** Whether materialized inferences were included (null: dataset has none). */
    reasoning?: boolean | null;
    error?: api.ApiError | Error;
    view: View;
    startedAt: number;
    elapsed?: number;
    controller?: AbortController;
    diagnosis?: EmptyDiagnosis;
    /** The text that ran, for the explanation of its plan (none for a stored query). */
    query?: string;
    /** The text of an **Explain** outcome. */
    explainQuery?: string;
    /** Whether **Explain why** is open on a failed query. */
    whyOpen?: boolean;
  };

  const DEFAULT_QUERY = EXAMPLES[0].query;
  const STORE_KEY = 'sparkles.queryTabs';
  const FORMAT_ON_RUN_KEY = 'sparkles.formatOnRun';
  const LINT_KEY = 'sparkles.lint';
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
  /**
   * The last result of each tab that reached the screen. While the slider moves, a read that
   * is superseded or refused puts it back, whichever reads were in flight in between.
   */
  const lastShown: Record<string, Outcome> = {};
  let examplesOpen = $state(false);
  let savedOpen = $state(false);
  let formatMenuOpen = $state(false);
  let formatting = $state(false);
  /** Format the query before each Run (per viewer, off by default). */
  let formatOnRun = $state(load<boolean>(FORMAT_ON_RUN_KEY, false) === true);
  $effect(() => save(FORMAT_ON_RUN_KEY, formatOnRun));
  /** Lint the query while it is edited (per viewer, on by default). */
  let lintOn = $state(load<boolean>(LINT_KEY, true) !== false);
  $effect(() => save(LINT_KEY, lintOn));
  let lintDiagnostics = $state<api.LintDiagnostic[]>([]);
  /** the server refused linting (turned off, or not for this caller): stop asking */
  let lintRefused = false;
  let lintSeq = 0;
  const lintCount = $derived(lintCounts(lintDiagnostics));
  const fixable = $derived(lintDiagnostics.some((d) => d.fix));
  let renaming = $state<string | null>(null);
  let editor: SparqlEditor | undefined = $state();

  // the Pattern pane beside the editor (spec U08): open or not and its width, per viewer
  const PATTERN_KEY = 'sparkles.patternPane';
  const patternSaved = load<{ open?: boolean; w?: number }>(PATTERN_KEY, {});
  let patternOpen = $state(patternSaved.open === true);
  let patternW = $state(Math.min(Math.max(patternSaved.w ?? 480, 240), 1400));
  $effect(() => save(PATTERN_KEY, { open: patternOpen, w: patternW }));
  let pattern: PatternPane | undefined = $state();
  /** On a phone the editor and the pane are two tabs over the same query (§2.6). */
  let phoneTab = $state<'text' | 'pattern'>('text');
  let isPhone = $state(false);
  $effect(() => {
    if (typeof matchMedia !== 'function') return;
    const m = matchMedia('(max-width: 600px)');
    isPhone = m.matches;
    const on = () => (isPhone = m.matches);
    m.addEventListener('change', on);
    return () => m.removeEventListener('change', on);
  });
  const patternShown = $derived(patternOpen && (!isPhone || phoneTab === 'pattern'));

  // the plan drawn on the pattern (spec U04 §11): a switch shared by the Plan and Pattern
  // tabs, per viewer, and the operator selected in either view
  const PLAN_PATTERN_KEY = 'sparkles.planOnPattern';
  let planOnPattern = $state(load<boolean>(PLAN_PATTERN_KEY, false) === true);
  $effect(() => save(PLAN_PATTERN_KEY, planOnPattern));
  let planSel = $state<string | null>(null);
  let planHover = $state<string | null>(null);
  /** Where a result's plan ran: its commit, or the head, and its branch. */
  function resultHeader(o: Outcome): string {
    const c = o.result?.meta.commit;
    return `${typeof c === 'number' ? `commit ${c}` : 'the head'}${o.branch ? ` on ${o.branch}` : ''}`;
  }
  /** The plan of the active tab's outcome and the text it is of, when there is one: the
   * explanation when the Explain view is open or there is no result's plan, since an
   * Explain keeps the result of the run before it, and else the result's. */
  const planSource = $derived.by(() => {
    const o = outcomes[activeId];
    if (!o) return null;
    const explained = !!(o.explain?.plan && o.explainQuery);
    const ran = !!(o.result?.meta?.plan && o.query);
    if (explained && (o.view === 'explain' || !ran))
      return {
        kind: 'explain' as const,
        plan: o.explain!.plan,
        text: o.explainQuery!,
        executed: false,
        header: `the head${o.branch ? ` on ${o.branch}` : ''}`,
      };
    if (ran)
      return {
        kind: 'result' as const,
        plan: o.result!.meta.plan!,
        text: o.query!,
        executed: true,
        header: resultHeader(o),
      };
    return null;
  });
  // a new plan starts with nothing selected; the source is a new object whenever the view
  // changes, so the reset follows the plan itself
  const planShown = $derived(planSource?.plan);
  $effect(() => {
    void planShown;
    planSel = null;
    planHover = null;
  });
  /** Show an operator in the plan tree it comes from, the Explain view's or the Plan tab's,
   * with it selected (the phone's sheet). */
  function showInPlan(id: string, kind = planSource?.kind) {
    planSel = id;
    const o = outcomes[activeId];
    if (o && kind) o.view = kind === 'explain' ? 'explain' : 'plan';
  }

  /** Alt+P: open the pane and focus it, or move focus back to the editor, or close it. */
  async function togglePattern(from: 'editor' | 'pane' | 'page') {
    if (!patternOpen) {
      patternOpen = true;
      if (isPhone) phoneTab = 'pattern';
      await tick();
      pattern?.focus();
    } else if (from === 'pane') {
      if (isPhone) {
        phoneTab = 'text';
        await tick();
      } else patternOpen = false;
      editor?.focus();
    } else {
      if (isPhone) phoneTab = 'pattern';
      await tick();
      pattern?.focus();
    }
  }

  function startPatternSplit(e: PointerEvent) {
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = patternW;
    const move = (ev: PointerEvent) =>
      (patternW = Math.min(Math.max(w0 - (ev.clientX - x0), 240), window.innerWidth - 240));
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  // graph options
  let hideTypes = $state(false);
  let hideLiterals = $state(false);
  let maxNodes = $state(400);
  /** The renderer the toolbar switch chose; `null` follows the node count (spec U06 §2.1). */
  let rendererOverride = $state<Renderer | null>(null);
  let webglOk = $state(true);
  let webglReason = $state<string | null>(null);
  let largeSelected = $state<string | null>(null);
  /** Node counts the canvas renderer draws at most; larger choices offer WebGL only. */
  const CANVAS_MAX = 2500;
  let gCols = $state<{ s: string; p: string; o: string }>({ s: '', p: '', o: '' });

  const active = $derived(tabs.find((t) => t.id === activeId) ?? tabs[0]);
  const outcome = $derived(outcomes[activeId]);
  const ds = $derived(app.current);
  const prefixes = $derived(app.prefixes(outcome?.ds ?? ds));
  const kind = $derived(queryKind(active.query));
  const reasoningInfo = $derived(app.datasets.find((d) => d.name === ds)?.reasoning ?? null);
  /** The `at` field: a past state to read, or empty for the head. */
  const atOk = $derived(validAt(app.queryAt));
  // --- the time slider (docs/specs/U01-time-slider.md) ---------------------------------

  const urlParams = page.url.searchParams;
  const fromUrl = urlParams.has('at') || urlParams.has('from') || urlParams.has('against');
  if (fromUrl) {
    const urlDs = urlParams.get('ds');
    if (urlDs && urlDs !== app.current) app.setDataset(urlDs);
    const urlBranch = urlParams.get('branch');
    // a link without `branch` is on main, whatever branch the page was left on
    app.queryBranch = !urlBranch || urlBranch === MAIN ? '' : urlBranch;
    app.queryAt = urlParams.get('at') ?? '';
  }
  /** The slider bar under the toolbar, opened by History (`H`). */
  let timeOpen = $state(fromUrl);
  /** Compare mode: the left handle's commit (`from`), with the right handle's marked against it. */
  let compare = $state(urlParams.has('from') || urlParams.has('against'));
  let timeFrom = $state(urlParams.get('from') ?? '');
  /** Compare with the head of another branch instead of an earlier commit (`against`). */
  let timeAgainst = $state(urlParams.get('against') ?? '');
  let slider: TimeSlider | undefined = $state();
  let changeFilter = $state<ChangeFilter>('all');
  /** Focus is in the results, so Play waits. */
  let resultsFocused = $state(false);

  /** The `branch` field: the branch to read and write, or empty for `main`. */
  const branch = $derived(branchParam(app.queryBranch));
  /** The dataset on that branch, as the API client takes it. */
  const target = $derived(ds ? onBranch(ds, branch) : null);
  /** The schema reads of the Pattern pane's pickers, under the caller's view. */
  const patternSchemaApi: SchemaApi = {
    summary: (name, opts) => api.schemaSummary(name, opts),
    page: (name, list, cursor, opts) => api.schemaPage(name, list, cursor, opts),
    profile: async (name, cls, opts) =>
      (await api.schemaProfiles(name, { classes: [cls], ...opts })).classes.find(
        (c) => c.class === cls,
      ) ?? null,
  };

  /** The state the Pattern pane's pickers and count read: the At field when it is valid. */
  const patternAt = $derived.by(() => {
    try {
      return normalizeAt(app.queryAt) ?? undefined;
    } catch {
      return undefined;
    }
  });
  /** The schema behind the Pattern pane's pickers, at the page's branch and state (U08 §3.1). */
  let schemaMemo: { key: string; source: SchemaSource } | null = null;
  const patternSchema = $derived.by(() => {
    if (!target) return null;
    // one source per dataset, branch and state, so a field that changes back reads nothing
    const key = `${target}|${patternAt ?? ''}`;
    if (schemaMemo?.key !== key)
      schemaMemo = { key, source: new SchemaSource(patternSchemaApi, target, patternAt) };
    return schemaMemo.source;
  });

  /** Count the matches of the Pattern pane's pattern under a five-second timeout (§2.2). */
  async function patternCount(sparql: string, signal: AbortSignal): Promise<CountAnswer> {
    if (!target || !ds) throw new Error('No dataset is selected');
    try {
      const rows = await api.select(target, addMissingPrefixes(sparql, app.prefixes(ds)), {
        timeout: 5,
        at: patternAt,
        signal,
      });
      // the count is the only column, whatever alias the query needed
      const n = rows[0] ? Object.values(rows[0])[0] : undefined;
      return n && n.type === 'literal' ? Number(n.value) : 0;
    } catch (e) {
      if (e instanceof api.ApiError && e.status === 408) return 'timeout';
      throw e;
    }
  }

  /** The dataset's branches, offered in the Branch field (null: it has none). */
  let branchList = $state<api.Branch[] | null>(null);
  $effect(() => {
    const name = ds;
    branchList = null;
    if (!name) return;
    api
      .branches(name)
      .then((l) => {
        if (ds === name) branchList = l.branches;
      })
      .catch(() => {});
  });
  /** Named snapshots of the dataset, offered in the `at` field. */
  let snapshotNames = $state<string[]>([]);
  $effect(() => {
    const name = target;
    snapshotNames = [];
    if (!name) return;
    api
      .snapshots(name)
      .then((l) => {
        if (target === name) snapshotNames = l.snapshots.map((s) => s.ref);
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
  /**
   * Save as example: the question stored with the query, the parameters proposed for its
   * constant entities, the text they are replaced in, and the suggestion being promoted.
   */
  let example = $state<{
    question: string;
    base: string;
    prefixes: Record<string, string>;
    proposals: (Proposal & { use: boolean })[];
    suggestion?: string;
  } | null>(null);
  const exampleRows = $derived(
    (example?.proposals ?? [])
      .filter((p) => p.use)
      .map((p) => ({
        name: p.name,
        type: 'iri' as const,
        default: p.iri,
        description: p.label ?? p.term,
      })),
  );
  const saveIssues = $derived(
    saveProblems(saveName, [...saveRows.filter((r) => r.use), ...exampleRows]),
  );

  function openSave() {
    savedOpen = false;
    example = null;
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

  /**
   * Open the save dialog for a question and its query (§4.6): the question as the first
   * example question, the explanation as the description, and a parameter proposed for
   * each constant entity with a type, bound to that entity by default.
   */
  async function openSaveExample(
    src: { question: string; query: string; explanation?: string },
    known?: askApi.CheckResult,
    suggestion?: string,
  ) {
    savedOpen = false;
    const name = ds;
    if (!name) return;
    let checked = known?.terms ? known : undefined;
    if (!checked) {
      try {
        checked = await askApi.checkQuery(onBranch(name, branch), src.query, {
          terms: true,
          reasoning: reasoningFor(name),
        });
      } catch {
        // without terms nothing is proposed, and the dialog still opens
      }
    }
    const prefixes = { ...app.prefixes(name), ...(checked?.prefixes ?? {}) };
    example = {
      question: src.question,
      base: src.query,
      prefixes,
      proposals: proposeParameters(src.query, checked?.terms, prefixes).map((p) => ({
        ...p,
        use: true,
      })),
      suggestion,
    };
    saveName = nameFromQuestion(src.question);
    saveDescription = src.explanation ?? '';
    saveRows = queryVariables(src.query).map((v) => ({
      name: v,
      use: false,
      type: 'string',
      default: '',
      description: '',
    }));
    saveOpen = true;
  }

  async function saveStored() {
    if (!ds || saveIssues.length) return;
    saving = true;
    const ex = example;
    const rows = [...saveRows.filter((r) => r.use), ...exampleRows];
    const text = ex
      ? applyProposals(
          ex.base,
          ex.proposals.filter((p) => p.use),
          ex.prefixes,
        )
      : active.query;
    const def = buildDefinition(text, saveDescription, rows, ex ? [ex.question] : []);
    try {
      const r = await api.putStoredQuery(ds, saveName, def);
      storedDefs[`${ds}/${saveName}`] = r;
      if (!ex) {
        active.stored = saveName;
        forms[activeId] = formDefaults(r.parameters);
      }
      saveOpen = false;
      toasts.push(
        'success',
        r.changed ? `Saved ${saveName} (version ${r.version.version})` : `${saveName} is unchanged`,
      );
      if (ex?.suggestion) {
        try {
          await askApi.deleteSuggestion(ds, ex.suggestion);
        } catch (e) {
          toasts.error('The query is saved, but the suggestion could not be removed', e);
        }
        void loadSuggestions(ds);
      }
      example = null;
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

  // --- questions (C18) ------------------------------------------------------------

  /** Suggested examples, listed for dataset admins only. */
  let suggestionList = $state<askApi.Suggestion[]>([]);
  let suggestionsError = $state<string | null>(null);

  async function loadSuggestions(name: string) {
    try {
      const list = await askApi.suggestions(name);
      if (ds === name) {
        suggestionList = list;
        suggestionsError = null;
      }
    } catch (e) {
      if (ds === name) {
        suggestionList = [];
        suggestionsError =
          e instanceof api.ApiError && e.status === 404 ? null : api.errorMessage(e);
      }
    }
  }
  $effect(() => {
    const name = ds;
    suggestionList = [];
    if (name && savedOpen && canAdmin) void loadSuggestions(name);
  });

  async function dismissSuggestion(s: askApi.Suggestion) {
    if (!ds) return;
    try {
      await askApi.deleteSuggestion(ds, s.id);
      suggestionList = suggestionList.filter((x) => x.id !== s.id);
      toasts.push('success', 'Dismissed the suggestion');
    } catch (e) {
      toasts.error('Could not dismiss the suggestion', e);
    }
  }

  async function suggestExample() {
    const q = active.ask;
    if (!ds || !q?.question) return;
    try {
      await askApi.suggestExample(ds, {
        question: q.question,
        query: active.query,
        ...(q.explanation ? { explanation: q.explanation } : {}),
      });
      toasts.push(
        'success',
        'Suggested as an example',
        `An admin of /${ds} can store it as a saved query.`,
      );
    } catch (e) {
      toasts.error('Could not suggest the example', e);
    }
  }

  /** The signed-in principal, which keys the local Asked history. */
  const principal = $derived(auth.enabled ? (auth.who?.principal.name ?? null) : null);
  /** The questions asked in this browser for the dataset, newest first. */
  let askedList = $state<Asked[]>([]);
  $effect(() => {
    if (savedOpen && ds) askedList = loadAsked(ds, principal);
  });

  function remember(name: string, q: TabQuestion, query: string) {
    if (!q.question) return;
    askedList = rememberAsked(name, principal, {
      question: q.question,
      query,
      ...(q.explanation ? { explanation: q.explanation } : {}),
      ...(q.assumptions?.length ? { assumptions: q.assumptions } : {}),
      at: new Date().toISOString(),
    });
  }

  /** Open a question with its query in a new tab. */
  function openQuestion(q: TabQuestion, query: string) {
    addTab(query, q.question ? questionTitle(q.question) : undefined);
    active.ask = q;
  }

  function openAsked(a: Asked) {
    savedOpen = false;
    openQuestion(
      {
        question: a.question,
        explanation: a.explanation,
        assumptions: a.assumptions,
        original: a.query,
      },
      a.query,
    );
  }

  // --- asking the server (C18 Phase 2) ----------------------------------------------

  /** What the server says about asking this dataset; null without an assistant. */
  let assistant = $state<askApi.AssistantStatus | null>(null);
  $effect(() => {
    const name = ds;
    assistant = null;
    if (!name) return;
    const ctl = new AbortController();
    askApi
      .assistantSettings(name, ctl.signal)
      .then((s) => {
        if (ds === name) assistant = s?.status ?? null;
      })
      .catch(() => {});
    return () => ctl.abort();
  });
  const canAsk = $derived(!!assistant?.ask);
  const draftModel = $derived(
    assistant?.draft?.length
      ? `${assistant.draft[0].provider} · ${assistant.draft[0].model}`
      : null,
  );

  let askPrefs = $state<AskPrefs>(loadAskPrefs(null));
  let prefsFor: string | null | undefined = undefined;
  $effect(() => {
    const who = principal;
    if (who === prefsFor) return;
    prefsFor = who;
    askPrefs = loadAskPrefs(who);
  });
  $effect(() => {
    const p = { mode: askPrefs.mode, summaryCollapsed: askPrefs.summaryCollapsed };
    untrack(() => saveAskPrefs(prefsFor, p));
  });

  let askText = $state('');
  /** New question was pressed: the next ask carries no earlier turns. */
  let fresh = $state(false);
  /** The ask in progress: its tab, the step the server is on, and Stop. */
  let asking = $state<{ tabId: string; step: string; controller: AbortController } | null>(null);
  const askContext = $derived(fresh ? [] : followUpContext(active.ask, active.query));
  let askBar: AskBar | undefined = $state();
  /** The row under the pointer and the row a citation chose (0-based). */
  let hoverRow = $state<number | null>(null);
  let highlight = $state<{ row: number } | null>(null);

  /**
   * Ask the server. Without `tabId` the answer opens in a new tab. With it the tab is
   * asked again, for a clarification, Try harder or Summarize again.
   */
  async function ask(o: {
    question: string;
    tabId?: string;
    context?: Turn[];
    clarification?: { id: string; value: string };
    tryHarder?: string;
    query?: string;
    run?: boolean;
    /** answer from the data or from community reports (C22 §2.4) */
    mode?: askApi.AskMode;
  }) {
    const name = ds;
    if (!name) return;
    let at: string | undefined;
    try {
      at = normalizeAt(app.queryAt) ?? undefined;
    } catch (e) {
      toasts.push('error', 'Invalid At', (e as Error).message);
      return;
    }
    asking?.controller.abort();
    let tabId = o.tabId;
    const context = o.context ?? [];
    if (!tabId) {
      addTab('', questionTitle(o.question));
      tabId = activeId;
    }
    const tab = tabs.find((t) => t.id === tabId);
    if (!tab) return;
    tab.ask = {
      question: o.question,
      original: o.query ?? tab.ask?.original ?? '',
      ...(context.length ? { context } : {}),
    };
    if (!o.query) {
      outcomes[tabId]?.controller?.abort();
      delete outcomes[tabId];
      delete lastShown[tabId];
    }
    const run = o.run ?? askPrefs.mode === 'run';
    const summary = !!assistant?.summary;
    const controller = new AbortController();
    asking = { tabId, step: FIRST_STEP, controller };
    // the state proxy, so that a later ask can tell it is no longer this one
    const mine = asking;
    const owns = claim(tabId);
    let diagnosis: askApi.Diagnosis | undefined;
    const started = performance.now();
    const reasoning = reasoningFor(name);
    const t = () => tabs.find((x) => x.id === tabId);
    const q = () => t()?.ask;
    const setText = (text: string | undefined) => {
      const cur = t();
      if (!cur || text == null) return;
      cur.query = text;
      if (cur.ask) cur.ask.original = text;
    };
    const on = (e: askApi.AskEvent) => {
      if (asking !== mine) return;
      const step = nextStep(e, { run, summary });
      if (step) mine.step = step;
      const a = q();
      if (!a) return;
      switch (e.event) {
        case 'draft':
          setText(e.data.query);
          a.explanation = e.data.explanation || a.explanation;
          a.assumptions = e.data.assumptions ?? a.assumptions;
          break;
        case 'clarify':
          a.clarify = e.data;
          break;
        case 'diagnosis':
          if (e.data.empty) diagnosis = e.data as askApi.Diagnosis;
          break;
        case 'result': {
          const r = e.data;
          setText(r.query);
          if (r.explanation) a.explanation = r.explanation;
          if (r.assumptions) a.assumptions = r.assumptions;
          a.graph = r.graph ?? null;
          if (r.results && owns()) {
            const res = r.results;
            const cols = graphColumns(r.graph, res.vars ?? []);
            outcomes[tabId!] = {
              status: 'done',
              ds: name,
              branch,
              kind: res.queryType,
              result: res,
              view: cols ? 'graph' : defaultView(res),
              startedAt: started,
              elapsed: performance.now() - started,
              reasoning,
            };
            if (cols) gCols = cols;
            else autoPickColumns(res);
            if (isEmptyResult(res)) {
              if (diagnosis) outcomes[tabId!].diagnosis = { state: 'done', d: diagnosis };
              else if (r.query)
                void diagnose(tabId!, started, onBranch(name, branch), r.query, at, reasoning);
            }
          }
          break;
        }
        case 'summary':
          a.summary = e.data;
          break;
        case 'global':
          a.mode = 'global';
          a.global = e.data;
          break;
        case 'usage':
          a.askId = e.data.askId;
          if (!a.global && e.data.outcome !== 'error') a.mode = 'local';
          a.usage = {
            outcome: e.data.outcome,
            askId: e.data.askId,
            answeredBy: e.data.answeredBy,
            steps: e.data.steps,
            escalations: e.data.escalations,
            tryHarder: e.data.tryHarder,
            smallModel: e.data.smallModel,
            notes: e.data.notes,
          };
          break;
        case 'error':
          a.failure = askFailure(e.data.code, e.data.message, e.data.resetAt);
          if (e.data.result?.query) setText(e.data.result.query);
          if (e.data.result?.explanation) a.explanation = e.data.result.explanation;
          break;
      }
    };
    try {
      await askApi.askStream(
        name,
        {
          question: o.question,
          ...(context.length ? { context } : {}),
          ...(o.clarification ? { clarification: o.clarification } : {}),
          ...(o.tryHarder ? { tryHarder: o.tryHarder } : {}),
          ...(o.query ? { query: o.query } : {}),
          ...(o.mode ? { mode: o.mode } : {}),
          ...(at ? { at } : {}),
          ...(branch ? { branch } : {}),
          ...(reasoning != null ? { reasoning } : {}),
          run,
          maxRows: limit,
        },
        on,
        controller.signal,
      );
    } catch (e) {
      const a = q();
      if (a && !(e instanceof DOMException && e.name === 'AbortError')) {
        const err = e instanceof api.ApiError ? e : null;
        a.failure = askFailure(
          err?.code ?? '',
          err?.message ?? api.errorMessage(e),
          typeof err?.body?.resetAt === 'string' ? err.body.resetAt : undefined,
        );
      }
    } finally {
      if (asking === mine) asking = null;
    }
    const a = q();
    const cur = t();
    // the server keeps the question when the dataset has a history, else this browser does
    if (a && cur?.query.trim() && !a.clarify && !assistant?.historyDays)
      remember(name, a, cur.query);
  }

  function askQuestion() {
    const question = askText.trim();
    if (!question) return;
    const context = fresh ? [] : followUpContext(active.ask, active.query);
    fresh = false;
    void ask({ question, context });
  }

  function newQuestion() {
    fresh = true;
    askText = '';
    askBar?.focus();
  }

  function stopAsk() {
    asking?.controller.abort();
  }

  /** Answer a clarification: the same tab asks again with the choice. */
  function clarify(value: string) {
    const a = active.ask;
    if (!a?.question || !a.clarify) return;
    void ask({
      question: a.question,
      tabId: activeId,
      context: a.context,
      clarification: { id: a.clarify.id, value },
    });
  }

  function tryHarder() {
    const a = active.ask;
    if (!a?.question || !a.askId) return;
    void ask({ question: a.question, tabId: activeId, context: a.context, tryHarder: a.askId });
  }

  /** Ask the tab's question again, from the data or from the community reports. */
  function answerFrom(mode: 'local' | 'global') {
    const a = active.ask;
    if (!a?.question) return;
    void ask({ question: a.question, tabId: activeId, context: a.context, mode });
  }

  /** A new summary of the edited query's rows. */
  function summarizeAgain() {
    const a = active.ask;
    if (!a?.question || !active.query.trim()) return;
    void ask({ question: a.question, tabId: activeId, query: active.query, run: true });
  }

  async function sendFeedback(outcome: askApi.Feedback) {
    const a = active.ask;
    const name = ds;
    if (!a?.askId || !name) return;
    try {
      await askApi.askFeedback(name, a.askId, outcome);
      a.feedback = outcome;
    } catch (e) {
      toasts.error('Could not send the feedback', e);
    }
  }

  // an edit clears the summary, which described the old query
  $effect(() => {
    const a = active.ask;
    if (a?.summary && active.query !== a.original) a.summary = undefined;
  });
  // a new result forgets the chosen row
  $effect(() => {
    void outcome?.result;
    highlight = null;
    hoverRow = null;
  });

  const answer = $derived(active.ask?.askId ? active.ask : undefined);
  const askedAnswer = $derived(answeredBy(answer?.usage));
  const showSummary = $derived(
    !!answer?.summary &&
      !!outcome?.result &&
      outcome.view !== 'explain' &&
      outcome.status === 'done',
  );
  const citedRows = $derived(new Set((answer?.summary?.citations ?? []).map((n) => n - 1)));

  function cite(row: number) {
    setView('table');
    highlight = { row: row - 1 };
  }

  /** The server's history of the caller's questions, when the dataset keeps one. */
  let serverAsked = $state<(Asked & { id: string })[] | null>(null);
  $effect(() => {
    const name = ds;
    if (!savedOpen || !name || !assistant || !assistant.historyDays) {
      serverAsked = null;
      return;
    }
    const ctl = new AbortController();
    askApi
      .askHistory(name, ctl.signal)
      .then((list) => {
        if (ds === name) serverAsked = askedFromRecords(list);
      })
      .catch(() => (serverAsked = null));
    return () => ctl.abort();
  });

  /** The questions of this browser that the server's history does not hold. */
  const localAsked = $derived(
    askedList.filter(
      (a) => !serverAsked?.some((x) => x.question === a.question && x.query === a.query),
    ),
  );

  async function forgetAsked(id: string) {
    const name = ds;
    if (!name) return;
    try {
      await askApi.deleteAsk(name, id);
      serverAsked = serverAsked?.filter((a) => a.id !== id) ?? null;
    } catch (e) {
      toasts.error('Could not remove the question', e);
    }
  }

  // the check of a tab with a question: its terms, issues and estimated rows, refreshed a
  // moment after the query stops changing
  let checks = $state<Record<string, TabCheck>>({});
  let checking = $state<Record<string, boolean>>({});
  const checkSeq: Record<string, number> = {};

  async function runCheck(tabId: string, text: string, dsTarget: string, dsName: string) {
    const seq = (checkSeq[tabId] = (checkSeq[tabId] ?? 0) + 1);
    checking[tabId] = true;
    let at: string | undefined;
    try {
      at = normalizeAt(app.queryAt) ?? undefined;
    } catch {
      at = undefined;
    }
    let next: TabCheck;
    try {
      const result = await askApi.checkQuery(dsTarget, text, {
        terms: true,
        explain: true,
        reasoning: reasoningFor(dsName),
        at,
      });
      next = { text, result };
    } catch (e) {
      next = { text, error: api.errorMessage(e) };
    }
    if (checkSeq[tabId] !== seq) return;
    checks[tabId] = next;
    checking[tabId] = false;
  }

  $effect(() => {
    const t = active;
    const q = t.ask;
    const text = t.query;
    const name = ds;
    const dsTarget = target;
    if (!hasQuestion(q) || !name || !dsTarget || !text.trim()) return;
    const prev = untrack(() => checks[t.id]);
    if (prev?.text === text) return;
    const timer = setTimeout(() => void runCheck(t.id, text, dsTarget, name), prev ? 1200 : 0);
    return () => clearTimeout(timer);
  });

  /** A refusal of a handoff link, shown until dismissed. */
  let linkNotice = $state<string | null>(null);

  /** Open a handoff link's payload (`#ask=`): re-check it, never run it. */
  async function openHandoff(h: Handoff) {
    const name = h.dataset;
    if (app.datasets.some((d) => d.name === name) || !app.datasetsLoaded) app.setDataset(name);
    if (h.branch) app.queryBranch = h.branch === MAIN ? '' : h.branch;
    if (h.atCommit != null) app.queryAt = `commit:${h.atCommit}`;
    const q: TabQuestion = {
      question: h.question,
      explanation: h.explanation,
      assumptions: h.assumptions,
      original: h.query,
    };
    // an update never reaches the editor, whatever the server says about it
    if (queryKind(h.query) === 'UPDATE') {
      refuseLink();
      return;
    }
    let check: TabCheck;
    try {
      const result = await askApi.checkQuery(onBranch(name, branchParam(h.branch ?? '')), h.query, {
        terms: true,
        explain: true,
        reasoning: reasoningFor(name),
        ...(h.atCommit != null ? { atCommit: h.atCommit } : {}),
      });
      if (isNotAQuery(result)) {
        refuseLink();
        return;
      }
      check = { text: h.query, result };
    } catch (e) {
      check = { text: h.query, error: api.errorMessage(e) };
    }
    openQuestion(q, h.query);
    checks[activeId] = check;
    remember(name, q, h.query);
  }

  /** Remove the fragment from the address without a navigation. */
  function dropFragment() {
    const url = location.pathname + location.search;
    try {
      replaceState(url, page.state);
    } catch {
      history.replaceState(history.state, '', url);
    }
  }

  function refuseLink() {
    linkNotice = UPDATE_REFUSAL;
    addTab('', 'Refused link');
  }

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
    delete lastShown[id];
    delete checks[id];
    editor?.forget(id);
    tabs.splice(i, 1);
    if (!tabs.length) tabs.push({ id: newId(), title: 'Query 1', query: DEFAULT_QUERY });
    if (activeId === id) activeId = tabs[Math.min(i, tabs.length - 1)].id;
  }

  function setQuery(q: string) {
    const t = tabs.find((x) => x.id === activeId);
    if (t) t.query = q;
  }

  // lint the active query a moment after it stops changing, in the browser when the
  // module is there, else through POST /$/lint
  $effect(() => {
    const text = active.query;
    const tab = activeId;
    if (!lintOn || !editor) {
      lintDiagnostics = [];
      editor?.setLint([]);
      return;
    }
    const timer = setTimeout(() => void lintQuery(text, tab), 400);
    return () => clearTimeout(timer);
  });

  async function lintQuery(text: string, tab: string) {
    const seq = ++lintSeq;
    let found: api.LintDiagnostic[] = [];
    if (text.trim() && !lintRefused) {
      try {
        found = (await lintAny({ text, language: 'sparql' })).diagnostics;
      } catch (e) {
        if (e instanceof api.ApiError && [401, 403, 404].includes(e.status)) lintRefused = true;
      }
    }
    if (seq !== lintSeq || tab !== activeId) return;
    lintDiagnostics = found;
    editor?.setLint(found);
  }

  /** Apply the lint's safe fixes, as one undoable change. */
  async function fixQuery() {
    formatMenuOpen = false;
    const ed = editor;
    if (!ed) return;
    const { text } = ed.snapshot();
    try {
      const r = await lintAny({ text, language: 'sparql', fix: true });
      if (r.text != null && r.text !== text) ed.replaceFormatted(r.text, null);
      toasts.push(
        'success',
        r.applied
          ? `Fixed ${r.applied} lint problem${r.applied === 1 ? '' : 's'}`
          : 'Nothing to fix',
      );
    } catch (e) {
      toasts.error('Fixing the query failed', e);
    }
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
    const dsBranch = branch;
    const dsTarget = onBranch(dsName, dsBranch);
    // capture the tab and its text before awaiting: the user may switch tabs meanwhile
    const tab = active;
    // running an answer after an edit tells the server the answer was edited (§5.5)
    const a = tab.ask;
    if (!stored && a?.askId && !a.feedback && tab.query !== a.original) {
      a.feedback = 'edited';
      void askApi.askFeedback(dsName, a.askId, 'edited').catch(() => {});
    }
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
    // compare mode reads the query at the left handle too, and marks what differs
    const against = timeOpen && compare ? timeAgainst : '';
    const compareOn =
      timeOpen && compare && (!!timeFrom || !!against) && k !== 'UPDATE' && k !== 'ASK';
    let fromAt: string | undefined;
    if (compareOn && !against) {
      try {
        fromAt = normalizeAt(timeFrom) ?? undefined;
      } catch (e) {
        toasts.push('error', 'Invalid From', (e as Error).message);
        return;
      }
    }
    // while the slider moves, the last state stays on screen until the next one has arrived
    const shown = lastShown[tabId];
    const keepShown = timeOpen && !!shown?.result && shown.ds === dsName ? shown : undefined;
    const controller = new AbortController();
    const prevView = outcomes[tabId]?.view;
    const prevKind = outcomes[tabId]?.result?.queryType;
    const started = performance.now();
    const reasoning = reasoningFor(dsName);
    outcomes[tabId] = {
      status: 'running',
      ds: dsName,
      branch: dsBranch,
      kind: k,
      view: prevView ?? 'table',
      startedAt: started,
      controller,
      reasoning,
      ...(keepShown
        ? { result: keepShown.result, base: keepShown.base, baseFrom: keepShown.baseFrom }
        : {}),
    };
    try {
      if (k === 'UPDATE') {
        const result = await api.update(dsTarget, text, controller.signal);
        const ms = performance.now() - started;
        delete lastShown[tabId];
        // the update was applied either way; only the outcome display is ownership-bound
        delete app.vocab[dsName];
        void app.refreshDatasets();
        if (!owns()) return;
        outcomes[tabId] = {
          status: 'done',
          ds: dsName,
          branch: dsBranch,
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
            ? `${dsTarget}: ${receiptSummary(result.receipt)} in ${fmtMs(ms)}`
            : result
              ? `${dsTarget}: +${fmtInt(result.inserted)} / −${fmtInt(result.deleted)} quads in ${fmtMs(ms)}`
              : `${dsTarget} in ${fmtMs(ms)}`,
        );
      } else {
        const opts = { send: limit, reasoning, at, signal: controller.signal };
        const read = (o: api.QueryOptions, target = dsTarget) =>
          stored
            ? api.runStoredQuery(target, stored.name, stored.values, o)
            : api.query(target, text, o);
        // the left side is an earlier commit, or the head of another branch
        const [result, base] = await Promise.all([
          read(opts),
          compareOn
            ? against
              ? read({ ...opts, at: undefined }, onBranch(dsName, against))
              : read({ ...opts, at: fromAt })
            : Promise.resolve(undefined),
        ]);
        const elapsed = performance.now() - started;
        if (!owns()) return;
        if (timeOpen) slider?.accept();
        // Keep the user's chosen view only when re-running the same kind of query.
        const sameKind = prevKind === result.queryType;
        const keep =
          sameKind &&
          prevView &&
          prevView !== 'explain' &&
          (prevView !== 'graph' || result.queryType !== 'ASK') &&
          (prevView !== 'map' ||
            geoColumns(result.vars ?? [], result.rows ?? []).length > 0 ||
            entityColumns(result.vars ?? [], result.rows ?? []).length > 1);
        outcomes[tabId] = {
          status: 'done',
          ds: dsName,
          branch: dsBranch,
          kind: result.queryType,
          result,
          ...(base && base.queryType === result.queryType
            ? {
                base,
                baseFrom: against
                  ? { ds: onBranch(dsName, against), at: '' }
                  : { ds: dsTarget, at: readAt(base.at) || (fromAt ?? '') },
              }
            : {}),
          view: keep ? prevView! : defaultView(result),
          startedAt: started,
          elapsed,
          reasoning,
          query: stored ? undefined : text,
        };
        lastShown[tabId] = outcomes[tabId];
        autoPickColumns(result);
        // an empty result gets a diagnosis, after the result is on screen
        if (!stored && isEmptyResult(result))
          void diagnose(tabId, started, dsTarget, text, at, reasoning);
      }
    } catch (e) {
      if (!owns()) return;
      if (e instanceof DOMException && e.name === 'AbortError') {
        // a slider move that was superseded leaves the state that is on screen
        if (keepShown) outcomes[tabId] = keepShown;
        else delete outcomes[tabId];
        return;
      }
      // a state the server can no longer read: the handle goes back and the view keeps its state
      const gone = timeOpen ? timeFailure(e) : null;
      if (gone) {
        toasts.push('error', gone.title, gone.detail);
        slider?.reject({ seq: gone.seq, readable: gone.readable });
        if (keepShown) {
          outcomes[tabId] = keepShown;
          return;
        }
        // nothing was on screen: read the state the handle went back to
        if (!stored) {
          delete outcomes[tabId];
          queueMicrotask(() =>
            onTimeChange({ at: app.queryAt, from: timeFrom, compare, against: timeAgainst }),
          );
          return;
        }
      }
      // a read that failed for any other reason stops Play on the commit that failed
      slider?.fail();
      delete lastShown[tabId];
      outcomes[tabId] = {
        status: 'error',
        ds: dsName,
        branch: dsBranch,
        kind: k,
        error: e as Error,
        view: 'table',
        startedAt: started,
        query: stored ? undefined : text,
      };
      if (e instanceof api.ApiError && e.line && tabId === activeId)
        editor?.showError(e.line, e.column);
    }
  }

  /** Ask the server why a query had no solutions (`/sparql/diagnose`), as the run asked. */
  async function diagnose(
    tabId: string,
    started: number,
    dsTarget: string,
    text: string,
    at: string | undefined,
    reasoning: boolean | undefined,
  ) {
    const mine = () => outcomes[tabId]?.startedAt === started && outcomes[tabId]?.status === 'done';
    if (!mine()) return;
    outcomes[tabId].diagnosis = { state: 'loading' };
    let next: EmptyDiagnosis;
    try {
      const d = await askApi.diagnoseQuery(dsTarget, text, { reasoning, at });
      next = { state: 'done', d };
    } catch {
      next = { state: 'error' };
    }
    if (mine()) outcomes[tabId].diagnosis = next;
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
      ...(prev ?? { ds: dsName, branch, kind: 'EXPLAIN', startedAt: started }),
      updated: undefined,
      error: undefined,
    };
    outcomes[tabId] = { ...base, status: 'running', view: 'explain' } as Outcome;
    try {
      const ex = await api.explain(onBranch(dsName, branch), text, {
        reasoning: reasoningFor(dsName),
      });
      if (!owns()) return;
      outcomes[tabId] = {
        ...base,
        status: 'done',
        explain: ex,
        view: 'explain',
        explainQuery: text,
      } as Outcome;
    } catch (e) {
      if (!owns()) return;
      outcomes[tabId] = {
        status: 'error',
        ds: dsName,
        branch,
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

  /** The triples the Graph view draws, and in compare mode the change of each. */
  const graphData = $derived.by((): { triples: Triple[]; marks?: Mark[] } => {
    const r = outcome?.result;
    if (!r) return { triples: [] };
    const d = diffView && !isTooLarge(diffView) ? diffView : null;
    if (r.triples)
      return d ? { triples: d.rows as Triple[], marks: d.marks } : { triples: r.triples };
    if (r.queryType !== 'SELECT' || !r.vars || !r.rows) return { triples: [] };
    const si = r.vars.indexOf(gCols.s);
    const pi = r.vars.indexOf(gCols.p);
    const oi = r.vars.indexOf(gCols.o);
    if (si < 0 || oi < 0) return { triples: [] };
    const out: Triple[] = [];
    const marks: Mark[] = [];
    const label: api.Term = { type: 'uri', value: `urn:var:${gCols.o}` };
    (d ? d.rows : r.rows).forEach((row, i) => {
      const s = row[si];
      const o = row[oi];
      const p = pi >= 0 ? row[pi] : label;
      if (s && o && p) {
        out.push([s, p, o]);
        if (d) marks.push(d.marks[i]);
      }
    });
    return d ? { triples: out, marks } : { triples: out };
  });
  const graphTriples = $derived(graphData.triples);

  // --- compare mode ----------------------------------------------------------------------

  /** Whether the results are compared: the slider is open with two handles and both reads arrived. */
  const comparing = $derived(timeOpen && compare && !!outcome?.base);
  /** The two results as bags, or that they are too many to compare in the browser. */
  const diffView = $derived.by(() => {
    const o = outcome;
    if (!comparing || !o?.result || !o.base) return null;
    const r = o.result;
    const b = o.base;
    return compareResults(b, r);
  });
  /** The Map view draws both states in compare mode, the changed rows marked. */
  const mapDiff = $derived(diffView && !isTooLarge(diffView) ? diffView : null);
  const shownDiff = $derived(
    diffView && !isTooLarge(diffView) ? filterDiff(diffView, changeFilter) : null,
  );
  const filterCount = (f: ChangeFilter) =>
    !diffView || isTooLarge(diffView)
      ? 0
      : f === 'all'
        ? diffView.rows.length
        : f === 'added'
          ? diffView.added
          : f === 'removed'
            ? diffView.removed
            : diffView.unchanged;
  /** What the slider's live region says after a read. */
  const announce = $derived.by(() => {
    const o = outcome;
    if (!timeOpen || !o || o.status !== 'done' || !o.result) return '';
    const c = o.result.at?.commit ?? o.result.meta.commit;
    const label = c != null ? `Commit ${c}` : 'Head';
    if (diffView && !isTooLarge(diffView)) return rowAnnouncement(label, diffView);
    const n = o.result.rows?.length ?? o.result.triples?.length;
    return n == null
      ? label
      : `${label}: ${n} ${o.result.triples ? 'triple' : 'row'}${n === 1 ? '' : 's'}`;
  });

  // --- comparing on the server ---------------------------------------------------------

  type ServerDiffState = {
    /** The run it compares, so that a newer run drops it. */
    key: number;
    status: 'running' | 'done' | 'error';
    data?: api.SparqlDiff;
    error?: string;
  };
  let serverDiff = $state<ServerDiffState | null>(null);
  const serverNow = $derived(
    serverDiff && outcome && serverDiff.key === outcome.startedAt ? serverDiff : null,
  );
  const serverTable = $derived(
    serverNow?.status === 'done' && serverNow.data ? serverRows(serverNow.data) : null,
  );
  /**
   * The browser cannot compare the results (too many rows, or a read cut them) and the server
   * can: it runs the query text at both states, which a stored query or another branch has not.
   */
  const needsServer = $derived.by(() => {
    const o = outcome;
    if (!comparing || !o?.result || !o.base || !o.query || timeAgainst) return false;
    return !!diffView && isTooLarge(diffView);
  });

  async function compareOnServer() {
    const o = outcome;
    if (!o?.query || !timeFrom) return;
    const key = o.startedAt;
    serverDiff = { key, status: 'running' };
    try {
      const data = await api.sparqlDiff(onBranch(o.ds, o.branch), {
        query: o.query,
        from: normalizeAt(timeFrom) ?? timeFrom,
        to: normalizeAt(app.queryAt) ?? undefined,
        reasoning: o.reasoning ?? undefined,
      });
      if (serverDiff?.key === key) serverDiff = { key, status: 'done', data };
    } catch (e) {
      if (serverDiff?.key === key)
        serverDiff = { key, status: 'error', error: api.errorMessage(e) };
    }
  }

  function onTimeChange(c: TimeChange) {
    void c;
    if (!ds || !active.query.trim() || kind === 'UPDATE') return;
    if (activeStored) runSaved();
    else void run();
  }

  /** History is pressed: the bar opens, or closes and drops the comparison. */
  function toggleTime() {
    if (timeOpen) {
      timeOpen = false;
      compare = false;
      timeFrom = '';
      timeAgainst = '';
    } else {
      timeOpen = true;
    }
  }

  /** The bar's close button and Escape: back to the head. */
  function closeTime() {
    const moved = !!app.queryAt || compare;
    timeOpen = false;
    compare = false;
    timeFrom = '';
    timeAgainst = '';
    app.queryAt = '';
    if (moved && outcome?.result) onTimeChange({ at: '', from: '', compare: false, against: '' });
  }

  // the slider's state is in the address while the bar is open, so that a link reproduces it
  // the router takes `replaceState` once the first navigation has finished
  let mounted = $state(false);
  afterNavigate(() => (mounted = true));
  $effect(() => {
    if (!mounted) return;
    const open = timeOpen;
    const at = app.queryAt;
    const from = timeFrom;
    const comparing = compare;
    const against = timeAgainst;
    const br = branch;
    const name = ds;
    untrack(() => {
      const url = new URL(location.href);
      for (const k of ['ds', 'at', 'from', 'against', 'branch']) url.searchParams.delete(k);
      if (open) {
        if (name) url.searchParams.set('ds', name);
        if (at) url.searchParams.set('at', at);
        if (comparing && from) url.searchParams.set('from', from);
        if (comparing && against) url.searchParams.set('against', against);
        if (br) url.searchParams.set('branch', br);
      }
      const next = url.pathname + url.search + url.hash;
      if (next === location.pathname + location.search + location.hash) return;
      try {
        replaceState(next, page.state);
      } catch {
        history.replaceState(history.state, '', next);
      }
    });
  });

  /** The result's columns holding geometry literals (the Map view draws them). */
  const mapColumns = $derived(
    outcome?.result?.vars && outcome.result.rows
      ? geoColumns(outcome.result.vars, outcome.result.rows)
      : [],
  );

  /**
   * A result with two columns of entities and no geometry column gets a Map tab when some
   * of its entities have geometries (U11 section 2.2). One lookup of the first 200 of them
   * decides, run when the result arrives and not while the time slider is open.
   */
  let arcProbe = $state<{ rows: unknown; ok: boolean } | null>(null);
  $effect(() => {
    const r = outcome?.status === 'done' ? outcome.result : undefined;
    const target = outcome ? onBranch(outcome.ds, outcome.branch) : '';
    const vars = r?.vars;
    const rows = r?.rows;
    // not while the time slider reads state after state: the lookup would be one more read of each
    if (!r || !vars || !rows || mapColumns.length || timeOpen) return;
    const cols = entityColumns(vars, rows);
    if (cols.length < 2) return;
    const at = readAt(r.at) || undefined;
    const iris: string[] = [];
    const idx = cols.map((c) => vars.indexOf(c));
    for (const row of rows) {
      for (const i of idx) {
        const t = row[i];
        if (t?.type === 'uri' && iris.length < 200) iris.push(t.value);
      }
      if (iris.length >= 200) break;
    }
    const ctl = new AbortController();
    arcProbe = { rows, ok: false };
    locateEntities(iris, {
      run: (sparql, limit) => api.select(target, sparql, { at, send: limit, signal: ctl.signal }),
      convert: async (batch) => (await api.geoConvert(batch, ctl.signal)).results,
      signal: ctl.signal,
    })
      .then((found) => {
        if (!ctl.signal.aborted) arcProbe = { rows, ok: found.found.size > 0 };
      })
      .catch(() => {});
    return () => ctl.abort();
  });
  const hasMap = $derived(
    mapColumns.length > 0 || (!!arcProbe?.ok && arcProbe.rows === outcome?.result?.rows),
  );

  const graph = $derived(
    outcome?.view === 'graph'
      ? triplesToGraph(graphTriples, prefixes, {
          hideTypes,
          hideLiterals,
          maxNodes,
          marks: graphData.marks,
        })
      : { nodes: [], edges: [], truncated: false, totalNodes: 0 },
  );
  const choice = $derived(
    chooseRenderer(graph.nodes.length, rendererOverride, webglOk && !webglReason),
  );
  /** The graph Cytoscape draws: at most CANVAS_MAX nodes. */
  const canvasGraph = $derived(
    choice.renderer === 'cytoscape' && graph.nodes.length > CANVAS_MAX
      ? triplesToGraph(graphTriples, prefixes, {
          hideTypes,
          hideLiterals,
          maxNodes: CANVAS_MAX,
          marks: graphData.marks,
        })
      : graph,
  );
  const largeNodes: LNode[] = $derived.by(() => {
    if (choice.renderer !== 'webgl') return [];
    const colors = graphColors();
    return graph.nodes.map((n) => {
      const p = seed(n.id, undefined, 100 + Math.sqrt(graph.nodes.length) * 4);
      return {
        key: n.id,
        label: n.label,
        size: n.kind === 'literal' ? 2.5 : 3.5,
        color: colors[n.kind] ?? colors.iri,
        x: p.x,
        y: p.y,
      };
    });
  });
  const largeEdges = $derived(
    choice.renderer === 'webgl'
      ? graph.edges.map((e) => ({ key: e.id, source: e.source, target: e.target, size: 0.6 }))
      : [],
  );
  function graphColors(): Record<string, string> {
    const css = typeof document === 'undefined' ? null : getComputedStyle(document.documentElement);
    const v = (n: string, d: string) => css?.getPropertyValue(n).trim() || d;
    return {
      iri: v('--iri', '#2459c7'),
      literal: v('--literal', '#0b7d69'),
      bnode: v('--bnode', '#7d45c9'),
      triple: v('--text-2', '#525b6d'),
    };
  }
  const hasTypeEdges = $derived(
    graphTriples.some(([, p]) => p.type === 'uri' && p.value === RDF_TYPE),
  );

  // --- lenses (docs/specs/U10-canvas-lenses.md) ------------------------------------------

  const lenses = new Lenses();
  onDestroy(() => lenses.destroy());
  /** The drawn graph as a list, with a column per active lens. */
  let graphList = $state(false);
  /** The legend entry whose nodes are selected, and its nodes while it exists. */
  let lensPick = $state<string | null>(null);
  const lensMarked = $derived(entryIds(lenses.results, lensPick));
  /** The graph on the canvas: the WebGL view's or the canvas renderer's. */
  const drawnGraph = $derived(choice.renderer === 'webgl' ? graph : canvasGraph);
  $effect(() => {
    const o = outcome;
    if (!o || o.view !== 'graph' || o.status !== 'done' || !o.result) return;
    const info = app.datasets.find((d) => d.name === o.ds);
    lenses.setInput({
      ds: onBranch(o.ds, o.branch),
      branch: o.branch,
      // the lenses read the state the result was read at, the head's commit included
      at: resultState(o.result),
      // only a result read with an At selector shows a past state
      past: !!o.result.at,
      epoch: o.startedAt,
      nodes: drawnGraph.nodes.map((n) => ({ id: n.id, term: n.term })),
      edges: drawnGraph.edges
        .filter((e) => e.change !== 'removed')
        .map((e) => ({ id: e.id, source: e.source, target: e.target, triple: e.triple })),
      reasoning: (!!info?.reasoning && o.reasoning !== false) || !!info?.rdfs,
      // a result's triples need not be in the data, so they are read with reasoning too
      verify: true,
      label: (id) => drawnGraph.nodes.find((n) => n.id === id)?.label ?? id,
      short: (iri) => shortLabel(iri, prefixes),
    });
  });
  $effect(() => {
    void drawnGraph;
    lensPick = null;
  });
  const graphRows = $derived.by((): ListRow[] => {
    if (!graphList) return [];
    const size = lenses.results.size;
    const degree = new Map<string, number>();
    for (const e of drawnGraph.edges) {
      degree.set(e.source, (degree.get(e.source) ?? 0) + 1);
      degree.set(e.target, (degree.get(e.target) ?? 0) + 1);
    }
    const look = lenses.encoding.nodes;
    const pick = new Set(lensMarked ?? []);
    return drawnGraph.nodes.map((n) => {
      const v = size?.values.get(n.id);
      const c = look.get(n.id)?.colour;
      return {
        key: n.id,
        label: n.label,
        kind: n.kind === 'iri' ? 'resource' : n.kind,
        size: typeof v === 'number' ? v : -Infinity,
        degree: degree.get(n.id) ?? 0,
        color:
          c == null
            ? `var(--${n.kind === 'iri' ? 'iri' : n.kind === 'triple' ? 'text-2' : n.kind})`
            : c === OTHER
              ? 'var(--lens-other)'
              : `var(--lens-${c + 1})`,
        openable: n.kind === 'iri',
        marked: pick.has(n.id),
        title: [n.title ?? n.label, lensDescribe(n.id, lenses.results)].filter(Boolean).join('\n'),
      };
    });
  });

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
    const dsBranch = outcome ? outcome.branch : branch;
    downloading = fmt.label;
    try {
      const reasoning = outcome?.reasoning ?? reasoningFor(dsName);
      const blob = await api.queryRaw(onBranch(dsName, dsBranch), active.query, fmt.accept, {
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
    webglOk = hasWebGL2();
    if (app.pendingQuery) {
      const { query, title, question, ask: text } = app.pendingQuery;
      app.pendingQuery = null;
      if (text) {
        askText = text;
        queueMicrotask(() => askBar?.focus());
      } else {
        addTab(query, title);
        if (question) active.ask = question;
      }
    }
    // a handoff link: read the fragment once and drop it, so a reload does not open it again
    const payload = askPayload(location.hash);
    if (payload != null) {
      dropFragment();
      try {
        void openHandoff(decodeHandoff(payload));
      } catch (e) {
        linkNotice = (e as Error).message;
      }
    }
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      // Alt+P outside the editor (whose own keymap has it): the pane, or back to the editor
      if (
        e.altKey &&
        !e.ctrlKey &&
        !e.metaKey &&
        e.code === 'KeyP' &&
        !el?.closest?.('.cm-editor')
      ) {
        e.preventDefault();
        void togglePattern(el?.closest?.('.ppane') ? 'pane' : 'page');
        return;
      }
      if (
        (e.key === 'h' || e.key === 'H') &&
        !e.metaKey &&
        !e.ctrlKey &&
        !e.altKey &&
        !el?.closest?.('input, textarea, select, [contenteditable], .cm-editor')
      ) {
        e.preventDefault();
        toggleTime();
        return;
      }
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
    // a link with a commit reads it
    if (fromUrl && kind !== 'UPDATE' && active.query.trim()) void run();
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
  <!-- the Ask bar, when the dataset has an assistant -->
  <div class="askbar-slot">
    {#if canAsk}
      <AskBar
        bind:this={askBar}
        bind:value={askText}
        bind:mode={askPrefs.mode}
        model={draftModel}
        step={asking?.step ?? null}
        context={askContext}
        onask={askQuestion}
        onstop={stopAsk}
        onnew={newQuestion}
      />
    {/if}
  </div>
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

  <!-- the question of the tab, between the tab strip and the editor -->
  <div class="qhead-slot">
    {#if linkNotice}
      <div class="notice warn link-notice" role="alert">
        <Icon name="alert" size={14} />
        <span>{linkNotice}</span>
        <span class="spacer"></span>
        <button
          class="btn ghost icon sm"
          aria-label="Dismiss"
          title="Dismiss"
          onclick={() => (linkNotice = null)}><Icon name="x" size={12} /></button
        >
      </div>
    {/if}
    {#if hasQuestion(active.ask)}
      <QuestionHeader
        q={active.ask}
        check={checks[activeId]}
        edited={active.query !== active.ask.original}
        checking={!!checking[activeId]}
        {prefixes}
        {canAdmin}
        onrecheck={() => ds && target && void runCheck(activeId, active.query, target, ds)}
        onopen={openIri}
        onsave={() =>
          active.ask &&
          void openSaveExample(
            {
              question: active.ask.question ?? '',
              query: active.query,
              explanation: active.ask.explanation,
            },
            checks[activeId]?.text === active.query ? checks[activeId]?.result : undefined,
          )}
        onsuggest={() => void suggestExample()}
        onclose={() => (active.ask = undefined)}
      />
    {/if}
    {#if active.ask?.clarify && !(asking && asking.tabId === activeId)}
      {@const c = active.ask.clarify}
      <form
        class="clarify"
        aria-label="Clarify the question"
        onsubmit={(e) => {
          e.preventDefault();
          const v = new FormData(e.currentTarget).get('choice');
          if (typeof v === 'string') clarify(v);
        }}
      >
        <p class="clarify-q">{c.question}</p>
        <div class="choices" role="radiogroup" aria-label={c.question}>
          {#each c.choices as ch, i (ch.value + i)}
            <label><input type="radio" name="choice" value={ch.value} required /> {ch.label}</label>
          {/each}
        </div>
        <button class="btn sm primary" type="submit">Continue</button>
      </form>
    {/if}
    {#if active.ask?.failure && !(asking && asking.tabId === activeId)}
      <div class="notice warn ask-failure" role="alert">
        <Icon name="alert" size={14} />
        <span>{active.ask.failure}</span>
        <span class="spacer"></span>
        <button
          class="btn ghost icon sm"
          aria-label="Dismiss"
          title="Dismiss"
          onclick={() => active.ask && (active.ask.failure = undefined)}
          ><Icon name="x" size={12} /></button
        >
      </div>
    {/if}
  </div>

  <!-- toolbar -->
  <div class="toolstack">
    <div class="toolbar">
      <span class="kind" data-kind={kind ?? ''}>{kind ?? 'SPARQL'}</span>
      <span class="target faint">
        {kind === 'UPDATE' ? 'POST' : 'POST'}
        <span class="mono">/{target ?? '…'}/{kind === 'UPDATE' ? 'update' : 'sparql'}</span>
      </span>
      <span class="spacer"></span>
      <button
        class="btn sm"
        aria-pressed={patternOpen}
        title="Show the query's pattern beside the editor (Alt+P)"
        onclick={() => (patternOpen ? (patternOpen = false) : void togglePattern('page'))}
      >
        <Icon name="graph" size={13} /> Pattern
      </button>
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
              <span class="faint"
                >A named query with typed parameters, for HTTP, MCP and the CLI</span
              >
            </button>
            {#if serverAsked?.length || askedList.length}
              <div class="menu-head faint small" role="presentation">Asked</div>
              <div class="menu-scroll" role="group" aria-label="Asked">
                {#each serverAsked ?? [] as a (a.id)}
                  <div class="suggestion">
                    <button
                      role="menuitem"
                      class="menu-item s-text"
                      title={a.query}
                      onclick={() => openAsked(a)}
                    >
                      <span>{a.question}</span>
                      <span class="faint mono small one-line">{a.query.replace(/\s+/g, ' ')}</span>
                    </button>
                    <button
                      class="btn ghost icon sm"
                      aria-label="Forget this question"
                      title="Remove it from your history on the server"
                      onclick={() => void forgetAsked(a.id)}><Icon name="trash" size={12} /></button
                    >
                  </div>
                {/each}
                {#each localAsked as a (a.question + a.at)}
                  <button
                    role="menuitem"
                    class="menu-item"
                    title={a.query}
                    onclick={() => openAsked(a)}
                  >
                    <span>{a.question}</span>
                    <span class="faint mono small one-line">{a.query.replace(/\s+/g, ' ')}</span>
                  </button>
                {/each}
              </div>
            {/if}
            {#if canAdmin}
              <div class="menu-head faint small" role="presentation">Suggestions</div>
              <div class="menu-scroll" role="group" aria-label="Suggestions">
                {#each suggestionList as s (s.id)}
                  <div class="suggestion">
                    <span class="s-text" title={s.query}
                      >{s.question}
                      <span class="faint small">by {s.by}</span></span
                    >
                    <span class="s-actions">
                      <button
                        class="btn sm"
                        onclick={() =>
                          void openSaveExample(
                            { question: s.question, query: s.query, explanation: s.explanation },
                            undefined,
                            s.id,
                          )}>Promote</button
                      >
                      <button class="btn ghost sm" onclick={() => void dismissSuggestion(s)}
                        >Dismiss</button
                      >
                    </span>
                  </div>
                {:else}
                  <p class="faint small menu-note">
                    {suggestionsError ?? 'No suggested examples.'}
                  </p>
                {/each}
              </div>
            {/if}
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
      {#if branchList}
        <label class="at" title="The branch queries read and updates change (branch=)">
          <span class="faint">Branch</span>
          <select class="select sm mono" bind:value={app.queryBranch} aria-label="Branch">
            {#each branchList as b (b.name)}
              <option value={b.name === MAIN ? '' : b.name}>{branchOption(b)}</option>
            {/each}
            {#if branch && !branchList.some((b) => b.name === branch)}
              <option value={branch}>{branch}</option>
            {/if}
          </select>
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
      <button
        class="btn sm"
        aria-pressed={timeOpen}
        title="Move through the commits of the branch (H)"
        onclick={toggleTime}
      >
        <Icon name="clock" size={13} /> History
      </button>
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
            <label class="menu-item check">
              <input type="checkbox" bind:checked={lintOn} />
              <span>Lint while typing</span>
            </label>
            <button class="menu-item" disabled={!fixable} onclick={() => void fixQuery()}>
              Fix lint problems
            </button>
          </div>
        {/if}
      </div>
      {#if lintOn && lintDiagnostics.length}
        <span
          class="lint-status"
          class:error={lintCount.error > 0}
          class:warning={lintCount.error === 0 && lintCount.warning > 0}
          role="status"
          title={lintDiagnostics
            .map((d) => `${d.line}:${d.column} ${d.message} [${d.rule}]`)
            .join('\n')}
        >
          <Icon name={lintCount.error || lintCount.warning ? 'alert' : 'info'} size={12} />
          {lintSummary(lintDiagnostics)}
        </span>
      {/if}
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
                  {#each p.enum as v (String(v))}<option value={String(v)}>{String(v)}</option
                    >{/each}
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
    {#if timeOpen && target}
      <TimeSlider
        bind:this={slider}
        ds={target}
        bind:at={app.queryAt}
        bind:from={timeFrom}
        bind:against={timeAgainst}
        branches={(branchList ?? []).map((b) => b.name).filter((n) => n !== (branch || MAIN))}
        bind:compare
        memory={app.datasets.find((d) => d.name === ds)?.type === 'mem'}
        busy={outcome?.status === 'running'}
        lastReadMs={outcome?.elapsed ?? 0}
        paused={resultsFocused}
        announcement={announce}
        onshowdiff={(from, to) => {
          if (!ds) return;
          goto(diffGraphHref(resolve('/explore'), ds, to, from, { branch: branch || null }));
        }}
        onchange={onTimeChange}
        onclose={closeTime}
      />
    {/if}
  </div>

  <div
    class="editor-wrap"
    class:with-pattern={patternOpen}
    class:phone-pattern={isPhone && patternOpen && phoneTab === 'pattern'}
    style:--pattern-w="{patternW}px"
  >
    {#if isPhone && patternOpen}
      <div class="phone-tabs" role="tablist" aria-label="Query views">
        <button
          class="tab"
          role="tab"
          aria-selected={phoneTab === 'text'}
          onclick={() => (phoneTab = 'text')}>Text</button
        >
        <button
          class="tab"
          role="tab"
          aria-selected={phoneTab === 'pattern'}
          onclick={() => (phoneTab = 'pattern')}>Pattern</button
        >
      </div>
    {/if}
    <div class="editor-cell">
      <SparqlEditor
        bind:this={editor}
        docId={active.id}
        value={active.query}
        onchange={setQuery}
        onrun={() => run()}
        onformat={() => void formatQuery()}
        onpattern={() => void togglePattern('editor')}
        {completion}
      />
    </div>
    {#if patternOpen && !isPhone}
      <div
        class="vsplit"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the Pattern pane"
        onpointerdown={startPatternSplit}
      ></div>
    {/if}
    {#if patternShown}
      <div class="pattern-cell">
        <!-- a pane per tab, so each keeps its own drawing and layout -->
        {#key active.id}
          <PatternPane
            bind:this={pattern}
            text={active.query}
            tab={active.id}
            phone={isPhone}
            prefixes={app.prefixes(ds)}
            schema={patternSchema}
            count={patternCount}
            countKey={`${target ?? ''}|${patternAt ?? ''}`}
            onedit={(changes) => editor?.applyPattern(changes)}
            onundo={() => editor?.undo()}
            onredo={() => editor?.redo()}
            onescape={() => (isPhone ? (phoneTab = 'text') : editor?.focus())}
            onclose={() => (patternOpen = false)}
            plan={planOnPattern ? planSource?.plan : null}
            planText={planSource?.text ?? null}
            planExecuted={planSource?.executed ?? true}
            planHeader={planSource?.header ?? null}
            planSelected={planHover ?? planSel}
            onplanselect={(id) => (planSel = id)}
            onshowplan={showInPlan}
            planSwitch={planSource && planSource.text === active.query
              ? { on: planOnPattern, set: (on) => (planOnPattern = on) }
              : null}
          />
        {/key}
      </div>
    {/if}
  </div>

  <div
    class="splitter"
    role="separator"
    aria-orientation="horizontal"
    aria-label="Resize editor"
    onpointerdown={startSplit}
  ></div>

  <!-- results -->
  <section
    class="results"
    aria-label="Results"
    onfocusin={() => (resultsFocused = true)}
    onfocusout={() => (resultsFocused = false)}
  >
    <!-- a global answer's summary cites points, not rows, so it never renders here -->
    {#if showSummary && answer?.summary && !answer.global && outcome?.result}
      <AnswerSummary
        summary={answer.summary}
        rows={outcome.result.rows?.length ?? outcome.result.triples?.length ?? 0}
        bind:collapsed={askPrefs.summaryCollapsed}
        {hoverRow}
        oncite={cite}
      />
    {/if}
    {#if answer?.global && !outcome && ds}
      <GlobalAnswer
        {ds}
        answer={answer.global}
        summary={answer.summary}
        busy={!!asking}
        onlocal={() => answerFrom('local')}
      />
    {:else if !outcome}
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
              {#if hasMap}
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
              {#if outcome.query}
                <button
                  class="tab"
                  role="tab"
                  aria-selected={outcome.view === 'pattern'}
                  onclick={() => setView('pattern')}
                >
                  <Icon name="graph" size={14} /> Pattern
                </button>
              {/if}
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
              title="A past state of {onBranch(
                outcome.ds,
                outcome.branch,
              )}, read with at={at.selector}{at.datetime ? ` (${at.datetime})` : ''}"
              >{onBranchLabel(outcome.branch, atLabel(at))}</span
            >
          {:else if outcome.result?.meta.commit != null && outcome.view !== 'explain'}
            {@const c = outcome.result.meta.commit}
            <span
              class="badge commit"
              title="The result was read at commit {c} of {onBranch(
                outcome.ds,
                outcome.branch,
              )}{outcome.result.meta.datasetId
                ? ` (dataset id ${outcome.result.meta.datasetId})`
                : ''}"
              >{outcome.branch
                ? onBranchLabel(outcome.branch, `at commit ${c}`)
                : `commit ${c}`}</span
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

      {#if comparing && diffView && outcome.view !== 'explain'}
        <div class="diffbar" role="group" aria-label="Changes between the two commits">
          {#if isTooLarge(diffView)}
            <Icon name="info" size={14} />
            {#if diffView.cut}
              <span
                >The read cut the rows ({fmtInt(diffView.left)} before, {fmtInt(diffView.right)} after),
                so only the counts are compared and the later state is shown.</span
              >
            {:else}
              <span
                >More than {fmtInt(ROW_DIFF_LIMIT)} rows on one side ({fmtInt(diffView.left)} before,
                {fmtInt(diffView.right)} after), so the browser does not compare them and shows the later
                state.</span
              >
            {/if}
            {#if needsServer}{@render serverCompare()}{/if}
          {:else}
            {#if outcome.view !== 'table'}
              <span
                >{fmtInt(diffView.added)} added, {fmtInt(diffView.removed)} removed, {fmtInt(
                  diffView.unchanged,
                )} unchanged</span
              >
            {/if}
            {#each outcome.view === 'table' ? CHANGE_FILTERS : [] as f (f)}
              <button
                class="chip"
                class:on={changeFilter === f}
                aria-pressed={changeFilter === f}
                onclick={() => (changeFilter = f)}
              >
                {f}
                <span class="faint">{fmtInt(filterCount(f))}</span>
              </button>
            {/each}
            <span class="faint small">
              {outcome.result?.triples ? 'triples' : 'rows'} added (<span class="plus">+</span>) or
              removed (<span class="minus">−</span>) {timeAgainst
                ? `against the head of ${timeAgainst}`
                : 'since the earlier commit'}
            </span>
          {/if}
        </div>
      {/if}
      {#if serverNow?.status === 'done' && serverNow.data}
        <div class="diffbar" role="status">
          <Icon name="check" size={14} />
          <span>{serverSummary(serverNow.data)}</span>
          <span class="faint small"
            >Compared on the server, commit {serverNow.data.from.commit.seq} to commit {serverNow
              .data.to.commit.seq}. The table lists the differing rows.</span
          >
        </div>
      {/if}

      <div class="rbody">
        {#if outcome.status === 'error' && outcome.error}
          {@const err = outcome.error}
          {@const rejection = rejectionReport(err)}
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
              {#if rejection}<GuardReportView
                  report={rejection}
                  prefixes={app.prefixes(onBranch(outcome.ds, outcome.branch))}
                />{/if}
              {#if err instanceof api.ApiError && err.detail}<pre>{err.detail}</pre>{/if}
              {#if err instanceof api.ApiError && err.requestId}
                <div class="faint rid">Request <span class="mono">{err.requestId}</span></div>
              {/if}
              {#if err instanceof api.ApiError && err.body?.plan && outcome.query}
                <div>
                  <button
                    class="btn sm"
                    aria-expanded={!!outcome.whyOpen}
                    onclick={() => (outcome.whyOpen = !outcome.whyOpen)}
                  >
                    <Icon name="info" size={13} /> Explain why
                  </button>
                </div>
              {/if}
            </div>
          </div>
          {#if outcome.whyOpen && err instanceof api.ApiError && err.body?.plan}
            {@const { plan: failedPlan, commit: failedCommit, ...failedBody } = err.body}
            <div class="why-plan">
              <ExplainedPlan
                ds={onBranch(outcome.ds, outcome.branch)}
                query={outcome.query}
                plan={failedPlan as api.PlanNode}
                commit={typeof failedCommit === 'number' ? failedCommit : undefined}
                error={failedBody}
                {prefixes}
                open
              />
            </div>
          {/if}
        {:else if outcome.updated}
          <div class="empty">
            <Icon name="check" size={22} />
            <p>
              Update applied to <strong>{outcome.ds}</strong>{#if outcome.branch}
                on the branch <strong class="mono">{outcome.branch}</strong>{/if} in {fmtMs(
                outcome.updated.ms,
              )}.
            </p>
            {#if outcome.updated.result?.receipt}
              {@const r = outcome.updated.result.receipt}
              {#if r.committed}
                <p class="receipt">
                  <span class="badge iri" title="{r.commit.ref} · {r.commit.timestamp}"
                    >{receiptSummary(r)}</span
                  >
                  <span class="faint">{fmtInt(r.commit.quads)} quads after</span>
                  <a
                    class="btn ghost sm"
                    title="Draw what this write changed as a graph"
                    href={diffGraphHref(
                      resolve('/explore'),
                      outcome.ds,
                      `commit:${r.commit.seq}`,
                      `commit:${Math.max(r.commit.seq - 1, 0)}`,
                      { branch: outcome.branch },
                    )}
                    onclick={() => app.setDataset(outcome.ds)}
                    ><Icon name="graph" size={12} /> Show on graph</a
                  >
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
                <ExplainedPlan
                  ds={onBranch(outcome.ds, outcome.branch)}
                  query={outcome.explainQuery}
                  plan={outcome.explain.plan}
                  executed={false}
                  {prefixes}
                  onPattern={planOnPattern}
                  onpattern={(on) => (planOnPattern = on)}
                  select={planSel}
                  onselectop={(id) => (planSel = id)}
                  onhoverop={(id) => (planHover = id)}
                />
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
                {@render emptyWhy(outcome.diagnosis)}
              </div>
            {:else if serverTable}
              {#if serverTable.rows.length}
                <ResultTable
                  vars={serverTable.vars}
                  rows={serverTable.rows}
                  marks={serverTable.marks}
                  {prefixes}
                  onopen={openIri}
                />
              {:else}
                <div class="empty">The two commits return the same rows.</div>
              {/if}
            {:else if r.triples}
              {#if (shownDiff ? shownDiff.rows : r.triples).length}
                <ResultTable
                  vars={['subject', 'predicate', 'object']}
                  rows={shownDiff ? shownDiff.rows : r.triples}
                  marks={shownDiff?.marks}
                  {prefixes}
                  onopen={openIri}
                />
              {:else if shownDiff}
                <div class="empty">No {changeFilter} triples between these commits.</div>
              {:else}
                <div class="empty">The query matched no triples.</div>
              {/if}
            {:else if (shownDiff ? shownDiff.rows.length : r.rows?.length) ?? 0}
              <ResultTable
                vars={r.vars ?? []}
                rows={shownDiff ? shownDiff.rows : (r.rows ?? [])}
                marks={shownDiff?.marks}
                {prefixes}
                onopen={openIri}
                cited={showSummary && !shownDiff ? citedRows : undefined}
                {highlight}
                onhover={showSummary && !shownDiff ? (row) => (hoverRow = row) : undefined}
              />
            {:else if shownDiff}
              <div class="empty">
                No {changeFilter === 'all' ? '' : `${changeFilter} `}rows between these commits.
              </div>
            {:else}
              <div class="empty">
                <p>No results.</p>
                {#if outcome.diagnosis?.state === 'done' && outcome.diagnosis.d.empty}
                  {@render emptyWhy(outcome.diagnosis)}
                {:else}
                  <p class="faint">
                    The pattern matched nothing in <strong>{outcome.ds}</strong>. Check IRIs and
                    prefixes, or try the Explain view.
                  </p>
                  {@render emptyWhy(outcome.diagnosis)}
                {/if}
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
                    {#if webglOk && !webglReason}
                      {#each [10000, 50000, 100000] as n (n)}<option value={n}
                          >{fmtInt(n)} (WebGL)</option
                        >{/each}
                    {/if}
                  </select>
                </label>
                <label class="row">
                  <span class="faint">Renderer</span>
                  <select
                    class="select sm"
                    value={rendererOverride ?? 'auto'}
                    onchange={(e) => {
                      const v = (e.currentTarget as HTMLSelectElement).value;
                      rendererOverride = v === 'auto' ? null : (v as Renderer);
                    }}
                    aria-label="Renderer"
                  >
                    <option value="auto">Automatic</option>
                    <option value="cytoscape">Canvas</option>
                    <option value="webgl" disabled={!webglOk || !!webglReason}>WebGL</option>
                  </select>
                </label>
                <span class="spacer"></span>
                {#if drawnGraph.nodes.length}
                  <LensMenu
                    {lenses}
                    short={(iri) => shortLabel(iri, prefixes)}
                    keys={outcome.view === 'graph'}
                  />
                  <button
                    class="btn sm"
                    aria-pressed={graphList}
                    title="The drawn nodes as a list, with a column per lens"
                    onclick={() => (graphList = !graphList)}
                    ><Icon name="table" size={13} /> List</button
                  >
                {/if}
                <span class="faint"
                  >{fmtInt(
                    choice.renderer === 'webgl' ? graph.nodes.length : canvasGraph.nodes.length,
                  )} nodes,
                  {fmtInt(
                    choice.renderer === 'webgl' ? graph.edges.length : canvasGraph.edges.length,
                  )} edges</span
                >
              </div>
              {#if graph.totalNodes > 100000}
                <div class="notice warn">
                  <Icon name="alert" size={14} />
                  This result has {fmtInt(graph.totalNodes)} nodes. Open the overview or narrow the query
                  ({fmtInt(r.triples?.length ?? r.rows?.length ?? 0)} rows).
                  <a class="btn sm" href={resolve('/explore')}>Overview</a>
                </div>
              {:else if canvasGraph.truncated || (choice.renderer === 'webgl' && graph.truncated)}
                <div class="notice warn">
                  <Icon name="alert" size={14} />
                  Showing {fmtInt(
                    choice.renderer === 'webgl' ? graph.nodes.length : canvasGraph.nodes.length,
                  )}
                  of {fmtInt(graph.totalNodes)} nodes. Large graphs are hard to read; add a LIMIT or raise
                  Max nodes.
                </div>
              {/if}
              {#if choice.reason && graph.nodes.length > CANVAS_MAX}
                <div class="notice">
                  <Icon name="info" size={14} />
                  {choice.reason}.
                </div>
              {/if}
              <div class="gcanvas">
                {#if graphList}
                  <GraphListView
                    rows={graphRows}
                    plain
                    label="Drawn nodes"
                    lensColumns={lenses.columns}
                    selected={largeSelected}
                    onselect={(k) => (largeSelected = k)}
                    openLabel="Explore"
                    onopen={(k) => k.startsWith('<') && openIri(k.slice(1, -1))}
                  />
                {:else if choice.renderer === 'webgl'}
                  <LargeGraphView
                    nodes={largeNodes}
                    edges={largeEdges}
                    selected={largeSelected}
                    label="Result graph"
                    onselect={(k) => (largeSelected = k)}
                    onopen={(k) => (largeSelected = k)}
                    onfail={(reason) => (webglReason = reason)}
                    lens={lenses.encoding}
                    marked={lensMarked}
                  />
                {:else}
                  <GraphView
                    lens={lenses.encoding}
                    marked={lensMarked}
                    nodes={canvasGraph.nodes}
                    edges={canvasGraph.edges}
                    onexpand={(id) => id.startsWith('<') && openIri(id.slice(1, -1))}
                    emptyText={r.queryType === 'SELECT'
                      ? 'Pick subject and object columns to draw a graph'
                      : 'No triples to draw'}
                  />
                {/if}
                <LensLegend
                  results={lenses.results}
                  ds={outcome?.ds ?? ''}
                  selected={lensMarked ? lensPick : null}
                  onselect={(key) => (lensPick = key)}
                />
              </div>
              {#if choice.renderer === 'webgl'}
                <div class="ghint faint">
                  {#if largeSelected}
                    Selected <span class="mono"
                      >{graph.nodes.find((n) => n.id === largeSelected)?.label}</span
                    >
                    {#if largeSelected.startsWith('<')}
                      <button
                        class="btn sm"
                        onclick={() => largeSelected && openIri(largeSelected.slice(1, -1))}
                        >Open in Explore</button
                      >
                    {/if}
                  {:else}
                    WebGL view. Click a node to select it and highlight its neighbours.
                  {/if}
                </div>
              {:else}
                <div class="ghint faint">
                  Double-click an IRI node to open it in Explore. Hover highlights its
                  neighbourhood.
                </div>
              {/if}
            </div>
          {:else if outcome.view === 'map'}
            <ResultMap
              vars={r.vars ?? []}
              rows={(mapDiff ? mapDiff.rows : r.rows) ?? []}
              marks={mapDiff?.marks}
              columns={mapColumns}
              {prefixes}
              onopen={openIri}
              ds={onBranch(outcome.ds, outcome.branch)}
              at={readAt(r.at)}
              baseDs={mapDiff ? outcome.baseFrom?.ds : undefined}
              baseAt={mapDiff ? outcome.baseFrom?.at : undefined}
            />
          {:else if outcome.view === 'plan'}
            {#if r.meta.plan}
              <ExplainedPlan
                ds={onBranch(outcome.ds, outcome.branch)}
                query={outcome.query}
                plan={r.meta.plan}
                commit={r.meta.commit}
                {prefixes}
                onPattern={planOnPattern}
                onpattern={outcome.query ? (on) => (planOnPattern = on) : undefined}
                select={planSel}
                onselectop={(id) => (planSel = id)}
                onhoverop={(id) => (planHover = id)}
              />
            {:else}
              <div class="empty">The server did not return a plan for this query.</div>
            {/if}
          {:else if outcome.view === 'pattern' && outcome.query}
            {@const drawPlan = planOnPattern && !!r.meta.plan}
            <div class="result-pattern" class:with-plan={drawPlan && !isPhone}>
              <div class="result-pattern-main">
                <PatternPane
                  text={outcome.query}
                  readonly
                  phone={isPhone}
                  plan={drawPlan ? r.meta.plan : null}
                  planText={outcome.query}
                  planExecuted
                  planHeader={resultHeader(outcome)}
                  planSelected={planHover ?? planSel}
                  onplanselect={(id) => (planSel = id)}
                  onshowplan={isPhone ? (id) => showInPlan(id, 'result') : undefined}
                  planSwitch={r.meta.plan
                    ? { on: planOnPattern, set: (on) => (planOnPattern = on) }
                    : null}
                  onopen={() => {
                    setQuery(outcome!.query!);
                    patternOpen = true;
                    if (isPhone) phoneTab = 'pattern';
                  }}
                />
              </div>
              {#if drawPlan && !isPhone && r.meta.plan}
                <div class="result-pattern-tree">
                  <PlanView
                    plan={r.meta.plan}
                    {prefixes}
                    selected={planSel}
                    lit={planSel ? new Set([planSel]) : null}
                    onselect={(id) => (planSel = id)}
                    onhover={(id) => (planHover = id)}
                  />
                </div>
              {/if}
            </div>
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
    {#if answer && !(asking && asking.tabId === activeId)}
      {@const u = answer.usage}
      <div class="answer-bar small" aria-label="Answer feedback" role="group">
        {#if askedAnswer}
          <span
            class="mono faint"
            title={['The model that wrote the final query', ...routingLines(u)].join('\n')}
            >{askedAnswer}</span
          >
          {#if u?.smallModel}<span class="badge" title="A model with a small context window"
              >answered by a small model</span
            >{/if}
          {#if u?.escalations?.length}<span class="faint"
              >{u.escalations.length} escalation{u.escalations.length === 1 ? '' : 's'}</span
            >{/if}
        {/if}
        <span class="spacer"></span>
        {#if answer.feedback}
          <span class="faint">Feedback sent: {answer.feedback}</span>
        {:else}
          <button class="btn sm" onclick={() => void sendFeedback('accepted')}
            ><Icon name="check" size={12} /> Correct</button
          >
          <button class="btn sm" onclick={() => void sendFeedback('rejected')}
            ><Icon name="x" size={12} /> Not correct</button
          >
        {/if}
        {#if answer.mode === 'local' && !answer.global}
          <button
            class="btn sm"
            disabled={!!asking}
            onclick={() => answerFrom('global')}
            title="Ask again and answer from the dataset's community summaries"
            ><Icon name="sparkle" size={12} /> Answer from summaries</button
          >
        {/if}
        {#if u?.tryHarder}
          <button
            class="btn sm"
            onclick={tryHarder}
            title="Ask again, starting with the next model of the draft list"
            ><Icon name="zap" size={12} /> Try harder</button
          >
        {/if}
        {#if assistant?.summary && active.query !== answer.original && active.query.trim()}
          <button
            class="btn sm"
            onclick={summarizeAgain}
            title="Run the edited query and summarize its rows"
            ><Icon name="sparkle" size={12} /> Summarize again</button
          >
        {/if}
      </div>
    {/if}
  </section>
</div>

{#snippet serverCompare()}
  <button
    class="btn sm"
    disabled={serverNow?.status === 'running'}
    title="Run the query at both commits on the server and list the rows that differ"
    onclick={() => void compareOnServer()}
  >
    {#if serverNow?.status === 'running'}<span class="spinner"></span>{/if}
    Compare on the server
  </button>
  {#if serverNow?.status === 'error'}
    <span class="error-text small">{serverNow.error}</span>
  {/if}
{/snippet}

{#snippet emptyWhy(diag: EmptyDiagnosis | undefined)}
  {#if diag?.state === 'loading'}
    <p class="faint small"><span class="spinner"></span> Looking for what matched nothing…</p>
  {:else if diag?.state === 'done'}
    {@const why = emptyExplanation(diag.d)}
    {#if why}
      {@const dp = { ...prefixes, ...diag.d.prefixes }}
      <div class="why" role="note" aria-label="Why the result is empty">
        <p><strong>{why.headline}</strong></p>
        <p class="muted">{why.message}</p>
        {#if why.first}
          <p class="small">
            <span class="faint">First {why.first.kind} without solutions</span>
            <code class="mono">{why.first.text}</code>
          </p>
          {#if why.first.missing.length}
            <p class="small">
              <span class="faint">Not in the data you can read:</span>
              {#each why.first.missing as m, i (m)}{#if i},
                {/if}<span class="mono"><TermView term={parseCompact(m, dp)} prefixes={dp} /></span
                >{/each}
            </p>
          {/if}
          {#each why.first.issues as issue, i (i)}
            <p class="small warn-text"><Icon name="alert" size={12} /> {issue}</p>
          {/each}
        {/if}
      </div>
    {/if}
  {/if}
{/snippet}

<Modal
  bind:open={saveOpen}
  title={example ? 'Save as example' : 'Save as a stored query'}
  width={560}
>
  <p class="muted small">
    {example ? 'The query' : "The editor's query"} is stored on the server for /{ds}. Runs bind each
    parameter to one value of its type, so a value never changes the query. It is also an MCP tool.
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
    {#if example}
      <label class="field">
        <span class="small">Example question</span>
        <input class="input" bind:value={example.question} placeholder="The question it answers" />
      </label>
      {#if example.proposals.length}
        <div class="small">Proposed parameters</div>
        <div class="param-rows proposals" aria-label="Proposed parameters">
          {#each example.proposals as p (p.iri)}
            <label class="row small">
              <input type="checkbox" bind:checked={p.use} />
              <span class="mono">?{p.name}</span>
            </label>
            <span class="faint small">iri</span>
            <span class="small proposal-value" title={p.iri}
              ><span class="mono">{p.term}</span>{#if p.label}
                <span class="faint">"{p.label}"</span>{/if}
              <span class="faint">· {p.type}</span></span
            >
          {/each}
        </div>
      {/if}
    {/if}
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
    /* the Ask bar, the tabs, the question and the toolbar, then the editor, the splitter
       and the results; a phone changes only the two variables, so the tracks stay in step
       when a row is added */
    --editor-row: var(--editor-h);
    --results-row: minmax(0, 1fr);
    grid-template-rows: auto auto auto auto var(--editor-row) 7px var(--results-row);
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
  .toolstack {
    min-width: 0;
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
  .qhead-slot,
  .askbar-slot {
    min-width: 0;
  }
  .clarify {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
    font-size: var(--fs-sm);
  }
  .clarify-q {
    margin: 0;
    font-weight: 600;
    flex-basis: 100%;
  }
  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
  }
  .choices label {
    display: inline-flex;
    gap: 4px;
    align-items: center;
    cursor: pointer;
  }
  .ask-failure {
    border-bottom: 1px solid var(--border);
  }
  .answer-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    padding: 6px 12px;
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .link-notice {
    border-bottom: 1px solid var(--border);
  }
  .menu-head {
    padding: 8px 8px 2px;
    border-top: 1px solid var(--border);
    margin-top: 4px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .menu-scroll {
    display: grid;
    max-height: 240px;
    overflow: auto;
  }
  .one-line {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .suggestion {
    display: grid;
    gap: 4px;
    padding: 6px 8px;
    border-radius: 4px;
  }
  .suggestion:hover {
    background: var(--hover);
  }
  .s-text {
    font-weight: 500;
    overflow-wrap: anywhere;
  }
  .s-actions {
    display: flex;
    gap: 4px;
  }
  .proposal-value {
    overflow-wrap: anywhere;
  }
  .why {
    display: grid;
    gap: 4px;
    max-width: 640px;
    text-align: left;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .why p {
    margin: 0;
  }
  .warn-text {
    color: var(--warn);
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
    /* when the toolbar wraps the group to the start of a row, its right-aligned menu stays clear of the sidebar */
    margin-left: 16px;
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
  .lint-status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    white-space: nowrap;
  }
  .lint-status.warning {
    color: var(--warn);
  }
  .lint-status.error {
    color: var(--danger);
  }
  .menu-item.check {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .editor-wrap {
    min-height: 0;
    min-width: 0;
    background: var(--surface);
    display: flex;
  }
  .editor-cell {
    flex: 1;
    min-width: 0;
    min-height: 0;
  }
  .pattern-cell {
    flex: 0 0 var(--pattern-w);
    max-width: calc(100% - 240px);
    min-width: 0;
    min-height: 0;
    border-left: 1px solid var(--border);
  }
  .vsplit {
    flex: 0 0 6px;
    cursor: col-resize;
    background: var(--surface-2);
    border-left: 1px solid var(--border);
  }
  .vsplit:hover {
    background: var(--surface-3);
  }
  .result-pattern {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .result-pattern-main {
    flex: 1;
    min-height: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .result-pattern.with-plan {
    flex-direction: row;
  }
  .result-pattern-tree {
    width: min(42%, 520px);
    min-height: 0;
    overflow: auto;
    border-left: 1px solid var(--border);
    display: flex;
    flex-direction: column;
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
  .diffbar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px 8px;
    padding: 5px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-2);
    font-size: var(--fs-sm);
  }
  .diffbar .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: 11px;
    background: var(--surface);
    color: var(--text);
    font: inherit;
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .diffbar .chip.on {
    background: var(--spark-soft);
    border-color: var(--spark);
  }
  .diffbar .plus {
    color: var(--added);
    font-weight: 700;
  }
  .diffbar .minus {
    color: var(--removed);
    font-weight: 700;
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
  .why-plan {
    flex: 1;
    min-height: 320px;
    border-top: 1px solid var(--border);
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
    /* the lens legend docks in its corner */
    position: relative;
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
      --editor-row: min(var(--editor-h), 40vh);
      --results-row: max(320px, 70vh);
    }
    .toolbar {
      position: relative;
    }
    /* the Text and Pattern tabs over the same query */
    .editor-wrap.with-pattern {
      flex-direction: column;
    }
    .phone-tabs {
      display: flex;
      gap: 2px;
      border-bottom: 1px solid var(--border);
    }
    .phone-tabs .tab {
      flex: 1;
      min-height: 44px;
    }
    .phone-pattern .editor-cell {
      display: none;
    }
    .pattern-cell {
      flex: 1;
      max-width: none;
      border-left: none;
    }
    .page:has(.phone-pattern) {
      --editor-row: max(420px, 65vh);
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
