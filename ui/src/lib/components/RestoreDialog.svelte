<script lang="ts">
  import { untrack } from 'svelte';
  import { resolve } from '$app/paths';
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import { auth } from '$lib/auth.svelte';
  import * as b from '$lib/backups';
  import { DATASET_NAME, defaultRestoreName, fmtCommitAge, restoreLoss } from '$lib/backups-format';
  import { fmtBytes, fmtInt, fmtTime } from '$lib/format';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import TaskProgress from './TaskProgress.svelte';

  let {
    backup = $bindable(null),
    choices = [],
    readOnly = false,
    onfinished,
  }: {
    /** The backup to restore (preselected); null closes the dialog. */
    backup?: b.BackupSummary | null;
    /** Other backups offered in the first step (the dataset's). */
    choices?: b.BackupSummary[];
    /** The server is read-only: restores are refused. */
    readOnly?: boolean;
    onfinished?: (t: api.Task) => void;
  } = $props();

  const STEPS = ['Backup', 'Target', 'Options', 'Restore'];

  let step = $state(0);
  let chosen = $state<b.BackupSummary | null>(null);
  let mode = $state<'new' | 'replace'>('new');
  let target = $state('');
  let confirmText = $state('');
  let identity = $state<b.Identity>('auto');
  let check = $state<b.RestoreCheck>('quick');
  let keepReplaced = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let task = $state<api.Task | null>(null);
  let result = $state<api.Task | null>(null);

  $effect(() => {
    const bk = backup;
    if (bk) untrack(() => reset(bk));
  });

  /** A fresh dialog for `bk` (not rerun when the dataset list refreshes). */
  function reset(bk: b.BackupSummary) {
    chosen = bk;
    step = 0;
    mode = 'new';
    // disaster recovery: a backup of a dataset this server lacks restores under its name
    const names = app.datasets.map((d) => d.name);
    target = names.includes(bk.dataset.name)
      ? defaultRestoreName(bk.dataset.name, names)
      : bk.dataset.name;
    confirmText = '';
    identity = 'auto';
    check = 'quick';
    keepReplaced = false;
    error = null;
    task = null;
    result = null;
  }

  const cur = $derived(chosen ?? backup);
  const options = $derived(
    cur && !choices.some((c) => c.name === cur.name && c.repository === cur.repository)
      ? [cur, ...choices]
      : choices,
  );
  const live = $derived(cur ? app.datasets.find((d) => d.name === cur.dataset.name) : undefined);
  const sameLineage = $derived(!!cur && !!live && live.id === cur.dataset.id);
  const canReplace = $derived(!!live && live.type === 'persistent' && auth.can(live.name, 'admin'));
  const replaceWhy = $derived(
    !live
      ? `No dataset named ${cur?.dataset.name} on this server.`
      : live.type !== 'persistent'
        ? 'In-memory datasets cannot be replaced.'
        : !auth.can(live.name, 'admin')
          ? `Replacing needs admin on ${live.name}.`
          : '',
  );
  const loss = $derived(cur && live ? restoreLoss(live.head, cur.commit.seq, sameLineage) : null);
  const finalTarget = $derived(mode === 'replace' ? (live?.name ?? '') : target);

  const targetTaken = $derived(app.datasets.some((d) => d.name === target));
  const targetOk = $derived(
    mode === 'replace'
      ? canReplace && confirmText === live?.name
      : DATASET_NAME.test(target) && !targetTaken,
  );

  // the identity rule, as the server applies it
  const idElsewhere = $derived(
    !!cur &&
      app.datasets.some(
        (d) => d.id === cur.dataset.id && !(mode === 'replace' && d.name === live?.name),
      ),
  );
  const replacesSource = $derived(mode === 'replace' && sameLineage);
  const autoKeeps = $derived(!idElsewhere && !replacesSource);
  const keepWhy = $derived(
    idElsewhere
      ? 'Another dataset on this server has this id.'
      : replacesSource && live?.head != null && cur && live.head > cur.commit.seq
        ? `${live.name} has commits after ${cur.commit.seq}; keeping the id would issue their numbers again.`
        : '',
  );
  $effect(() => {
    if (keepWhy && identity === 'keep') identity = 'auto';
  });

  const canNext = $derived(step === 0 ? !!cur : step === 1 ? targetOk : true);

  async function submit() {
    if (!cur) return;
    busy = true;
    error = null;
    try {
      task = await b.restoreBackup(...b.pathOf(cur), {
        target: finalTarget,
        replace: mode === 'replace',
        identity,
        check,
        ...(mode === 'replace' ? { keepReplaced } : {}),
      });
      step = 3;
    } catch (e) {
      error = api.errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function finished(t: api.Task) {
    result = t;
    if (t.state === 'done') {
      toasts.push('success', `Restored ${cur?.name} into /${t.target ?? finalTarget}`);
      void app.refreshDatasets();
    }
    onfinished?.(t);
  }

  const pick = (key: string) => {
    chosen = options.find((o) => `${o.repository}/${o.name}` === key) ?? chosen;
  };
</script>

<Modal
  open={backup != null}
  title="Restore {cur?.dataset.name ?? ''}"
  width={600}
  onclose={() => (backup = null)}
>
  <ol class="stepper" aria-label="Restore steps">
    {#each STEPS as s, i (s)}
      <li class:done={i < step} aria-current={i === step ? 'step' : undefined}>
        <span class="n">{i < step ? '✓' : i + 1}</span><span class="step">{s}</span>
      </li>
    {/each}
  </ol>

  {#if cur}
    {#if step === 0}
      {#if options.length > 1}
        <label class="field">
          Backup
          <select
            class="select"
            value="{cur.repository}/{cur.name}"
            onchange={(e) => pick(e.currentTarget.value)}
          >
            {#each options as o (`${o.repository}/${o.name}`)}
              <option value="{o.repository}/{o.name}"
                >{o.name} · {o.repository} · commit {o.commit.seq}</option
              >
            {/each}
          </select>
        </label>
      {/if}
      <dl class="kv">
        <dt>Backup</dt>
        <dd><span class="mono">{cur.name}</span> <span class="faint">in {cur.repository}</span></dd>
        <dt>Dataset</dt>
        <dd>
          <span class="mono">{cur.dataset.name}</span>
          {#if live && !sameLineage}<span
              class="badge warn"
              title="The backup's dataset id differs from the live dataset's">other lineage</span
            >{:else if !live}<span class="badge">not on this server</span>{/if}
        </dd>
        <dt>Commit</dt>
        <dd>
          {fmtCommitAge(cur.commit)} <span class="faint">· {fmtInt(cur.commit.quads)} quads</span>
        </dd>
        <dt>Size</dt>
        <dd>{fmtBytes(cur.logicalBytes)} <span class="faint">to download</span></dd>
        <dt>Completed</dt>
        <dd>{fmtTime(cur.completed)}</dd>
        {#if cur.note}
          <dt>Note</dt>
          <dd>{cur.note}</dd>
        {/if}
      </dl>
      {#if cur.verified?.status === 'error'}
        <p class="note bad">
          <Icon name="alert" size={13} /> Its last verify ({cur.verified.level}) found errors. The
          restore checks every blob and fails cleanly if one is damaged.
        </p>
      {/if}
    {:else if step === 1}
      <fieldset class="modes">
        <legend>Restore into</legend>
        <label class="opt" class:sel={mode === 'new'}>
          <input type="radio" bind:group={mode} value="new" />
          <span
            ><strong>A new dataset</strong><span class="muted"
              >Leaves everything else untouched. Also a way to clone from a backup.</span
            ></span
          >
        </label>
        <label class="opt" class:sel={mode === 'replace'} class:off={!canReplace}>
          <input type="radio" bind:group={mode} value="replace" disabled={!canReplace} />
          <span
            ><strong>Replace <span class="mono">{cur.dataset.name}</span></strong><span
              class="muted"
              >{canReplace
                ? 'Swaps the dataset for the restored copy. Requests to it wait for a few seconds.'
                : replaceWhy}</span
            ></span
          >
        </label>
      </fieldset>
      {#if mode === 'new'}
        <label class="field">
          Name
          <input
            class="input mono"
            bind:value={target}
            autocomplete="off"
            spellcheck="false"
            aria-invalid={!!target && !targetOk}
          />
          <span class="hint" class:bad={!!target && !targetOk}>
            {#if target && !DATASET_NAME.test(target)}Use letters, digits, “_”, “-” or “.” (not
              first), up to 64.{:else if targetTaken}A dataset with this name already exists.{:else}Served
              at <span class="mono">/{target || 'name'}/sparql</span>{/if}
          </span>
        </label>
      {:else if live}
        <div class="compare">
          <div>
            <span class="faint small">Now</span>
            <strong>commit {live.head ?? '?'}</strong>
            <span class="faint small">{fmtInt(live.quads)} quads</span>
          </div>
          <Icon name="chevron" size={14} />
          <div>
            <span class="faint small">After the restore</span>
            <strong>commit {cur.commit.seq}</strong>
            <span class="faint small">{fmtInt(cur.commit.quads)} quads</span>
          </div>
        </div>
        {#if loss}
          <p class="note" class:bad={loss.lost > 0}>
            <Icon name={loss.lost > 0 ? 'alert' : 'info'} size={13} />
            {loss.text}.
          </p>
        {/if}
        <label class="field">
          <span>Type <span class="mono">{live.name}</span> to confirm</span>
          <input
            class="input mono"
            bind:value={confirmText}
            placeholder={live.name}
            autocomplete="off"
            spellcheck="false"
          />
        </label>
      {/if}
    {:else if step === 2}
      <fieldset class="modes">
        <legend>Dataset id</legend>
        <label class="opt" class:sel={identity === 'auto'}>
          <input type="radio" bind:group={identity} value="auto" />
          <span
            ><strong>Automatic</strong><span class="muted"
              >Keeps the backup's id unless another dataset here has it (such as its live source);
              then it gets a new one. Here: <strong
                >{autoKeeps ? 'keeps the id' : 'a new id'}</strong
              >.</span
            ></span
          >
        </label>
        <label class="opt" class:sel={identity === 'new'}>
          <input type="radio" bind:group={identity} value="new" />
          <span
            ><strong>New id</strong><span class="muted"
              >A new lineage that records the backup as where it forked from. Its commit numbers
              continue after {cur.commit.seq}.</span
            ></span
          >
        </label>
        <label class="opt" class:sel={identity === 'keep'} class:off={!!keepWhy}>
          <input type="radio" bind:group={identity} value="keep" disabled={!!keepWhy} />
          <span
            ><strong>Keep the id</strong><span class="muted"
              >{keepWhy ||
                'For disaster recovery: the restored dataset continues the lineage.'}</span
            ></span
          >
        </label>
      </fieldset>
      <fieldset class="inline">
        <legend>Check after restoring</legend>
        <div class="seg" role="radiogroup" aria-label="Check after restoring">
          {#each [['quick', 'Quick'], ['full', 'Full'], ['none', 'Skip']] as [v, label] (v)}
            <label class:sel={check === v}
              ><input type="radio" bind:group={check} value={v} />{label}</label
            >
          {/each}
        </div>
        <span class="hint"
          >{check === 'full'
            ? 'Reads every index file: slower, for suspect storage.'
            : check === 'quick'
              ? 'Checks the structure and the commit catalog. Every blob is hashed either way.'
              : 'Only the blob hashes and the head commit are checked.'}</span
        >
      </fieldset>
      {#if mode === 'replace'}
        <label class="check">
          <input type="checkbox" bind:checked={keepReplaced} /> Keep the replaced copy
          <span class="faint"
            >(as <span class="mono">.replaced-{live?.name}-…</span> in the data directory)</span
          >
        </label>
      {/if}
      <p class="summary">
        Restore <span class="mono">{cur.name}</span> (commit {cur.commit.seq}) {mode === 'replace'
          ? 'in place of'
          : 'into a new dataset'}
        <span class="mono">/{finalTarget}</span>.
      </p>
      {#if readOnly}<p class="note bad">The server is read-only: it refuses restores.</p>{/if}
      {#if error}<div class="error-box">{error}</div>{/if}
    {:else if task}
      {#key task.id}
        <TaskProgress {task} onfinish={finished} />
      {/key}
      {#if result?.state === 'done'}
        <p class="done">
          <Icon name="check" size={14} />
          <span>/{result.target ?? finalTarget} is ready.</span>
          <a
            class="btn sm"
            href={resolve('/datasets/[name]', { name: result.target ?? finalTarget })}
            onclick={() => (backup = null)}>Open</a
          >
        </p>
      {/if}
    {/if}
  {/if}

  {#snippet actions()}
    {#if step === 3}
      <button class="btn primary" onclick={() => (backup = null)}>Close</button>
    {:else}
      <button class="btn" onclick={() => (step > 0 ? step-- : (backup = null))}
        >{step > 0 ? 'Back' : 'Cancel'}</button
      >
      {#if step < 2}
        <button class="btn primary" onclick={() => step++} disabled={!canNext}>Next</button>
      {:else}
        <button
          class="btn primary"
          class:danger={mode === 'replace'}
          class:solid={mode === 'replace'}
          onclick={submit}
          disabled={busy || readOnly || !targetOk}
        >
          {#if busy}<span class="spinner"></span>{/if}
          {mode === 'replace' ? `Replace ${live?.name}` : 'Restore'}
        </button>
      {/if}
    {/if}
  {/snippet}
</Modal>

<style>
  .stepper {
    list-style: none;
    display: flex;
    gap: 4px;
    margin: 0;
    padding: 0 0 4px;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .stepper li {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
    padding-bottom: 6px;
    border-bottom: 2px solid var(--border);
  }
  .stepper li[aria-current='step'] {
    color: var(--text);
    border-bottom-color: var(--spark);
    font-weight: 600;
  }
  .stepper li.done {
    color: var(--text-2);
  }
  /* a phone: only the current step is named, the others are numbered */
  @media (max-width: 480px) {
    .stepper li:not([aria-current='step']) .step {
      display: none;
    }
  }
  .n {
    display: inline-grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--surface-3);
    font-size: var(--fs-xs);
  }
  [aria-current='step'] .n {
    background: var(--spark);
    color: var(--on-spark);
  }
  .kv {
    display: grid;
    grid-template-columns: 100px 1fr;
    gap: 6px 12px;
    margin: 0;
  }
  .kv dt {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .kv dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 6px;
  }
  legend {
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
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    cursor: pointer;
  }
  .opt.sel {
    border-color: var(--iri);
    background: color-mix(in srgb, var(--iri) 6%, transparent);
  }
  .opt.off {
    cursor: not-allowed;
    opacity: 0.7;
  }
  .opt > span {
    display: grid;
    gap: 2px;
    font-size: var(--fs-sm);
  }
  .opt input,
  .check input,
  .seg input {
    margin: 2px 0 0;
    accent-color: var(--iri);
  }
  .seg {
    display: flex;
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
    flex-wrap: wrap;
    font-size: var(--fs-sm);
  }
  .compare {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .compare div {
    display: grid;
  }
  .note {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .note.bad {
    color: var(--danger);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .summary {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .done {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    color: var(--ok);
  }
  .done span {
    color: var(--text);
  }
  a.btn {
    text-decoration: none;
  }
</style>
