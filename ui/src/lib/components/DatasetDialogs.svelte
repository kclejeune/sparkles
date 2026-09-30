<script lang="ts">
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import Modal from './Modal.svelte';

  let {
    createOpen = $bindable(false),
    deleteTarget = $bindable(null),
    oncreated,
    ondeleted,
  }: {
    createOpen?: boolean;
    deleteTarget?: string | null;
    oncreated?: (name: string) => void;
    ondeleted?: (name: string) => void;
  } = $props();

  let name = $state('');
  let type = $state<api.DatasetType>('persistent');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let confirmText = $state('');

  const validName = $derived(/^[A-Za-z0-9_.-]+$/.test(name));
  const exists = $derived(app.datasets.some((d) => d.name === name));

  async function create(e: Event) {
    e.preventDefault();
    if (!validName || exists) return;
    busy = true;
    error = null;
    try {
      await api.createDataset(name, type);
      toasts.push('success', `Created dataset ${name}`);
      await app.refreshDatasets();
      oncreated?.(name);
      createOpen = false;
      name = '';
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (!deleteTarget) return;
    const target = deleteTarget;
    busy = true;
    error = null;
    try {
      await api.deleteDataset(target);
      toasts.push('success', `Deleted dataset ${target}`);
      if (app.current === target) app.setDataset(null);
      await app.refreshDatasets();
      deleteTarget = null;
      ondeleted?.(target);
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal bind:open={createOpen} title="New dataset" onclose={() => (error = null)}>
  <form id="create-ds" onsubmit={create} class="form">
    <label class="field">
      Name
      <input
        class="input mono"
        bind:value={name}
        placeholder="my-dataset"
        autocomplete="off"
        required
        {@attach (el) => el.focus()}
      />
      <span class="hint" class:bad={name && (!validName || exists)}>
        {#if name && !validName}Use letters, digits, “_”, “-” or “.”.{:else if exists}A dataset with
          this name already exists.{:else}Served at <span class="mono"
            >/{name || 'name'}/sparql</span
          >{/if}
      </span>
    </label>
    <fieldset class="types">
      <legend>Storage</legend>
      <label class="opt" class:sel={type === 'persistent'}>
        <input type="radio" bind:group={type} value="persistent" />
        <span
          ><strong>Persistent</strong><span class="muted">Indexed on disk. Survives restarts.</span
          ></span
        >
      </label>
      <label class="opt" class:sel={type === 'mem'}>
        <input type="radio" bind:group={type} value="mem" />
        <span
          ><strong>In-memory</strong><span class="muted">Fast scratch space. Gone on restart.</span
          ></span
        >
      </label>
    </fieldset>
    {#if error}<div class="error-box">{error}</div>{/if}
  </form>
  {#snippet actions()}
    <button class="btn" onclick={() => (createOpen = false)}>Cancel</button>
    <button
      class="btn primary"
      type="submit"
      form="create-ds"
      disabled={busy || !validName || exists}
    >
      {#if busy}<span class="spinner"></span>{/if} Create dataset
    </button>
  {/snippet}
</Modal>

<Modal
  open={deleteTarget != null}
  title="Delete dataset?"
  onclose={() => {
    deleteTarget = null;
    confirmText = '';
    error = null;
  }}
>
  <p>
    This permanently removes <strong class="mono">{deleteTarget}</strong> and all of its files.
    Queries against
    <span class="mono">/{deleteTarget}</span> will return 404.
  </p>
  <label class="field">
    Type the dataset name to confirm
    <input
      class="input mono"
      bind:value={confirmText}
      placeholder={deleteTarget ?? ''}
      autocomplete="off"
    />
  </label>
  {#if error}<div class="error-box">{error}</div>{/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (deleteTarget = null)}>Cancel</button>
    <button
      class="btn danger solid"
      onclick={remove}
      disabled={busy || confirmText !== deleteTarget}
    >
      {#if busy}<span class="spinner"></span>{/if} Delete {deleteTarget}
    </button>
  {/snippet}
</Modal>

<style>
  .form {
    display: grid;
    gap: 14px;
  }
  .hint {
    font-weight: 400;
    color: var(--text-3);
  }
  .hint.bad {
    color: var(--danger);
  }
  .types {
    border: 0;
    padding: 0;
    margin: 0;
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .types legend {
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 500;
    margin-bottom: 4px;
    padding: 0;
  }
  .opt {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    cursor: pointer;
  }
  .opt.sel {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 6%, transparent);
  }
  .opt > span {
    display: grid;
    gap: 2px;
    font-size: var(--fs-sm);
  }
  .opt input {
    margin-top: 2px;
    accent-color: var(--iri);
  }
</style>
