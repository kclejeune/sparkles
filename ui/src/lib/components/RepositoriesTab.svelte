<script lang="ts">
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import * as b from '$lib/backups';
  import { fmtBytesPerSec, fmtRatio, insecureEndpoint, repoLocation } from '$lib/backups-format';
  import { fmtBytes, fmtInt, fmtRelative, fmtTime } from '$lib/format';
  import GcDialog from './GcDialog.svelte';
  import Icon from './Icon.svelte';
  import LocksDialog from './LocksDialog.svelte';
  import Modal from './Modal.svelte';
  import RepositoryDialog from './RepositoryDialog.svelte';
  import TestReportView from './TestReportView.svelte';
  import VerifyDialog from './VerifyDialog.svelte';

  let {
    repositories,
    admin,
    readOnly = false,
    policyNames = [],
    onchanged,
  }: {
    repositories: (b.Repository | b.RepositoryBrief)[];
    admin: boolean;
    /** The server is read-only: registration changes are refused. */
    readOnly?: boolean;
    /** Policy names (repositories and policies share one namespace). */
    policyNames?: string[];
    onchanged: () => void;
  } = $props();

  let dialogOpen = $state(false);
  let editing = $state<b.Repository | null>(null);
  let testing = $state<string | null>(null);
  let test = $state<{ repo: string; report: b.TestReport } | null>(null);
  let verify = $state<b.VerifyTarget | null>(null);
  let gc = $state<b.Repository | null>(null);
  let locks = $state<string | null>(null);

  const full = $derived(repositories.filter(b.isFull));
  const taken = $derived([...repositories.map((r) => r.name), ...policyNames]);

  function add() {
    editing = null;
    dialogOpen = true;
  }

  function edit(r: b.Repository) {
    editing = r;
    dialogOpen = true;
  }

  async function runTest(r: b.Repository) {
    testing = r.name;
    try {
      test = { repo: r.name, report: await b.testRepository(r.name) };
      onchanged();
    } catch (e) {
      toasts.error(`Could not test ${r.name}`, e);
    } finally {
      testing = null;
    }
  }

  async function remove(r: b.Repository) {
    const msg = `Unregister ${r.name}? Its contents at ${repoLocation(r)} stay untouched, and it can be added again later.`;
    if (!confirm(msg)) return;
    try {
      await b.removeRepository(r.name);
      toasts.push('success', `Removed ${r.name}`);
      onchanged();
    } catch (e) {
      toasts.error(`Could not remove ${r.name}`, e);
    }
  }

  const credLabel = (r: b.Repository) =>
    r.type === 'fs'
      ? null
      : r.credentials?.source === 'env'
        ? `env ${r.credentials.accessKeyIdVar}`
        : r.credentials?.source === 'file'
          ? `file ${r.credentials.path}`
          : r.credentials?.source === 'named'
            ? `credentials ${r.credentials.name}`
            : 'default credential chain';

  const mutable = (r: b.Repository) => r.source === 'api' && !readOnly;
</script>

