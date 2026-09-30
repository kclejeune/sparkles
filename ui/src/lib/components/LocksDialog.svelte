<script lang="ts">
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import * as b from '$lib/backups';
  import { fmtRelative, fmtTime } from '$lib/format';
  import Modal from './Modal.svelte';

  let {
    repository = $bindable(null),
  }: {
    /** The repository whose locks to show; null closes the dialog. */
    repository?: string | null;
  } = $props();

  let locks = $state<b.LockInfo[] | null>(null);
  let error = $state<string | null>(null);
  let breaking = $state<string | null>(null);

  async function load() {
    if (!repository) return;
    try {
      locks = await b.listLocks(repository);
      error = null;
    } catch (e) {
      error = api.errorMessage(e);
    }
  }

  $effect(() => {
    if (!repository) return;
    locks = null;
    void load();
  });

  async function breakIt(l: b.LockInfo) {
    if (!repository) return;
    const warn = l.stale
      ? ''
      : '\n\nIt is not stale: its holder may still be working, and breaking it can let a garbage collection delete blobs that holder needs.';
    if (!confirm(`Break the ${l.kind} ${l.operation} lock held by ${l.holder.host}?${warn}`))
      return;
    breaking = l.id;
    try {
      await b.breakLock(repository, l.id);
      toasts.push('success', 'Lock broken', l.id);
      await load();
    } catch (e) {
      toasts.error('Could not break the lock', e);
    } finally {
      breaking = null;
    }
  }
</script>

<Modal
  open={repository != null}
  title="Locks: {repository ?? ''}"
  width={640}
  onclose={() => (repository = null)}
>
  <p class="small muted">
    Backups, restores and verifies hold shared locks; garbage collection holds an exclusive one
    while it deletes. A lock not refreshed for 30 minutes is stale and ignored.
  </p>
  {#if error}
    <div class="error-box">{error}</div>
  {:else if !locks}
    <p class="faint small"><span class="spinner"></span> Loading…</p>
  {:else if locks.length === 0}
    <p class="faint small">No locks: nothing is working on this repository.</p>
  {:else}
    <table class="data">
      <thead>
        <tr><th>Kind</th><th>Operation</th><th>Holder</th><th>Refreshed</th><th></th></tr>
      </thead>
      <tbody>
        {#each locks as l (l.id)}
          <tr>
            <td>
              <span class="badge {l.kind === 'exclusive' ? 'spark' : ''}">{l.kind}</span>
              {#if l.stale}<span class="badge danger">stale</span>{/if}
            </td>
            <td>{l.operation}</td>
            <td class="small">
              <span class="mono">{l.holder.host}</span>
              <span class="faint">pid {l.holder.pid} · v{l.holder.version}</span>
            </td>
            <td class="small" title="Created {fmtTime(l.created)}">{fmtRelative(l.lastModified)}</td
            >
            <td class="actions">
              <button
                class="btn sm danger"
                onclick={() => breakIt(l)}
                disabled={breaking === l.id}
                aria-label="Break lock {l.id}">Break</button
              >
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={load}>Refresh</button>
    <button class="btn primary" onclick={() => (repository = null)}>Close</button>
  {/snippet}
</Modal>

<style>
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .actions {
    text-align: right;
  }
</style>
