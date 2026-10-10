<script lang="ts">
  // The dataset page's Ingest section (C18 §7.10): upload a document, follow its task,
  // confirm a large estimate, approve a preview, and open the review page. A CSV or TSV
  // file gets a mapping draft, which a dry run and then an upload apply.
  import { base } from '$app/paths';
  import * as api from '$lib/api';
  import { toasts } from '$lib/app.svelte';
  import { fmtInt } from '$lib/format';
  import { INGEST_STATUS, pageList, reviewHref } from '$lib/review';
  import * as review from '$lib/review-api';

  let {
    ds,
    canWrite = false,
    canAdmin = false,
  }: { ds: string; canWrite?: boolean; canAdmin?: boolean } = $props();

  const ACCEPT = '.md,.markdown,.txt,.html,.htm,.pdf,.csv,.tsv';

  /** null until the first listing; false when the server has no ingestion. */
  let available = $state<boolean | null>(null);
  let caps = $state({ pdf: false, ocr: false });
  let tasks = $state<review.IngestTask[]>([]);
  let file = $state<File | null>(null);
  let mode = $state<review.IngestMode>('branch');
  let allowPartial = $state(false);
  let busy = $state(false);
  /** The task this panel follows. */
  let task = $state<review.IngestTask | null>(null);
  /** The mapping draft as editable JSON, and what its dry run said. */
  let mappingText = $state('');
  let dryRun = $state<string | null>(null);
  let input: HTMLInputElement | undefined = $state();
  let poll: AbortController | null = null;

  async function list() {
    try {
      const l = await review.ingestTasks(ds);
      available = true;
      caps = l.capabilities;
      tasks = l.tasks.slice(0, 5);
    } catch (e) {
      if (e instanceof api.ApiError && (e.status === 404 || e.status === 405)) available = false;
      else available = true;
    }
  }

  $effect(() => {
    if (ds) void list();
    return () => poll?.abort();
  });

  /** Follow `t` until it ends or waits for the caller. */
  async function follow(t: review.IngestTask) {
    poll?.abort();
    const ctl = new AbortController();
    poll = ctl;
    task = t;
    mappingText = t.result?.mapping ? JSON.stringify(t.result.mapping, null, 2) : '';
    dryRun = null;
    try {
      while (task && review.running(task) && !ctl.signal.aborted) {
        const next = await review.ingestTask(ds, task.id, 20, ctl.signal);
        if (ctl.signal.aborted) return;
        task = next;
      }
      if (task?.result?.mapping) mappingText = JSON.stringify(task.result.mapping, null, 2);
    } catch (e) {
      if (!(e instanceof DOMException && e.name === 'AbortError'))
        toasts.error('Lost track of the ingestion', e);
    } finally {
      if (poll === ctl) poll = null;
      void list();
    }
  }

  async function start(partial = allowPartial) {
    if (!file) return;
    busy = true;
    try {
      const t = await review.startIngest(ds, file, {
        mode,
        allowPartial: partial || undefined,
      });
      void follow(t);
    } catch (e) {
      toasts.error('The ingestion did not start', e);
    } finally {
      busy = false;
    }
  }

  async function act(what: 'confirm' | 'approve' | 'cancel') {
    if (!task) return;
    busy = true;
    try {
      if (what === 'cancel') {
        await review.cancelIngest(ds, task.id);
        task = await review.ingestTask(ds, task.id, 5);
      } else {
        const t =
          what === 'confirm'
            ? await review.confirmIngest(ds, task.id)
            : await review.approveIngest(ds, task.id);
        if (what === 'approve') toasts.push('success', 'The facts were written to main');
        void follow(t);
      }
    } catch (e) {
      toasts.error(what === 'approve' ? 'The preview was not written' : `The ${what} failed`, e);
    } finally {
      busy = false;
    }
  }

  /** Upload the table with the drafted mapping, as a dry run or for real. */
  async function applyMapping(dry: boolean) {
    if (!file || !mappingText.trim()) return;
    try {
      JSON.parse(mappingText);
    } catch (e) {
      toasts.error('The mapping is not JSON', e);
      return;
    }
    busy = true;
    try {
      const mapping = new File([mappingText], 'mapping.csv-metadata.json', {
        type: 'application/json',
      });
      const r = await api.upload(ds, [file], { tables: { mapping }, dryRun: dry });
      const commit = (r as { commit?: { inserted?: number } }).commit;
      const n =
        typeof r === 'object' ? (commit?.inserted ?? r.tables?.[0]?.triples ?? r.count ?? 0) : 0;
      if (dry) dryRun = `The dry run would add ${fmtInt(n)} triples.`;
      else {
        toasts.push('success', `Uploaded the table with its mapping`);
        dryRun = null;
      }
    } catch (e) {
      toasts.error(dry ? 'The dry run failed' : 'The upload failed', e);
    } finally {
      busy = false;
    }
  }

  const pct = $derived(task ? Math.round(task.progress * 100) : 0);
  const r = $derived(task?.result);
  const err = $derived(task?.error);
  const label = (t: review.IngestTask) => t.input.name ?? t.input.url ?? t.id;
