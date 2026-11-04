<script lang="ts">
  // The ontology header in the editor (docs/specs/U12-ontology-workbench/SPEC.md): the
  // ontology IRI, the version IRI, the imports and the header's annotations of the graph
  // the editor reads, with the counts of the summary. The axiom edits do not cover the
  // header, so its changes are SPARQL updates of ground triples, previewed like the rest.
  import * as api from '$lib/api';
  import { fmtInt } from '$lib/format';
  import {
    dataUpdate,
    headerQuery,
    isLit,
    validIri,
    type Annotation,
    type HeaderTriple,
    type HeaderValue,
    type Lit,
    type Op,
    type OntologySummary,
    type Selection,
  } from '$lib/ontology';
  import type { PrefixMap } from '$lib/rdf';
  import AnnotationRows from './AnnotationRows.svelte';
  import Icon from './Icon.svelte';

  const OWL = 'http://www.w3.org/2002/07/owl#';
  const TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';

  let {
    target,
    sel,
    summary,
    name,
    prefixes,
    annotationProperties,
    readonly = false,
    onedit,
    onrename,
  }: {
    target: string;
    sel: Selection;
    summary: OntologySummary;
    name: (iri: string) => string;
    prefixes: PrefixMap;
    annotationProperties: string[];
    readonly?: boolean;
    onedit: (op: Op, label: string) => void;
    onrename: (iri: string) => void;
  } = $props();

  let triples = $state<{ p: string; o: HeaderValue }[]>([]);
  let error = $state<string | null>(null);
  let newImport = $state('');
  let versionIri = $state('');
  let newIri = $state('');

  const iri = $derived(summary.header.iri);

  const valueOf = (t: api.Term | undefined): HeaderValue | null => {
    if (!t) return null;
    if (t.type === 'uri') return { iri: t.value };
    if (t.type === 'literal') {
      const l: Lit = { '@value': t.value };
      if (t['xml:lang']) l['@language'] = t['xml:lang'];
      else if (t.datatype && !t.datatype.endsWith('#string') && !t.datatype.endsWith('#langString'))
        l['@type'] = t.datatype;
      return l;
    }
    return null;
  };

  $effect(() => {
    const i = iri;
    void summary.commit;
    if (!i) {
      triples = [];
      return;
    }
    const ctl = new AbortController();
    api
      .select(target, headerQuery(i, sel.graph), { signal: ctl.signal, nocache: true })
      .then((rows) => {
        triples = rows
          .map((r) => ({ p: r.p?.type === 'uri' ? r.p.value : '', o: valueOf(r.o) }))
          .filter((t): t is { p: string; o: HeaderValue } => !!t.p && !!t.o);
        versionIri =
          (triples.find((t) => t.p === OWL + 'versionIRI')?.o as { iri: string } | undefined)
            ?.iri ?? '';
        error = null;
      })
      .catch((e) => {
        if (!ctl.signal.aborted) error = api.errorMessage(e);
      });
    return () => ctl.abort();
  });

  const STRUCTURAL = new Set([TYPE, OWL + 'versionIRI', OWL + 'imports']);
  const annotations = $derived(
    triples
      .filter((t) => !STRUCTURAL.has(t.p))
      .map((t): Annotation => ({ property: t.p, value: 'iri' in t.o ? t.o.iri : t.o })),
  );
  const imports = $derived(
    triples.filter((t) => t.p === OWL + 'imports').map((t) => (t.o as { iri: string }).iri),
  );

  const triple = (p: string, o: HeaderValue): HeaderTriple => ({ s: iri!, p, o });
  const fromAnn = (a: Annotation): HeaderTriple =>
    triple(a.property, isLit(a.value) ? a.value : { iri: String(a.value) });

  function update(remove: HeaderTriple[], add: HeaderTriple[], label: string) {
    const { sparql } = dataUpdate(remove, add, sel.graph);
    if (sparql) onedit({ kind: 'update', sparql }, label);
  }

  function changeAnnotations(next: Annotation[], what: string) {
    const key = (t: HeaderTriple) => JSON.stringify([t.p, t.o]);
    const before = annotations.map(fromAnn);
    const after = next.map(fromAnn);
    const b = new Set(before.map(key));
    const a = new Set(after.map(key));
    update(
      before.filter((t) => !a.has(key(t))),
      after.filter((t) => !b.has(key(t))),
      `${what} (ontology header)`,
    );
  }

  function saveVersion() {
    const old = triples.filter((t) => t.p === OWL + 'versionIRI');
    const v = versionIri.trim();
    if (v && !validIri(v)) return void (error = 'The version IRI is not an absolute IRI.');
    update(
      old.map((t) => triple(t.p, t.o)),
      v ? [triple(OWL + 'versionIRI', { iri: v })] : [],
      v ? `Set the version IRI to ${v}` : 'Remove the version IRI',
    );
  }

  function addImport() {
    const v = newImport.trim();
    if (!validIri(v)) return void (error = 'The import is not an absolute IRI.');
    newImport = '';
    update([], [triple(OWL + 'imports', { iri: v })], `Import ${v}`);
  }

  function addHeader() {
    const v = newIri.trim();
    if (!validIri(v)) return void (error = 'The ontology IRI is not an absolute IRI.');
    const { sparql } = dataUpdate([], [{ s: v, p: TYPE, o: { iri: OWL + 'Ontology' } }], sel.graph);
    onedit({ kind: 'update', sparql }, `Add the ontology header ${v}`);
  }

  const counts = $derived(Object.entries(summary.entities).sort((a, b) => b[1] - a[1]));
  const axiomCounts = $derived(Object.entries(summary.axioms).sort((a, b) => b[1] - a[1]));
