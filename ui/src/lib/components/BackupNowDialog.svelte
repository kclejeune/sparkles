<script lang="ts">
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import * as b from '$lib/backups';
  import { BACKUP_NAME, fmtTimeToken } from '$lib/backups-format';
  import Modal from './Modal.svelte';

  let {
    dataset = $bindable(null),
    repositories = [],
    onstarted,
  }: {
    /** The dataset to back up; null closes the dialog. */
    dataset?: string | null;
    repositories?: (b.Repository | b.RepositoryBrief)[];
    onstarted?: (t: api.Task) => void;
  } = $props();

  let repository = $state('');
  let name = $state('');
  let note = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  const usable = (r: b.Repository | b.RepositoryBrief) => !b.isReadonly(r) && b.reachable(r);
  const why = (r: b.Repository | b.RepositoryBrief) =>
    b.isReadonly(r) ? 'read-only' : !b.reachable(r) ? 'unreachable' : '';

  $effect(() => {
    if (!dataset) return;
    untrack(() => {
      repository = repositories.find(usable)?.name ?? '';
      name = '';
      note = '';
      error = null;
    });
  });

  const placeholder = $derived(`${dataset ?? 'dataset'}-${fmtTimeToken(new Date())}`);
  const nameOk = $derived(!name || BACKUP_NAME.test(name));

  async function submit(e: Event) {
    e.preventDefault();
    if (!dataset || !repository || !nameOk) return;
    busy = true;
    error = null;
    try {
      const t = await b.createBackup(dataset, {
        repository,
        ...(name ? { name } : {}),
        ...(note.trim() ? { note: note.trim() } : {}),
      });
      toasts.push('info', `Backing up ${dataset} into ${repository}`, `Task ${t.id}`);
      onstarted?.(t);
      dataset = null;
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }
</script>

<Modal open={dataset != null} title="Back up {dataset ?? ''}" onclose={() => (dataset = null)}>
  <form id="backup-now" class="form" onsubmit={submit}>
    <p class="faint intro">
      Copies the dataset at its current commit into a repository. Writes go on meanwhile; only files
      the repository lacks are uploaded.
    </p>
    <label class="field">
      Repository
      <select class="select" bind:value={repository} required>
        {#each repositories as r (r.name)}
          <option value={r.name} disabled={!usable(r)}
            >{r.name} ({r.type}){why(r) ? ` · ${why(r)}` : ''}</option
          >
        {/each}
      </select>
      {#if !repositories.some(usable)}
        <span class="hint bad">No writable, reachable repository. A server admin adds them.</span>
      {/if}
    </label>
    <label class="field">
      <span>Name <span class="faint">(optional)</span></span>
      <input
        class="input mono"
        bind:value={name}
        {placeholder}
        autocomplete="off"
        spellcheck="false"
        aria-invalid={!nameOk}
      />
      <span class="hint" class:bad={!nameOk}
        >{nameOk
          ? 'Unique within the repository; defaults to the dataset and the time.'
          : 'Up to 64 letters, digits, “.”, “_” or “-”, starting with a letter or digit.'}</span
      >
    </label>
    <label class="field">
      <span>Note <span class="faint">(optional)</span></span>
      <input class="input" bind:value={note} placeholder="before the schema migration" />
    </label>
    {#if error}<div class="error-box">{error}</div>{/if}
  </form>
  {#snippet actions()}
    <button class="btn" type="button" onclick={() => (dataset = null)}>Cancel</button>
    <button
      class="btn primary"
      type="submit"
      form="backup-now"
      disabled={busy || !repository || !nameOk}
    >
      {#if busy}<span class="spinner"></span>{/if} Back up
    </button>
  {/snippet}
</Modal>

<style>
  .form {
    display: grid;
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
</style>
