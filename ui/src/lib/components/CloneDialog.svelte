<script lang="ts">
  import * as api from '$lib/api';
  import { onBranch } from '$lib/branches';
  import { app, toasts } from '$lib/app.svelte';
  import { INFERRED_GRAPH, selectedGraphs, type CloneMode, type CloneType } from '$lib/clone';
  import { fmtInt } from '$lib/format';
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    source,
    branch = null,
    hasInferences,
    graphs = [],
    onstarted,
  }: {
    open?: boolean;
    /** The dataset to clone, and the branch to copy (null: `main`). */
    source: string;
    branch?: string | null;
    /** Offer the "Include inferences" choice. */
    hasInferences: boolean;
    /** The source's graphs (from its statistics), offered for a partial clone. */
    graphs?: { name: string | null; quads: number }[];
    onstarted?: (t: api.Task) => void;
  } = $props();

  let name = $state('');
  let inferences = $state(true);
  let type = $state<CloneType>('persistent');
  let mode = $state<CloneMode>('auto');
  let partial = $state(false);
  let chosen = $state<string[]>([]);
  let typed = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (open) {
      name = `${source}-sandbox`;
      inferences = true;
      type = 'persistent';
      mode = 'auto';
      partial = false;
      chosen = [];
      typed = '';
      error = null;
    }
  });

  const validName = $derived(/^[A-Za-z0-9_-][A-Za-z0-9_.-]*$/.test(name) && name.length <= 64);
  const exists = $derived(app.datasets.some((d) => d.name === name));
  /** The graphs to choose from: the default graph by its keyword, and not the inferred
   *  graph, which the inferences checkbox stands for. */
  const choices = $derived(
    graphs
      .map((g) => ({ name: g.name ?? 'default', quads: g.quads }))
      .filter((g) => g.name !== INFERRED_GRAPH),
  );
  const picked = $derived(
    partial ? selectedGraphs(chosen, typed, hasInferences && inferences) : [],
  );
  const noGraphs = $derived(partial && picked.length === 0);

  async function submit(e: Event) {
    e.preventDefault();
    if (!validName || exists || noGraphs) return;
    busy = true;
    error = null;
    try {
      const t = await api.cloneDataset(onBranch(source, branch), name, {
        type,
        inferences: hasInferences && !inferences ? 'drop' : 'copy',
        mode,
        graphs: picked,
      });
      toasts.push('info', `Cloning into ${name}`, t?.id ? `Task ${t.id}` : undefined);
      onstarted?.(t);
      open = false;
    } catch (err) {
      // conflicts (name taken or being created) show under the name
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal bind:open title="Clone {source}" onclose={() => (error = null)}>
  <form id="clone-ds" onsubmit={submit} class="form">
    <p class="faint intro">
      Copies the current data into a new, independent dataset. Changes to the copy never affect
      <span class="mono">{source}</span>.
    </p>
    <label class="field">
      New name
      <input
        class="input mono"
        bind:value={name}
        autocomplete="off"
        spellcheck="false"
        {@attach (el) => el.select()}
        aria-invalid={!!name && (!validName || exists || !!error)}
      />
      <span class="hint" class:bad={!!error || (name && (!validName || exists))}>
        {#if error}{error}{:else if name && !validName}Use letters, digits, “_”, “-” or “.” (not
          first).{:else if exists}A dataset with this name already exists.{:else}Served at <span
            class="mono">/{name || 'name'}/sparql</span
          >{/if}
      </span>
    </label>
    <fieldset>
      <legend>Type</legend>
      <div class="seg" role="radiogroup" aria-label="Type">
        <label class:sel={type === 'persistent'}
          ><input type="radio" bind:group={type} value="persistent" />Persistent</label
        >
        <label class:sel={type === 'mem'}
          ><input type="radio" bind:group={type} value="mem" />In memory</label
        >
      </div>
      <span class="hint"
        >{type === 'mem'
          ? 'Held in memory. It stays registered after a restart but starts empty, and write-time validation and stored queries are not copied.'
          : 'Stored in the data directory, like any persistent dataset.'}</span
      >
    </fieldset>
    <fieldset>
      <legend>Graphs</legend>
      <div class="seg" role="radiogroup" aria-label="Graphs">
        <label class:sel={!partial}
          ><input type="radio" bind:group={partial} value={false} />All graphs</label
        >
        <label class:sel={partial}
          ><input type="radio" bind:group={partial} value={true} />Only some</label
        >
      </div>
      {#if partial}
        {#if choices.length}
          <div class="graphs" role="group" aria-label="Graphs to copy">
            {#each choices as g (g.name)}
              <label class="check">
                <input type="checkbox" bind:group={chosen} value={g.name} />
                <span class="mono gname" title={g.name}>{g.name}</span>
                <span class="faint">{fmtInt(g.quads)}</span>
              </label>
            {/each}
          </div>
        {/if}
        <label class="field">
          More graphs
          <textarea
            class="input mono"
            rows="2"
            bind:value={typed}
            spellcheck="false"
            placeholder="http://example.org/graphs/*"></textarea>
        </label>
        <span class="hint" class:bad={noGraphs}
          >{noGraphs
            ? 'Choose at least one graph.'
            : 'Graph IRIs or patterns with *, one per line. A partial clone always rebuilds its index.'}</span
        >
      {/if}
    </fieldset>
    {#if type === 'persistent'}
      <label class="field">
        Index files
        <select class="select" bind:value={mode}>
          <option value="auto">Share when possible (reflink, else copy)</option>
          <option value="link">Hard-link when possible</option>
          <option value="rebuild">Always rebuild</option>
        </select>
        <span class="hint"
          >A full clone of a source with no changes since its last compaction shares its index
          files. Any other clone rebuilds the index, which costs about as much as a compaction.</span
        >
      </label>
    {/if}
    {#if hasInferences}
      <label class="check">
        <input type="checkbox" bind:checked={inferences} /> Include inferences
        <span class="faint">(the inferred graph and the reasoning status)</span>
      </label>
    {/if}
  </form>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (open = false)}>Cancel</button>
    <button
      class="btn primary"
      type="submit"
      form="clone-ds"
      disabled={busy || !validName || exists || noGraphs}
    >
      {#if busy}<span class="spinner"></span>{/if} Clone
    </button>
  {/snippet}
</Modal>

<style>
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .intro {
    margin: 0;
    font-size: var(--fs-sm);
  }
  .hint {
    font-weight: 400;
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .hint.bad {
    color: var(--danger);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
  }
  .check input {
    margin: 0;
    accent-color: var(--iri);
  }
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  legend {
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
    margin-bottom: 4px;
    padding: 0;
  }
  .seg {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .seg label {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .seg label.sel {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 8%, transparent);
  }
  .seg input {
    margin: 0;
    accent-color: var(--iri);
  }
  .graphs {
    display: grid;
    gap: 4px;
    max-height: 180px;
    overflow: auto;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .gname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
