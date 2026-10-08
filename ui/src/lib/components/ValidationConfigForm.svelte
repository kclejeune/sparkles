<script lang="ts">
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { guardConfig, guardForm } from '$lib/validation-config';
  import GuardReportView from './GuardReportView.svelte';
  import type { PrefixMap } from '$lib/rdf';

  let {
    target,
    current,
    prefixes,
    onsaved,
    oncancel,
  }: {
    target: string;
    current: api.WriteValidation | null;
    prefixes: PrefixMap;
    onsaved: (target: string) => void;
    oncancel: () => void;
  } = $props();
  // Capture the target/config at opening; the parent remounts on a branch change.
  const initial = untrack(() => current);
  const initialTarget = untrack(() => target);
  let form = $state(guardForm(initial));
  let busy = $state(false);
  let error = $state<string | null>(null);
  let report = $state<api.GuardReport | null>(null);
  function languageChanged() {
    const language = form.language;
    form = { ...guardForm(null), language, syntax: language === 'shex' ? 'shexc' : 'text/turtle' };
  }
  async function save(e: Event) {
    e.preventDefault();
    busy = true;
    error = null;
    report = null;
    try {
      await api.setWriteValidation(initialTarget, guardConfig(form, initial));
      onsaved(initialTarget);
    } catch (e) {
      error = api.errorMessage(e);
      if (e instanceof api.ApiError && e.status === 409 && e.body?.validation)
        report = e.body.validation as api.GuardReport;
    } finally {
      busy = false;
    }
  }
</script>

<form onsubmit={save} aria-label="Write validation configuration" class="guard-form">
  <div class="fields">
    <label
      >Language <select class="select" bind:value={form.language} onchange={languageChanged}
        ><option value="shacl">SHACL</option><option value="shex">ShEx</option></select
      ></label
    >
    <label
      >Mode <select class="select" bind:value={form.mode}
        ><option value="warn">Warn</option><option value="reject">Reject</option><option value="off"
          >Off</option
        ></select
      ></label
    >
    <label
      >Baseline <select class="select" bind:value={form.baseline}
        ><option value="strict">Strict: all blocking results</option><option value="grandfather"
          >Grandfather: newly introduced results</option
        ></select
      ></label
    >
    {#if form.language === 'shacl'}<label
        >Threshold <select class="select" bind:value={form.threshold}
          ><option value="violation">Violation</option><option value="warning">Warning</option
          ><option value="info">Info</option></select
        ></label
      >{/if}
    <label
      >Data <select class="select" bind:value={form.dataSelection}
        ><option value="default">Default graph</option><option value="union">Union of graphs</option
        ><option value="graphs">Selected graphs</option></select
      ></label
    >
    <label
      >Timeout (seconds) <input
        class="input"
        type="number"
        min="0.001"
        step="any"
        required
        bind:value={form.timeoutSeconds}
      /></label
    >
    <label
      >Report limit <input
        class="input"
        type="number"
        min="1"
        max="10000"
        required
        bind:value={form.reportLimit}
      /></label
    >
  </div>
  <label class="check"
    ><input type="checkbox" bind:checked={form.includeInferences} /> Include materialized inferences</label
  >
  {#if form.dataSelection === 'graphs'}<label
      >Data graph IRIs (one per line) <textarea
        class="input mono"
        rows="3"
        bind:value={form.dataGraphs}></textarea></label
    >{/if}
  <label
    >Shapes or schema source <select class="select" bind:value={form.source}>
      {#if initial?.language === form.language}<option value="keep"
          >Keep the installed source</option
        >{/if}
      <option value="inline">Enter shapes or schema</option><option value="graphs"
        >Read from named graphs</option
      >
    </select></label
  >
  {#if form.source === 'inline'}
    <label
      >Syntax <select class="select" bind:value={form.syntax}>
        {#if form.language === 'shacl'}<option value="text/turtle">Turtle</option><option
            value="text/shaclc">SHACL Compact</option
          ><option value="application/ld+json">JSON-LD</option><option value="application/rdf+xml"
            >RDF/XML</option
          >
        {:else}<option value="shexc">ShEx Compact</option><option value="shexj">ShEx JSON</option
          ><option value="shexr">ShEx RDF (Turtle)</option>{/if}
      </select></label
    >
    <label
      >{form.language === 'shacl' ? 'Shapes' : 'Schema'}
      <textarea class="input mono" rows="8" required bind:value={form.text}></textarea></label
    >
  {/if}
  {#if form.source === 'graphs' || (form.source === 'inline' && form.language === 'shacl')}
    <label
      >{form.language === 'shacl' ? 'Shape' : 'Schema'} graph IRIs (one per line)
      <textarea class="input mono" rows="3" bind:value={form.sourceGraphs}></textarea></label
    >
  {/if}
  {#if form.language === 'shex'}
    <label>Schema base IRI <input class="input mono" bind:value={form.base} /></label>
    <label
      >Shape map (compact text or JSON array) <textarea
        class="input mono"
        rows="4"
        required
        bind:value={form.shapeMap}></textarea></label
    >
  {/if}
  <p class="faint">
    Reject with a strict baseline requires the current data to conform. Grandfather allows existing
    violations while refusing new ones.
  </p>
  {#if error}<div class="error-box" role="alert">{error}</div>{/if}
  {#if report}<GuardReportView {report} {prefixes} />{/if}
  <div class="row">
    <button class="btn primary" disabled={busy}>{busy ? 'Saving…' : 'Save validation'}</button
    ><button class="btn" type="button" disabled={busy} onclick={oncancel}>Cancel</button>
  </div>
</form>

<style>
  .guard-form {
    display: grid;
    gap: 12px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .fields {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
  }
  label {
    display: grid;
    gap: 4px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  textarea {
    width: 100%;
    resize: vertical;
  }
</style>
