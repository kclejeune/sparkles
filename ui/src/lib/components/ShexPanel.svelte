<script lang="ts">
  // The ShEx side of the dataset page's Validate panel: a schema (ShExC or ShExJ) and a
  // shape map, validated through `POST /{ds}/shex`. Both are kept per dataset in
  // localStorage; a dataset without a draft starts from an example built from its
  // classes with the most instances.
  import * as api from '$lib/api';
  import { onBranch } from '$lib/branches';
  import { toasts } from '$lib/app.svelte';
  import { displayIri, type PrefixMap } from '$lib/rdf';
  import {
    exampleDraft,
    exampleMaps,
    readDraft,
    schemaPrefixes,
    shexDraftKey,
    syntaxError,
    topClasses,
  } from '$lib/shex';
  import { load, save } from '$lib/storage';
  import Icon from './Icon.svelte';
  import ShexResults from './ShexResults.svelte';
  import TurtleEditor from './TurtleEditor.svelte';

  let {
    name,
    branch = null,
    info,
    prefixes,
    namedGraphs,
    explore,
    conforms = $bindable(null),
  }: {
    name: string;
    /** The branch to work on (null: `main`). */
    branch?: string | null;
    info: api.DatasetInfo | undefined;
    prefixes: PrefixMap;
    /** The dataset's named graphs, for the data graph choice. */
    namedGraphs: string[];
    explore: (iri: string) => string;
    /** Whether the last validation conformed (null: none yet). */
    conforms?: boolean | null;
  } = $props();

  const target = $derived(onBranch(name, branch));

  let schema = $state('');
  let map = $state('');
  let classes = $state<string[]>([]);
  let graph = $state('default');
  let useInferences = $state(true);
  let onlyNonconformant = $state(false);
  let validating = $state(false);
  let downloading = $state(false);
  let report = $state<api.ShexReport | null>(null);
  let error = $state<string | null>(null);
  let ctl: AbortController | null = null;
  let editor = $state<TurtleEditor>();

  /** The schema and map are the example (no draft yet, nothing edited). */
  let example = $state(false);

  // a dataset's draft, or the example from its classes
  $effect(() => {
    const ds = name;
    const draft = readDraft(load<unknown>(shexDraftKey(ds), null));
    report = null;
    error = null;
    conforms = null;
    graph = 'default';
    classes = [];
    example = !draft;
    if (draft) ({ schema, map } = draft);
    // the classes feed the example maps, and the example schema
    const ctl = new AbortController();
    api.schemaSummary(target, { limit: 50, signal: ctl.signal }).then(
      (s) => (classes = topClasses(s)),
      () => {},
    );
    return () => ctl.abort();
  });

  // the example follows the classes and the prefixes as they arrive
  $effect(() => {
    if (example) ({ schema, map } = exampleDraft(classes, prefixes));
  });

  function keep() {
    example = false;
    save(shexDraftKey(name), { schema, map });
  }

  // prefixes of the dataset and of the schema, for the results
  const resultPrefixes = $derived({ ...prefixes, ...schemaPrefixes(schema) });
  const examples = $derived(exampleMaps(classes, prefixes));

  const opts = () => ({
    graph,
    reasoning: info?.reasoning ? useInferences : undefined,
    onlyNonconformant,
  });

  function failed(e: unknown) {
    const at = syntaxError(e);
    if (at?.schema) editor?.showError(at.line, at.column);
    error = api.errorMessage(e);
  }

  async function validate() {
    ctl?.abort();
    ctl = new AbortController();
    validating = true;
    error = null;
    editor?.showError(undefined);
    keep();
    try {
      report = await api.shex(target, schema, map, { ...opts(), signal: ctl.signal });
      conforms = report.conforms;
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError')) {
        failed(e);
        report = null;
        conforms = null;
      }
    } finally {
      validating = false;
      ctl = null;
    }
  }

  async function download() {
    downloading = true;
    try {
      const blob = await api.shexRaw(target, schema, map, 'shapemap', opts());
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${name.replace(/[^\w.-]+/g, '_')}-shex-result.json`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) {
      const at = syntaxError(e);
      if (at?.schema) editor?.showError(at.line, at.column);
      toasts.error('Could not download the result map', e);
    } finally {
      downloading = false;
    }
  }

  const ready = $derived(schema.trim() !== '' && map.trim() !== '');
</script>

<div class="panel-body shex">
  <TurtleEditor
    bind:this={editor}
    value={schema}
    onchange={(v) => {
      schema = v;
      keep();
    }}
    label="ShEx schema"
  />
  <label class="field">
    <span class="faint">Shape map</span>
    <textarea
      class="textarea mono"
      rows="2"
      spellcheck="false"
      aria-label="Shape map"
      bind:value={map}
      oninput={keep}></textarea>
  </label>
  <div class="examples faint">
    Examples:
    {#each examples as ex (ex)}
      <button
        class="linkish mono"
        title="Use this shape map"
        onclick={() => {
          map = ex;
          keep();
        }}>{ex.length > 48 ? `${ex.slice(0, 46)}…` : ex}</button
      >
    {/each}
  </div>
  <div class="row opts">
    <label class="inline">
      <span class="faint">Data graph</span>
      <select class="select" bind:value={graph} aria-label="Data graph">
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
    <label class="inline check" title="List only the nodes that do not conform (counts cover all)">
      <input type="checkbox" bind:checked={onlyNonconformant} /> Only nonconformant
    </label>
    <span class="spacer"></span>
    <button
      class="btn"
      onclick={download}
      disabled={downloading || !ready}
      title="Validate and download the result map (ShapeMap JSON)"
    >
      {#if downloading}<span class="spinner"></span>{:else}<Icon name="download" size={13} />{/if} Result
      (.json)
    </button>
    {#if validating}
      <button class="btn" onclick={() => ctl?.abort()}>Cancel</button>
    {/if}
    <button class="btn primary" onclick={validate} disabled={validating || !ready}>
      {#if validating}<span class="spinner"></span>{:else}<Icon name="check" size={14} />{/if} Validate
    </button>
  </div>
  {#if error}<div class="error-box">
      <strong>Validation failed.</strong>
      {error}
    </div>{/if}
</div>
{#if report}
  <ShexResults {report} prefixes={resultPrefixes} {explore} />
{/if}

<style>
  .shex {
    display: grid;
    gap: 10px;
  }
  .field {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .textarea {
    width: 100%;
    min-height: 44px;
    resize: vertical;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
    color: var(--text);
    font-size: 12px;
    line-height: 1.5;
  }
  .textarea:focus {
    outline: none;
    border-color: var(--iri);
    box-shadow: 0 0 0 3px rgba(36, 89, 199, 0.18);
  }
  .examples {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 12px;
    margin-top: -4px;
    font-size: var(--fs-sm);
  }
  .linkish {
    all: unset;
    cursor: pointer;
    font-size: 12px;
    color: var(--iri);
  }
  .linkish:hover {
    text-decoration: underline;
  }
  .linkish:focus-visible {
    box-shadow: var(--focus);
  }
  .opts {
    flex-wrap: wrap;
    gap: 8px;
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .inline .select {
    max-width: 260px;
  }
  .inline.check input {
    accent-color: var(--iri);
    margin: 0;
  }
</style>
