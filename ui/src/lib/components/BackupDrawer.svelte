<script lang="ts">
  import * as api from '$lib/api';
  import * as b from '$lib/backups';
  import { fmtDedup } from '$lib/backups-format';
  import { fmtBytes, fmtInt, fmtMs, fmtRelative, fmtTime } from '$lib/format';
  import Icon from './Icon.svelte';

  let {
    backup = $bindable(null),
    canAdmin = false,
    canDelete = false,
    onaction,
  }: {
    /** The backup to show; null closes the drawer. */
    backup?: b.BackupSummary | null;
    /** The caller may restore and verify it. */
    canAdmin?: boolean;
    /** …and delete it (not in a read-only repository). */
    canDelete?: boolean;
    onaction?: (kind: 'restore' | 'verify' | 'delete', backup: b.BackupSummary) => void;
  } = $props();

  let dialog: HTMLDialogElement | undefined = $state();
  let manifest = $state<b.Backup | null>(null);
  let error = $state<string | null>(null);
  let filesShown = $state(12);

  $effect(() => {
    if (!dialog) return;
    if (backup && !dialog.open) dialog.showModal();
    else if (!backup && dialog.open) dialog.close();
  });

  $effect(() => {
    const cur = backup;
    manifest = null;
    error = null;
    filesShown = 12;
    if (!cur) return;
    b.getBackup(...b.pathOf(cur)).then(
      (m) => {
        if (backup === cur) manifest = m;
      },
      (e) => {
        if (backup === cur) error = api.errorMessage(e);
      },
    );
  });

  const kinds = $derived.by(() => {
    const m = new Map<string, { files: number; bytes: number }>();
    for (const f of manifest?.files ?? []) {
      const k = m.get(f.kind) ?? { files: 0, bytes: 0 };
      k.files++;
      k.bytes += f.size;
      m.set(f.kind, k);
    }
    return [...m];
  });

  function openParent() {
    if (!manifest?.parent || !backup) return;
    backup = { ...backup, name: manifest.parent };
  }

  function act(kind: 'restore' | 'verify' | 'delete') {
    if (!backup) return;
    const cur = backup;
    backup = null;
    onaction?.(kind, cur);
  }
</script>

<dialog
  bind:this={dialog}
  class="drawer"
  aria-labelledby="drawer-title"
  onclose={() => backup && (backup = null)}
  onclick={(e) => e.target === dialog && (backup = null)}
