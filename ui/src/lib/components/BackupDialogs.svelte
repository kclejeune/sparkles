<script lang="ts">
  // The dialogs of a backup row: details drawer, restore, verify and delete (typed
  // confirmation). Pages call `show(kind, backup)` (bind:this) and get `onchanged`.
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import * as b from '$lib/backups';
  import BackupDrawer from './BackupDrawer.svelte';
  import Modal from './Modal.svelte';
  import RestoreDialog from './RestoreDialog.svelte';
  import VerifyDialog from './VerifyDialog.svelte';

  let {
    backups = [],
    readonlyRepos = [],
    readOnly = false,
    onchanged,
  }: {
    /** The backups listed next to the dialogs (restore offers the same dataset's). */
    backups?: b.BackupSummary[];
    /** Repositories that refuse deletes. */
    readonlyRepos?: string[];
    /** The server is read-only. */
    readOnly?: boolean;
    onchanged?: () => void;
  } = $props();

  let drawer = $state<b.BackupSummary | null>(null);
  let restore = $state<b.BackupSummary | null>(null);
  let verify = $state<b.VerifyTarget | null>(null);
  let del = $state<b.BackupSummary | null>(null);
  let confirmText = $state('');
  let deleting = $state(false);
  let error = $state<string | null>(null);

  /** Opens the details drawer or one of the dialogs for a backup. */
  export function show(kind: 'details' | 'restore' | 'verify' | 'delete', bk: b.BackupSummary) {
    if (kind === 'details') drawer = bk;
    else if (kind === 'restore') restore = bk;
    else if (kind === 'verify') verify = { kind: 'backup', backup: bk };
    else {
      del = bk;
      confirmText = '';
      error = null;
    }
  }

  const canAdmin = (bk: b.BackupSummary | null) => !!bk && auth.can(bk.dataset.name, 'admin');
  const canDelete = (bk: b.BackupSummary | null) =>
    canAdmin(bk) && !!bk && !readonlyRepos.includes(bk.repository);

  const siblings = $derived(
    restore
      ? backups.filter(
          (x) => x.dataset.id === restore?.dataset.id || x.dataset.name === restore?.dataset.name,
        )
      : [],
  );

  async function remove() {
    if (!del) return;
    const target = del;
    deleting = true;
    error = null;
    try {
      await b.deleteBackup(...b.pathOf(target));
      toasts.push(
        'success',
        `Deleted ${target.name}`,
        'Its blobs are removed by the next garbage collection.',
      );
      del = null;
      onchanged?.();
    } catch (e) {
      error = api.errorMessage(e);
    } finally {
      deleting = false;
    }
  }
</script>

<BackupDrawer
  bind:backup={drawer}
  canAdmin={canAdmin(drawer)}
  canDelete={canDelete(drawer)}
  onaction={show}
/>
<RestoreDialog
  bind:backup={restore}
  choices={siblings}
  {readOnly}
  onfinished={() => onchanged?.()}
/>
<VerifyDialog bind:target={verify} onfinished={() => onchanged?.()} />

<Modal open={del != null} title="Delete backup?" onclose={() => (del = null)}>
  {#if del}
    <p>
      This deletes the manifest of <strong class="mono">{del.name}</strong> (commit {del.commit.seq} of
      {del.dataset.name}) from <span class="mono">{del.repository}</span>. Other backups stay
      complete; blobs only this one used are removed by the next garbage collection.
    </p>
    <label class="field">
      Type the backup name to confirm
      <input
        class="input mono"
        bind:value={confirmText}
        placeholder={del.name}
        autocomplete="off"
        spellcheck="false"
      />
    </label>
    {#if error}<div class="error-box">{error}</div>{/if}
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (del = null)}>Cancel</button>
    <button
      class="btn danger solid"
      onclick={remove}
      disabled={deleting || !del || confirmText !== del.name}
    >
      {#if deleting}<span class="spinner"></span>{/if} Delete backup
    </button>
  {/snippet}
</Modal>
