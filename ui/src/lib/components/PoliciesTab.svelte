<script lang="ts">
  import * as api from '$lib/api';
  import { app, toasts } from '$lib/app.svelte';
  import * as b from '$lib/backups';
  import { retentionSentence } from '$lib/backups-format';
  import { fmtRelative, fmtTime } from '$lib/format';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import PolicyEditor from './PolicyEditor.svelte';

  let {
    repositories,
    readOnly = false,
    refreshKey = 0,
    onloaded,
    onstarted,
  }: {
    repositories: b.Repository[];
    /** The server is read-only: policy changes and runs are refused. */
    readOnly?: boolean;
    refreshKey?: number;
    /** The loaded policies (the page counts them and checks names). */
    onloaded?: (p: b.Policy[]) => void;
    onstarted?: (t: api.Task) => void;
  } = $props();

  let policies = $state<b.Policy[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let editorOpen = $state(false);
  let editing = $state<b.Policy | null>(null);
  let busy = $state<Record<string, boolean>>({});
  let retention = $state<{ policy: b.Policy; result: b.RetentionResult | null } | null>(null);
  let retentionError = $state<string | null>(null);
  let applying = $state(false);

  async function load() {
    try {
      policies = await b.listPolicies();
      error = null;
      onloaded?.(policies);
    } catch (e) {
      error = api.errorMessage(e);
    } finally {
      loaded = true;
    }
  }

  $effect(() => {
    void refreshKey;
    void load();
  });

  function open(p: b.Policy | null) {
    editing = p;
    editorOpen = true;
  }

  function inputOf(p: b.Policy): b.PolicyInput {
    const { source, state, ...rest } = p;
    void source;
    void state;
    return rest;
  }

  async function toggle(p: b.Policy) {
    busy[p.name] = true;
    try {
      await b.updatePolicy(p.name, { ...inputOf(p), enabled: !p.enabled });
      toasts.push('success', `${p.enabled ? 'Paused' : 'Enabled'} ${p.name}`);
      await load();
    } catch (e) {
      toasts.error(`Could not change ${p.name}`, e);
    } finally {
      busy[p.name] = false;
    }
  }

  async function run(p: b.Policy) {
    busy[p.name] = true;
    try {
      const t = await b.runPolicy(p.name);
      toasts.push('info', `Running ${p.name}`, `Task ${t.id}; the schedule is unchanged`);
      onstarted?.(t);
      await load();
    } catch (e) {
      toasts.error(`Could not run ${p.name}`, e);
    } finally {
      busy[p.name] = false;
    }
  }

  async function remove(p: b.Policy) {
    if (
      !confirm(
        `Delete the policy ${p.name}? Its backups stay; retention no longer applies to them.`,
      )
    )
      return;
    try {
      await b.deletePolicy(p.name);
      toasts.push('success', `Deleted ${p.name}`);
      await load();
    } catch (e) {
      toasts.error(`Could not delete ${p.name}`, e);
    }
  }

  async function previewRetention(p: b.Policy) {
    retention = { policy: p, result: null };
    retentionError = null;
    try {
      const result = await b.applyRetention(p.name, true);
      if (retention?.policy === p) retention.result = result;
    } catch (e) {
      retentionError = api.errorMessage(e);
    }
  }

  async function applyNow() {
    if (!retention?.result) return;
    const p = retention.policy;
    const n = retention.result.delete.length;
    if (!confirm(`Delete ${n} backup${n === 1 ? '' : 's'} of ${p.name} now?`)) return;
    applying = true;
    try {
      const r = await b.applyRetention(p.name, false);
      toasts.push('success', `Retention of ${p.name}: deleted ${r.delete.length}`);
      if (r.errors?.length) toasts.push('error', 'Some deletions failed', r.errors.join('; '));
      retention = null;
      await load();
    } catch (e) {
      retentionError = api.errorMessage(e);
    } finally {
      applying = false;
    }
  }

  const resultClass = (r: b.PolicyRun['result'] | undefined) =>
    r === 'ok' ? 'ok' : r === 'failed' ? 'danger' : r ? 'warn' : '';
</script>

<div class="bar">
  <p class="muted small">
    Scheduled backups with retention. Runs share the server's backup task slots.
  </p>
  <span class="spacer"></span>
  <button
    class="btn primary"
    onclick={() => open(null)}
    disabled={readOnly || repositories.length === 0}
    title={readOnly
      ? 'The server is read-only'
      : repositories.length === 0
        ? 'Add a repository first'
        : undefined}><Icon name="plus" size={14} /> New policy</button
  >
</div>

{#if error}
  <div class="error-box"><strong>Could not load policies.</strong> {error}</div>
{/if}

<section class="panel">
  <div class="scroll-x">
    <table class="data">
      <thead>
        <tr>
          <th>Policy</th>
          <th>Datasets</th>
          <th>Schedule</th>
          <th>Next run</th>
          <th>Last run</th>
          <th class="num" title="Consecutive failed or partial runs">Failures</th>
          <th>Enabled</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each policies as p (p.name)}
          {@const last = p.state.lastRun}
          <tr>
            <td>
              <button class="linkish mono" onclick={() => open(p)}>{p.name}</button>
              {#if p.source === 'config'}<span class="badge" title="From the backup config file"
                  >config</span
                >{/if}
              <div class="faint small">
                into <span class="mono">{p.repository}</span> · {retentionSentence(p.retention)}
              </div>
            </td>
            <td class="mono small">{p.datasets.join(', ')}</td>
            <td class="small">
              <span class="mono">{p.schedule}</span>
              <div class="faint">{p.timezone}</div>
            </td>
            <td class="small" title={p.state.nextRun ? fmtTime(p.state.nextRun) : ''}>
              {#if p.state.runningTask}
                <span class="badge spark">running</span>
              {:else if p.state.nextRun}
                {fmtTime(p.state.nextRun)}
              {:else}<span class="faint">paused</span>{/if}
            </td>
            <td class="small">
              {#if last}
                <span class="badge {resultClass(last.result)}">{last.result}</span>
                <span class="faint" title={fmtTime(last.started)}>{fmtRelative(last.started)}</span>
              {:else}<span class="faint">never</span>{/if}
            </td>
            <td class="num" class:bad={p.state.consecutiveFailures > 0}
              >{p.state.consecutiveFailures}</td
            >
            <td>
              <label
                class="switch"
                title={p.source === 'config'
                  ? 'Defined in the config file'
                  : p.enabled
                    ? 'Pause the schedule'
                    : 'Resume the schedule'}
              >
                <input
                  type="checkbox"
                  role="switch"
                  checked={p.enabled}
                  disabled={p.source === 'config' || readOnly || busy[p.name]}
                  onchange={() => toggle(p)}
                  aria-label="{p.name} enabled"
                />
                <span aria-hidden="true"></span>
              </label>
            </td>
            <td class="actions">
              <div class="cell-actions">
                <button
                  class="btn sm"
                  onclick={() => run(p)}
                  disabled={readOnly || busy[p.name]}
                  title={readOnly ? 'The server is read-only: policies do not run' : undefined}
                  >Run now</button
                >
                <button class="btn sm" onclick={() => previewRetention(p)}>Retention</button>
                <button class="btn sm ghost" onclick={() => open(p)}
                  >{p.source === 'config' ? 'View' : 'Edit'}</button
                >
                {#if p.source === 'api' && !readOnly}
                  <button
                    class="btn sm icon danger"
                    onclick={() => remove(p)}
                    aria-label="Delete {p.name}"
                    title="Delete {p.name}"><Icon name="trash" size={12} /></button
                  >
                {/if}
              </div>
            </td>
          </tr>
        {:else}
          <tr>
            <td colspan="8" class="faint">{loaded ? 'No policies yet.' : 'Loading…'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</section>

<PolicyEditor
  bind:open={editorOpen}
  {editing}
  {repositories}
  datasets={app.datasets}
  taken={[...policies.map((p) => p.name), ...repositories.map((r) => r.name)]}
  onsaved={load}
  onrun={readOnly
    ? undefined
    : (p) => {
        editorOpen = false;
        void run(p);
      }}
  onretention={(p) => {
    editorOpen = false;
    void previewRetention(p);
  }}
/>

<Modal
  open={retention != null}
  title="Retention of {retention?.policy.name ?? ''}"
  width={620}
  onclose={() => (retention = null)}
>
  {#if retention}
    <p class="small muted">{retentionSentence(retention.policy.retention)} Per dataset.</p>
    {#if retentionError}
      <div class="error-box">{retentionError}</div>
    {:else if !retention.result}
      <p class="faint small"><span class="spinner"></span> Evaluating…</p>
    {:else}
      {@const r = retention.result}
      <h3>Would delete <span class="faint">{r.delete.length}</span></h3>
      {#if r.delete.length}
        <ul class="list">
          {#each r.delete as x (`${x.repository}/${x.name}`)}
            <li>
              <span class="mono">{x.name}</span>
              <span class="faint">{x.dataset.name} · completed {fmtRelative(x.completed)}</span>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="faint small">Nothing: every backup is within the rules.</p>
      {/if}
      <h3>Keeps <span class="faint">{r.keep.length}</span></h3>
      <ul class="list keep">
        {#each r.keep as x (`${x.repository}/${x.name}`)}
          <li>
            <span class="mono">{x.name}</span>
            <span class="faint">{x.dataset.name} · completed {fmtRelative(x.completed)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
  {#snippet actions()}
    <button class="btn" onclick={() => (retention = null)}>Close</button>
    <button
      class="btn danger solid"
      onclick={applyNow}
      disabled={applying || !retention?.result?.delete.length || readOnly}
    >
      {#if applying}<span class="spinner"></span>{/if} Apply now
    </button>
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
  .scroll-x {
    overflow-x: auto;
  }
  .linkish {
    border: 0;
    background: none;
    padding: 0;
    color: var(--text);
    font-weight: 600;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: var(--fs);
  }
  .linkish:hover {
    color: var(--iri);
    text-decoration: underline;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  td.bad {
    color: var(--danger);
    font-weight: 600;
  }
  .actions {
    text-align: right;
    white-space: nowrap;
  }
  .switch {
    position: relative;
    display: inline-block;
    width: 30px;
    height: 17px;
  }
  .switch input {
    position: absolute;
    inset: 0;
    opacity: 0;
    margin: 0;
    cursor: pointer;
  }
  .switch span {
    position: absolute;
    inset: 0;
    border-radius: 9px;
    background: var(--surface-3);
    border: 1px solid var(--border-strong);
    transition: background 0.15s;
    pointer-events: none;
  }
  .switch span::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 2px;
    width: 11px;
    height: 11px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
    transition: transform 0.15s;
  }
  .switch input:checked + span {
    background: var(--ok);
    border-color: transparent;
  }
  .switch input:checked + span::after {
    transform: translateX(13px);
  }
  .switch input:focus-visible + span {
    box-shadow: var(--focus);
  }
  .switch input:disabled + span {
    opacity: 0.55;
  }
  h3 {
    font-size: var(--fs);
    margin: 4px 0 0;
  }
  .list {
    margin: 0;
    padding-left: 18px;
    display: grid;
    gap: 3px;
    font-size: var(--fs-sm);
    max-height: 200px;
    overflow: auto;
  }
  .list li span + span {
    margin-left: 6px;
  }
  .list:not(.keep) li {
    color: var(--danger);
  }
</style>