</script>

<div class="header-ed">
  {#if !iri}
    <p class="muted">This graph has no ontology header, so its ontology is anonymous.</p>
    {#if !readonly}
      <form
        class="add"
        onsubmit={(e) => {
          e.preventDefault();
          addHeader();
        }}
      >
        <input
          class="input mono"
          placeholder="http://example.org/my-ontology"
          aria-label="Ontology IRI"
          bind:value={newIri}
        />
        <button class="btn sm primary" type="submit" disabled={!newIri.trim()}>Add header</button>
      </form>
    {/if}
  {:else}
    <dl class="facts">
      <dt>Ontology IRI</dt>
      <dd>
        <span class="mono iri">{iri}</span>
        {#if !readonly}
          <button class="btn ghost sm" onclick={() => onrename(iri)}
            ><Icon name="pencil" size={12} /> Change</button
          >
        {/if}
      </dd>
      <dt>Version IRI</dt>
      <dd>
        {#if readonly}
          <span class="mono">{versionIri || 'none'}</span>
        {:else}
          <form
            class="add"
            onsubmit={(e) => {
              e.preventDefault();
              saveVersion();
            }}
          >
            <input
              class="input mono"
              aria-label="Version IRI"
              bind:value={versionIri}
              placeholder="none"
            />
            <button class="btn sm" type="submit">Save</button>
          </form>
        {/if}
      </dd>
      <dt>Imports</dt>
      <dd>
        {#if imports.length}
          <ul class="imports">
            {#each imports as im (im)}
              <li>
                <span class="mono">{im}</span>
                {#if summary.imports.find((x) => x.iri === im)?.graph == null}<span class="faint"
                    >not a graph of the dataset</span
                  >{/if}
                {#if !readonly}
                  <button
                    class="btn ghost icon sm"
                    aria-label="Remove the import {im}"
                    onclick={() =>
                      update([triple(OWL + 'imports', { iri: im })], [], `Remove the import ${im}`)}
                    ><Icon name="trash" size={13} /></button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        {:else}<span class="faint">none</span>{/if}
        {#if !readonly}
          <form
            class="add"
            onsubmit={(e) => {
              e.preventDefault();
              addImport();
            }}
          >
            <input
              class="input mono"
              aria-label="Import"
              bind:value={newImport}
              placeholder="IRI of an ontology to import"
            />
            <button class="btn sm" type="submit" disabled={!newImport.trim()}>Add</button>
          </form>
        {/if}
      </dd>
    </dl>
    <section aria-label="Header annotations">
      <h3>Annotations</h3>
      <AnnotationRows
        {annotations}
        {name}
        {prefixes}
        properties={annotationProperties}
        {readonly}
        label="the ontology"
        onchange={changeAnnotations}
      />
    </section>
  {/if}
  {#if error}<p class="error-box" role="alert">{error}</p>{/if}
  <section aria-label="Contents">
    <h3>Contents at commit {summary.commit}</h3>
    <div class="stats">
      <table>
        <tbody>
          {#each counts as [k, n] (k)}<tr><th>{k}</th><td class="num">{fmtInt(n)}</td></tr>{/each}
        </tbody>
      </table>
      <table>
        <tbody>
          {#each axiomCounts.slice(0, 12) as [k, n] (k)}<tr
              ><th>{k}</th><td class="num">{fmtInt(n)}</td></tr
            >{/each}
        </tbody>
      </table>
    </div>
    <p class="faint">
      {fmtInt(summary.triples)} triples read, {fmtInt(summary.unmapped)} outside the OWL 2 mapping.
      {#if summary.diagnostics.warnings || summary.diagnostics.errors}
        {fmtInt(summary.diagnostics.warnings)} warnings and {fmtInt(summary.diagnostics.errors)} errors.
      {/if}
    </p>
    {#if summary.diagnostics.list.length}
      <ul class="diag">
        {#each summary.diagnostics.list.slice(0, 20) as d, i (i)}
          <li class:err={d.severity === 'error'}><span class="mono">{d.code}</span> {d.message}</li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  .header-ed {
    display: grid;
    gap: 14px;
    min-width: 0;
  }
  h3 {
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text-2);
    border-bottom: 1px solid var(--border);
    padding-bottom: 3px;
    margin-bottom: 6px;
  }
  .facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 8px 14px;
    margin: 0;
    align-items: baseline;
  }
  dt {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  dd {
    margin: 0;
    min-width: 0;
    display: grid;
    gap: 4px;
  }
  .iri {
    overflow-wrap: anywhere;
  }
  .add {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    min-width: 0;
  }
  .add .input {
    flex: 1 1 16em;
    min-width: 0;
  }
  .imports {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .imports li {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-wrap: wrap;
    overflow-wrap: anywhere;
  }
  /* the counts as key and value columns, numbers right-aligned, like the panel's other rows */
  .stats {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 14rem), 1fr));
    gap: 0 var(--sp-6);
  }
  .stats table {
    width: 100%;
    border-collapse: collapse;
  }
  .stats table:not(:has(tr)) {
    display: none;
  }
  .stats th,
  .stats td {
    padding: 3px 0;
    border-bottom: 1px solid var(--border);
  }
  .stats th {
    text-align: left;
    font-weight: 500;
    color: var(--text-2);
    padding-right: 14px;
    font-size: var(--fs-sm);
  }
  .stats td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .diag {
    margin: 0;
    padding-left: 18px;
    font-size: var(--fs-sm);
  }
  .diag .err {
    color: var(--danger);
  }
  @media (max-width: 600px) {
    .facts {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
