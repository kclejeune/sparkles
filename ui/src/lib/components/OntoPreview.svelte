<script lang="ts">
  // The preview of an ontology edit (docs/specs/U12-ontology-workbench/SPEC.md): the edit
  // runs as a dry run with `If-Match` naming the commit the editor shows, the dialog lists
  // the axioms it adds and removes and the triples that would change, and Commit writes it
  // with the same precondition, so another writer's commit in between is refused. An undo or
  // a redo is a revert of one commit, which expects the head the editor shows instead.
  import { ApiError, errorMessage } from '$lib/api';
  import { splitTarget } from '$lib/branches';
  import { fmtCount, fmtInt } from '$lib/format';
  import {
    isPreview,
    OUTCOME,
    opAxioms,
    runOp,
    type AxiomInput,
    type Op,
    type OpAnswer,
    type Selection,
  } from '$lib/ontology';
  import type { WritePreview } from '$lib/validation-overlay';
  import DiffLines from './DiffLines.svelte';
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    title,
    target,
    sel,
    op,
    etag,
    head,
    pin,
    describe,
    oncommitted,
    onstale,
    disabled = false,
  }: {
    open?: boolean;
    title: string;
    target: string;
    sel: Selection;
    op: Op | null;
    etag: string | null;
    /** The head commit the editor shows, which a revert expects. */
    head: number | null;
    /** Captured synchronously when the edit was accepted, before reactive refresh. */
    pin?: {
      op: Op;
      target: string;
      sel: Selection;
      etag: string | null;
      head: number | null;
      sourceId?: string;
    } | null;
    /** An axiom as the editor shows it. */
    describe: (a: AxiomInput) => string;
    oncommitted: (op: Op, a: OpAnswer, message: string) => void;
    /** The precondition failed: another commit landed since the editor loaded. */
    onstale: () => void;
    /** The origin is no longer writable or its read is stale. */
    disabled?: boolean;
  } = $props();

  let preview = $state<WritePreview | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let committing = $state(false);
  let message = $state('');
  /** The dry run of a revert found that the head moved on. */
  let movedOn = $state(false);
  let ran: Op | null = null;
  let dryCtl: AbortController | null = null;
  /** The tag and head of the state the dry run read, which the commit names too. */
  let tag: string | null = null;
  let at: number | null = null;
  let sourceId: string | undefined;
  /** The target and graphs the dry run ran on, which the commit writes to. */
  let ranTarget = '';
  let ranSel: Selection = { graph: null, imports: true };
  /** A refusal because another commit landed: `412`, or a revert's `409` `head-moved`. */
  const isStale = (e: unknown) =>
    e instanceof ApiError && (e.status === 412 || (e.status === 409 && e.code === 'head-moved'));
  /** A refusal for want of access says so first, then gives the server's reason. */
  const problemOf = (e: unknown) =>
    e instanceof ApiError && e.status === 403
      ? `You can read ${splitTarget(ranTarget || target).name} but not write to it${
          ranSel.graph ? ` in the graph ${ranSel.graph}` : ''
        }. ${errorMessage(e)}`
      : errorMessage(e);

  $effect(() => {
    if (!open || !op || ran === op) return;
    ran = op;
    const origin = pin?.op === op ? pin : null;
    tag = origin ? origin.etag : etag;
    at = origin ? origin.head : head;
    sourceId = origin?.sourceId;
    ranTarget = origin ? origin.target : target;
    ranSel = { ...(origin ? origin.sel : sel) };
    message = title;
    void dry(op);
  });

  $effect(() => {
    if (!open) {
      ran = null;
      dryCtl?.abort();
    }
  });

  $effect(() => () => dryCtl?.abort());

  async function dry(o: Op) {
    dryCtl?.abort();
    const ctl = new AbortController();
    dryCtl = ctl;
    preview = null;
    error = null;
    movedOn = false;
    busy = true;
    try {
      const r = await runOp(ranTarget, ranSel, o, {
        dryRun: true,
        etag: tag,
        head: at,
        sourceId,
        signal: ctl.signal,
      });
      if (ran !== o || ctl.signal.aborted) return;
      if (isPreview(r)) preview = r;
      else error = 'The server answered a dry run without its preview.';
    } catch (e) {
      if (ran !== o || ctl.signal.aborted) return;
      if (isStale(e)) movedOn = true;
      else error = problemOf(e);
    } finally {
      if (ran === o) busy = false;
    }
  }

  async function commit() {
    if (!op || ran !== op || disabled || !canCommit) return;
    const operation = op;
    // the page moved to another dataset, branch or graph since the dry run
    if (ranTarget !== target || ranSel.graph !== sel.graph) {
      open = false;
      return;
    }
    busy = true;
    committing = true;
    error = null;
    try {
      const r = await runOp(ranTarget, ranSel, operation, {
        etag: tag,
        head: at,
        sourceId,
        message: message.trim() || title,
      });
      if (ran !== operation || !open) return;
      if (isPreview(r)) throw new Error('The server answered the write as a dry run.');
      open = false;
      oncommitted(operation, r, message.trim() || title);
    } catch (e) {
      if (ran !== operation || !open) return;
      if (isStale(e)) {
        open = false;
        onstale();
      } else error = problemOf(e);
    } finally {
      committing = false;
      if (ran === operation) busy = false;
    }
  }

  const lists = $derived(op ? opAxioms(op) : { add: [], remove: [] });
  const stale = $derived(movedOn || preview?.outcome === 'precondition-failed');
  const canCommit = $derived(!!preview && preview.outcome === 'commit' && !busy && !disabled);
  /** An undo or redo whose changes the head no longer holds, which only leaves the list. */
  const spent = $derived(op?.kind === 'revert' && preview?.outcome === 'no-change');
