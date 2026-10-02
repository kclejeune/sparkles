<script lang="ts">
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    source,
    hasInferences,
    onstarted,
  }: {
    open?: boolean;
    /** The dataset to clone. */
    source: string;
    /** Offer the "Include inferences" choice. */
    hasInferences: boolean;
    onstarted?: (t: api.Task) => void;
  } = $props();

  let name = $state('');
  let inferences = $state(true);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (open) {
      name = `${source}-sandbox`;
      inferences = true;
      error = null;
    }
  });

  const validName = $derived(/^[A-Za-z0-9_-][A-Za-z0-9_.-]*$/.test(name) && name.length <= 64);
  const exists = $derived(app.datasets.some((d) => d.name === name));

  async function submit(e: Event) {
    e.preventDefault();
    if (!validName || exists) return;
    busy = true;
    error = null;
    try {
      const t = await api.cloneDataset(
        source,
        name,
        hasInferences && !inferences ? 'drop' : 'copy',
      );
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
      Copies the current data into a new, independent persistent dataset. Changes to the copy never
      affect <span class="mono">{source}</span>.
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
      disabled={busy || !validName || exists}
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
</style>