{#if admin}
  <div class="bar">
    <p class="muted small">
      Where backups go. Use one writing server per repository and attach it read-only elsewhere.
    </p>
    <span class="spacer"></span>
    <button
      class="btn primary"
      onclick={add}
      disabled={readOnly}
      title={readOnly ? 'The server is read-only' : undefined}
      ><Icon name="plus" size={14} /> Add repository</button
    >
  </div>
{/if}

{#if repositories.length === 0}
  <div class="empty panel">
    <Icon name="archive" size={24} />
    <p>No backup repositories yet.</p>
    {#if admin && !readOnly}
      <button class="btn primary" onclick={add}
        ><Icon name="plus" size={14} /> Add the first repository</button
      >
    {:else if !admin}
      <p class="faint small">A server admin registers them.</p>
    {/if}
  </div>
{:else if !admin}
  <ul class="brief panel">
    {#each repositories as r (r.name)}
      <li>
        <span class="dot" class:on={b.reachable(r)} aria-hidden="true"></span>
        <strong class="mono">{r.name}</strong>
        <span class="badge">{r.type}</span>
        {#if b.isReadonly(r)}<span class="badge">read-only</span>{/if}
        <span class="faint small">{b.reachable(r) ? 'reachable' : 'unreachable'}</span>
      </li>
    {/each}
  </ul>
{:else}
  <div class="cards">
    {#each full as r (r.name)}
      <article class="panel card" aria-labelledby="repo-{r.name}">
        <header class="card-head">
          <span
            class="dot"
            class:on={r.status.reachable}
            title={r.status.reachable ? 'Reachable' : 'Unreachable'}
            aria-hidden="true"
          ></span>
          <h3 id="repo-{r.name}" class="mono">{r.name}</h3>
          <span class="badge">{r.type}</span>
          {#if r.source === 'config'}<span class="badge" title="From the backup config file"
              >config</span
            >{/if}
          {#if r.readonly}<span class="badge iri">read-only</span>{/if}
          {#if r.status.singleWriter}<span
              class="badge warn"
              title="No conditional writes: only one server may write to it">single writer</span
            >{/if}
          {#if insecureEndpoint(r)}<span class="badge danger" title="Plain http:// endpoint"
              >http</span
            >{/if}
          <span class="spacer"></span>
          {#if mutable(r)}
            <button class="btn sm ghost" onclick={() => edit(r)}>Edit</button>
            <button
              class="btn sm icon ghost danger"
              onclick={() => remove(r)}
              aria-label="Remove {r.name}"
              title="Unregister {r.name}"><Icon name="trash" size={12} /></button
            >
          {/if}
        </header>
        <div class="loc mono" title={repoLocation(r)}>{repoLocation(r)}</div>
        {#if r.endpoint || r.region || credLabel(r)}
          <div class="faint small">
            {[r.endpoint, r.region, credLabel(r), r.sse ? `SSE ${r.sse}` : null]
              .filter(Boolean)
              .join(' · ')}
          </div>
        {/if}
        <div class="status small" class:bad={!r.status.reachable}>
          {#if r.status.reachable}
            Reachable, checked {fmtRelative(r.status.checked)}
          {:else}
            <Icon name="alert" size={12} /> Unreachable{r.status.error ? `: ${r.status.error}` : ''}
          {/if}
        </div>
        {#if r.stats}
          <dl class="stats">
            <div>
              <dt>Stored</dt>
              <dd>{fmtBytes(r.stats.storedBytes)}</dd>
            </div>
            <div>
              <dt>Logical</dt>
              <dd>{fmtBytes(r.stats.logicalBytes)}</dd>
            </div>
            <div>
              <dt>Dedup</dt>
              <dd>{fmtRatio(r.stats.dedupRatio)}</dd>
            </div>
            <div>
              <dt>Backups</dt>
              <dd>
                {fmtInt(r.stats.backups)}
                <span class="faint small"
                  >of {fmtInt(r.stats.datasets)} dataset{r.stats.datasets === 1 ? '' : 's'}</span
                >
              </dd>
            </div>
          </dl>
        {/if}
        <div class="faint small meta">
          {#if r.policies?.length}Policies: <span class="mono">{r.policies.join(', ')}</span> ·
          {/if}
          {r.maxConcurrency ?? (r.type === 'fs' ? 4 : 8)} parallel requests · up {fmtBytesPerSec(
            r.maxUploadBytesPerSec,
          )}, down {fmtBytesPerSec(r.maxDownloadBytesPerSec)}
          {#if r.lastGc}
            <br /><span title={fmtTime(r.lastGc.finished)}
              >Last GC {fmtRelative(r.lastGc.finished)}: {fmtInt(r.lastGc.deleted)} blobs, {fmtBytes(
                r.lastGc.deletedBytes,
              )} freed</span
            >
          {/if}
        </div>
        <div class="card-actions">
          <button class="btn sm" onclick={() => runTest(r)} disabled={testing === r.name}>
            {#if testing === r.name}<span class="spinner"></span>{/if} Test connection
          </button>
          <button
            class="btn sm"
            onclick={() => (verify = { kind: 'repository', repository: r.name })}
            disabled={!r.status.reachable}>Verify</button
          >
          {#if !r.readonly}
            <button
              class="btn sm"
              onclick={() => (gc = r)}
              disabled={!r.status.reachable}
              title="Delete blobs no backup references (dry run first)">Run GC</button
            >
            <button class="btn sm" onclick={() => (locks = r.name)} disabled={!r.status.reachable}
              >Locks</button
            >
          {/if}
        </div>
      </article>
    {/each}
  </div>
{/if}

<RepositoryDialog bind:open={dialogOpen} {editing} {taken} onsaved={() => onchanged()} />
<VerifyDialog bind:target={verify} />
<GcDialog bind:repository={gc} onfinished={onchanged} />
<LocksDialog bind:repository={locks} />

<Modal
  open={test != null}
  title="Connection test: {test?.repo ?? ''}"
  onclose={() => (test = null)}
>
  {#if test}<TestReportView report={test.report} />{/if}
  {#snippet actions()}
    <button class="btn primary" onclick={() => (test = null)}>Close</button>
  {/snippet}
</Modal>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .bar p {
    margin: 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(340px, 100%), 1fr));
    gap: 12px;
  }
  .card {
    display: grid;
    gap: 8px;
    padding: 12px 14px;
    align-content: start;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .card-head h3 {
    font-size: var(--fs-md);
    margin-right: 4px;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--danger);
    flex: none;
  }
  .dot.on {
    background: var(--ok);
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .loc {
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .status.bad {
    color: var(--danger);
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(4, auto);
    justify-content: space-between;
    gap: 8px;
    margin: 0;
    padding: 8px 0;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }
  .stats dt {
    font-size: var(--fs-xs);
    color: var(--text-2);
  }
  .stats dd {
    margin: 2px 0 0;
    font-weight: 600;
  }
  .meta {
    line-height: 1.6;
  }
  .card-actions {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: wrap;
  }
  .brief {
    list-style: none;
    margin: 0;
    padding: 4px 14px;
  }
  .brief li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 0;
    border-bottom: 1px solid var(--border);
  }
  .brief li:last-child {
    border-bottom: 0;
  }
</style>