</script>

<Modal bind:open {title} width={620} closeDisabled={committing}>
  <div class="preview" aria-live="polite">
    {#if op?.kind === 'axioms'}
      {#if lists.remove.length}
        <h3>Removes</h3>
        <ul class="axioms del">
          {#each lists.remove as a, i (i)}<li>{describe(a)}</li>{/each}
        </ul>
      {/if}
      {#if lists.add.length}
        <h3>Adds</h3>
        <ul class="axioms ins">
          {#each lists.add as a, i (i)}<li>{describe(a)}</li>{/each}
        </ul>
      {/if}
    {:else if op?.kind === 'removeEntity'}
      <p>
        Removes the entity with its declarations, the axioms that mention it and the assertions
        about it.
      </p>
    {:else if op?.kind === 'revert'}
      <p>
        Reverts commit {op.commit}, which restores exactly the triples that commit changed. A later
        commit that changed the same triples makes the revert conflict.
      </p>
      {#if spent}
        <p class="warn-text">
          The branch no longer holds the changes of commit {op.commit}, so there is nothing to
          revert. The entry can only leave the list.
        </p>
      {/if}
    {/if}

    {#if busy && !preview}
      <p class="faint"><span class="spinner"></span> Running the edit as a dry run…</p>
    {/if}
    {#if movedOn}
      <p class="outcome warn-text" data-outcome="precondition-failed">
        <strong>{OUTCOME['precondition-failed']}</strong>
      </p>
      <p class="warn-text">Reload the editor to see the new state, then undo again.</p>
    {/if}
    {#if preview}
      <p
        class="outcome"
        class:warn-text={preview.outcome !== 'commit'}
        data-outcome={preview.outcome}
      >
        <strong>{OUTCOME[preview.outcome] ?? preview.outcome}</strong>
        {#if preview.commit && preview.outcome === 'commit'}
          {fmtCount(preview.commit.inserted, 'triple')} added, {fmtInt(preview.commit.deleted)} removed.
        {/if}
      </p>
      {#if stale}
        <p class="warn-text">Reload the editor to see the new state, then make the edit again.</p>
      {/if}
      {#if preview.validation}
        <p class="faint">Write-time validation: {preview.validation.status ?? 'ran'}.</p>
      {/if}
      {#if preview.changes?.quads.length}
        <DiffLines
          quads={preview.changes.quads}
          total={preview.changes.total}
          label="Triples the edit would change"
          maxHeight={200}
          ds={target}
        />
      {/if}
    {/if}
    <!-- the request as sent, collapsed below the rendered change set -->
    {#if op?.kind === 'update'}
      <details class="raw">
        <summary>SPARQL update</summary>
        <pre class="sparql">{op.sparql}</pre>
      </details>
    {:else if op?.kind === 'patch'}
      <details class="raw">
        <summary>RDF changes</summary>
        <pre class="sparql">{op.patch}</pre>
      </details>
    {/if}
    {#if error}<p class="error-box" role="alert">{error}</p>{/if}
    {#if preview && !stale}
      <label class="msg">
        <span>Commit message</span>
        <input class="input" bind:value={message} maxlength="500" />
      </label>
    {/if}
  </div>
  {#snippet actions()}
    <button class="btn" disabled={committing} onclick={() => (open = false)}>Cancel</button>
    {#if stale}
      <button
        class="btn primary"
        onclick={() => {
          open = false;
          onstale();
        }}>Reload</button
      >
    {:else if spent && op}
      <button
        class="btn primary"
        onclick={() => {
          const o = op;
          open = false;
          if (o) oncommitted(o, { inserted: 0, deleted: 0 }, message.trim() || title);
        }}>Remove from the list</button
      >
    {:else}
      <button class="btn primary" disabled={!canCommit} onclick={commit}>
        {busy && preview ? 'Committing…' : 'Commit'}
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  .preview {
    display: grid;
    gap: 10px;
    min-width: 0;
  }
  h3 {
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: 600;
  }
  .axioms {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 2px;
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    max-height: 160px;
    overflow: auto;
  }
  .axioms li {
    padding: 2px 6px;
    border-radius: var(--r-sm);
    overflow-wrap: anywhere;
  }
  .axioms.ins li {
    background: var(--added-soft);
  }
  .axioms.del li {
    background: var(--removed-soft);
  }
  .sparql {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 160px;
    overflow: auto;
    margin: 6px 0 0;
  }
  .msg {
    display: grid;
    gap: 4px;
    font-size: var(--fs-sm);
    color: var(--text-2);
  }
</style>
