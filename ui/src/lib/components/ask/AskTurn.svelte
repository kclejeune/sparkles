<script lang="ts">
  // One turn of an Ask thread (C18 §6.2): the question as a heading, the answer with its
  // citation markers, and the work behind it on one rail. While the turn runs, each step
  // lands on the rail as the server finishes it; once it ends, the work folds into Query,
  // Rows, Sources and Steps, each with a one-line summary. The actions act on the answer.
  import type { PrefixMap } from '$lib/rdf';
  import type { Feedback } from '$lib/ask-api';
  import { routingLines, summaryParts, graphColumns } from '$lib/ask';
  import { chipText, foldLines, type ThreadTurn } from '$lib/ask-thread';
  import { triplesToGraph } from '$lib/graph';
  import { fmtInt } from '$lib/format';
  import Icon from '../Icon.svelte';
  import ResultTable from '../ResultTable.svelte';
  import GraphView from '../GraphView.svelte';
  import AskSources from './AskSources.svelte';

  let {
    turn,
    ds,
    prefixes,
    compact = false,
    busy = false,
    onquery,
    onexplore,
    onsave,
    onfeedback,
    onclarify,
    ontryharder,
    onsource,
    onrerun,
  }: {
    turn: ThreadTurn;
    ds: string;
    prefixes: PrefixMap;
    /** The drawer's narrower form: Open in Explore and Save as query go under More. */
    compact?: boolean;
    /** Another turn is running. */
    busy?: boolean;
    onquery: (t: ThreadTurn) => void;
    onexplore: (t: ThreadTurn) => void;
    onsave: (t: ThreadTurn) => void;
    onfeedback: (t: ThreadTurn, f: Feedback) => void;
    onclarify: (t: ThreadTurn, value: string) => void;
    ontryharder: (t: ThreadTurn) => void;
    onsource: (t: ThreadTurn, mode: 'local' | 'global') => void;
    onrerun: (t: ThreadTurn) => Promise<string | null>;
  } = $props();

  type Fold = 'query' | 'rows' | 'sources' | 'steps';
  let opened = $state<Record<Fold, boolean>>({
    query: false,
    rows: false,
    sources: false,
    steps: false,
  });
  /** The citation chosen (1-based row), which marks its row and its source. */
  let chosen = $state<number | null>(null);
  /** In a global answer, the point a citation chose. */
  let chosenPoint = $state<number | null>(null);
  let rowsView = $state<'table' | 'graph'>('table');
  let rerunNote = $state<string | null>(null);
  let rerunError = $state<string | null>(null);
  let moreOpen = $state(false);

  const running = $derived(turn.status === 'running');
  const lines = $derived(foldLines(turn));
  const res = $derived(turn.result?.results);
  const parts = $derived(
    turn.summary ? summaryParts(turn.summary.text, turn.summary.citations) : [],
  );
  const cols = $derived(graphColumns(turn.result?.graph, res?.vars ?? []));
  const uid = $derived(`turn-${turn.key}`);

  /** A global answer's markers cite its points, not rows. */
  const isGlobal = $derived(turn.mode === 'global' || !!turn.global?.points.length);
  const reportTitle = (iri: string) =>
    turn.global?.reports.find((r) => r.iri === iri)?.title ?? iri;
  /** The graph of a CONSTRUCT or DESCRIBE result. */
  const tripleGraph = $derived(
    res?.triples ? triplesToGraph(res.triples, prefixes, { maxNodes: 120 }) : null,
  );

  const graph = $derived.by(() => {
    if (!res?.rows || !res.vars || !cols) return null;
    const si = res.vars.indexOf(cols.s);
    const oi = res.vars.indexOf(cols.o);
    const pi = cols.p ? res.vars.indexOf(cols.p) : -1;
    const label = { type: 'uri' as const, value: `urn:var:${cols.o}` };
    const triples = res.rows.flatMap((r) => {
      const s = r[si];
      const o = r[oi];
      const p = pi >= 0 ? r[pi] : label;
      return s && o && p ? [[s, p, o] as [typeof s, typeof p, typeof o]] : [];
    });
    return triplesToGraph(triples, prefixes, { maxNodes: 120 });
  });

  $effect(() => {
    if (cols) rowsView = 'graph';
  });

  function cite(rows: number[]) {
    if (isGlobal) {
      chosenPoint = rows[0];
      return;
    }
    chosen = rows[0];
    opened.sources = true;
    if (res) {
      opened.rows = true;
      rowsView = 'table';
    }
  }

  async function showRows() {
    rerunError = null;
    try {
      rerunNote = await onrerun(turn);
      opened.rows = true;
    } catch (e) {
      rerunError = (e as Error).message;
    }
  }

  const fbLabels: [Feedback, string][] = [
    ['accepted', 'Right'],
    ['edited', 'Edited'],
    ['rejected', 'Wrong'],
  ];
  const canTryHarder = $derived(!!turn.usage?.tryHarder && !!turn.id);
