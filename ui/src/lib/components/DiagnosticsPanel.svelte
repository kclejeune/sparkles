<script lang="ts">
  import { goto } from '$app/navigation';
  import * as api from '$lib/api';
  import { fmtInt, fmtMs } from '$lib/format';
  import type { PrefixMap } from '$lib/rdf';
  import Icon from './Icon.svelte';
  import TermView from './TermView.svelte';

  let {
    name,
    hasInferences,
    stale,
    prefixes,
    explore,
  }: {
    name: string;
    /** The dataset has materialized inferences. */
    hasInferences: boolean;
    /** Freshness of those inferences (null: unknown). */
    stale: boolean | null;
    prefixes: PrefixMap;
    explore: (iri: string) => string;
  } = $props();

  const CHECKS: { id: api.DiagnosticCheck; label: string }[] = [
    { id: 'nothing-member', label: 'owl:Nothing members' },
    { id: 'disjoint-classes', label: 'Disjoint classes' },
    { id: 'all-disjoint-classes', label: 'AllDisjointClasses' },
    { id: 'same-different', label: 'sameAs vs differentFrom' },
    { id: 'functional-literal-conflict', label: 'Functional property values' },
    { id: 'thing-empty', label: 'Empty owl:Thing' },
    { id: 'unsatisfiable-class', label: 'Unsatisfiable classes (warning)' },
  ];
  /** OWL 2 RL rule table (OWL 2 Profiles §4.3). */
  const RULES_URL =
    'https://www.w3.org/TR/owl2-profiles/#Reasoning_in_OWL_2_RL_and_RDF_Graphs_using_Rules';
  const isRlRule = (r: string) => /^(cls|cax|eq|prp|dt|scm)-[a-z0-9]+$/.test(r);

  let selected = $state<Record<string, boolean>>(
    Object.fromEntries(CHECKS.map((c) => [c.id, true])),
  );
  let closure = $state<'subclass' | 'none'>('subclass');
  let useInferences = $state(true);
  let running = $state(false);
  let error = $state<string | null>(null);
  let report = $state<api.DiagnosticsReport | null>(null);
  let ms = $state(0);
  let lastLimit = $state(100);
  let ctl: AbortController | null = null;

  $effect(() => {
    // a different dataset: forget the old report
    void name;
    report = null;
    error = null;
  });

  async function run(limit = 100) {
    ctl?.abort();
    ctl = new AbortController();
    running = true;
    error = null;
    lastLimit = limit;
    const t0 = performance.now();
    try {
      const checks = CHECKS.filter((c) => selected[c.id]).map((c) => c.id);
      report = await api.diagnostics(name, {
        checks: checks.length === CHECKS.length ? undefined : checks,
        closure,
        limit,
        reasoning: hasInferences ? useInferences : undefined,
        signal: ctl.signal,
      });
      ms = performance.now() - t0;
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) {
        error = api.errorMessage(e);
        report = null;
      }
    } finally {
      running = false;
      ctl = null;
    }
  }

  const anySelected = $derived(CHECKS.some((c) => selected[c.id]));
  const staleScope = $derived(
    !!report?.scope.inferences.included && report.scope.inferences.stale !== false,
  );
  const truncated = $derived(report?.checks.filter((c) => c.status === 'truncated') ?? []);
  const problems = $derived(
    report?.checks.filter((c) => c.status === 'timeout' || c.status === 'error') ?? [],
  );
  const statusBadge = $derived.by(() => {
    switch (report?.status) {
      case 'violations-found':
        return { cls: 'danger', icon: 'alert', label: 'Violations found' };
      case 'none-found':
        return { cls: 'ok', icon: 'check', label: 'None found' };
      case 'incomplete':
        return { cls: 'warn', icon: 'alert', label: 'Incomplete' };
      default:
        return null;
    }
  });
  const checkLabel = (id: string) => CHECKS.find((c) => c.id === id)?.label ?? id;
  const evidenceTerms = (v: api.Term | api.Term[]) => (Array.isArray(v) ? v : [v]);
  const openIri = (iri: string) => goto(explore(iri));
</script>

