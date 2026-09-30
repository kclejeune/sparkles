<script lang="ts">
  import * as api from '$lib/api';
  import * as b from '$lib/backups';

  let {
    schedule,
    timezone,
    next = $bindable([]),
    valid = $bindable(false),
  }: {
    schedule: string;
    timezone: string;
    /** The next runs (ISO instants), for the name template sample. */
    next?: string[];
    /** The server accepted the schedule. */
    valid?: boolean;
  } = $props();

  let preview = $state<b.SchedulePreview | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);

  // the server evaluates cron and `every` schedules, so the UI needs no cron library
  $effect(() => {
    const s = schedule.trim();
    const tz = timezone;
    preview = null;
    valid = false;
    next = [];
    if (!s) {
      error = null;
      return;
    }
    const ctl = new AbortController();
    loading = true;
    const timer = setTimeout(async () => {
      try {
        const p = await b.previewSchedule({ schedule: s, timezone: tz, count: 5 }, ctl.signal);
        preview = p;
        next = p.next;
        valid = true;
        error = null;
      } catch (e) {
        if (e instanceof DOMException && e.name === 'AbortError') return;
        error = api.errorMessage(e);
      } finally {
        if (!ctl.signal.aborted) loading = false;
      }
    }, 300);
    return () => {
      clearTimeout(timer);
      ctl.abort();
    };
  });

  function inZone(iso: string, tz: string) {
    try {
      return new Date(iso).toLocaleString(undefined, {
        timeZone: tz,
        weekday: 'short',
        day: 'numeric',
        month: 'short',
        hour: '2-digit',
        minute: '2-digit',
      });
    } catch {
      return iso;
    }
  }
  const inHow = (iso: string) => {
    const s = Math.round((new Date(iso).getTime() - Date.now()) / 1000);
    if (s < 60) return 'in under a minute';
    if (s < 3600) return `in ${Math.round(s / 60)} min`;
    if (s < 86400 * 2) return `in ${Math.round(s / 3600)} h`;
    return `in ${Math.round(s / 86400)} days`;
  };
  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
</script>

<div class="preview" aria-live="polite">
  {#if error}
    <p class="bad">{error}</p>
  {:else if preview}
    <p class="desc">{preview.description}</p>
    <ol class="runs" aria-label="Next runs">
      {#each preview.next as t (t)}
        <li>
          <span>{inZone(t, timezone)}</span>
          {#if timezone !== localZone}
            <span class="faint" title="Your time ({localZone})">{inZone(t, localZone)} here</span>
          {/if}
          <span class="faint">{inHow(t)}</span>
        </li>
      {/each}
    </ol>
  {:else if loading}
    <p class="faint"><span class="spinner"></span> Working out the next runs…</p>
  {:else}
    <p class="faint">Enter a schedule to see its next runs.</p>
  {/if}
</div>

<style>
  .preview {
    padding: 8px 10px;
    border: 1px dashed var(--border-strong);
    border-radius: var(--r);
    font-size: var(--fs-sm);
    min-height: 40px;
  }
  p {
    margin: 0;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .desc {
    font-weight: 600;
    margin-bottom: 4px;
  }
  .bad {
    color: var(--danger);
  }
  .runs {
    margin: 0;
    padding-left: 18px;
    display: grid;
    gap: 2px;
  }
  .runs li span + span {
    margin-left: 8px;
  }
</style>