</script>

<article class="turn" aria-labelledby="{uid}-q" aria-busy={running}>
  <div class="q-line">
    <h3 id="{uid}-q">{turn.question}</h3>
    <span class="tags">
      {#if turn.chip}<span class="tag ctx" title={chipText(turn.chip)}>{chipText(turn.chip)}</span
        >{/if}
      {#if turn.mode === 'global'}<span class="tag">Summaries</span>{/if}
      {#if turn.feedback === 'accepted'}<span class="tag ok">Right</span>
      {:else if turn.feedback === 'edited'}<span class="tag">Edited</span>
      {:else if turn.feedback === 'rejected'}<span class="tag bad">Wrong</span>{/if}
    </span>
  </div>

  {#if turn.summary}
    <p class="answer">
      {#each parts as p, i (i)}{#if p.rows}<button
            type="button"
            class="cite"
            class:hot={(isGlobal ? chosenPoint : chosen) != null &&
              p.rows.includes((isGlobal ? chosenPoint : chosen)!)}
            aria-label="Citation {p.text}"
            onclick={() => cite(p.rows!)}>{p.text.replace(/^\[|\]$/g, '')}</button
          >{:else}{p.text}{/if}{/each}
    </p>
  {/if}
  {#if turn.global?.points.length}
    <ol class="points" aria-label="Points of the answer">
      {#each turn.global.points as pt (pt.n)}
        <li class:hot={chosenPoint === pt.n} value={pt.n}>
          {pt.text}
          {#if pt.reports.length}<span class="faint small">
              from {pt.reports.map(reportTitle).join(', ')}</span
            >{/if}
        </li>
      {/each}
    </ol>
  {:else if !turn.summary && turn.result?.explanation && !running}
    <p class="answer muted">{turn.result.explanation}</p>
  {/if}
  {#if turn.withheld}
    <p class="note">
      The answer was kept, but your access no longer covers what it read. Show rows runs its query
      again under your access now.
    </p>
  {:else if turn.status === 'stored' && !turn.summary && !turn.global && !turn.result?.explanation}
    <p class="note">
      This answer was kept before answers were stored. Show rows runs its query again.
    </p>
  {/if}
  {#if turn.failure}<p class="failure" role="alert">{turn.failure}</p>{/if}
  {#if turn.status === 'stopped'}<p class="note">Stopped.</p>{/if}

  {#if turn.clarify?.choices.length && !running}
    <div class="clarify" role="group" aria-label="Which one is meant">
      <p>{turn.clarify.question}</p>
      <div class="choices">
        {#each turn.clarify.choices as c (c.value)}
          <button
            type="button"
            class="btn sm"
            disabled={busy || turn.status === 'stored'}
            onclick={() => onclarify(turn, c.value)}>{c.label}</button
          >
        {/each}
      </div>
    </div>
  {/if}

  {#if running}
    <ol class="work live" aria-label="Steps so far">
      {#each turn.trail as s (s.key)}
        <li class="step">
          <span class="mark" class:bad={!s.ok} aria-hidden="true"
            ><Icon name={s.ok ? 'check' : 'alert'} size={11} /></span
          >
          <span class="n">{s.name}</span><span class="s">{s.detail}</span>
        </li>
      {/each}
      <li class="step now" role="status" aria-live="polite">
        <span class="spin" aria-hidden="true"></span><span class="n">{turn.step ?? 'Working'}…</span
        >
      </li>
    </ol>
  {:else}
    <div class="work">
      {#if lines.query}
        <div class="fold">
          <button
            type="button"
            class="fh"
            aria-expanded={opened.query}
            aria-controls="{uid}-query"
            onclick={() => (opened.query = !opened.query)}
          >
            <span class="chev" class:open={opened.query}><Icon name="chevron" size={10} /></span>
            <span class="n">Query</span><span class="s">{lines.query}</span>
          </button>
          {#if opened.query}
            <div class="fb" id="{uid}-query">
              <pre class="q">{turn.result?.query}</pre>
              {#if turn.check?.issues.length}
                <ul class="issues">
                  {#each turn.check.issues as is, i (i)}<li class:err={is.severity === 'error'}>
                      {is.message}
                    </li>{/each}
                </ul>
              {/if}
              {#if turn.result?.assumptions?.length}
                <p class="small faint">Assumes {turn.result.assumptions.join('; ')}</p>
              {/if}
            </div>
          {/if}
        </div>
      {/if}
      {#if lines.rows}
        <div class="fold">
          <button
            type="button"
            class="fh"
            aria-expanded={opened.rows}
            aria-controls="{uid}-rows"
            onclick={() => (opened.rows = !opened.rows)}
          >
            <span class="chev" class:open={opened.rows}><Icon name="chevron" size={10} /></span>
            <span class="n">Rows</span><span class="s">{lines.rows}</span>
          </button>
          {#if opened.rows}
            <div class="fb" id="{uid}-rows">
              {#if res?.vars && res.rows}
                {#if graph}
                  <div class="segmented seg-row" role="radiogroup" aria-label="Show the rows as">
                    <button
                      type="button"
                      role="radio"
                      aria-checked={rowsView === 'table'}
                      onclick={() => (rowsView = 'table')}>Table</button
                    >
                    <button
                      type="button"
                      role="radio"
                      aria-checked={rowsView === 'graph'}
                      onclick={() => (rowsView = 'graph')}>Graph</button
                    >
                  </div>
                {/if}
                {#if rowsView === 'graph' && graph}
                  <div class="graph-box">
                    <GraphView nodes={graph.nodes} edges={graph.edges} showControls={false} />
                  </div>
                {:else}
                  <div
                    class="table-box"
                    style:height="{Math.min(320, 40 + res.rows.length * 27)}px"
                  >
                    <ResultTable
                      vars={res.vars}
                      rows={res.rows}
                      {prefixes}
                      cited={new Set((turn.summary?.citations ?? []).map((n) => n - 1))}
                      highlight={chosen != null ? { row: chosen - 1 } : null}
                    />
                  </div>
                {/if}
                {#if rerunNote}<p class="small faint">{rerunNote}</p>{/if}
              {:else if res?.boolean != null}
                <p>{res.boolean ? 'Yes' : 'No'}</p>
              {:else if tripleGraph && !res?.triples?.length}
                <p class="small faint">The query ran and made no triples.</p>
                {#if rerunNote}<p class="small faint">{rerunNote}</p>{/if}
              {:else if tripleGraph}
                <div class="graph-box">
                  <GraphView
                    nodes={tripleGraph.nodes}
                    edges={tripleGraph.edges}
                    showControls={false}
                  />
                </div>
                <p class="small faint">{fmtInt(res?.triples?.length ?? 0)} triples</p>
                {#if rerunNote}<p class="small faint">{rerunNote}</p>{/if}
              {:else if turn.result?.query}
                <p class="small faint">
                  {turn.status === 'stored' || turn.rowsDropped
                    ? 'Rows are not kept with the thread.'
                    : 'The query was previewed, not run.'}
                </p>
                <button
                  type="button"
                  class="btn sm"
                  disabled={turn.rerun === 'loading'}
                  onclick={showRows}
                  ><Icon name="play" size={12} />
                  {turn.rerun === 'loading' ? 'Running…' : 'Show rows'}</button
                >
                {#if rerunError}<p class="failure" role="alert">{rerunError}</p>{/if}
              {/if}
            </div>
          {/if}
        </div>
      {/if}
      {#if lines.sources}
        <div class="fold">
          <button
            type="button"
            class="fh"
            aria-expanded={opened.sources}
            aria-controls="{uid}-sources"
            onclick={() => (opened.sources = !opened.sources)}
          >
            <span class="chev" class:open={opened.sources}><Icon name="chevron" size={10} /></span>
            <span class="n">Sources</span><span class="s">{lines.sources}</span>
          </button>
          {#if opened.sources}
            <div class="fb" id="{uid}-sources">
              <AskSources {turn} {ds} {prefixes} {chosen} oncite={(n) => (chosen = n)} />
            </div>
          {/if}
        </div>
      {/if}
      <div class="fold">
        <button
          type="button"
          class="fh"
          aria-expanded={opened.steps}
          aria-controls="{uid}-steps"
          onclick={() => (opened.steps = !opened.steps)}
        >
          <span class="chev" class:open={opened.steps}><Icon name="chevron" size={10} /></span>
          <span class="n">Steps</span><span class="s">{lines.steps}</span>
        </button>
        {#if opened.steps}
          <div class="fb" id="{uid}-steps">
            {#if turn.usage?.steps?.length}
              <ul class="steps">
                {#each turn.usage.steps as s, i (i)}
                  <li>
                    <span class="r">{s.role}</span>
                    <span>{s.provider} · {s.model}, {s.outcome}</span>
                    <span class="ms">{s.latencyMs != null ? `${fmtInt(s.latencyMs)} ms` : ''}</span>
                  </li>
                {/each}
              </ul>
            {:else if turn.trail.length}
              <ul class="steps">
                {#each turn.trail as s (s.key)}<li>
                    <span class="r">{s.name}</span><span>{s.detail}</span><span></span>
                  </li>{/each}
              </ul>
            {/if}
            {#each routingLines(turn.usage).filter((l) => l.includes('moved from')) as l (l)}
              <p class="small faint">{l}</p>
            {/each}
            {#if turn.diagnosis?.text || turn.diagnosis?.message}
              <p class="diag">
                <strong>Why nothing matched:</strong>
                {turn.diagnosis.text ?? turn.diagnosis.message}
              </p>
            {/if}
            {#each turn.usage?.notes ?? [] as n (n)}<p class="small faint">{n}</p>{/each}
            <div class="step-acts">
              {#if canTryHarder}
                <button
                  type="button"
                  class="btn sm"
                  disabled={busy}
                  onclick={() => ontryharder(turn)}><Icon name="zap" size={12} /> Try harder</button
                >
              {/if}
              {#if turn.status !== 'stored'}
                {#if turn.mode === 'global'}
                  <button
                    type="button"
                    class="btn sm"
                    disabled={busy}
                    onclick={() => onsource(turn, 'local')}>Answer from the data</button
                  >
                {:else if turn.global || turn.communities?.length}
                  <button
                    type="button"
                    class="btn sm"
                    disabled={busy}
                    onclick={() => onsource(turn, 'global')}>Answer from summaries</button
                  >
                {/if}
              {/if}
            </div>
          </div>
        {/if}
      </div>
    </div>

    {#if turn.result?.query || turn.id}
      <div class="acts">
        {#if turn.result?.query}
          <button type="button" class="btn sm" onclick={() => onquery(turn)}>Open in Query</button>
          {#if !compact}
            <button type="button" class="btn sm" onclick={() => onexplore(turn)}
              >Open in Explore</button
            >
            <button type="button" class="btn sm" onclick={() => onsave(turn)}>Save as query</button>
          {:else}
            <div class="more">
              <button
                type="button"
                class="btn sm"
                aria-haspopup="menu"
                aria-expanded={moreOpen}
                onclick={() => (moreOpen = !moreOpen)}
                onkeydown={(e) => e.key === 'Escape' && (moreOpen = false)}
                >More <Icon name="chevronDown" size={10} /></button
              >
              {#if moreOpen}
                <div class="dropdown-menu" role="menu">
                  <button
                    type="button"
                    role="menuitem"
                    class="dropdown-item"
                    onclick={() => {
                      moreOpen = false;
                      onexplore(turn);
                    }}>Open in Explore</button
                  >
                  <button
                    type="button"
                    role="menuitem"
                    class="dropdown-item"
                    onclick={() => {
                      moreOpen = false;
                      onsave(turn);
                    }}>Save as query</button
                  >
                </div>
              {/if}
            </div>
          {/if}
        {/if}
        {#if turn.id}
          <span class="end">
            {#if !compact}<span class="faint small">Was this right?</span>{/if}
            <span class="segmented" role="group" aria-label="Feedback">
              {#each fbLabels as [v, l] (v)}
                <button
                  type="button"
                  aria-pressed={turn.feedback === v}
                  onclick={() => onfeedback(turn, v)}>{l}</button
                >
              {/each}
            </span>
          </span>
        {/if}
      </div>
    {/if}
  {/if}
</article>

<style>
  .turn {
    padding: var(--sp-5) 0;
    min-width: 0;
  }
  .turn + :global(.turn) {
    border-top: 1px solid var(--border);
  }
  .q-line {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }
  h3 {
    margin: 0;
    flex: 1 1 24ch;
    min-width: 0;
    font-size: var(--fs-lg);
    font-weight: 600;
    line-height: var(--lh-tight);
    overflow-wrap: anywhere;
  }
  .tags {
    display: flex;
    gap: 4px;
    flex: none;
  }
  .tag {
    display: inline-flex;
    align-items: center;
    height: 18px;
    max-width: 22ch;
    padding: 0 6px;
    border-radius: var(--r-pill);
    font-size: var(--fs-xs);
    font-weight: 600;
    background: var(--surface-3);
    color: var(--text-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tag.ok {
    background: var(--ok-soft);
    color: var(--ok);
  }
  .tag.bad {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .tag.ctx {
    background: var(--spark-soft);
    color: var(--spark-ink);
  }
  .answer {
    margin: var(--sp-3) 0 0;
    font-size: var(--fs-md);
    line-height: 1.6;
    max-width: 68ch;
    overflow-wrap: anywhere;
  }
  .points {
    margin: var(--sp-3) 0 0;
    padding-left: 20px;
    font-size: var(--fs-md);
    line-height: 1.6;
    max-width: 68ch;
  }
  .points li.hot {
    background: var(--spark-soft);
    border-radius: var(--r-sm);
  }
  .cite {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 18px;
    height: 16px;
    padding: 0 4px;
    margin: 0 1px;
    border: 0;
    border-radius: var(--r-sm);
    background: var(--surface-3);
    color: var(--text-2);
    font: inherit;
    font-size: var(--fs-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    vertical-align: 2px;
    cursor: pointer;
  }
  .cite.hot {
    background: var(--spark-soft);
    color: var(--spark-ink);
  }
  .note {
    margin: var(--sp-2) 0 0;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .failure {
    margin: var(--sp-2) 0 0;
    color: var(--danger);
  }
  .clarify {
    margin-top: var(--sp-3);
  }
  .clarify p {
    margin: 0 0 var(--sp-2);
  }
  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  /* the work: one rail, amber while the turn runs */
  .work {
    margin: var(--sp-4) 0 0;
    padding: 0 0 0 var(--sp-3);
    border-left: 2px solid var(--border);
    display: grid;
    gap: 2px;
    list-style: none;
    min-width: 0;
  }
  .work.live {
    border-left-color: var(--spark);
  }
  .step {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 28px;
    min-width: 0;
  }
  .mark {
    display: inline-flex;
    width: 12px;
    color: var(--ok);
    flex: none;
  }
  .mark.bad {
    color: var(--warn);
  }
  .spin {
    width: 10px;
    height: 10px;
    margin: 0 1px;
    border-radius: 50%;
    border: 2px solid var(--spark-soft);
    border-top-color: var(--spark);
    flex: none;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: none;
      border-color: var(--spark);
    }
  }
  .n {
    font-weight: 600;
    flex: none;
  }
  .s {
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .fold {
    min-width: 0;
  }
  .fh {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: 28px;
    padding: 2px var(--sp-1);
    margin-left: calc(-1 * var(--sp-1));
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
    min-width: 0;
  }
  .fh:hover {
    background: var(--hover);
  }
  .fh:focus-visible {
    outline: none;
    box-shadow: var(--focus);
  }
  .chev {
    display: inline-flex;
    color: var(--text-3);
    flex: none;
    transition: transform 0.12s;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  @media (prefers-reduced-motion: reduce) {
    .chev {
      transition: none;
    }
  }
  .fb {
    padding: var(--sp-1) 0 var(--sp-3) 18px;
    min-width: 0;
  }
  pre.q {
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    background: var(--surface-2);
    border-radius: var(--r);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .issues {
    margin: var(--sp-2) 0 0;
    padding-left: 18px;
    font-size: var(--fs-sm);
    color: var(--warn);
  }
  .issues .err {
    color: var(--danger);
  }
  .seg-row {
    margin-bottom: var(--sp-2);
  }
  .table-box {
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r);
    display: flex;
    flex-direction: column;
  }
  .graph-box {
    height: 280px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    overflow: hidden;
    position: relative;
  }
  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .steps li {
    display: grid;
    grid-template-columns: 82px minmax(0, 1fr) auto;
    gap: var(--sp-2);
  }
  .steps .r {
    color: var(--text-2);
  }
  .steps .ms {
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .diag {
    margin: var(--sp-2) 0 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .step-acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }
  .step-acts:empty {
    display: none;
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
  }
  .acts .end {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .more {
    position: relative;
  }
  .more .dropdown-menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 10;
  }
  @media (max-width: 760px) {
    h3 {
      font-size: var(--fs-md);
    }
    .answer,
    .points {
      font-size: var(--fs);
    }
    .acts .end {
      margin-left: 0;
      width: 100%;
    }
    .acts .end .segmented {
      flex: 1;
    }
    .acts .end .segmented > :global(*) {
      flex: 1;
    }
    .steps li {
      grid-template-columns: 70px minmax(0, 1fr) auto;
    }
  }
</style>