>
  {#if backup}
    {@const s = manifest ?? backup}
    <header class="head">
      <div>
        <h2 id="drawer-title" class="mono">{backup.name}</h2>
        <p class="faint small">
          {s.repository} · {s.dataset.name} · commit {s.commit.seq}
        </p>
      </div>
      <span class="spacer"></span>
      <button class="btn ghost icon sm" onclick={() => (backup = null)} aria-label="Close"
        ><Icon name="x" size={14} /></button
      >
    </header>
    <div class="body">
      {#if canAdmin}
        <div class="row">
          <button class="btn primary sm" onclick={() => act('restore')}>Restore…</button>
          <button class="btn sm" onclick={() => act('verify')}>Verify…</button>
          {#if canDelete}
            <button class="btn sm danger" onclick={() => act('delete')}
              ><Icon name="trash" size={12} /> Delete</button
            >
          {/if}
        </div>
      {/if}

      <dl class="kv">
        <dt>Dataset</dt>
        <dd>
          <span class="mono">{s.dataset.name}</span>
          <span class="faint mono small" title="Dataset id">{s.dataset.id}</span>
        </dd>
        <dt>Commit</dt>
        <dd>
          <span class="mono">{s.commit.ref}</span>, {fmtTime(s.commit.timestamp)}
          <span class="faint">· {fmtInt(s.commit.quads)} quads</span>
        </dd>
        <dt>Completed</dt>
        <dd>
          {fmtTime(s.completed)}
          <span class="faint">({fmtRelative(s.completed)}, took {fmtMs(s.millis)})</span>
        </dd>
        <dt>Size</dt>
        <dd>
          {fmtBytes(s.logicalBytes)}
          <span class="faint"
            >· {fmtBytes(s.addedBytes)} new in the repository · dedup {fmtDedup(s)}</span
          >
        </dd>
        <dt>Made by</dt>
        <dd>
          {#if s.policy}
            policy <span class="mono">{s.policy}</span>
            {#if s.run}<span class="faint small"
                >· run <span class="mono">{s.run.slice(0, 8)}</span></span
              >{/if}
          {:else}hand{/if}
        </dd>
        {#if s.note}
          <dt>Note</dt>
          <dd>{s.note}</dd>
        {/if}
        <dt>Last verify</dt>
        <dd>
          {#if s.verified}
            <span class="badge {s.verified.status === 'ok' ? 'ok' : 'danger'}"
              >{s.verified.status}</span
            >
            {s.verified.level}, {fmtRelative(s.verified.at)}
          {:else}<span class="faint">never on this server</span>{/if}
        </dd>
        {#if manifest}
          <dt>Generation</dt>
          <dd>
            <span class="mono">{manifest.generation}</span>
            <span class="faint"
              >· index format {manifest.indexFormat} · written by v{manifest.server.version}</span
            >
          </dd>
          <dt>Parent</dt>
          <dd>
            {#if manifest.parent}
              <button class="linkish mono" onclick={openParent}>{manifest.parent}</button>
              <span class="faint small">(unchanged blobs are shared with it)</span>
            {:else}<span class="faint">none: a full upload</span>{/if}
          </dd>
          {#if manifest.derived.text}
            <dt>Full-text index</dt>
            <dd class="faint">not stored; rebuilt when the backup is restored</dd>
          {/if}
        {/if}
      </dl>

      {#if error}
        <div class="error-box"><strong>Could not load the manifest.</strong> {error}</div>
      {:else if !manifest}
        <p class="faint small"><span class="spinner"></span> Loading the manifest…</p>
      {:else}
        <section>
          <h3>Files <span class="faint">{manifest.files.length}</span></h3>
          <p class="faint small">
            {#each kinds as [k, v], i (k)}{i ? ' · ' : ''}{v.files} {k} ({fmtBytes(v.bytes)}){/each}
          </p>
          <table class="data files">
            <thead>
              <tr><th>Path</th><th>Kind</th><th class="num">Size</th><th class="num">Blobs</th></tr>
            </thead>
            <tbody>
              {#each manifest.files.slice(0, filesShown) as f (f.path)}
                <tr>
                  <td class="mono small" title={`sha256 ${f.sha256}`}>{f.path}</td>
                  <td class="small">{f.kind}</td>
                  <td class="num small">{fmtBytes(f.size)}</td>
                  <td class="num small">{f.blobs.length}</td>
                </tr>
              {/each}
            </tbody>
          </table>
          {#if manifest.files.length > filesShown}
            <button class="btn ghost sm" onclick={() => (filesShown = manifest?.files.length ?? 0)}
              >Show all {manifest.files.length} files</button
            >
          {/if}
        </section>
      {/if}
    </div>
  {/if}
</dialog>

<style>
  .drawer {
    margin: 0 0 0 auto;
    height: 100vh;
    height: 100dvh;
    max-height: none;
    width: min(620px, 100vw);
    max-width: 100vw;
    padding: 0;
    border: 0;
    border-left: 1px solid var(--border);
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-pop);
  }
  .drawer[open] {
    display: flex;
    flex-direction: column;
    animation: slide 0.16s ease-out;
  }
  .drawer::backdrop {
    background: rgba(10, 12, 18, 0.35);
  }
  @keyframes slide {
    from {
      transform: translateX(24px);
      opacity: 0;
    }
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 14px 16px 10px;
    border-bottom: 1px solid var(--border);
  }
  .head h2 {
    font-size: var(--fs-md);
    overflow-wrap: anywhere;
  }
  .head p {
    margin: 2px 0 0;
  }
  .body {
    padding: 12px 16px 24px;
    display: grid;
    gap: 14px;
    overflow: auto;
    align-content: start;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .kv {
    display: grid;
    grid-template-columns: 110px 1fr;
    gap: 6px 12px;
    margin: 0;
    font-size: var(--fs);
  }
  .kv dt {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .kv dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  h3 {
    font-size: var(--fs);
    margin: 0 0 4px;
  }
  section p {
    margin: 0 0 6px;
  }
  .linkish {
    border: 0;
    background: none;
    padding: 0;
    color: var(--iri);
    cursor: pointer;
    font: inherit;
  }
  .linkish:hover {
    text-decoration: underline;
  }
</style>
