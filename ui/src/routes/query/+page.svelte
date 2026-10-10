<script lang="ts">
  import { goto, replaceState } from '$app/navigation';
  import { page } from '$app/state';
  import { resolve } from '$app/paths';
  import { onMount, untrack } from 'svelte';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import { branchOption, branchParam, MAIN, onBranch, onBranchLabel } from '$lib/branches';
  import { receiptSummary } from '$lib/commits';
  import { atLabel, normalizeAt, validAt } from '$lib/history';
  import { EXAMPLES } from '$lib/examples';
  import { fmtInt, fmtMs, formatSse } from '$lib/format';
  import { geoColumns } from '$lib/geo';
  import { triplesToGraph, type Triple } from '$lib/graph';
  import { applyMissingPrefixes, queryKind, RDF_TYPE } from '$lib/rdf';
  import { formatEditor } from '$lib/fmt-edit';
  import { formatAny, lintAny } from '$lib/fmt-wasm';
  import { lintCounts, lintSummary } from '$lib/lint-view';
  import { LatestRun } from '$lib/supersede';
  import { load, save } from '$lib/storage';
  import { rejectionReport } from '$lib/write-validation';
  import GuardReportView from '$components/GuardReportView.svelte';
  import GraphView from '$components/GraphView.svelte';
  import Icon from '$components/Icon.svelte';
  import ExplainedPlan from '$components/ExplainedPlan.svelte';
  import ResultMap from '$components/ResultMap.svelte';
  import Modal from '$components/Modal.svelte';
  import ResultTable from '$components/ResultTable.svelte';
  import SparqlEditor from '$components/SparqlEditor.svelte';
  import QuestionHeader from '$components/QuestionHeader.svelte';
  import AskBar from '$components/AskBar.svelte';
  import AnswerSummary from '$components/AnswerSummary.svelte';
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
  type View = 'table' | 'graph' | 'map' | 'plan' | 'raw' | 'explain';
  type Outcome = {
    status: 'running' | 'done' | 'error';
    ds: string;
    /** The branch it ran on (null: `main`). */
    branch: string | null;
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
  /** The `branch` field: the branch to read and write, or empty for `main`. */
  const branch = $derived(branchParam(app.queryBranch));
  /** The dataset on that branch, as the API client takes it. */
  const target = $derived(ds ? onBranch(ds, branch) : null);
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
        case 'usage':
          a.askId = e.data.askId;
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
    };
    try {
      if (k === 'UPDATE') {
        const result = await api.update(dsTarget, text, controller.signal);
        const ms = performance.now() - started;
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
        const result = stored
          ? await api.runStoredQuery(dsTarget, stored.name, stored.values, opts)
          : await api.query(dsTarget, text, opts);
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
          branch: dsBranch,
          kind: result.queryType,
          result,
          view: keep ? prevView! : defaultView(result),
          startedAt: started,
          elapsed,
          reasoning,
          query: stored ? undefined : text,
        };
        autoPickColumns(result);
        // an empty result gets a diagnosis, after the result is on screen
        if (!stored && isEmptyResult(result))
          void diagnose(tabId, started, dsTarget, text, at, reasoning);
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
  <div class="toolbar">
    <span class="kind" data-kind={kind ?? ''}>{kind ?? 'SPARQL'}</span>
    <span class="target faint">
      {kind === 'UPDATE' ? 'POST' : 'POST'}
      <span class="mono">/{target ?? '…'}/{kind === 'UPDATE' ? 'update' : 'sparql'}</span>
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
    {#if showSummary && answer?.summary && outcome?.result}
      <AnswerSummary
        summary={answer.summary}
        rows={outcome.result.rows?.length ?? outcome.result.triples?.length ?? 0}
        bind:collapsed={askPrefs.summaryCollapsed}
        {hoverRow}
        oncite={cite}
      />
    {/if}
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
              <ResultTable
                vars={r.vars ?? []}
                rows={r.rows}
                {prefixes}
                onopen={openIri}
                cited={showSummary ? citedRows : undefined}
                {highlight}
                onhover={showSummary ? (row) => (hoverRow = row) : undefined}
              />
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
              <ExplainedPlan
                ds={onBranch(outcome.ds, outcome.branch)}
                query={outcome.query}
                plan={r.meta.plan}
                commit={r.meta.commit}
                {prefixes}
              />
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
    grid-template-rows: auto auto auto auto var(--editor-h) 7px minmax(0, 1fr);
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
      grid-template-rows: auto auto auto min(var(--editor-h), 40vh) 7px max(320px, 70vh);
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
