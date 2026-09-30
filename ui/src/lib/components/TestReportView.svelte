<script lang="ts">
  import type { TestReport } from '$lib/backups';
  import { fmtMs } from '$lib/format';
  import Icon from './Icon.svelte';

  let { report }: { report: TestReport } = $props();

  /** The probe could be created, so the conditional-write result means something. */
  const reached = $derived(report.steps.find((s) => s.step === 'create')?.ok === true);

  const LABELS: Record<string, string> = {
    create: 'Create a probe object',
    'create-again': 'Create it again (must be refused)',
    read: 'Read it back',
    list: 'List the probe prefix',
    delete: 'Delete it',
  };
</script>

<div class="report">
  <div class="row">
    <span class="badge {report.ok ? 'ok' : 'danger'}">
      <Icon name={report.ok ? 'check' : 'alert'} size={12} />
      {report.ok ? 'Connection works' : 'Connection failed'}
    </span>
    {#if reached}
      <span
        class="badge {report.conditionalWrites ? 'ok' : 'warn'}"
        title={report.conditionalWrites
          ? 'Backup names are unique even with several writing servers'
          : 'The service ignores conditional writes: use one writing server only'}
      >
        {report.conditionalWrites ? 'conditional writes' : 'no conditional writes: single writer'}
      </span>
    {/if}
  </div>
  <ol class="steps">
    {#each report.steps as s (s.step)}
      <li class:bad={!s.ok}>
        <Icon name={s.ok ? 'check' : 'x'} size={13} />
        <span>{LABELS[s.step] ?? s.step}</span>
        <span class="faint mono">{s.ok ? fmtMs(s.millis) : ''}</span>
        {#if s.error}<span class="err">{s.error}</span>{/if}
      </li>
    {/each}
  </ol>
</div>

<style>
  .report {
    display: grid;
    gap: 8px;
  }
  .badge.warn {
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    color: var(--warn);
  }
  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
  }
  .steps li {
    display: grid;
    grid-template-columns: 16px 1fr auto;
    align-items: center;
    gap: 6px;
    color: var(--ok);
  }
  .steps li > span:not(.err) {
    color: var(--text);
  }
  .steps li.bad {
    color: var(--danger);
  }
  .err {
    grid-column: 2 / -1;
    color: var(--danger);
    overflow-wrap: anywhere;
  }
</style>
