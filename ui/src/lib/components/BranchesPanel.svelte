<script lang="ts">
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import {
    aheadBehind,
    deleteWarning,
    fromLabel,
    MAIN,
    storageText,
    validBranchName,
  } from '$lib/branches';
  import { fmtCommitTime } from '$lib/commits';
  import { fmtInt, fmtRelative } from '$lib/format';
  import { normalizeAt, validAt } from '$lib/history';
  import Icon from './Icon.svelte';
  import MergeDialog from './MergeDialog.svelte';
  import Modal from './Modal.svelte';

  let {
    name,
    list,
    error = null,
    loading = false,
    current,
    canEdit = false,
    canMerge = false,
    onreload,
    onchange,
    onselect,
  }: {
    name: string;
    /** The dataset's branches, `main` first (null while loading). */
    list: api.Branch[] | null;
    error?: api.ApiError | Error | null;
    loading?: boolean;
    /** The branch the page shows. */
    current: string;
    /** Create, rename, protect and delete branches (admin on a writable server). */
    canEdit?: boolean;
    /** Merge branches (write access on a writable server). */
    canMerge?: boolean;
    onreload?: () => void;
    /** Called after a branch was created, changed, deleted or merged. */
    onchange?: () => void;
    /** Show another branch on the page. */
    onselect?: (branch: string) => void;
  } = $props();

  let busy = $state(false);
  let formOpen = $state(false);
  let form = $state({ name: '', from: MAIN, at: '', note: '', protected: false });
  const formName = $derived(form.name.trim());
  const nameOk = $derived(validBranchName(formName));
  const taken = $derived(!!list?.some((b) => b.name === formName));
  const atOk = $derived(validAt(form.at));

  function openForm() {
    formOpen = !formOpen;
    if (formOpen) form = { name: '', from: current, at: '', note: '', protected: false };
  }

  async function create(e: Event) {
    e.preventDefault();
    if (!nameOk || taken || !atOk) return;
    busy = true;
    try {
      const b = await api.createBranch(name, {
        name: formName,
        from: form.from,
        at: normalizeAt(form.at) ?? undefined,
        protected: form.protected || undefined,
        note: form.note.trim() || undefined,
      });
      toasts.push('success', `Created branch ${b.name}`, `from ${fromLabel(b)}`);
      formOpen = false;
      onchange?.();
    } catch (e) {
      toasts.error('Could not create the branch', e);
    } finally {
      busy = false;
    }
  }

  async function setProtected(b: api.Branch, on: boolean) {
    busy = true;
    try {
      await api.updateBranch(name, b.name, { protected: on });
      toasts.push(
        'success',
        on ? `${b.name} is protected` : `${b.name} is no longer protected`,
        on ? 'It takes changes through merges only.' : undefined,
      );
      onchange?.();
    } catch (e) {
      toasts.error(on ? 'Could not protect the branch' : 'Could not unprotect the branch', e);
    } finally {
      busy = false;
    }
  }

  let renaming = $state<{ branch: api.Branch; name: string; error: string | null } | null>(null);
  const renameName = $derived(renaming?.name.trim() ?? '');
  const renameOk = $derived(
    validBranchName(renameName) && !list?.some((b) => b.name === renameName),
  );

  async function rename() {
    if (!renaming || !renameOk) return;
    const r = renaming;
    busy = true;
    try {
      const b = await api.updateBranch(name, r.branch.name, { name: renameName });
      renaming = null;
      toasts.push('success', `Renamed ${r.branch.name} to ${b.name}`);
      if (b.grantsChanged)
        toasts.push(
          'info',
          'Review branch grants',
          `${b.grantsChanged} grants still refer to the old name. Update them to grant access to ${b.name}.`,
        );
      if (current === r.branch.name) onselect?.(b.name);
      onchange?.();
    } catch (e) {
      renaming = { ...r, error: api.errorMessage(e) };
    } finally {
      busy = false;
    }
  }

  // deleting: a branch with commits its upstream lacks needs a forced delete
  let deleting = $state<{
    branch: api.Branch;
    warning: string | null;
    error: string | null;
  } | null>(null);

  function askDelete(b: api.Branch) {
    deleting = { branch: b, warning: deleteWarning(b), error: null };
  }

  async function remove() {
    if (!deleting) return;
    const d = deleting;
    busy = true;
    try {
      await api.deleteBranch(name, d.branch.name, d.warning != null);
      toasts.push('success', `Deleted branch ${d.branch.name}`);
      deleting = null;
      if (current === d.branch.name) onselect?.(MAIN);
      onchange?.();
    } catch (e) {
      if (e instanceof api.ApiError && e.code === 'unmerged')
        deleting = { ...d, warning: api.errorMessage(e), error: null };
      else deleting = { ...d, error: api.errorMessage(e) };
    } finally {
      busy = false;
    }
  }

  /** The dataset page on a branch. */
  const branchHref = (b: string) => {
    const path = resolve('/datasets/[name]', { name });
    return b === MAIN ? path : `${path}?branch=${encodeURIComponent(b)}`;
  };

  let mergeOpen = $state(false);
  let mergeSource = $state('');
  function askMerge(b: api.Branch) {
    mergeSource = b.name;
    mergeOpen = true;
  }

  const unsupported = $derived(
    error instanceof api.ApiError && (error.status === 404 || error.status === 501) && !list,
  );
