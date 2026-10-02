<script lang="ts">
  import { goto } from '$app/navigation';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { fmtCommitTime } from '$lib/commits';
  import { fmtBytes, fmtInt, fmtRelative } from '$lib/format';
  import { expiresParam, normalizeAt, validAt, validSnapshotName } from '$lib/history';
  import { LatestRun } from '$lib/supersede';
  import Icon from './Icon.svelte';

  let {
    name,
    canEdit = false,
    refreshKey = 0,
    onchange,
  }: {
    name: string;
    /** Create and delete snapshots (admin on a writable server). */
    canEdit?: boolean;
    /** Bump to reload (after a write or compaction). */
    refreshKey?: number;
    /** Called after a snapshot was created or deleted. */
    onchange?: () => void;
  } = $props();

  let list = $state<api.SnapshotList | null>(null);
  let status = $state<api.HistoryStatus | null>(null);
  let error = $state<api.ApiError | Error | null>(null);
  let loading = $state(false);
  let busy = $state(false);
  let formOpen = $state(false);
  let form = $state({ name: '', at: '', note: '', expires: '' });
  const runs = new LatestRun();

  async function reload() {
    const owns = runs.claim('snapshots');
    loading = true;
    try {
      const [l, h] = await Promise.all([api.snapshots(name), api.history(name)]);
      if (!owns()) return;
      list = l;
      status = h;
      error = null;
    } catch (e) {
      if (owns()) error = e as Error;
    } finally {
      if (owns()) loading = false;
    }
  }

  $effect(() => {
    void name;
    void refreshKey;
    void reload();
  });

  const nameOk = $derived(validSnapshotName(form.name.trim()));
  const atOk = $derived(validAt(form.at));
  const expiresOk = $derived.by(() => {
    try {
      expiresParam(form.expires);
      return true;
    } catch {
      return false;
    }
  });
  const unsupported = $derived(error instanceof api.ApiError && error.status === 404 && !list);

  async function create(e: Event) {
    e.preventDefault();
    if (!nameOk || !atOk || !expiresOk) return;
    busy = true;
    try {
      const s = await api.createSnapshot(name, {
        name: form.name.trim(),
        at: normalizeAt(form.at) ?? undefined,
        note: form.note.trim() || undefined,
        expires: expiresParam(form.expires) ?? undefined,
      });
      toasts.push('success', `Snapshot ${s.name}`, `pins commit ${s.seq}`);
      form = { name: '', at: '', note: '', expires: '' };
      formOpen = false;
      await reload();
      onchange?.();
    } catch (e) {
      toasts.error('Could not create the snapshot', e);
    } finally {
      busy = false;
    }
  }

  async function remove(s: api.NamedSnapshot) {
    if (!confirm(`Delete snapshot ${s.name}? History only it keeps is removed.`)) return;
    busy = true;
    try {
      await api.deleteSnapshot(name, s.name);
      await reload();
      onchange?.();
    } catch (e) {
      toasts.error('Could not delete the snapshot', e);
    } finally {
      busy = false;
    }
  }

  function queryAt(ref: string) {
    app.setDataset(name);
    app.queryAt = ref;
    goto(resolve('/query'));
  }

  /** "0–57", "0–3, 5–57". */
  const ranges = $derived(
    (status?.reconstructable ?? [])
      .map((r) => (r.from === r.to ? `${r.from}` : `${r.from}–${r.to}`))
      .join(', '),
  );
  const retention = $derived.by(() => {
    const r = status?.retention;
    if (!r) return '';
    const parts: string[] = [];
    if (r.keepCommits != null) parts.push(`last ${fmtInt(r.keepCommits)} commits`);
    if (r.keepAge) parts.push(`last ${r.keepAge}`);
    if (r.maxBytes != null) parts.push(`at most ${fmtBytes(r.maxBytes)}`);
    return parts.length ? parts.join(', ') : 'off';
  });
</script>

