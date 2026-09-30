<script lang="ts">
  import { untrack } from 'svelte';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import * as b from '$lib/backups';
  import {
    DEFAULT_NAME_TEMPLATE,
    POLICY_NAME,
    WEEKDAYS,
    matchDatasets,
    parseDuration,
    parsePatterns,
    presetToSchedule,
    renderNameTemplate,
    retentionSentence,
    scheduleToPreset,
    type SchedulePreset,
  } from '$lib/backups-format';
  import Modal from './Modal.svelte';
  import SchedulePreview from './SchedulePreview.svelte';

  let {
    open = $bindable(false),
    editing = null,
    repositories = [],
    datasets = [],
    taken = [],
    onsaved,
    onrun,
    onretention,
  }: {
    open?: boolean;
    /** The policy to edit; null creates one. */
    editing?: b.Policy | null;
    repositories?: b.Repository[];
    datasets?: api.DatasetInfo[];
    /** Names in use (policies and repositories share one namespace). */
    taken?: string[];
    onsaved?: (p: b.Policy) => void;
    onrun?: (p: b.Policy) => void;
    onretention?: (p: b.Policy) => void;
  } = $props();

  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const ZONES = (() => {
    try {
      const all = Intl.supportedValuesOf('timeZone');
      return all.includes('UTC') ? all : ['UTC', ...all];
    } catch {
      return ['UTC', localZone];
    }
  })();
  const DAY_ORDER = [1, 2, 3, 4, 5, 6, 0];

  let name = $state('');
  let repository = $state('');
  let selected = $state<string[]>([]);
  let globs = $state('');
  let kind = $state<SchedulePreset['kind']>('daily');
  let minute = $state(0);
  let time = $state('02:30');
  let day = $state(0);
  let custom = $state('');
  let timezone = $state('UTC');
  let nameTemplate = $state(DEFAULT_NAME_TEMPLATE);
  let expireAfter = $state('');
  let minCount = $state(1);
  let maxCount = $state<number | null>(null);
  let skipUnchanged = $state(false);
  let gcAfterRetention = $state(false);
  let catchUp = $state<'one' | 'none'>('one');
  let enabled = $state(true);

  let next = $state<string[]>([]);
  let scheduleOk = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  const runId = crypto.randomUUID();

  $effect(() => {
    if (open) untrack(reset);
  });

  function reset() {
    const p = editing;
    error = null;
    name = p?.name ?? '';
    repository = p?.repository ?? repositories.find((r) => !r.readonly)?.name ?? '';
    const names = new Set(datasets.map((d) => d.name));
    const list = p?.datasets ?? ['*'];
    selected = list.filter((x) => !x.includes('*') && names.has(x));
    globs = list.filter((x) => !selected.includes(x)).join(', ');
    const preset = scheduleToPreset(p?.schedule ?? '30 2 * * *');
    kind = preset.kind;
    minute = preset.kind === 'hourly' ? preset.minute : 0;
    time =
      preset.kind === 'daily' || preset.kind === 'weekly'
        ? `${String(preset.hour).padStart(2, '0')}:${String(preset.minute).padStart(2, '0')}`
        : '02:30';
    day = preset.kind === 'weekly' ? preset.day : 0;
    custom = preset.kind === 'custom' ? preset.expr : '';
    timezone = p?.timezone ?? (ZONES.includes(localZone) ? localZone : 'UTC');
    nameTemplate = p?.nameTemplate ?? DEFAULT_NAME_TEMPLATE;
    expireAfter = p?.retention.expireAfter ?? '';
    minCount = p?.retention.minCount ?? 1;
    maxCount = p?.retention.maxCount ?? null;
    skipUnchanged = p?.skipUnchanged ?? false;
    gcAfterRetention = p?.gcAfterRetention ?? false;
    catchUp = p?.catchUp ?? 'one';
    enabled = p?.enabled ?? true;
  }

  const isEdit = $derived(editing != null);
  const fromConfig = $derived(editing?.source === 'config');
  const nameOk = $derived(isEdit || (POLICY_NAME.test(name) && !taken.includes(name)));

  const hhmm = $derived(time.split(':').map((x) => Number(x) || 0));
  const preset = $derived<SchedulePreset>(
    kind === 'hourly'
      ? { kind, minute }
      : kind === 'daily'
        ? { kind, hour: hhmm[0], minute: hhmm[1] }
        : kind === 'weekly'
          ? { kind, day, hour: hhmm[0], minute: hhmm[1] }
          : { kind, expr: custom },
  );
  const schedule = $derived(presetToSchedule(preset));

  const patterns = $derived([...selected, ...parsePatterns(globs)]);
  const matched = $derived(
    matchDatasets(
      patterns,
      datasets.map((d) => d.name),
    ),
  );
  const inMemory = $derived(
    matched.filter((n) => datasets.find((d) => d.name === n)?.type === 'mem'),
  );
  const sampleDs = $derived(
    datasets.find((d) => matched.includes(d.name) && d.type !== 'mem') ?? null,
  );
  const sample = $derived(
    renderNameTemplate(nameTemplate, {
      policy: name || 'policy',
      dataset: sampleDs?.name ?? 'dataset',
      seq: sampleDs?.head ?? 42,
      run: runId,
      time: next[0] ? new Date(next[0]) : new Date(),
      timezone,
    }),
  );

  const expire = $derived(expireAfter.trim() ? parseDuration(expireAfter) : null);
  const expireOk = $derived(!expireAfter.trim() || expire != null);
  const retention = $derived<b.Retention>({
    expireAfter: expireAfter.trim() || null,
    minCount: Number.isFinite(minCount) ? minCount : 0,
    maxCount: maxCount == null || (maxCount as unknown) === '' ? null : maxCount,
  });
  const countsOk = $derived(
    Number.isInteger(retention.minCount) &&
      retention.minCount >= 0 &&
      (retention.maxCount == null ||
        (Number.isInteger(retention.maxCount) && retention.maxCount >= 1)),
  );

  const valid = $derived(
    nameOk &&
      !!repository &&
      patterns.length > 0 &&
      scheduleOk &&
      !sample.error &&
      expireOk &&
      countsOk,
  );

  function input(): b.PolicyInput {
    return {
      name,
      repository,
      datasets: patterns,
      schedule,
      timezone,
      nameTemplate,
      retention,
      skipUnchanged,
      gcAfterRetention,
      catchUp,
      enabled,
    };
  }

  async function save(e: Event) {
    e.preventDefault();
    if (!valid || busy || fromConfig) return;
    busy = true;
    error = null;
    try {
      const p = editing
        ? await b.updatePolicy(editing.name, input())
        : await b.createPolicy(input());
      toasts.push('success', `${editing ? 'Saved' : 'Created'} policy ${p.name}`);
      onsaved?.(p);
      open = false;
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }

  function toggleDataset(n: string, on: boolean) {
    selected = on ? [...selected, n] : selected.filter((x) => x !== n);
  }
</script>

<Modal
  bind:open
  title={fromConfig
    ? `Policy ${editing?.name}`
    : isEdit
      ? `Edit policy ${editing?.name}`
      : 'New backup policy'}
  width={680}
>
  {#if fromConfig}
    <p class="note">
      Defined in the backup config file: change it there and reload the server (SIGHUP). You can
      still run it now and preview its retention.
    </p>
  {/if}
  <form id="policy-form" onsubmit={save}>
    <fieldset class="form" disabled={fromConfig}>
      <div class="grid2">
        <label class="field">
          Name
          <input
            class="input mono"
            bind:value={name}
            disabled={isEdit}
            placeholder="nightly"
            autocomplete="off"
            spellcheck="false"
            aria-invalid={!!name && !nameOk}
          />
          {#if name && !nameOk}
            <span class="hint bad"
              >{taken.includes(name)
                ? 'This name is taken (by a policy or a repository).'
                : 'Lowercase letters, digits, “_” and “-”.'}</span
            >
          {/if}
        </label>
        <label class="field">
          Repository
          <select class="select" bind:value={repository}>
            {#each repositories as r (r.name)}
              <option value={r.name} disabled={r.readonly}
                >{r.name}{r.readonly ? ' (read-only)' : ''}</option
              >
            {/each}
          </select>
        </label>
      </div>

      <fieldset class="group">
        <legend>Datasets</legend>
        <div class="dslist">
          {#each datasets as d (d.name)}
            <label class="check">
              <input
                type="checkbox"
                checked={selected.includes(d.name)}
                onchange={(e) => toggleDataset(d.name, e.currentTarget.checked)}
              />
              <span class="mono">{d.name}</span>
              {#if d.type === 'mem'}<span class="faint">in-memory</span>{/if}
            </label>
          {/each}
        </div>
        <label class="field">
          <span
            >Name patterns <span class="faint">(“*” matches anything; comma separated)</span></span
          >
          <input
            class="input mono"
            bind:value={globs}
            placeholder="wiki-*, team-*-prod"
            spellcheck="false"
          />
        </label>
        <p class="hint" aria-live="polite">
          {#if patterns.length === 0}
            <span class="bad">Choose datasets or enter a pattern.</span>
          {:else if matched.length === 0}
            No dataset matches now; datasets created later that match are included.
          {:else}
            Matches now: <span class="mono">{matched.join(', ')}</span>{#if inMemory.length}.
              In-memory datasets are skipped: <span class="mono">{inMemory.join(', ')}</span>{/if}.
          {/if}
        </p>
      </fieldset>

      <fieldset class="group">
        <legend>Schedule</legend>
        <div class="seg" role="radiogroup" aria-label="Schedule kind">
          {#each [['hourly', 'Hourly'], ['daily', 'Daily'], ['weekly', 'Weekly'], ['custom', 'Custom']] as [v, label] (v)}
            <label class:sel={kind === v}
              ><input type="radio" bind:group={kind} value={v} />{label}</label
            >
          {/each}
        </div>
        <div class="row wrap">
          {#if kind === 'hourly'}
            <label class="inline"
              >At minute <input
                class="input narrow"
                aria-label="Minute"
                type="number"
                min="0"
                max="59"
                bind:value={minute}
              /></label
            >
          {:else if kind === 'daily' || kind === 'weekly'}
            {#if kind === 'weekly'}
              <label class="inline"
                >On <select class="select" bind:value={day} aria-label="Weekday">
                  {#each DAY_ORDER as d (d)}<option value={d}>{WEEKDAYS[d]}</option>{/each}
                </select></label
              >
            {/if}
            <label class="inline"
              >At <input class="input" type="time" bind:value={time} aria-label="Time" /></label
            >
          {:else}
            <label class="inline grow"
              >Cron or interval <input
                class="input mono grow"
                aria-label="Cron or interval"
                bind:value={custom}
                placeholder="0 */4 * * *  or  every 6h"
                spellcheck="false"
              /></label
            >
          {/if}
          <label class="inline"
            >Time zone <select class="select" bind:value={timezone} aria-label="Time zone">
              {#each ZONES as z (z)}<option value={z}>{z}</option>{/each}
            </select></label
          >
        </div>
        {#if kind === 'custom'}
          <p class="hint">
            Cron has 5 fields (minute hour day month weekday), or 6 with seconds first. <span
              class="mono">every 6h</span
            > counts from 00:00 UTC, so it is stable across restarts.
          </p>
        {/if}
        <SchedulePreview {schedule} {timezone} bind:next bind:valid={scheduleOk} />
      </fieldset>

      <label class="field">
        Backup names
        <input class="input mono" bind:value={nameTemplate} spellcheck="false" />
        <span class="hint" class:bad={!!sample.error} aria-live="polite">
          {#if sample.error}{sample.error}{:else}Next: <span class="mono sample">{sample.name}</span
            >{/if}
        </span>
        <span class="hint"
          ><span class="mono">{'{policy} {dataset} {seq} {run} {time}'}</span> and
          <span class="mono">{'{date:%Y%m%d}'}</span> (%Y %m %d %H %M %S %j %V, in the policy's time zone).
          A taken name gets -2, -3, …</span
        >
      </label>

      <fieldset class="group">
        <legend
          >Retention <span class="faint">(per dataset, this policy's backups only)</span></legend
        >
        <div class="grid3">
          <label class="field">
            Expire after
            <input
              class="input mono"
              bind:value={expireAfter}
              placeholder="30d"
              aria-invalid={!expireOk}
            />
          </label>
          <label class="field">
            Keep at least
            <input class="input" type="number" min="0" bind:value={minCount} />
          </label>
          <label class="field">
            Keep at most
            <input
              class="input"
              type="number"
              min="1"
              bind:value={maxCount}
              placeholder="no limit"
            />
          </label>
        </div>
        <p class="hint" class:bad={!expireOk || !countsOk} aria-live="polite">
          {#if !expireOk}Durations look like 30d, 12h, 2w or 1d 12h.{:else if !countsOk}Counts are
            whole numbers (at most: 1 or more).{:else}{retentionSentence(retention)}{/if}
        </p>
      </fieldset>

      <fieldset class="group">
        <legend>Options</legend>
        <label class="check">
          <input type="checkbox" bind:checked={skipUnchanged} /> Skip datasets that did not change
          <span class="faint">since this policy's last backup of them</span>
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={gcAfterRetention} /> Collect garbage after retention
          <span class="faint">(at most once a day per repository)</span>
        </label>
        <label class="check">
          After downtime
          <select class="select" bind:value={catchUp}>
            <option value="one">run once for the missed runs</option>
            <option value="none">skip missed runs</option>
          </select>
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={enabled} /> Enabled
        </label>
      </fieldset>
      {#if error}<div class="error-box">{error}</div>{/if}
    </fieldset>
  </form>
  {#snippet actions()}
    {#if editing}
      <button class="btn" type="button" onclick={() => editing && onrun?.(editing)}>Run now</button>
      <button class="btn" type="button" onclick={() => editing && onretention?.(editing)}
        >Preview retention</button
      >
      <span class="spacer"></span>
    {/if}
    <button class="btn" type="button" onclick={() => (open = false)}
      >{fromConfig ? 'Close' : 'Cancel'}</button
    >
    {#if !fromConfig}
      <button class="btn primary" type="submit" form="policy-form" disabled={busy || !valid}>
        {#if busy}<span class="spinner"></span>{/if}
        {isEdit ? 'Save' : 'Create policy'}
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    min-width: 0;
  }
  .form {
    display: grid;
    gap: 14px;
  }
  .group {
    display: grid;
    gap: 8px;
  }
  legend {
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 600;
    margin-bottom: 4px;
    padding: 0;
  }
  .grid2 {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .grid3 {
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    gap: 10px;
  }
  .hint {
    margin: 0;
    font-weight: 400;
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .hint.bad,
  .bad {
    color: var(--danger);
  }
  .sample {
    color: var(--text);
  }
  .note {
    margin: 0;
    padding: 8px 10px;
    border-radius: var(--r);
    background: var(--surface-2);
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .dslist {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    max-height: 120px;
    overflow: auto;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    font-size: var(--fs-sm);
  }
  .check input,
  .seg input {
    margin: 0;
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
  .wrap {
    flex-wrap: wrap;
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
  .grow {
    flex: 1;
  }
  .narrow {
    width: 70px;
  }
  @media (max-width: 640px) {
    .grid2,
    .grid3 {
      grid-template-columns: 1fr;
    }
  }
</style>