<div class="diag">
  <div class="row head">
    <h3>Consistency checks</h3>
    <span class="faint">OWL 2 RL inconsistency rules</span>
    <span class="spacer"></span>
    {#if statusBadge}
      <span class="badge {statusBadge.cls}"
        ><Icon name={statusBadge.icon} size={12} /> {statusBadge.label}</span
      >
    {/if}
  </div>
  <div class="checks" role="group" aria-label="Checks to run">
    {#each CHECKS as c (c.id)}
      <label class="chip" class:on={selected[c.id]} title={c.id}>
        <input type="checkbox" bind:checked={selected[c.id]} />
        {c.label}
      </label>
    {/each}
  </div>
  <div class="row opts">
    <label class="inline">
      <span class="faint">Types</span>
      <select class="select" bind:value={closure} aria-label="Type closure">
        <option value="subclass">follow rdfs:subClassOf</option>
        <option value="none">stated types only</option>
      </select>
    </label>
    {#if hasInferences}
      <label class="inline check" title="Check the data together with the materialized inferences">
        <input type="checkbox" bind:checked={useInferences} /> Use inferences
      </label>
    {/if}
    <span class="spacer"></span>
    {#if running}<button class="btn sm" onclick={() => ctl?.abort()}>Cancel</button>{/if}
    <button class="btn sm" onclick={() => run()} disabled={running || !anySelected}>
      {#if running}<span class="spinner"></span>{:else}<Icon name="check" size={13} />{/if} Run checks
    </button>
  </div>
  {#if error}
    <div class="error-box"><strong>Checks failed.</strong> {error}</div>
  {/if}
  {#if report}
    <div class="scope" class:warn={staleScope}>
      <Icon name="info" size={13} />
      <span>
        {report.note}
        {#if staleScope}
          <strong
            >Inferences are {report.scope.inferences.stale === null
              ? 'of unknown freshness'
              : 'stale'}: findings marked <em>uses inferences</em> may be outdated.</strong
          >
        {/if}
        <span class="faint"
          >Commit {report.commit}, {report.checks.length} check{report.checks.length === 1
            ? ''
            : 's'}, {fmtMs(ms)}.</span
        >
      </span>
    </div>
    {#each truncated as c (c.id)}
      <div class="row faint more">
        {checkLabel(c.id)}: first {fmtInt(c.findings)} of more.
        {#if lastLimit < 1000}
          <button class="btn ghost sm" onclick={() => run(1000)} disabled={running}
            >Load 1000</button
          >
        {/if}
      </div>
    {/each}
    {#each problems as c (c.id)}
      <div class="row faint more">
        <Icon name="alert" size={12} />
        {checkLabel(c.id)}: {c.status === 'timeout' ? 'timed out' : `error: ${c.error ?? ''}`}
      </div>
    {/each}
    {#if report.findings.length}
      <div class="findings">
        <table class="data">
          <thead>
            <tr
              ><th>Check</th><th>Rule</th><th>Focus</th><th>Evidence</th><th>Basis</th><th
                >Message</th
              ></tr
            >
          </thead>
          <tbody>
            {#each report.findings as f, i (i)}
              <tr>
                <td class="cell">
                  <span class="badge {f.severity === 'warning' ? 'warn' : 'danger'}"
                    >{checkLabel(f.check)}</span
                  >
                </td>
                <td class="mono cell rule">
                  {#if isRlRule(f.rule)}
                    <a href={RULES_URL} target="_blank" rel="noreferrer" title="OWL 2 RL rule table"
                      >{f.rule}</a
                    >
                  {:else}{f.rule}{/if}
                </td>
                <td class="mono cell">
                  <TermView term={f.focus} {prefixes} onopen={openIri} />
                </td>
                <td class="mono cell">
                  <div class="ev">
                    {#each Object.entries(f.evidence) as [k, v] (k)}
                      <span class="evk">{k}</span>
                      {#each evidenceTerms(v) as t, j (j)}
                        <TermView term={t} {prefixes} onopen={openIri} />
                      {/each}
                    {/each}
                  </div>
                </td>
                <td>
                  <span
                    class="badge {f.basis === 'asserted' ? 'iri' : 'warn'}"
                    title={f.basis === 'asserted'
                      ? 'Also holds on the asserted data alone'
                      : 'Depends on materialized inferences'}
                    >{f.basis === 'asserted' ? 'asserted' : 'uses inferences'}</span
                  >
                </td>
                <td class="cell msg" title={f.message}>{f.message}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {/if}
</div>

<style>
  .diag {
    display: grid;
    gap: 10px;
    padding: 12px 14px;
    border-top: 1px solid var(--border);
  }
  .head h3 {
    font-size: var(--fs-md);
  }
  .head {
    flex-wrap: wrap;
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 12px;
    font-size: var(--fs-sm);
    color: var(--text-2);
    cursor: pointer;
  }
  .chip.on {
    border-color: var(--iri);
    color: var(--text);
  }
  .chip input {
    margin: 0;
    accent-color: var(--iri);
  }
  .opts {
    flex-wrap: wrap;
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
  }
  .inline.check input {
    accent-color: var(--iri);
    margin: 0;
  }
  .scope {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    padding: 8px 10px;
    border-radius: var(--r);
    background: var(--surface-2);
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .scope.warn {
    background: color-mix(in srgb, var(--warn) 8%, var(--surface));
  }
  .scope.warn strong {
    color: var(--warn);
    font-weight: 600;
  }
  .more {
    font-size: var(--fs-sm);
    flex-wrap: wrap;
  }
  .findings {
    overflow-x: auto;
    margin: 0 -14px -12px;
    border-top: 1px solid var(--border);
  }
  .findings table {
    min-width: 640px;
  }
  .cell {
    font-size: 12px;
  }
  .rule {
    white-space: nowrap;
  }
  .ev {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 6px;
    align-items: baseline;
  }
  .evk {
    color: var(--text-3);
    font-family: var(--font-sans, inherit);
  }
  .msg {
    color: var(--text-2);
    min-width: 200px;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
</style>