<section class="panel">
  <div class="panel-head">
    <h2>Snapshots</h2>
    {#if list}<span class="badge">{fmtInt(list.snapshots.length)}</span>{/if}
    <span class="spacer"></span>
    {#if canEdit && list}
      <button class="btn sm" onclick={() => (formOpen = !formOpen)} disabled={busy}>
        <Icon name={formOpen ? 'x' : 'plus'} size={13} />
        {formOpen ? 'Cancel' : 'New snapshot'}
      </button>
    {/if}
    <button
      class="btn ghost icon sm"
      title="Reload"
      aria-label="Reload snapshots"
      onclick={reload}
      disabled={loading}
    >
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
    </button>
  </div>

  {#if unsupported}
    <p class="faint pad">This server does not keep named snapshots.</p>
  {:else if error && !list}
    <div class="pad">
      <div class="error-box">
        <strong>Could not load the snapshots.</strong>
        <span class="muted">{api.errorMessage(error)}</span>
      </div>
    </div>
  {:else if list}
    {#if status}
      <div class="panel-body summary">
        <span><span class="faint">Readable commits</span> <span class="mono">{ranges}</span></span>
        {#if status.bytes}
          <span><span class="faint">Kept history</span> {fmtBytes(status.bytes)}</span>
        {/if}
        <span><span class="faint">Retention</span> {retention}</span>
      </div>
    {/if}

    {#if formOpen}
      <form class="create" onsubmit={create}>
        <label>
          <span class="faint">Name</span>
          <input
            class="input sm mono"
            class:invalid={form.name !== '' && !nameOk}
            placeholder="release-1"
            maxlength="64"
            required
            bind:value={form.name}
          />
        </label>
        <label>
          <span class="faint">At</span>
          <input
            class="input sm mono"
            class:invalid={!atOk}
            placeholder="head"
            bind:value={form.at}
          />
        </label>
        <label>
          <span class="faint">Expires</span>
          <input
            class="input sm"
            class:invalid={!expiresOk}
            placeholder="never (or 7d, 12h)"
            bind:value={form.expires}
          />
        </label>
        <label class="wide">
          <span class="faint">Note</span>
          <input class="input sm" maxlength="1024" bind:value={form.note} />
        </label>
        <button class="btn sm primary" disabled={busy || !nameOk || !atOk || !expiresOk}>
          {#if busy}<span class="spinner"></span>{/if} Create
        </button>
      </form>
    {/if}

    {#if list.snapshots.length === 0}
      <p class="faint pad">
        No named snapshots. A snapshot keeps a commit readable after compaction.
      </p>
    {:else}
      <div class="list">
        <table class="data snaps">
          <thead>
            <tr>
              <th>Name</th>
              <th class="num">Commit</th>
              <th>Created</th>
              <th>Note</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each list.snapshots as s (s.name)}
              <tr class:gone={!s.reconstructable}>
                <td class="mono name">
                  {s.name}
                  {#if !s.reconstructable}
                    <span class="badge warn" title="Its data is gone (external damage)"
                      >unreadable</span
                    >
                  {/if}
                  {#if s.expires}
                    <span class="badge" title="Removed at {fmtCommitTime(s.expires)}"
                      >expires {fmtRelative(s.expires)}</span
                    >
                  {/if}
                </td>
                <td class="num mono">{s.seq}</td>
                <td title={s.created}>{fmtRelative(s.created)}</td>
                <td class="note">{s.note ?? ''}</td>
                <td class="actions">
                  <button
                    class="btn ghost sm"
                    title="Query the dataset at this snapshot"
                    disabled={!s.reconstructable}
                    onclick={() => queryAt(s.ref)}><Icon name="query" size={12} /> Query</button
                  >
                  {#if canEdit}
                    <button
                      class="btn ghost icon sm"
                      aria-label="Delete snapshot {s.name}"
                      title="Delete"
                      disabled={busy}
                      onclick={() => remove(s)}><Icon name="trash" size={12} /></button
                    >
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {:else}
    <div class="pad faint row"><span class="spinner"></span> Loading snapshots…</div>
  {/if}
</section>

<style>
  .pad {
    padding: 12px 14px;
    font-size: var(--fs-sm);
  }
  .summary {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 18px;
    font-size: var(--fs-sm);
  }
  .create {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 8px 12px;
    padding: 10px 14px;
    border-top: 1px solid var(--border);
    font-size: var(--fs-sm);
  }
  .create label {
    display: grid;
    gap: 3px;
  }
  .create .wide {
    flex: 1 1 180px;
  }
  .create input.invalid {
    border-color: var(--danger);
  }
  .list {
    max-height: 320px;
    overflow: auto;
    border-top: 1px solid var(--border);
  }
  .snaps {
    font-size: var(--fs-sm);
  }
  .snaps td {
    white-space: nowrap;
  }
  .snaps td.note {
    white-space: normal;
    color: var(--text-2);
  }
  .name .badge {
    margin-left: 6px;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  tr.gone .name {
    color: var(--text-2);
  }
  .actions {
    text-align: right;
    width: 1%;
  }
</style>
