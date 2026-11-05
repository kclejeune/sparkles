<script lang="ts">
  import { errorMessage } from '$lib/api';
  import { KIND_LABEL, ontologyMetrics, type OntologyMetrics, type Selection } from '$lib/ontology';

  let { target, sel, at = null }: { target: string; sel: Selection; at?: string | null } = $props();
  let requested = $state(false);
  let result = $state<OntologyMetrics | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let refresh = $state(0);

  // A different scope must never retain a report from the previous ontology or commit.
  $effect(() => {
    void target;
    void sel.graph;
    void sel.imports;
    void at;
    requested = false;
    result = null;
    error = null;
  });
  $effect(() => {
    const t = target,
      graph = sel.graph,
      imports = sel.imports,
      revision = at;
    void refresh;
    if (!requested) return;
    const ctl = new AbortController();
    busy = true;
    result = null;
    error = null;
    ontologyMetrics(t, { graph, imports }, revision, ctl.signal)
      .then((r) => {
        if (!ctl.signal.aborted) result = r;
      })
      .catch((e) => {
        if (!ctl.signal.aborted) error = errorMessage(e);
      })
      .finally(() => {
        if (!ctl.signal.aborted) busy = false;
      });
    return () => {
      ctl.abort();
      busy = false;
    };
  });
</script>

<section aria-label="Ontology metrics" class="metrics">
  <div class="row">
    <h3>Ontology metrics</h3>
    <button
      class="btn sm"
      disabled={busy}
      onclick={() => {
        requested = true;
        refresh++;
      }}
    >
      {busy ? 'Reading metrics…' : result ? 'Refresh metrics' : 'Read metrics'}
    </button>
  </div>
  <p class="faint">
    Reads mapped axioms and instance assertions in the selected graph{sel.imports
      ? ' and its dataset-local imports'
      : ''}. This can take time on a large ontology.
  </p>
  {#if busy}<p role="status">Reading the complete selected scope…</p>{/if}
  {#if error}<p class="error-box" role="alert">{error}</p>{/if}
  {#if result}
    {#if !result.scopeComplete}<p class="notice" role="status">
        The complete ontology is not represented. Counts and profile reports cover only the mapped
        readable scope.
      </p>{/if}
    <p>
      Commit {result.commit} · {result.graphs.join(', ')} · {result.axiomCount.toLocaleString()} mapped
      axioms
    </p>
    <p><strong>DL expressivity bound: {result.expressivity.notation ?? 'Unknown'}</strong></p>
    <p class="faint">
      A conservative containing logic, not a claim of minimal expressivity or a classification
      result. Basic conjunctions and existential restrictions use EL; other constructors extend an
      ALC bound.
    </p>
    {#if result.expressivity.reasons.length}
      <ul>
        {#each result.expressivity.reasons as reason}<li>{reason}</li>{/each}
      </ul>
      {#if result.expressivity.observedBound}<p>
          Observed mapped fragment bound: {result.expressivity.observedBound}
        </p>{/if}
    {/if}
    {#if result.warnings.length}<ul>
        {#each result.warnings as warning}<li>{warning}</li>{/each}
      </ul>{/if}
    <div class="tables">
      <table>
        <caption>Distinct signature entities</caption>
        <tbody
          >{#each Object.entries(result.entities) as [kind, count]}<tr
              ><th>{KIND_LABEL[kind as keyof typeof KIND_LABEL] ?? kind}</th><td
                >{count.toLocaleString()}</td
              ></tr
            >{/each}</tbody
        >
      </table>
      <table>
        <caption>Axioms by type</caption>
        <thead><tr><th>Type</th><th>Terminology</th><th>Streamed assertions</th></tr></thead>
        <tbody
          >{#each Object.keys(result.axioms).sort() as kind}<tr
              ><th>{kind}</th><td>{(result.terminologyAxioms[kind] ?? 0).toLocaleString()}</td><td
                >{(result.assertionOccurrences[kind] ?? 0).toLocaleString()}</td
              ></tr
            >{/each}</tbody
        >
      </table>
    </div>
    <p class="faint">
      Terminology axioms are distinct across the read graphs. Streamed assertions count occurrences,
      including repeats in different graphs. Entities are distinct by kind and IRI, including
      undeclared references and built-ins; a punned IRI counts once per kind. Annotation values are
      not automatically entities.
    </p>
    {#if !result.axiomCount}<p>The selected scope has no mapped axioms.</p>{/if}
    <h4>OWL 2 profiles</h4>
    <p class="faint">
      Reports cover the selected readable graphs and their resolved imports, including assertions.
      Unresolved imports or mapping errors prevent a conclusion about the full ontology. Each list
      shows at most 100 violations; every axiom is checked.
    </p>
    {#each result.reports as report (report.profile)}
      <details>
        <summary
          >{report.title}: {report.conforms
            ? 'conforms in the mapped scope'
            : `${report.total.toLocaleString()} violations`} · {report.checked.toLocaleString()} checked</summary
        >
        {#if report.truncated}<p>
            Showing {report.violations.length} of {report.total.toLocaleString()} violations.
          </p>{/if}
        <ul>
          {#each report.violations as violation}<li>
              <strong>{violation.rule}</strong>
              {violation.message}{#if violation.graph}
                <span class="faint">Graph {violation.graph}</span
                >{/if}{#if violation.axiom}<pre>{violation.axiom}</pre>{/if}
            </li>{/each}
        </ul>
      </details>
    {/each}
  {/if}
</section>

<style>
  p {
    overflow-wrap: anywhere;
  }
  .metrics {
    min-width: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .tables {
    display: flex;
    flex-wrap: wrap;
    gap: 1.5rem;
    overflow-x: auto;
  }
  th {
    text-align: left;
    font-weight: 500;
  }
  td {
    text-align: right;
  }
  th,
  td {
    padding: 0.25rem 0.5rem;
  }
  caption {
    text-align: left;
    font-weight: 600;
    padding-block: 0.5rem;
  }
  details {
    margin-block: 0.75rem;
  }
  pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