</script>

<section class="panel" aria-labelledby="branches-title">
  <div class="panel-head">
    <h2 id="branches-title">Branches</h2>
    {#if list}<span class="badge">{fmtInt(list.length)}</span>{/if}
    <span class="spacer"></span>
    {#if canEdit && list}
      <button class="btn sm" onclick={openForm} disabled={busy}>
        <Icon name={formOpen ? 'x' : 'plus'} size={13} />
        {formOpen ? 'Cancel' : 'New branch'}
      </button>
    {/if}
    <button
      class="btn ghost icon sm"
      title="Reload"
      aria-label="Reload branches"
      onclick={() => onreload?.()}
      disabled={loading}
    >
      {#if loading}<span class="spinner"></span>{:else}<Icon name="refresh" size={13} />{/if}
    </button>
  </div>

  {#if unsupported}
    <p class="faint pad">This dataset has no branches.</p>
  {:else if error && !list}
    <div class="pad">
      <div class="error-box">
        <strong>Could not load the branches.</strong>
        <span class="muted">{api.errorMessage(error)}</span>
      </div>
    </div>
  {:else if list}
    {#if formOpen}
      <form class="create" onsubmit={create} aria-label="New branch">
        <label>
          <span class="faint">Name</span>
          <input
            class="input sm mono"
            class:invalid={formName !== '' && (!nameOk || taken)}
            placeholder="dev"
            maxlength="64"
            required
            autocomplete="off"
            bind:value={form.name}
          />
        </label>
        <label>
          <span class="faint">From</span>
          <select class="select sm mono" bind:value={form.from}>
            {#each list as b (b.name)}<option value={b.name}>{b.name}</option>{/each}
          </select>
        </label>
        <label title="The head, a commit number, commit:N or snapshot:NAME of that branch">
          <span class="faint">At</span>
          <input
            class="input sm mono"
            class:invalid={!atOk}
            placeholder="head"
            size="12"
            bind:value={form.at}
          />
        </label>
        <label class="wide">
          <span class="faint">Note</span>
          <input class="input sm" maxlength="1024" bind:value={form.note} />
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={form.protected} /> Protected
        </label>
        <button class="btn sm primary" disabled={busy || !nameOk || taken || !atOk}>
          {#if busy}<span class="spinner"></span>{/if} Create
        </button>
        {#if formName && (!nameOk || taken)}
          <p class="hint">
            {taken
              ? `A branch named ${formName} exists.`
              : 'Use letters, digits, “.”, “_” or “-”, with at least one letter. head is reserved.'}
          </p>
        {/if}
      </form>
    {/if}

    <div class="list">
      <table class="data branches">
        <thead>
          <tr>
            <th>Branch</th>
            <th class="num">Head</th>
            <th title="The commit it started from, and its commits relative to that branch">
              From
            </th>
            <th title="Bytes only this branch uses, and bytes of other branches' files it keeps">
              Disk
            </th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each list as b (b.name)}
            <tr
              class:current={b.name === current}
              aria-current={b.name === current ? 'true' : undefined}
            >
              <td class="bname">
                <div class="row">
                  <a
                    class="mono"
                    href={branchHref(b.name)}
                    title={b.name === current ? 'The branch on show' : `Show ${b.name}`}
                    onclick={(e) => {
                      e.preventDefault();
                      onselect?.(b.name);
                    }}>{b.name}</a
                  >
                  {#if b.protected}
                    <span class="badge iri" title="Takes changes through merges only"
                      ><Icon name="lock" size={10} /> protected</span
                    >
                  {/if}
                  {#if b.broken}<span class="badge warn" title="Its files are damaged">broken</span
                    >{/if}
                </div>
                {#if b.note}<div class="note faint">{b.note}</div>{/if}
              </td>
              <td class="num mono" title="Made {fmtCommitTime(b.modified)}">
                {b.head}
                <div class="faint when">{fmtRelative(b.modified)}</div>
              </td>
              <td title={b.from ? `Created ${fmtCommitTime(b.created)}` : undefined}>
                {#if b.from}
                  <span class="mono">{fromLabel(b)}</span>
                  {#if b.upstream}<div class="faint">{aheadBehind(b)}</div>{/if}
                {:else}<span class="faint">—</span>{/if}
              </td>
              <td>
                {storageText(b)}
                {#if b.storage?.linked}
                  <div>
                    <span
                      class="badge"
                      title="Reads the index files of the branch it started from until its first compaction"
                      >linked</span
                    >
                  </div>
                {/if}
              </td>
              <td class="actions">
                {#if canMerge && b.name !== MAIN}
                  <button
                    class="btn ghost sm"
                    aria-label="Merge {b.name}"
                    title="Merge {b.name} into {b.upstream ?? MAIN}"
                    onclick={() => askMerge(b)}><Icon name="merge" size={12} /> Merge</button
                  >
                {/if}
                {#if canEdit}
                  <button
                    class="btn ghost icon sm"
                    aria-label="Protect {b.name}"
                    title={b.protected
                      ? 'Unprotect: accept updates, uploads and loads again'
                      : 'Protect: take changes through merges only'}
                    aria-pressed={b.protected}
                    disabled={busy}
                    onclick={() => setProtected(b, !b.protected)}
                    ><Icon name="lock" size={12} /></button
                  >
                  {#if b.name !== MAIN}
                    <button
                      class="btn ghost sm"
                      aria-label="Rename branch {b.name}"
                      disabled={busy}
                      onclick={() => (renaming = { branch: b, name: b.name, error: null })}
                      >Rename</button
                    >
                    <button
                      class="btn ghost icon sm"
                      aria-label="Delete branch {b.name}"
                      title="Delete"
                      disabled={busy}
                      onclick={() => askDelete(b)}><Icon name="trash" size={12} /></button
                    >
                  {/if}
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else}
    <div class="pad faint row"><span class="spinner"></span> Loading branches…</div>
  {/if}
</section>

<Modal
  open={renaming != null}
  title="Rename branch {renaming?.branch.name ?? ''}"
  onclose={() => (renaming = null)}
>
  {#if renaming}
    <label
      >New name <input
        class="input mono"
        bind:value={renaming.name}
        maxlength="64"
        autocomplete="off"
      /></label
    >
    <p class="faint">
      Branch grants refer to names. Review grants that use the old name after renaming.
    </p>
    {#if renameName && !renameOk}<p class="warn-text">Choose a valid, unused branch name.</p>{/if}
    {#if renaming.error}<div class="error-box">{renaming.error}</div>{/if}
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (renaming = null)}>Cancel</button>
    <button class="btn primary" onclick={rename} disabled={busy || !renameOk}>Rename</button>
  {/snippet}
</Modal>

<Modal
  open={deleting != null}
  title="Delete branch {deleting?.branch.name ?? ''}?"
  onclose={() => (deleting = null)}
>
  {#if deleting}
    {@const d = deleting}
    <div class="del">
      <p>
        This removes <strong class="mono">{d.branch.name}</strong>, its commits, its snapshots and
        its files. Commits that other branches merged stay in their history.
      </p>
      {#if d.warning}
        <p class="warn-text" role="alert"><Icon name="alert" size={14} /> {d.warning}</p>
      {/if}
      {#if d.error}<div class="error-box">{d.error}</div>{/if}
    </div>
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (deleting = null)}>Cancel</button>
    <button class="btn danger solid" onclick={remove} disabled={busy}>
      {#if busy}<span class="spinner"></span>{/if}
      {deleting?.warning ? 'Delete anyway' : `Delete ${deleting?.branch.name ?? ''}`}
    </button>
  {/snippet}
</Modal>

{#if list}
  <MergeDialog
    bind:open={mergeOpen}
    {name}
    source={mergeSource}
    branches={list}
    onmerged={() => onchange?.()}
  />
{/if}

<style>
  .pad {
    padding: 12px 14px;
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
    min-width: 0;
  }
  .create .wide {
    flex: 1 1 160px;
  }
  .create .check {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 26px;
  }
  .create .check input {
    accent-color: var(--iri);
    margin: 0;
  }
  .create input.invalid {
    border-color: var(--danger);
  }
  .create .select {
    max-width: 180px;
  }
  .hint {
    flex-basis: 100%;
    margin: 0;
    color: var(--danger);
  }
  .list {
    max-height: 360px;
    overflow: auto;
    border-top: 1px solid var(--border);
  }
  .branches {
    font-size: var(--fs-sm);
  }
  .branches td {
    white-space: nowrap;
    vertical-align: top;
  }
  .branches td.bname {
    white-space: normal;
  }
  .bname a {
    color: var(--text);
    text-decoration: none;
  }
  .bname a:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  tr.current .bname a {
    color: var(--iri);
    font-weight: 600;
  }
  .note,
  .when {
    font-size: 11px;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .actions {
    text-align: right;
    width: 1%;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .del {
    display: grid;
    gap: 10px;
  }
  .del p {
    margin: 0;
  }
  .warn-text {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    color: var(--warn);
  }
</style>