</script>

{#if available}
  <section class="panel" aria-label="Ingest" hidden={!canWrite}>
    <div class="panel-head"><h2>Ingest a document</h2></div>
    <div class="panel-body ingest">
      <p class="faint small">
        Markdown, HTML, text{caps.pdf ? ', PDF' : ''}, CSV or TSV. A document becomes a source on a
        review branch with the facts the dataset's model proposes; a table gets a mapping draft.{caps.pdf &&
        !caps.ocr
          ? ' Scanned PDF pages are not read.'
          : ''}
      </p>
      <div class="row">
        <input
          bind:this={input}
          type="file"
          accept={ACCEPT}
          aria-label="Document to ingest"
          onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)}
        />
      </div>
      <div class="row small">
        <label
          >Mode <select class="input sm" bind:value={mode} aria-label="Review mode">
            <option value="branch">Review branch</option>
            <option value="preview">Preview, then approve</option>
            {#if canAdmin}<option value="auto">Automatic when every check passes</option>{/if}
          </select></label
        >
        {#if caps.pdf}
          <label><input type="checkbox" bind:checked={allowPartial} /> Skip unreadable pages</label>
        {/if}
        <span class="spacer"></span>
        <button class="btn primary sm" disabled={!file || busy} onclick={() => start()}
          >Ingest</button
        >
      </div>

      {#if task}
        <div class="task" role="group" aria-label="Ingestion task">
          <div class="row">
            <strong>{INGEST_STATUS[task.status] ?? task.status}</strong>
            <span class="faint small mono">{label(task)}</span>
            <span class="spacer"></span>
            {#if review.running(task) || task.status === 'awaiting-confirmation'}
              <button class="btn sm" disabled={busy} onclick={() => act('cancel')}>Cancel</button>
            {/if}
          </div>
          {#if review.running(task)}
            <div
              class="bar"
              role="progressbar"
              aria-label="Progress"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={pct}
            >
              <span style:width="{pct}%"></span>
            </div>
          {/if}
          {#if task.message && task.status !== 'failed'}<p class="msg">{task.message}</p>{/if}

          {#if task.estimate && task.status === 'awaiting-confirmation'}
            <p class="msg">
              The extraction would take about {fmtInt(task.estimate.tokens)} tokens{task.estimate
                .estimatedCost != null
                ? ` (about $${task.estimate.estimatedCost.toFixed(4)})`
                : ''} over {task.estimate.chunks} chunk{task.estimate.chunks === 1 ? '' : 's'},
              above the dataset's threshold of {fmtInt(task.estimate.threshold ?? 0)}.
            </p>
            <div class="row">
              <button class="btn primary sm" disabled={busy} onclick={() => act('confirm')}
                >Go on</button
              >
            </div>
          {/if}

          {#if err}
            <div class="error-box" role="alert">
              <strong>{err.code}</strong>
              {err.message}
              {#if err.pages?.length}
                <ul class="pages">
                  {#each err.pages as p (p.page)}
                    <li>Page {p.page}: {p.reasons.join(', ')}</li>
                  {/each}
                </ul>
              {/if}
            </div>
            {#if err.code === 'needs-ocr' && file}
              <div class="row">
                <button class="btn sm" disabled={busy} onclick={() => start(true)}
                  >Ingest the readable pages</button
                >
              </div>
            {/if}
          {/if}

          {#if r && r.outcome === 'mapping-draft'}
            <p class="msg">
              Mapping draft ({r.drafted === 'model' ? 'by the model' : 'default'}): {fmtInt(
                r.rows ?? 0,
              )} rows become {fmtInt(r.triples ?? 0)} triples.
            </p>
            <label class="small"
              >Mapping (CSVW)
              <textarea class="input mono" rows="8" aria-label="Mapping" bind:value={mappingText}
              ></textarea>
            </label>
            {#if r.preview?.triples.length}
              <details>
                <summary class="small">Triples of the first {r.preview.rows} rows</summary>
                <pre class="mono small">{r.preview.triples.slice(0, 30).join('\n')}</pre>
              </details>
            {/if}
            {#if dryRun}<p class="msg">{dryRun}</p>{/if}
            <div class="row">
              <button class="btn sm" disabled={busy || !file} onclick={() => applyMapping(true)}
                >Dry run</button
              >
              <button
                class="btn primary sm"
                disabled={busy || !file}
                onclick={() => applyMapping(false)}>Upload the table</button
              >
            </div>
          {:else if r}
            <p class="msg">
              {#if r.outcome === 'already-registered'}
                This text is already registered; nothing was written.
              {:else if r.outcome === 'merged'}
                {r.proposed} fact{r.proposed === 1 ? '' : 's'} merged into main.
              {:else if r.outcome === 'approved'}
                Written to main.
              {:else if r.proposed != null}
                {r.proposed} fact{r.proposed === 1 ? '' : 's'} proposed{r.entities
                  ? `, ${r.entities.linked} linked and ${r.entities.new} new entities`
                  : ''}{r.failed?.length ? `, ${r.failed.length} failed their checks` : ''}.
              {:else}
                Registered as a source{r.length ? ` of ${fmtInt(r.length)} characters` : ''}.
              {/if}
            </p>
            {#if r.pages?.length}
              <p class="msg">
                {r.pages.length} page{r.pages.length === 1 ? '' : 's'}{r.omittedPages?.length
                  ? `; page ${pageList(r.omittedPages)} left out`
                  : ''}{r.ocrPages?.length ? `; page ${pageList(r.ocrPages)} read by OCR` : ''}.
              </p>
            {/if}
            {#if r.autoFallback}<p class="msg">Kept for review: {r.autoFallback}.</p>{/if}
            {#each r.notes ?? [] as n (n)}<p class="msg">{n}</p>{/each}
            <div class="row">
              {#if r.branch}
                <a class="btn primary sm" href={reviewHref(base, ds, r.branch)}>Review proposals</a>
              {/if}
              {#if task.status === 'awaiting-approval'}
                <button class="btn primary sm" disabled={busy} onclick={() => act('approve')}
                  >Approve and write to main</button
                >
              {/if}
            </div>
          {/if}
          {#if task.usage?.modelCalls}
            <p class="faint small">
              {task.usage.modelCalls} model call{task.usage.modelCalls === 1 ? '' : 's'},
              {fmtInt((task.usage.inputTokens ?? 0) + (task.usage.outputTokens ?? 0))} tokens
            </p>
          {/if}
        </div>
      {/if}

      {#if tasks.length}
        <h3>Recent</h3>
        <ul class="recent">
          {#each tasks as t (t.id)}
            <li>
              <button class="link" onclick={() => follow(t)}>{label(t)}</button>
              <span class="faint small">{INGEST_STATUS[t.status] ?? t.status}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </section>
{/if}

<style>
  .ingest {
    display: grid;
    gap: 8px;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .task {
    display: grid;
    gap: 6px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
    min-width: 0;
  }
  .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--spark);
    transition: width 0.3s;
  }
  .msg {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .pages {
    margin: 4px 0 0;
    padding-left: 18px;
  }
  textarea {
    width: 100%;
    box-sizing: border-box;
  }
  pre {
    overflow-x: auto;
    margin: 4px 0 0;
  }
  h3 {
    margin: 6px 0 0;
    font-size: var(--fs-sm);
  }
  .recent {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .recent li {
    display: flex;
    gap: 8px;
    align-items: baseline;
    min-width: 0;
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    color: var(--accent, inherit);
    cursor: pointer;
    text-decoration: underline;
    overflow-wrap: anywhere;
    text-align: left;
  }
</style>
